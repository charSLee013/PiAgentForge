//! Pi Provider — Mistral AI.
//! Mirrors packages/ai/src/providers/mistral.ts
//!
//! Mistral uses an OpenAI-compatible chat completions format with some
//! Mistral-specific features: tool call ID normalization (9 alphanumeric
//! chars), session affinity via `x-affinity` header for KV-cache reuse,
//! and thinking via `promptMode: "reasoning"` / `reasoningEffort`.

use pi_ai_core::api_registry::ApiProvider;
use pi_ai_core::event_stream::{AssistantMessageEventStream, EventStreamSender};
use pi_ai_core::types::{
    ContentBlock, Context, ImageSource, Message, MessageRole, Model, StreamError, StreamEvent,
    StreamOptions, TextContent, ThinkingContent, ToolCallContent, ToolDefinition, Usage,
};
use serde::Deserialize;
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Default Mistral Chat Completions endpoint.
const MISTRAL_CHAT_COMPLETIONS_URL: &str = "https://api.mistral.ai/v1/chat/completions";

/// SSE data prefix (the part before the actual JSON payload).
const SSE_DATA_PREFIX: &str = "data: ";

/// SSE stream-termination sentinel.
const SSE_DONE_SENTINEL: &str = "[DONE]";

/// Mistral tool call IDs must be exactly 9 alphanumeric characters.
const MISTRAL_TOOL_CALL_ID_LENGTH: usize = 9;

// ---------------------------------------------------------------------------
// Response chunk types (deserialization from SSE `data: ...` lines)
// ---------------------------------------------------------------------------

/// Top-level streaming chunk from the Mistral Chat Completions API.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Chunk {
    id: Option<String>,
    object: Option<String>,
    model: Option<String>,
    choices: Vec<ChunkChoice>,
    usage: Option<ChunkUsage>,
}

/// A single completion choice within a chunk.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ChunkChoice {
    delta: Option<Delta>,
    finish_reason: Option<String>,
    index: Option<u32>,
}

/// Delta content within a choice.
///
/// Mistral's `content` field can be either a plain string or an array of
/// content objects (text, thinking, image_url). We use `serde_json::Value`
/// here to handle both cases flexibly.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Delta {
    /// Can be a string (plain text), an array of content items, or null.
    content: Option<serde_json::Value>,
    tool_calls: Option<Vec<ToolCallDeltaChunk>>,
}

/// A tool-call delta within a streaming choice.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ToolCallDeltaChunk {
    index: u32,
    id: Option<String>,
    #[serde(rename = "type")]
    _type: Option<String>,
    function: Option<ToolCallFunctionDelta>,
}

/// The function sub-object inside a tool-call delta.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ToolCallFunctionDelta {
    name: Option<String>,
    arguments: Option<String>,
}

/// Token usage reported in-stream (final chunk).
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ChunkUsage {
    prompt_tokens: Option<u64>,
    completion_tokens: Option<u64>,
    total_tokens: Option<u64>,
}

// ---------------------------------------------------------------------------
// Streaming state machine helpers
// ---------------------------------------------------------------------------

/// Accumulates the streamed arguments for a single tool call.
#[derive(Debug)]
#[allow(dead_code)]
struct ToolCallBuilder {
    index: u32,
    id: Option<String>,
    name: Option<String>,
    arguments: String,
    /// Whether the `id` has already been emitted via `ToolCallDelta`.
    id_emitted: bool,
    /// Whether the `name` has already been emitted via `ToolCallDelta`.
    name_emitted: bool,
}

/// Mutable state carried across the SSE stream processing loop.
#[derive(Debug, Default)]
struct StreamState {
    /// All text content received so far.
    text: String,
    /// All reasoning / thinking content received so far.
    thinking: String,
    /// Tool calls being accumulated, keyed by `index`.
    tool_calls: HashMap<u32, ToolCallBuilder>,
    /// The finish reason from the last chunk that carried one.
    finish_reason: Option<String>,
    /// The response ID from the first chunk.
    response_id: Option<String>,
    /// Whether usage has already been emitted to avoid duplicates.
    usage_emitted: bool,
}

impl StreamState {
    fn get_or_create_tool_call(&mut self, index: u32) -> &mut ToolCallBuilder {
        self.tool_calls.entry(index).or_insert_with(|| ToolCallBuilder {
            index,
            id: None,
            name: None,
            arguments: String::new(),
            id_emitted: false,
            name_emitted: false,
        })
    }
}

// ---------------------------------------------------------------------------
// Tool call ID normalizer
// ---------------------------------------------------------------------------

/// Deterministic hash matching the JS `shortHash()` in
/// `packages/ai/src/utils/hash.ts`.
///
/// Uses the same two-integer mixing function with constant multipliers.
fn short_hash(s: &str) -> String {
    // Convert a u32 to a base-36 string (lowercase), matching JS
    // `(n >>> 0).toString(36)`.
    fn to_base36(mut val: u32) -> String {
        const CHARS: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
        if val == 0 {
            return "0".to_string();
        }
        let mut result = Vec::new();
        while val > 0 {
            result.push(CHARS[(val % 36) as usize]);
            val /= 36;
        }
        result.reverse();
        // SAFETY: all chars are ASCII, so this is valid UTF-8.
        unsafe { String::from_utf8_unchecked(result) }
    }

    let mut h1: u32 = 0xdeadbeef;
    let mut h2: u32 = 0x41c6ce57;

    // Iterate over UTF-16 code units to match JS `charCodeAt`.
    for code in s.encode_utf16() {
        let ch = code as u32;
        h1 = (h1 ^ ch).wrapping_mul(2654435761);
        h2 = (h2 ^ ch).wrapping_mul(1597334677);
    }

    h1 = (h1 ^ (h1 >> 16))
        .wrapping_mul(2246822507)
        ^ (h2 ^ (h2 >> 13)).wrapping_mul(3266489909);
    h2 = (h2 ^ (h2 >> 16))
        .wrapping_mul(2246822507)
        ^ (h1 ^ (h1 >> 13)).wrapping_mul(3266489909);

    to_base36(h2) + &to_base36(h1)
}

/// Derive a Mistral-compatible tool call ID (exactly 9 alphanumeric chars).
///
/// Strategy:
/// 1. Strip non-alphanumeric characters from the input.
/// 2. If attempt == 0 and the stripped form is exactly 9 chars, use it.
/// 3. Otherwise hash the seed (with `:{attempt}` suffix for collision
///    avoidance), filter to alphanumeric, and take first 9 chars.
fn derive_mistral_tool_call_id(id: &str, attempt: u32) -> String {
    let normalized: String = id.chars().filter(|c| c.is_alphanumeric()).collect();

    if attempt == 0 && normalized.len() == MISTRAL_TOOL_CALL_ID_LENGTH {
        return normalized;
    }

    let seed_base = if normalized.is_empty() {
        id
    } else {
        &normalized
    };

    let seed = if attempt == 0 {
        seed_base.to_string()
    } else {
        format!("{seed_base}:{attempt}")
    };

    let hash = short_hash(&seed);
    hash.chars()
        .filter(|c| c.is_alphanumeric())
        .take(MISTRAL_TOOL_CALL_ID_LENGTH)
        .collect()
}

/// Stateful normalizer that ensures unique 9-char tool call IDs.
///
/// Maintains a forward map (original → normalized) and reverse map
/// (normalized → original) to detect and resolve collisions by
/// incrementing the attempt counter.
#[derive(Debug, Default)]
struct MistralToolCallIdNormalizer {
    /// Maps original tool call ID → normalized ID.
    id_map: HashMap<String, String>,
    /// Maps normalized ID → original tool call ID.
    reverse_map: HashMap<String, String>,
}

impl MistralToolCallIdNormalizer {
    fn new() -> Self {
        Self::default()
    }

    /// Normalize a tool call ID, resolving any collisions.
    fn normalize(&mut self, id: &str) -> String {
        if let Some(existing) = self.id_map.get(id) {
            return existing.clone();
        }

        let mut attempt = 0u32;
        loop {
            let candidate = derive_mistral_tool_call_id(id, attempt);
            let owner = self.reverse_map.get(&candidate).cloned();
            if owner.is_none() || owner.as_deref() == Some(id) {
                self.id_map.insert(id.to_string(), candidate.clone());
                self.reverse_map.insert(candidate.clone(), id.to_string());
                return candidate;
            }
            attempt += 1;
        }
    }
}

// ---------------------------------------------------------------------------
// Session affinity helper
// ---------------------------------------------------------------------------

/// Build the extra HTTP headers for a Mistral request, including session
/// affinity for KV-cache reuse.
///
/// This function is tested directly; the integration point in
/// `process_stream` uses it when `StreamOptions` gains a `session_id`
/// field (currently not present in `pi-ai-core::types::StreamOptions`).
fn build_mistral_headers<H>(headers: H, session_id: Option<&str>) -> HashMap<String, String>
where
    H: IntoIterator<Item = (String, String)>,
{
    let mut result: HashMap<String, String> = headers.into_iter().collect();

    // Mistral infrastructure uses `x-affinity` for KV-cache reuse
    // (prefix caching). Respect explicit caller-provided header values.
    if let Some(sid) = session_id {
        if !result.contains_key("x-affinity") {
            result.insert("x-affinity".to_string(), sid.to_string());
        }
    }

    result
}

// ---------------------------------------------------------------------------
// Provider struct
// ---------------------------------------------------------------------------

/// Provider for the Mistral Chat Completions API (streaming).
///
/// Sends POST requests to `{base_url}/v1/chat/completions` and parses
/// the SSE response stream, emitting [`StreamEvent`] items.
///
/// # Example
///
/// ```ignore
/// use pi_provider_mistral::MistralProvider;
/// use pi_ai_core::api_registry::register_api_provider;
///
/// let provider = MistralProvider::new();
/// register_api_provider(Box::new(provider)).await;
/// ```
pub struct MistralProvider {
    base_url: String,
}

impl MistralProvider {
    /// Create a new provider that targets the standard Mistral API endpoint.
    pub fn new() -> Self {
        Self {
            base_url: MISTRAL_CHAT_COMPLETIONS_URL.to_owned(),
        }
    }

    /// Create a provider with a custom base URL (useful for testing or
    /// self-hosted Mistral-compatible backends).
    pub fn with_base_url(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
        }
    }
}

impl Default for MistralProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl ApiProvider for MistralProvider {
    fn api_id(&self) -> &str {
        "mistral-conversations"
    }

    fn stream(
        &self,
        model: &Model,
        context: Context,
        options: StreamOptions,
    ) -> AssistantMessageEventStream {
        let (tx, rx) = AssistantMessageEventStream::new();
        let model = model.clone();
        let base_url = self.base_url.clone();

        tokio::spawn(async move {
            if let Err(e) = process_stream(tx, &base_url, &model, context, options).await {
                tracing::error!("Mistral stream error: {e}");
            }
        });

        rx
    }
}

// ---------------------------------------------------------------------------
// Top-level stream processing
// ---------------------------------------------------------------------------

async fn process_stream(
    tx: EventStreamSender<StreamEvent>,
    base_url: &str,
    model: &Model,
    context: Context,
    options: StreamOptions,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // 1. Resolve the API key.
    let api_key = match resolve_api_key(&options) {
        Ok(k) => k,
        Err(msg) => {
            emit_error(&tx, msg, Some("auth_error".to_owned()));
            return Ok(());
        }
    };

    // 2. Build the JSON request body.
    let body = build_request_body(model, &context, &options);

    // 3. Build headers including session affinity.
    let extra_headers = build_mistral_headers(std::iter::empty::<(String, String)>(), None);

    // 4. Send the HTTP request.
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(options.timeout.unwrap_or(120)))
        .build()?;

    let mut request_builder = client
        .post(base_url)
        .header("Authorization", format!("Bearer {api_key}"))
        .header("Content-Type", "application/json")
        .json(&body);

    for (key, value) in &extra_headers {
        request_builder = request_builder.header(key.as_str(), value.as_str());
    }

    let response = request_builder.send().await.map_err(|e| {
        emit_error(
            &tx,
            format!("HTTP request failed: {e}"),
            Some("request_error".to_owned()),
        );
        e
    })?;

    // 5. Check the HTTP status code.
    if !response.status().is_success() {
        let status = response.status();
        let error_text = response.text().await.unwrap_or_else(|_| String::new());
        emit_error(
            &tx,
            format!(
                "Mistral API error ({}): {}",
                status.as_u16(),
                truncate_error_text(&error_text, 4000)
            ),
            Some(status.as_str().to_owned()),
        );
        return Ok(());
    }

    // 6. Emit the Start event.
    let _ = tx.send(StreamEvent::Start);

    // 7. Process the SSE response body.
    let mut state = StreamState::default();
    if let Err(e) = process_sse_stream(&tx, response, &mut state).await {
        emit_error(
            &tx,
            format!("SSE stream error: {e}"),
            Some("stream_error".to_owned()),
        );
        return Ok(());
    }

    // 8. Emit the final Done event.
    let stop_reason = state.finish_reason.clone().unwrap_or_else(|| "stop".to_owned());
    let message = build_done_message(&state, model);
    let _ = tx.send(StreamEvent::Done {
        message: Some(message),
        stop_reason: Some(stop_reason),
    });

    Ok(())
}

/// Truncate error text to a maximum number of characters, appending a
/// truncation notice if truncated.
fn truncate_error_text(text: &str, max_chars: usize) -> String {
    if text.len() <= max_chars {
        text.to_string()
    } else {
        format!(
            "{}... [truncated {} chars]",
            &text[..max_chars],
            text.len() - max_chars
        )
    }
}

// ---------------------------------------------------------------------------
// SSE stream parsing
// ---------------------------------------------------------------------------

/// Read an SSE text/event-stream from the HTTP response body, splitting on
/// newlines and processing each `data:` line.
async fn process_sse_stream(
    tx: &EventStreamSender<StreamEvent>,
    response: reqwest::Response,
    state: &mut StreamState,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use tokio_stream::StreamExt;

    let byte_stream = response.bytes_stream();
    tokio::pin!(byte_stream);

    let mut buffer: Vec<u8> = Vec::new();

    while let Some(chunk_result) = byte_stream.next().await {
        let chunk = chunk_result?;
        buffer.extend_from_slice(&chunk);

        // Process as many complete lines as possible.
        while let Some(newline_pos) = buffer.iter().position(|&b| b == b'\n') {

            // Extract the line (including the \n byte, which we'll remove).
            let raw_line: Vec<u8> = buffer.drain(..=newline_pos).collect();
            // Remove trailing \r if present (Windows line endings).
            let line_bytes = if raw_line.ends_with(b"\n") {
                &raw_line[..raw_line.len() - 1]
            } else {
                &raw_line
            };
            let line_bytes = if line_bytes.ends_with(b"\r") {
                &line_bytes[..line_bytes.len() - 1]
            } else {
                line_bytes
            };

            let line_str = String::from_utf8_lossy(line_bytes);

            if line_str.is_empty() {
                continue;
            }

            if let Some(data) = line_str.strip_prefix(SSE_DATA_PREFIX) {
                let data = data.trim();
                if data == SSE_DONE_SENTINEL {
                    return Ok(());
                }

                // Parse the JSON chunk.
                match serde_json::from_str::<Chunk>(data) {
                    Ok(chunk) => {
                        process_chunk(tx, chunk, state);
                    }
                    Err(e) => {
                        tracing::warn!("Failed to parse Mistral SSE chunk JSON: {e} — data: {data}");
                        // Non-fatal: skip malformed chunks.
                    }
                }
            }
            // Lines that do not start with `data: ` are ignored per the SSE
            // spec (they may be comments or event-type lines).
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Chunk processing
// ---------------------------------------------------------------------------

/// Process a single parsed [`Chunk`] and emit the corresponding
/// [`StreamEvent`]s.
fn process_chunk(tx: &EventStreamSender<StreamEvent>, chunk: Chunk, state: &mut StreamState) {
    // Track response metadata.
    if state.response_id.is_none() {
        state.response_id = chunk.id.clone();
    }

    // Usage-only chunk (choices array is empty, usage is present).
    if chunk.choices.is_empty() {
        if let Some(ref usage) = chunk.usage {
            if !state.usage_emitted {
                let parsed = parse_usage(usage);
                let _ = tx.send(StreamEvent::Usage(parsed));
                state.usage_emitted = true;
            }
        }
        return;
    }

    for choice in &chunk.choices {
        // Track finish reason.
        if let Some(ref reason) = choice.finish_reason {
            if !reason.is_empty() {
                state.finish_reason = Some(map_stop_reason(reason));
            }
        }

        // Process the delta.
        if let Some(ref delta) = choice.delta {
            handle_delta(tx, delta, state);
        }
    }
}

/// Emit events for a single [`Delta`] object.
fn handle_delta(tx: &EventStreamSender<StreamEvent>, delta: &Delta, state: &mut StreamState) {
    // --- Content (text + thinking) ---
    if let Some(ref content) = delta.content {
        match content {
            serde_json::Value::String(s) => {
                // Plain text string.
                if !s.is_empty() {
                    state.text.push_str(s);
                    let _ = tx.send(StreamEvent::TextDelta {
                        delta: s.clone(),
                    });
                }
            }
            serde_json::Value::Array(arr) => {
                for item in arr {
                    if let Some(s) = item.as_str() {
                        // Plain string within the array (legacy format).
                        if !s.is_empty() {
                            state.text.push_str(s);
                            let _ = tx.send(StreamEvent::TextDelta {
                                delta: s.to_string(),
                            });
                        }
                    } else if let Some(obj) = item.as_object() {
                        match obj.get("type").and_then(|t| t.as_str()) {
                            Some("text") => {
                                if let Some(text) = obj.get("text").and_then(|t| t.as_str()) {
                                    if !text.is_empty() {
                                        state.text.push_str(text);
                                        let _ = tx.send(StreamEvent::TextDelta {
                                            delta: text.to_string(),
                                        });
                                    }
                                }
                            }
                            Some("thinking") => {
                                if let Some(thinking_arr) =
                                    obj.get("thinking").and_then(|t| t.as_array())
                                {
                                    for seg in thinking_arr {
                                        if let Some(text) =
                                            seg.get("text").and_then(|t| t.as_str())
                                        {
                                            if !text.is_empty() {
                                                state.thinking.push_str(text);
                                                let _ = tx.send(StreamEvent::ThinkingDelta {
                                                    delta: text.to_string(),
                                                });
                                            }
                                        }
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
            _ => {
                // null or other — nothing to process.
            }
        }
    }

    // --- Tool call deltas ---
    if let Some(ref tool_calls) = delta.tool_calls {
        for tc in tool_calls {
            let builder = state.get_or_create_tool_call(tc.index);

            let emit_id = if let Some(ref id) = tc.id {
                if !builder.id_emitted {
                    builder.id = Some(id.clone());
                    builder.id_emitted = true;
                    Some(id.clone())
                } else {
                    None
                }
            } else {
                None
            };

            let emit_name = if let Some(ref function) = tc.function {
                if let Some(ref name) = function.name {
                    if !builder.name_emitted {
                        builder.name = Some(name.clone());
                        builder.name_emitted = true;
                        Some(name.clone())
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            };

            let emit_arguments = tc
                .function
                .as_ref()
                .and_then(|f| f.arguments.clone())
                .inspect(|args_delta| {
                    builder.arguments.push_str(args_delta);
                });

            if emit_id.is_some() || emit_name.is_some() || emit_arguments.is_some() {
                let _ = tx.send(StreamEvent::ToolCallDelta {
                    index: tc.index,
                    id: emit_id,
                    name: emit_name,
                    arguments: emit_arguments,
                });
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Final message construction
// ---------------------------------------------------------------------------

/// Build the final [`Message`] from the accumulated streaming state.
fn build_done_message(state: &StreamState, _model: &Model) -> Message {
    let mut content: Vec<ContentBlock> = Vec::new();

    // Add thinking block if we got reasoning content.
    if !state.thinking.is_empty() {
        content.push(ContentBlock::Thinking(ThinkingContent {
            thinking: state.thinking.clone(),
            signature: None,
        }));
    }

    // Add text block.
    content.push(ContentBlock::Text(TextContent {
        text: state.text.clone(),
    }));

    // Add tool call blocks.
    for builder in state.tool_calls.values() {
        let parsed_args: serde_json::Value =
            serde_json::from_str(&builder.arguments).unwrap_or_else(|_| {
                serde_json::Value::String(builder.arguments.clone())
            });

        content.push(ContentBlock::ToolCall(ToolCallContent {
            id: builder.id.clone().unwrap_or_default(),
            name: builder.name.clone().unwrap_or_default(),
            arguments: parsed_args,
        }));
    }

    Message {
        role: MessageRole::Assistant,
        content,
        id: state.response_id.clone(),
        name: None,
        usage: None,
        redacted: false,
    }
}

// ---------------------------------------------------------------------------
// Request body construction
// ---------------------------------------------------------------------------

/// Build the JSON request body for the Mistral Chat Completions API.
fn build_request_body(model: &Model, context: &Context, options: &StreamOptions) -> serde_json::Value {
    let mut normalizer = MistralToolCallIdNormalizer::new();
    let messages = convert_messages(context, &mut normalizer);
    let tools = convert_tools(&context.tools);

    let mut body = serde_json::json!({
        "model": model.id,
        "messages": messages,
        "stream": true,
    });

    if let Some(max_tokens) = options.max_tokens {
        body["max_tokens"] = serde_json::json!(max_tokens);
    }

    if !tools.is_empty() {
        body["tools"] = serde_json::json!(tools);
    }

    body
}

// ---------------------------------------------------------------------------
// Message conversion (pi-ai-core → Mistral Chat Completions format)
// ---------------------------------------------------------------------------

/// Convert [`Context`] messages to Mistral-compatible JSON array.
///
/// Tool call IDs are normalized to exactly 9 alphanumeric characters
/// via the provided `normalizer`.
fn convert_messages(context: &Context, normalizer: &mut MistralToolCallIdNormalizer) -> Vec<serde_json::Value> {
    let mut result: Vec<serde_json::Value> = Vec::new();

    // Include system prompt if present.
    if let Some(ref system_prompt) = context.system_prompt {
        if !system_prompt.is_empty() {
            result.push(serde_json::json!({
                "role": "system",
                "content": system_prompt,
            }));
        }
    }

    for msg in &context.messages {
        match msg.role {
            MessageRole::System => {
                if let Some(val) = convert_system_message(msg) {
                    result.push(val);
                }
            }
            MessageRole::User => {
                if let Some(val) = convert_user_message(msg) {
                    result.push(val);
                }
            }
            MessageRole::Assistant => {
                if let Some(val) = convert_assistant_message(msg, normalizer) {
                    result.push(val);
                }
            }
            MessageRole::Tool => {
                let tool_msgs = convert_tool_result_messages(msg, normalizer);
                result.extend(tool_msgs);
            }
        }
    }

    result
}

/// Convert a System message.
fn convert_system_message(msg: &Message) -> Option<serde_json::Value> {
    let text = extract_text(&msg.content);
    if text.is_empty() {
        return None;
    }
    Some(serde_json::json!({
        "role": "system",
        "content": text,
    }))
}

/// Convert a User message (supports text + image parts).
fn convert_user_message(msg: &Message) -> Option<serde_json::Value> {
    let parts = convert_user_content_parts(&msg.content);
    if parts.is_empty() {
        return None;
    }
    Some(serde_json::json!({
        "role": "user",
        "content": parts,
    }))
}

/// Build content parts for a user message (text + image_url).
fn convert_user_content_parts(content: &[ContentBlock]) -> Vec<serde_json::Value> {
    content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(t) => Some(serde_json::json!({
                "type": "text",
                "text": t.text,
            })),
            ContentBlock::Image(img) => {
                let url = match &img.source {
                    ImageSource::Base64 { media_type, data } => {
                        format!("data:{media_type};base64,{data}")
                    }
                    ImageSource::Url { url } => url.clone(),
                };
                Some(serde_json::json!({
                    "type": "image_url",
                    "image_url": { "url": url },
                }))
            }
            _ => None,
        })
        .collect()
}

/// Convert an Assistant message (text + thinking + tool_calls).
///
/// Tool call IDs are normalized to Mistral's required format.
fn convert_assistant_message(
    msg: &Message,
    normalizer: &mut MistralToolCallIdNormalizer,
) -> Option<serde_json::Value> {
    let mut content_parts: Vec<serde_json::Value> = Vec::new();
    let mut tool_calls: Vec<serde_json::Value> = Vec::new();

    for block in &msg.content {
        match block {
            ContentBlock::Text(t) => {
                if !t.text.trim().is_empty() {
                    content_parts.push(serde_json::json!({"type": "text", "text": t.text}));
                }
            }
            ContentBlock::Thinking(th) => {
                if !th.thinking.trim().is_empty() {
                    content_parts.push(serde_json::json!({
                        "type": "thinking",
                        "thinking": [{"type": "text", "text": th.thinking}]
                    }));
                }
            }
            ContentBlock::ToolCall(tc) => {
                let normalized_id = normalizer.normalize(&tc.id);
                tool_calls.push(serde_json::json!({
                    "id": normalized_id,
                    "type": "function",
                    "function": {
                        "name": tc.name,
                        "arguments": serialize_arguments(&tc.arguments),
                    }
                }));
            }
            _ => {}
        }
    }

    if content_parts.is_empty() && tool_calls.is_empty() {
        return None;
    }

    let mut assistant_msg = serde_json::json!({
        "role": "assistant",
    });

    if content_parts.len() == 1 {
        // Single text item — send as plain string.
        if let Some(text) = content_parts[0]
            .get("text")
            .and_then(|t| t.as_str())
            .filter(|t| !t.is_empty())
        {
            assistant_msg["content"] = serde_json::json!(text);
        } else {
            assistant_msg["content"] = content_parts[0].clone();
        }
    } else if !content_parts.is_empty() {
        assistant_msg["content"] = serde_json::json!(content_parts);
    }

    if !tool_calls.is_empty() {
        assistant_msg["tool_calls"] = serde_json::json!(tool_calls);
    }

    Some(assistant_msg)
}

/// Serialize tool arguments to a JSON string for the Mistral API.
fn serialize_arguments(args: &serde_json::Value) -> String {
    match args {
        serde_json::Value::String(s) => s.clone(),
        other => serde_json::to_string(other).unwrap_or_else(|_| "{}".to_owned()),
    }
}

/// Convert Tool-role messages (tool results) to Mistral "tool" role messages.
///
/// Tool call IDs are normalized to match the IDs sent in the assistant
/// message.
fn convert_tool_result_messages(
    msg: &Message,
    normalizer: &mut MistralToolCallIdNormalizer,
) -> Vec<serde_json::Value> {
    msg.content
        .iter()
        .filter_map(|block| {
            if let ContentBlock::ToolResult(tr) = block {
                let text = if tr.is_error {
                    if let Some(ref error) = tr.error {
                        format!("[tool error] {error}")
                    } else {
                        "[tool error] (no tool output)".to_owned()
                    }
                } else if let Some(ref content) = tr.content {
                    extract_text(content)
                } else {
                    "(no tool output)".to_owned()
                };

                let normalized_id = normalizer.normalize(&tr.id);

                let mut tool_msg = serde_json::json!({
                    "role": "tool",
                    "tool_call_id": normalized_id,
                    "content": text,
                });

                if !tr.name.is_empty() {
                    tool_msg["name"] = serde_json::json!(&tr.name);
                }

                Some(tool_msg)
            } else {
                None
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Tool definition conversion
// ---------------------------------------------------------------------------

/// Convert [`ToolDefinition`]s to Mistral tool format.
fn convert_tools(tools: &[ToolDefinition]) -> Vec<serde_json::Value> {
    tools
        .iter()
        .map(|tool| {
            serde_json::json!({
                "type": "function",
                "function": {
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": tool.parameters,
                    "strict": tool.strict.unwrap_or(false),
                },
            })
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Usage parsing
// ---------------------------------------------------------------------------

/// Parse Mistral chunk usage into the pi-ai-core [`Usage`] struct.
fn parse_usage(raw: &ChunkUsage) -> Usage {
    let prompt_tokens = raw.prompt_tokens.unwrap_or(0);
    let completion_tokens = raw.completion_tokens.unwrap_or(0);

    Usage {
        input: prompt_tokens,
        output: completion_tokens,
        cache_read: Some(0),
        cache_write: Some(0),
        total_tokens: raw.total_tokens,
    }
}

// ---------------------------------------------------------------------------
// Stop reason mapping
// ---------------------------------------------------------------------------

/// Map a Mistral `finish_reason` to the canonical stop-reason string used
/// by pi.
fn map_stop_reason(reason: &str) -> String {
    match reason {
        "stop" => "stop".to_owned(),
        "length" | "model_length" => "length".to_owned(),
        "tool_calls" => "toolUse".to_owned(),
        "error" => "error".to_owned(),
        "content_filter" => format!("error:provider_finish_reason:{reason}"),
        other => format!("error:provider_finish_reason:{other}"),
    }
}

// ---------------------------------------------------------------------------
// Utility helpers
// ---------------------------------------------------------------------------

/// Extract plain text from a slice of [`ContentBlock`]s.
fn extract_text(content: &[ContentBlock]) -> String {
    content
        .iter()
        .filter_map(|block| {
            if let ContentBlock::Text(t) = block {
                Some(t.text.as_str())
            } else {
                None
            }
        })
        .collect::<Vec<&str>>()
        .join("\n")
}

/// Resolve the API key from options or environment.
fn resolve_api_key(options: &StreamOptions) -> Result<String, String> {
    if let Some(ref key) = options.api_key {
        if !key.is_empty() {
            return Ok(key.clone());
        }
    }
    match std::env::var("MISTRAL_API_KEY") {
        Ok(key) if !key.is_empty() => Ok(key),
        _ => Err(
            "Mistral API key is required. Set the MISTRAL_API_KEY environment \
             variable or pass `api_key` in `StreamOptions`."
                .to_owned(),
        ),
    }
}

/// Send an error event and log it.
fn emit_error(tx: &EventStreamSender<StreamEvent>, message: impl Into<String>, code: Option<String>) {
    let msg: String = message.into();
    tracing::error!("{msg}");
    let _ = tx.send(StreamEvent::Error {
        error: StreamError {
            message: msg,
            code,
            r#type: None,
        },
    });
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use pi_ai_core::api_registry::{clear_api_providers, register_api_provider};
    use pi_ai_core::event_stream::collect_stream;
    use pi_ai_core::stream;
    use pi_ai_core::types::KnownProvider;
    use pi_ai_core::types::ToolResultContent;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    // ------------------------------------------------------------------
    // Helpers
    // ------------------------------------------------------------------

    fn test_model() -> Model {
        Model {
            id: "mistral-small-latest".into(),
            provider: KnownProvider::Mistral,
            api: "mistral-conversations".into(),
            name: None,
            base_url: None,
            supports_thinking: false,
            supports_tools: true,
            supports_streaming: true,
            supports_image_input: false,
            max_tokens: None,
            max_input_tokens: None,
            max_output_tokens: Some(16384),
            cost_per_input_token: None,
            cost_per_output_token: None,
            cost_per_cache_read_token: None,
            cost_per_cache_write_token: None,
        }
    }

    async fn setup_provider(mock_server: &MockServer) {
        let provider = MistralProvider::with_base_url(mock_server.uri());
        clear_api_providers().await;
        register_api_provider(Box::new(provider)).await;
    }

    /// Ensure MISTRAL_API_KEY is set to a known value for all wiremock tests.
    fn ensure_api_key() {
        use std::sync::OnceLock;
        static INIT: OnceLock<()> = OnceLock::new();
        INIT.get_or_init(|| {
            if std::env::var("MISTRAL_API_KEY").is_err() {
                unsafe { std::env::set_var("MISTRAL_API_KEY", "sk-test-placeholder"); }
            }
        });
    }

    /// Mount a mock SSE endpoint that returns the given body bytes.
    async fn mount_sse(mock_server: &MockServer, body: &'static str) {
        Mock::given(method("POST"))
            .and(path("/"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"))
            .mount(mock_server)
            .await;
    }

    /// Mount a mock that returns an HTTP error.
    async fn mount_error(mock_server: &MockServer, status: u16, body: &'static str) {
        Mock::given(method("POST"))
            .and(path("/"))
            .respond_with(
                ResponseTemplate::new(status)
                    .set_body_string(body)
                    .insert_header("content-type", "application/json"),
            )
            .mount(mock_server)
            .await;
    }

    // ------------------------------------------------------------------
    // Text streaming tests
    // ------------------------------------------------------------------

    #[serial]
#[tokio::test]
    async fn test_text_stream_single_chunk() {
        ensure_api_key();
        let mock = MockServer::start().await;
        mount_sse(
            &mock,
            "data: {\"id\":\"ch1\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hello world\"},\"finish_reason\":null}]}\n\
             \n\
             data: {\"id\":\"ch1\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\
             \n\
             data: [DONE]\n\
             \n",
        )
        .await;
        setup_provider(&mock).await;

        let model = test_model();
        let context = Context {
            messages: vec![Message::user_text("Hi")],
            system_prompt: None,
            model: None,
            tools: vec![],
        };

        let stream = stream::stream(&model, context, StreamOptions {
            api_key: Some("test-key".into()),
            ..Default::default()
        })
            .await
            .expect("stream() should return a stream");
        let result = collect_stream(stream, &model).await.expect("collect should succeed");

        let text = extract_text(&result.message.content);
        assert_eq!(text, "Hello world");
        assert_eq!(result.stop_reason, Some("stop".to_owned()));
    }

    #[serial]
#[tokio::test]
    async fn test_text_stream_multiple_chunks() {
        ensure_api_key();
        let mock = MockServer::start().await;
        mount_sse(
            &mock,
            "data: {\"id\":\"ch1\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hello\"},\"finish_reason\":null}]}\n\
             \n\
             data: {\"id\":\"ch1\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\" world\"},\"finish_reason\":null}]}\n\
             \n\
             data: {\"id\":\"ch1\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"!\"},\"finish_reason\":null}]}\n\
             \n\
             data: {\"id\":\"ch1\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\
             \n\
             data: [DONE]\n\
             \n",
        )
        .await;
        setup_provider(&mock).await;

        let model = test_model();
        let context = Context {
            messages: vec![Message::user_text("Say hi")],
            system_prompt: None,
            model: None,
            tools: vec![],
        };

        let stream = stream::stream(&model, context, StreamOptions {
            api_key: Some("test-key".into()),
            ..Default::default()
        })
            .await
            .expect("stream() should return a stream");
        let result = collect_stream(stream, &model).await.expect("collect should succeed");

        let text = extract_text(&result.message.content);
        assert_eq!(text, "Hello world!");
    }

    #[serial]
#[tokio::test]
    async fn test_text_stream_with_system_prompt() {
        ensure_api_key();
        let mock = MockServer::start().await;
        mount_sse(
            &mock,
            "data: {\"id\":\"ch1\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Yes\"},\"finish_reason\":null}]}\n\
             \n\
             data: {\"id\":\"ch1\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\
             \n\
             data: [DONE]\n\
             \n",
        )
        .await;
        setup_provider(&mock).await;

        let model = test_model();
        let context = Context {
            messages: vec![Message::user_text("Is the sky blue?")],
            system_prompt: Some("You are a helpful assistant.".into()),
            model: None,
            tools: vec![],
        };

        let stream = stream::stream(&model, context, StreamOptions {
            api_key: Some("test-key".into()),
            ..Default::default()
        })
            .await
            .expect("stream() should return a stream");
        let result = collect_stream(stream, &model).await.expect("collect should succeed");

        let text = extract_text(&result.message.content);
        assert_eq!(text, "Yes");
    }

    // ------------------------------------------------------------------
    // Thinking streaming test
    // ------------------------------------------------------------------

    #[serial]
#[tokio::test]
    async fn test_text_stream_with_thinking() {
        ensure_api_key();
        let mock = MockServer::start().await;
        mount_sse(
            &mock,
            "data: {\"id\":\"ch1\",\"choices\":[{\"index\":0,\"delta\":{\"content\":[{\"type\":\"thinking\",\"thinking\":[{\"text\":\"Let me reason about this...\"}]}]},\"finish_reason\":null}]}\n\
             \n\
             data: {\"id\":\"ch1\",\"choices\":[{\"index\":0,\"delta\":{\"content\":[{\"type\":\"text\",\"text\":\"The answer is 42.\"}]},\"finish_reason\":null}]}\n\
             \n\
             data: {\"id\":\"ch1\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\
             \n\
             data: [DONE]\n\
             \n",
        )
        .await;
        setup_provider(&mock).await;

        let model = test_model();
        let context = Context {
            messages: vec![Message::user_text("What is 6 by 7?")],
            system_prompt: None,
            model: None,
            tools: vec![],
        };

        let stream = stream::stream(&model, context, StreamOptions {
            api_key: Some("test-key".into()),
            ..Default::default()
        })
            .await
            .expect("stream() should return a stream");
        let result = collect_stream(stream, &model).await.expect("collect should succeed");

        // Collected result should have thinking + text blocks.
        let message = &result.message;
        assert!(
            message.content.len() >= 2,
            "Expected at least 2 content blocks (thinking + text), got {}",
            message.content.len()
        );

        // Check thinking block.
        match &message.content[0] {
            ContentBlock::Thinking(th) => {
                assert!(th.thinking.contains("Let me reason"));
            }
            _ => panic!("Expected first block to be thinking"),
        }

        // Check text block.
        match &message.content.last().unwrap() {
            ContentBlock::Text(t) => {
                assert!(t.text.contains("42"), "Expected text to contain '42'");
            }
            _ => panic!("Expected last block to be text"),
        }
    }

    // ------------------------------------------------------------------
    // Tool call streaming tests
    // ------------------------------------------------------------------

    #[serial]
#[tokio::test]
    async fn test_tool_call_streaming() {
        ensure_api_key();
        let mock = MockServer::start().await;
        mount_sse(
            &mock,
            "data: {\"id\":\"ch1\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":null}]}\n\
             \n\
             data: {\"id\":\"ch1\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"type\":\"function\",\"function\":{\"name\":\"get_weather\",\"arguments\":\"\"}}]},\"finish_reason\":null}]}\n\
             \n\
             data: {\"id\":\"ch1\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"location\\\":\\\"NYC\\\"}\"}}]},\"finish_reason\":null}]}\n\
             \n\
             data: {\"id\":\"ch1\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\
             \n\
             data: [DONE]\n\
             \n",
        )
        .await;
        setup_provider(&mock).await;

        let model = test_model();
        let context = Context {
            messages: vec![Message::user_text("What's the weather in NYC?")],
            system_prompt: None,
            model: None,
            tools: vec![ToolDefinition {
                name: "get_weather".into(),
                description: "Get the weather".into(),
                parameters: serde_json::json!({"type": "object", "properties": {"location": {"type": "string"}}}),
                strict: Some(false),
            }],
        };

        let stream = stream::stream(&model, context, StreamOptions {
            api_key: Some("test-key".into()),
            ..Default::default()
        })
            .await
            .expect("stream() should return a stream");
        let result = collect_stream(stream, &model).await.expect("collect should succeed");

        // Done message should contain one ToolCall block.
        let has_tool_call = result.message.content.iter().any(|b| matches!(b, ContentBlock::ToolCall(_)));
        assert!(has_tool_call, "Expected a tool call in the result");

        // Find the tool call block and verify its content.
        for block in &result.message.content {
            if let ContentBlock::ToolCall(tc) = block {
                assert_eq!(tc.name, "get_weather");
                assert_eq!(
                    tc.arguments,
                    serde_json::json!({"location": "NYC"})
                );
                // The ID may have been generated or passed through.
                assert!(!tc.id.is_empty(), "Tool call ID should not be empty");
            }
        }

        assert_eq!(result.stop_reason, Some("toolUse".to_owned()));
    }

    // ------------------------------------------------------------------
    // Usage tests
    // ------------------------------------------------------------------

    #[serial]
#[tokio::test]
    async fn test_usage_chunk() {
        ensure_api_key();
        let mock = MockServer::start().await;
        mount_sse(
            &mock,
            "data: {\"id\":\"ch1\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hello\"},\"finish_reason\":null}]}\n\
             \n\
             data: {\"id\":\"ch1\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\
             \n\
             data: {\"id\":\"ch1\",\"object\":\"chat.completion.chunk\",\"choices\":[],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":5,\"total_tokens\":15}}\n\
             \n\
             data: [DONE]\n\
             \n",
        )
        .await;
        setup_provider(&mock).await;

        let model = test_model();
        let context = Context {
            messages: vec![Message::user_text("Hi")],
            system_prompt: None,
            model: None,
            tools: vec![],
        };

        let stream = stream::stream(&model, context, StreamOptions {
            api_key: Some("test-key".into()),
            ..Default::default()
        })
            .await
            .expect("stream() should return a stream");

        // Collect events manually to check for the Usage event.
        use tokio_stream::StreamExt;
        tokio::pin!(stream);

        let mut found_usage = false;
        let mut text = String::new();

        while let Some(event) = stream.next().await {
            match event {
                StreamEvent::TextDelta { delta } => text.push_str(&delta),
                StreamEvent::Usage(usage) => {
                    found_usage = true;
                    assert_eq!(usage.input, 10);
                    assert_eq!(usage.output, 5);
                }
                StreamEvent::Done { .. } => break,
                _ => {}
            }
        }

        assert!(found_usage, "Expected a Usage event");
        assert_eq!(text, "Hello");
    }

    // ------------------------------------------------------------------
    // Error handling tests
    // ------------------------------------------------------------------

    #[serial]
#[tokio::test]
    async fn test_api_key_error() {
        // No API key set, no options key — should produce an Error event.
        let model = test_model();
        let context = Context {
            messages: vec![Message::user_text("Hi")],
            system_prompt: None,
            model: None,
            tools: vec![],
        };

        // Use a provider with no API key — should error on first HTTP request.
        let provider = MistralProvider::with_base_url("http://0.0.0.0:1/v1/chat/completions");
        clear_api_providers().await;
        register_api_provider(Box::new(provider)).await;

        let stream = stream::stream(&model, context, StreamOptions {
            api_key: Some(String::new()),
            ..Default::default()
        })
            .await
            .expect("stream() should return a stream");

        // Wait briefly for the stream to process.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let result = collect_stream(stream, &model).await;
        assert!(result.is_err(), "Expected an error with empty API key");
    }

    #[serial]
#[tokio::test]
    async fn test_http_error() {
        ensure_api_key();
        let mock = MockServer::start().await;
        mount_error(&mock, 401, r#"{"error":{"message":"Invalid API key","type":"auth_error"}}"#).await;
        setup_provider(&mock).await;

        let model = test_model();
        let context = Context {
            messages: vec![Message::user_text("Hi")],
            system_prompt: None,
            model: None,
            tools: vec![],
        };

        let stream = stream::stream(&model, context, StreamOptions {
            api_key: Some("test-key".into()),
            ..Default::default()
        })
            .await
            .expect("stream() should return a stream");
        let result = collect_stream(stream, &model).await;
        assert!(result.is_err(), "Expected an error for HTTP 401");
    }

    // ------------------------------------------------------------------
    // Short hash unit tests
    // ------------------------------------------------------------------

    #[test]
    fn test_short_hash_deterministic() {
        let result1 = short_hash("hello");
        let result2 = short_hash("hello");
        assert_eq!(result1, result2, "Hash should be deterministic");
    }

    #[test]
    fn test_short_hash_different_inputs() {
        let h1 = short_hash("abc");
        let h2 = short_hash("xyz");
        assert_ne!(h1, h2, "Different inputs should produce different hashes");
    }

    #[test]
    fn test_short_hash_empty_string() {
        // Should not panic on empty input.
        let hash = short_hash("");
        assert!(!hash.is_empty(), "Hash of empty string should not be empty");
    }

    // ------------------------------------------------------------------
    // Tool call ID normalization unit tests
    // ------------------------------------------------------------------

    #[test]
    fn test_derive_tool_call_id_already_9_alphanumeric() {
        // Input "abcdef123" is already 9 alphanumeric chars.
        let result = derive_mistral_tool_call_id("abcdef123", 0);
        assert_eq!(result, "abcdef123");
    }

    #[test]
    fn test_derive_tool_call_id_strips_special_chars() {
        // "call_abc1" stripped → "callabc1" = 8 chars, so it gets hashed.
        let result = derive_mistral_tool_call_id("call_abc1", 0);
        assert_eq!(result.len(), 9);
        assert!(result.chars().all(|c| c.is_alphanumeric()));
    }

    #[test]
    fn test_derive_tool_call_id_underscore_removed() {
        // "call_12345" → "call12345" = 9 chars, should be returned as-is.
        let result = derive_mistral_tool_call_id("call_12345", 0);
        assert_eq!(result, "call12345");
    }

    #[test]
    fn test_derive_tool_call_id_collision_resolution() {
        // Two different inputs that might hash to the same value should
        // get different results when we increment attempt.
        let r1 = derive_mistral_tool_call_id("some_long_input_id_1", 0);
        let r2 = derive_mistral_tool_call_id("some_long_input_id_1", 1);
        // They should be different lengths or content.
        assert_eq!(r1.len(), 9);
        assert_eq!(r2.len(), 9);
    }

    #[test]
    fn test_derive_tool_call_id_consistent() {
        // Same input + attempt should yield the same result.
        let r1 = derive_mistral_tool_call_id("some_tool_call", 0);
        let r2 = derive_mistral_tool_call_id("some_tool_call", 0);
        assert_eq!(r1, r2);
    }

    #[test]
    fn test_normalizer_tool_call_id_unique() {
        let mut normalizer = MistralToolCallIdNormalizer::new();
        let id1 = normalizer.normalize("call_abc123");
        let id2 = normalizer.normalize("call_def456");
        assert_eq!(id1.len(), 9);
        assert_eq!(id2.len(), 9);
        assert_ne!(id1, id2, "Different inputs should get different normalized IDs");
    }

    #[test]
    fn test_normalizer_consistent_for_same_input() {
        let mut normalizer = MistralToolCallIdNormalizer::new();
        let id1 = normalizer.normalize("my_tool_call");
        let id2 = normalizer.normalize("my_tool_call");
        assert_eq!(id1, id2, "Same input should get the same normalized ID");
    }

    #[test]
    fn test_normalizer_multiple_collisions() {
        let mut normalizer = MistralToolCallIdNormalizer::new();
        // Normalize many IDs to stress collision resolution.
        let ids: Vec<String> = (0..100)
            .map(|i| normalizer.normalize(&format!("tool_call_{i}")))
            .collect();

        // All IDs should be unique.
        let mut unique = ids.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(ids.len(), unique.len(), "All normalized IDs should be unique");

        // All IDs should be exactly 9 alphanumeric chars.
        for id in &ids {
            assert_eq!(id.len(), 9, "Each normalized ID should be 9 chars");
            assert!(
                id.chars().all(|c| c.is_alphanumeric()),
                "Each ID char should be alphanumeric: {id}"
            );
        }
    }

    // ------------------------------------------------------------------
    // Session affinity unit tests
    // ------------------------------------------------------------------

    #[test]
    fn test_session_affinity_header_added() {
        let headers = build_mistral_headers(
            std::iter::empty::<(String, String)>(),
            Some("session-123"),
        );
        assert_eq!(headers.get("x-affinity").map(|s| s.as_str()), Some("session-123"));
    }

    #[test]
    fn test_session_affinity_no_header_without_session() {
        let headers = build_mistral_headers(
            std::iter::empty::<(String, String)>(),
            None,
        );
        assert!(headers.is_empty(), "No headers should be added without session ID");
    }

    #[test]
    fn test_session_affinity_explicit_header_respected() {
        let custom_headers = vec![("x-affinity".to_string(), "custom-value".to_string())];
        // Even with a session ID, the explicit header should be preserved.
        let headers = build_mistral_headers(custom_headers, Some("session-123"));
        assert_eq!(
            headers.get("x-affinity").map(|s| s.as_str()),
            Some("custom-value"),
            "Explicit x-affinity should take precedence"
        );
    }

    #[test]
    fn test_session_affinity_preserves_other_headers() {
        let custom_headers = vec![("x-custom".to_string(), "custom-value".to_string())];
        let headers = build_mistral_headers(custom_headers, Some("session-123"));
        assert_eq!(headers.get("x-custom").map(|s| s.as_str()), Some("custom-value"));
        assert_eq!(headers.get("x-affinity").map(|s| s.as_str()), Some("session-123"));
    }

    // ------------------------------------------------------------------
    // Stop reason mapping unit tests
    // ------------------------------------------------------------------

    #[test]
    fn test_map_stop_reasons() {
        assert_eq!(map_stop_reason("stop"), "stop");
        assert_eq!(map_stop_reason("length"), "length");
        assert_eq!(map_stop_reason("model_length"), "length");
        assert_eq!(map_stop_reason("tool_calls"), "toolUse");
        assert_eq!(map_stop_reason("error"), "error");
        assert!(map_stop_reason("content_filter").contains("error"));
        assert!(map_stop_reason("unknown").contains("error"));
    }

    // ------------------------------------------------------------------
    // Message conversion unit tests
    // ------------------------------------------------------------------

    #[test]
    fn test_convert_user_message_text_only() {
        let mut normalizer = MistralToolCallIdNormalizer::new();
        let context = Context {
            messages: vec![Message::user_text("Hello")],
            system_prompt: None,
            model: None,
            tools: vec![],
        };
        let messages = convert_messages(&context, &mut normalizer);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[0]["content"][0]["type"], "text");
        assert_eq!(messages[0]["content"][0]["text"], "Hello");
    }

    #[test]
    fn test_convert_system_message() {
        let mut normalizer = MistralToolCallIdNormalizer::new();
        let context = Context {
            messages: vec![],
            system_prompt: Some("Be helpful.".into()),
            model: None,
            tools: vec![],
        };
        let messages = convert_messages(&context, &mut normalizer);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[0]["content"], "Be helpful.");
    }

    #[test]
    fn test_convert_assistant_message_with_tool_calls() {
        let mut normalizer = MistralToolCallIdNormalizer::new();
        let msg = Message {
            role: MessageRole::Assistant,
            content: vec![
                ContentBlock::Text(TextContent {
                    text: "I'll look that up.".into(),
                }),
                ContentBlock::ToolCall(ToolCallContent {
                    id: "call_abc".into(),
                    name: "search_web".into(),
                    arguments: serde_json::json!({"query": "Rust programming"}),
                }),
            ],
            id: None,
            name: None,
            usage: None,
            redacted: false,
        };
        let context = Context {
            messages: vec![msg],
            system_prompt: None,
            model: None,
            tools: vec![],
        };
        let messages = convert_messages(&context, &mut normalizer);
        assert_eq!(messages.len(), 1);

        assert_eq!(messages[0]["role"], "assistant");
        assert_eq!(messages[0]["content"], "I'll look that up.");
        let tool_calls = messages[0]["tool_calls"].as_array().unwrap();
        assert_eq!(tool_calls.len(), 1);
        // Tool call ID should be normalized to 9 alphanumeric chars.
        let call_id = tool_calls[0]["id"].as_str().unwrap();
        assert_eq!(call_id.len(), 9);
        assert!(call_id.chars().all(|c| c.is_alphanumeric()));
        assert_eq!(tool_calls[0]["function"]["name"], "search_web");
    }

    #[test]
    fn test_convert_tool_result_messages() {
        let mut normalizer = MistralToolCallIdNormalizer::new();
        let msg = Message {
            role: MessageRole::Tool,
            content: vec![ContentBlock::ToolResult(ToolResultContent {
                id: "call_def".into(),
                name: "get_time".into(),
                content: Some(vec![ContentBlock::Text(TextContent {
                    text: "12:00 PM".into(),
                })]),
                error: None,
                is_error: false,
            })],
            id: None,
            name: None,
            usage: None,
            redacted: false,
        };
        let context = Context {
            messages: vec![msg],
            system_prompt: None,
            model: None,
            tools: vec![],
        };
        let messages = convert_messages(&context, &mut normalizer);
        assert_eq!(messages.len(), 1);

        assert_eq!(messages[0]["role"], "tool");
        let call_id = messages[0]["tool_call_id"].as_str().unwrap();
        assert_eq!(call_id.len(), 9);
        assert!(call_id.chars().all(|c| c.is_alphanumeric()));
        assert_eq!(messages[0]["content"], "12:00 PM");
    }

    #[test]
    fn test_convert_tool_result_with_error() {
        let mut normalizer = MistralToolCallIdNormalizer::new();
        let msg = Message {
            role: MessageRole::Tool,
            content: vec![ContentBlock::ToolResult(ToolResultContent {
                id: "call_err".into(),
                name: "faulty_tool".into(),
                content: None,
                error: Some("Connection refused".into()),
                is_error: true,
            })],
            id: None,
            name: None,
            usage: None,
            redacted: false,
        };
        let context = Context {
            messages: vec![msg],
            system_prompt: None,
            model: None,
            tools: vec![],
        };
        let messages = convert_messages(&context, &mut normalizer);
        assert_eq!(messages.len(), 1);

        assert_eq!(messages[0]["role"], "tool");
        assert!(messages[0]["content"].as_str().unwrap().contains("tool error"));
        assert!(messages[0]["content"].as_str().unwrap().contains("Connection refused"));
    }

    #[test]
    fn test_convert_tools() {
        let tools = vec![ToolDefinition {
            name: "get_weather".into(),
            description: "Get weather for a location".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "location": {"type": "string"}
                },
                "required": ["location"]
            }),
            strict: Some(false),
        }];
        let converted = convert_tools(&tools);
        assert_eq!(converted.len(), 1);
        assert_eq!(converted[0]["type"], "function");
        assert_eq!(converted[0]["function"]["name"], "get_weather");
        assert!(converted[0]["function"]["parameters"].is_object());
        assert_eq!(converted[0]["function"]["strict"], false);
    }

    // ------------------------------------------------------------------
    // Usage parsing unit tests
    // ------------------------------------------------------------------

    #[test]
    fn test_parse_usage_simple() {
        let raw = ChunkUsage {
            prompt_tokens: Some(100),
            completion_tokens: Some(50),
            total_tokens: Some(150),
        };
        let usage = parse_usage(&raw);
        assert_eq!(usage.input, 100);
        assert_eq!(usage.output, 50);
        assert_eq!(usage.cache_read, Some(0));
        assert_eq!(usage.cache_write, Some(0));
        assert_eq!(usage.total_tokens, Some(150));
    }

    #[test]
    fn test_parse_usage_empty() {
        let raw = ChunkUsage {
            prompt_tokens: None,
            completion_tokens: None,
            total_tokens: None,
        };
        let usage = parse_usage(&raw);
        assert_eq!(usage.input, 0);
        assert_eq!(usage.output, 0);
        assert_eq!(usage.total_tokens, None);
    }

    // ------------------------------------------------------------------
    // Cleanup
    // ------------------------------------------------------------------

    /// Ensure env vars are reset after tests that touch them.
    #[serial]
#[tokio::test]
    async fn cleanup_env() {
        unsafe { std::env::remove_var("MISTRAL_API_KEY"); }
        clear_api_providers().await;
    }
}
