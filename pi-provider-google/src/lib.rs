//! Pi Provider — Google Generative AI (Gemini).
//!
//! Maps to `packages/ai/src/providers/google.ts` and `google-shared.ts` in the TS source.
//!
//! This provider implements the [`ApiProvider`] trait for Google's Gemini API,
//! supporting text streaming, thinking content, tool calls, image input, and
//! SSE-based event streaming via the `:streamGenerateContent` endpoint.
//!
//! # Endpoint
//!
//! `POST https://generativelanguage.googleapis.com/v1beta/models/{model}:streamGenerateContent`
//!
//! Auth is via the `x-goog-api-key` header (or `?key=` URL query param as fallback).
//! The API key is resolved from `StreamOptions.api_key` or the `GEMINI_API_KEY` /
//! `GOOGLE_API_KEY` environment variables.

use pi_ai_core::api_registry::ApiProvider;
use pi_ai_core::event_stream::{AssistantMessageEventStream, EventStreamSender};
use pi_ai_core::types::{
    ContentBlock, Context, ImageSource, Message, MessageRole, Model, StreamError, StreamEvent, StreamOptions,
    TextContent, ThinkingContent, ToolCallContent, Usage,
};
use serde::Deserialize;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Default Gemini API base URL.
const DEFAULT_GEMINI_API_URL: &str = "https://generativelanguage.googleapis.com";

/// SSE data prefix (the part before the actual JSON payload).
const SSE_DATA_PREFIX: &str = "data: ";

// ---------------------------------------------------------------------------
// Response chunk types (deserialized from SSE `data: ...` lines)
// ---------------------------------------------------------------------------

/// Top-level streaming chunk from the Gemini API.
#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct Chunk {
    /// The response candidates (typically one).
    candidates: Option<Vec<Candidate>>,
    /// Token usage metadata in the final chunk.
    usage_metadata: Option<UsageMetadata>,
}

/// A single response candidate.
#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct Candidate {
    /// The content produced by the model.
    content: Option<Content>,
    /// Reason why the model stopped generating.
    finish_reason: Option<String>,
}

/// Content object containing parts.
#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct Content {
    /// The parts that make up the content.
    parts: Option<Vec<Part>>,
    /// The role of the content (usually "model").
    role: Option<String>,
}

/// A single part within content.
#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct Part {
    /// Text content (also carries thinking text when `thought` is true).
    text: Option<String>,
    /// When true, this part represents thinking/reasoning content.
    thought: Option<bool>,
    /// Signature for round-tripping thinking context across multi-turn interactions.
    thought_signature: Option<String>,
    /// A function call requested by the model.
    function_call: Option<FunctionCall>,
}

/// A function call part.
#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct FunctionCall {
    /// Name of the function to call.
    name: Option<String>,
    /// Arguments to the function.
    args: Option<serde_json::Value>,
    /// Optional ID for the function call.
    id: Option<String>,
}

/// Token usage metadata from the Gemini API.
#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct UsageMetadata {
    /// Number of tokens in the prompt.
    prompt_token_count: Option<u64>,
    /// Number of tokens in the candidates.
    candidates_token_count: Option<u64>,
    /// Total token count.
    total_token_count: Option<u64>,
    /// Tokens from cached content.
    cached_content_token_count: Option<u64>,
    /// Tokens used for thinking/reasoning.
    thoughts_token_count: Option<u64>,
}

// ---------------------------------------------------------------------------
// Streaming state machine
// ---------------------------------------------------------------------------

/// Mutable state carried across the SSE stream processing loop.
#[derive(Debug, Default)]
struct StreamState {
    /// All text content received so far.
    text: String,
    /// All thinking content received so far.
    thinking: String,
    /// The most recent thinking signature (for round-tripping).
    thinking_signature: Option<String>,
    /// Tool calls collected during streaming, keyed by content index.
    tool_calls: Vec<CollectedToolCall>,
    /// Counter for generating unique tool call IDs.
    tool_call_counter: u64,
    /// The finish reason from the last chunk that carried one.
    finish_reason: Option<String>,
    /// Track whether we've seen any content (for use after stream end).
    saw_content: bool,
    /// Track the response ID from usage metadata chunks.
    response_id: Option<String>,
}

/// A completed tool call accumulated from the stream.
#[derive(Debug)]
struct CollectedToolCall {
    /// Index within the response parts (for ordering).
    index: usize,
    /// The tool call name.
    name: String,
    /// JSON-encoded arguments.
    arguments: String,
}

// ---------------------------------------------------------------------------
// Provider struct
// ---------------------------------------------------------------------------

/// Provider for the Google Generative AI (Gemini) API.
///
/// Sends POST requests to the `:streamGenerateContent` endpoint and parses
/// the SSE response stream, emitting [`StreamEvent`] items.
///
/// # Example
///
/// ```ignore
/// use pi_provider_google::GoogleProvider;
/// use pi_ai_core::api_registry::register_api_provider;
///
/// let provider = GoogleProvider::new();
/// register_api_provider(Box::new(provider)).await;
/// ```
pub struct GoogleProvider {
    /// Base URL for the Gemini API (defaults to `DEFAULT_GEMINI_API_URL`).
    base_url: String,
}

impl GoogleProvider {
    /// Create a new provider that targets the standard Gemini API.
    pub fn new() -> Self {
        Self { base_url: DEFAULT_GEMINI_API_URL.to_owned() }
    }

    /// Create a provider with a custom base URL (useful for testing or
    /// Gemini-compatible backends).
    pub fn with_base_url(base_url: impl Into<String>) -> Self {
        Self { base_url: base_url.into() }
    }
}

impl Default for GoogleProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl ApiProvider for GoogleProvider {
    fn api_id(&self) -> &str {
        "google-generative-ai"
    }

    fn stream(&self, model: &Model, context: Context, options: StreamOptions) -> AssistantMessageEventStream {
        let (tx, rx) = AssistantMessageEventStream::new();
        let model = model.clone();
        let base_url = self.base_url.clone();

        tokio::spawn(async move {
            if let Err(e) = process_stream(tx, &base_url, &model, context, options).await {
                tracing::error!("Google Gemini stream error: {e}");
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

    // 2. Build the endpoint URL.
    let model_id = &model.id;
    let endpoint = if let Some(ref custom_base) = model.base_url {
        // Custom base URL already includes version path.
        format!("{}/{}:streamGenerateContent", custom_base, model_id)
    } else {
        format!("{base_url}/v1beta/models/{model_id}:streamGenerateContent")
    };

    // 3. Build the JSON request body.
    let body = build_request_body(model, &context, &options);

    // 4. Send the HTTP request.
    let client =
        reqwest::Client::builder().timeout(std::time::Duration::from_secs(options.timeout.unwrap_or(120))).build()?;

    let response = client
        .post(&endpoint)
        .header("x-goog-api-key", &api_key)
        .header("Content-Type", "application/json")
        .query(&[("key", &api_key)]) // URL query param fallback
        .json(&body)
        .send()
        .await
        .map_err(|e| {
            emit_error(&tx, format!("HTTP request failed: {e}"), Some("request_error".to_owned()));
            e
        })?;

    // 5. Check the HTTP status code.
    if !response.status().is_success() {
        let status = response.status();
        let error_text = response.text().await.unwrap_or_else(|_| String::new());
        emit_error(
            &tx,
            format!("Gemini API error ({}): {error_text}", status.as_u16()),
            Some(status.as_str().to_owned()),
        );
        return Ok(());
    }

    // 6. Emit the Start event.
    let _ = tx.send(StreamEvent::Start);

    // 7. Process the SSE response body.
    let mut state = StreamState::default();
    if let Err(e) = process_sse_stream(&tx, response, &mut state).await {
        emit_error(&tx, format!("SSE stream error: {e}"), Some("stream_error".to_owned()));
        return Ok(());
    }

    // 8. Emit the final Done event.
    let stop_reason = state.finish_reason.clone().unwrap_or_else(|| "stop".to_owned());
    let message = build_done_message(&state, model);

    let _ = tx.send(StreamEvent::Done { message: Some(message), stop_reason: Some(stop_reason) });

    Ok(())
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
            let line_bytes = if raw_line.ends_with(b"\n") { &raw_line[..raw_line.len() - 1] } else { &raw_line };
            let line_bytes = if line_bytes.ends_with(b"\r") { &line_bytes[..line_bytes.len() - 1] } else { line_bytes };

            let line_str = String::from_utf8_lossy(line_bytes);

            if line_str.is_empty() {
                continue;
            }

            if let Some(data) = line_str.strip_prefix(SSE_DATA_PREFIX) {
                let data = data.trim();
                if data.is_empty() {
                    continue;
                }

                // Parse the JSON chunk.
                match serde_json::from_str::<Chunk>(data) {
                    Ok(chunk) => {
                        process_chunk(tx, chunk, state);
                    }
                    Err(e) => {
                        tracing::warn!("Failed to parse Gemini SSE chunk JSON: {e} — data: {data}");
                        // Non-fatal: skip malformed chunks.
                    }
                }
            }
            // Lines that do not start with `data: ` are ignored per the SSE spec
            // (they may be comments or event-type lines).
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Chunk processing
// ---------------------------------------------------------------------------

/// Process a single parsed [`Chunk`] and emit the corresponding [`StreamEvent`]s.
fn process_chunk(tx: &EventStreamSender<StreamEvent>, chunk: Chunk, state: &mut StreamState) {
    // --- Usage metadata ---
    if let Some(ref usage) = chunk.usage_metadata {
        let parsed = parse_usage(usage);
        let _ = tx.send(StreamEvent::Usage(parsed));
    }

    // --- Candidates ---
    if let Some(ref candidates) = chunk.candidates {
        for candidate in candidates {
            // Track finish reason.
            if let Some(ref reason) = candidate.finish_reason {
                if !reason.is_empty() {
                    state.finish_reason = Some(map_stop_reason(reason));
                }
            }

            // Process content parts.
            if let Some(ref content) = candidate.content {
                if let Some(ref parts) = content.parts {
                    for part in parts {
                        handle_part(tx, part, state);
                    }
                }
            }
        }
    }
}

/// Emit events for a single [`Part`] object.
fn handle_part(tx: &EventStreamSender<StreamEvent>, part: &Part, state: &mut StreamState) {
    // --- Text / thinking content ---
    if let Some(ref text) = part.text {
        if text.is_empty() {
            return;
        }

        let is_thinking = part.thought.unwrap_or(false);

        if is_thinking {
            state.thinking.push_str(text);
            // Preserve thought signature.
            if let Some(ref sig) = part.thought_signature {
                state.thinking_signature = Some(sig.clone());
            }
            let _ = tx.send(StreamEvent::ThinkingDelta { delta: text.clone() });
        } else {
            state.text.push_str(text);
            let _ = tx.send(StreamEvent::TextDelta { delta: text.clone() });
        }

        state.saw_content = true;
    }

    // --- Tool calls ---
    if let Some(ref func_call) = part.function_call {
        if let Some(ref name) = func_call.name {
            state.tool_call_counter += 1;

            let id = func_call.id.clone().unwrap_or_else(|| format!("call_{}", state.tool_call_counter));

            let args = func_call
                .args
                .as_ref()
                .map(|v| serde_json::to_string(v).unwrap_or_else(|_| "{}".to_owned()))
                .unwrap_or_else(|| "{}".to_owned());

            let index = state.tool_calls.len();
            state.tool_calls.push(CollectedToolCall { index, name: name.clone(), arguments: args.clone() });

            // Emit tool call delta events.
            let _ = tx.send(StreamEvent::ToolCallDelta {
                index: index as u32,
                id: Some(id),
                name: Some(name.clone()),
                arguments: Some(args),
            });

            state.saw_content = true;
        }
    }
}

// ---------------------------------------------------------------------------
// Final message construction
// ---------------------------------------------------------------------------

/// Build the final [`Message`] from the accumulated streaming state.
fn build_done_message(state: &StreamState, _model: &Model) -> Message {
    let mut content: Vec<ContentBlock> = Vec::new();

    // Add thinking block if we got thinking content.
    if !state.thinking.is_empty() {
        content.push(ContentBlock::Thinking(ThinkingContent {
            thinking: state.thinking.clone(),
            signature: state.thinking_signature.clone(),
        }));
    }

    // Add text block.
    if !state.text.is_empty() {
        content.push(ContentBlock::Text(TextContent { text: state.text.clone() }));
    }

    // Add tool call blocks (in order).
    for tool_call in &state.tool_calls {
        let parsed_args: serde_json::Value = serde_json::from_str(&tool_call.arguments)
            .unwrap_or_else(|_| serde_json::Value::String(tool_call.arguments.clone()));

        content.push(ContentBlock::ToolCall(ToolCallContent {
            id: format!("call_{}", tool_call.index + 1),
            name: tool_call.name.clone(),
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

/// Build the JSON request body for the Gemini API.
#[expect(unused_variables)]
fn build_request_body(model: &Model, context: &Context, options: &StreamOptions) -> serde_json::Value {
    let contents = convert_messages(context);

    let mut body = serde_json::json!({
        "contents": contents,
    });

    // System prompt (separate field, not part of contents).
    if let Some(ref system_prompt) = context.system_prompt {
        if !system_prompt.is_empty() {
            body["systemInstruction"] = serde_json::json!({
                "parts": [{"text": system_prompt}]
            });
        }
    }

    // Generation config.
    let mut generation_config = serde_json::Map::new();
    if let Some(max_tokens) = options.max_tokens {
        generation_config.insert("maxOutputTokens".to_owned(), serde_json::json!(max_tokens));
    }
    if !generation_config.is_empty() {
        body["generationConfig"] = serde_json::Value::Object(generation_config);
    }

    // Tools.
    if !context.tools.is_empty() {
        body["tools"] = serde_json::json!(convert_tools(&context.tools));
    }

    // Thinking config (not directly exposed via StreamOptions; we skip for now
    // since StreamOptions only has `thinking: Option<bool>`).
    // Advanced thinking configuration (level/budget) will be added when
    // StreamOptions is extended.

    body
}

// ---------------------------------------------------------------------------
// Message conversion (pi-ai-core -> Gemini contents format)
// ---------------------------------------------------------------------------

/// Convert [`Context`] messages to Gemini `contents` array format.
fn convert_messages(context: &Context) -> Vec<serde_json::Value> {
    let mut contents: Vec<serde_json::Value> = Vec::new();

    for msg in &context.messages {
        match msg.role {
            MessageRole::System => {
                // System messages are handled via `systemInstruction`; skip
                // them in the contents array.
                continue;
            }
            MessageRole::User => {
                let converted = convert_user_message(msg);
                if let Some(val) = converted {
                    contents.push(val);
                }
            }
            MessageRole::Assistant => {
                let converted = convert_assistant_message(msg);
                if let Some(val) = converted {
                    contents.push(val);
                }
            }
            MessageRole::Tool => {
                // Collect consecutive tool result messages into user messages
                // with functionResponse parts.
                let tool_parts = convert_tool_results(msg);
                if !tool_parts.is_empty() {
                    contents.push(serde_json::json!({
                        "role": "user",
                        "parts": tool_parts,
                    }));
                }
            }
        }
    }

    contents
}

/// Convert a user message to Gemini format.
fn convert_user_message(msg: &Message) -> Option<serde_json::Value> {
    let parts = convert_user_content_parts(&msg.content);
    if parts.is_empty() {
        return None;
    }

    Some(serde_json::json!({
        "role": "user",
        "parts": parts,
    }))
}

/// Build content parts for a user message (text + inlineData).
fn convert_user_content_parts(content: &[ContentBlock]) -> Vec<serde_json::Value> {
    content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(t) => {
                if t.text.is_empty() {
                    return None;
                }
                Some(serde_json::json!({
                    "text": t.text,
                }))
            }
            ContentBlock::Image(img) => {
                let (media_type, data) = match &img.source {
                    ImageSource::Base64 { media_type, data } => (media_type, data),
                    ImageSource::Url { url } => {
                        // For URLs, we'd need to fetch and re-encode.
                        // Skip for now.
                        tracing::warn!("Gemini API does not support URL-based images directly; skipping image: {url}");
                        return None;
                    }
                };
                Some(serde_json::json!({
                    "inlineData": {
                        "mimeType": media_type,
                        "data": data,
                    },
                }))
            }
            _ => None,
        })
        .collect()
}

/// Convert an assistant message to Gemini format ("model" role).
fn convert_assistant_message(msg: &Message) -> Option<serde_json::Value> {
    let parts = convert_assistant_content_parts(msg);
    if parts.is_empty() {
        return None;
    }

    Some(serde_json::json!({
        "role": "model",
        "parts": parts,
    }))
}

/// Build content parts for an assistant message (text + functionCall).
fn convert_assistant_content_parts(msg: &Message) -> Vec<serde_json::Value> {
    msg.content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(t) => {
                if t.text.is_empty() {
                    return None;
                }
                Some(serde_json::json!({
                    "text": t.text,
                }))
            }
            ContentBlock::Thinking(th) => {
                if th.thinking.trim().is_empty() {
                    return None;
                }
                let mut part = serde_json::json!({
                    "thought": true,
                    "text": th.thinking,
                });
                if let Some(ref sig) = th.signature {
                    if !sig.is_empty() {
                        part["thoughtSignature"] = serde_json::json!(sig);
                    }
                }
                Some(part)
            }
            ContentBlock::ToolCall(tc) => Some(serde_json::json!({
                "functionCall": {
                    "name": tc.name,
                    "args": tc.arguments,
                },
            })),
            _ => None,
        })
        .collect()
}

/// Convert tool result content to Gemini functionResponse parts.
fn convert_tool_results(msg: &Message) -> Vec<serde_json::Value> {
    msg.content
        .iter()
        .filter_map(|block| {
            if let ContentBlock::ToolResult(tr) = block {
                let response_value = if tr.is_error {
                    let error_msg = tr.error.as_deref().unwrap_or("Unknown error");
                    serde_json::json!({
                        "error": error_msg,
                    })
                } else {
                    let text = tr.content.as_ref().map(|c| extract_text(c)).unwrap_or_default();
                    serde_json::json!({
                        "output": text,
                    })
                };

                Some(serde_json::json!({
                    "functionResponse": {
                        "name": tr.name,
                        "response": response_value,
                    },
                }))
            } else {
                None
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Tool definition conversion
// ---------------------------------------------------------------------------

/// Convert [`ToolDefinition`]s to Gemini function declarations format.
fn convert_tools(tools: &[pi_ai_core::types::ToolDefinition]) -> Vec<serde_json::Value> {
    let function_declarations: Vec<serde_json::Value> = tools
        .iter()
        .map(|tool| {
            serde_json::json!({
                "name": tool.name,
                "description": tool.description,
                "parameters": tool.parameters,
            })
        })
        .collect();

    vec![serde_json::json!({
        "functionDeclarations": function_declarations,
    })]
}

// ---------------------------------------------------------------------------
// Usage parsing
// ---------------------------------------------------------------------------

/// Parse Gemini usage metadata into the pi-ai-core [`Usage`] struct.
fn parse_usage(raw: &UsageMetadata) -> Usage {
    let prompt = raw.prompt_token_count.unwrap_or(0);
    let candidates = raw.candidates_token_count.unwrap_or(0);
    let cached = raw.cached_content_token_count.unwrap_or(0);
    let thoughts = raw.thoughts_token_count.unwrap_or(0);

    let input = prompt.saturating_sub(cached);
    let output = candidates.saturating_add(thoughts);

    Usage { input, output, cache_read: Some(cached), cache_write: Some(0), total_tokens: raw.total_token_count }
}

// ---------------------------------------------------------------------------
// Stop reason mapping
// ---------------------------------------------------------------------------

/// Map a Gemini `finishReason` to the canonical stop-reason string.
fn map_stop_reason(reason: &str) -> String {
    match reason {
        "STOP" => "stop".to_owned(),
        "MAX_TOKENS" => "length".to_owned(),
        // Error / safety reasons
        "SAFETY"
        | "BLOCKLIST"
        | "PROHIBITED_CONTENT"
        | "SPII"
        | "RECITATION"
        | "LANGUAGE"
        | "OTHER"
        | "IMAGE_SAFETY"
        | "IMAGE_PROHIBITED_CONTENT"
        | "IMAGE_RECITATION"
        | "IMAGE_OTHER"
        | "FINISH_REASON_UNSPECIFIED"
        | "MALFORMED_FUNCTION_CALL"
        | "NO_IMAGE"
        | "UNEXPECTED_TOOL_CALL" => "error".to_owned(),
        other => format!("error:provider_finish_reason:{other}"),
    }
}

// ---------------------------------------------------------------------------
// API key resolution
// ---------------------------------------------------------------------------

/// Resolve the API key from options or environment.
///
/// Checks, in order:
/// 1. `StreamOptions.api_key`
/// 2. `GEMINI_API_KEY` environment variable
/// 3. `GOOGLE_API_KEY` environment variable
fn resolve_api_key(options: &StreamOptions) -> Result<String, String> {
    if let Some(ref key) = options.api_key {
        if !key.is_empty() {
            return Ok(key.clone());
        }
    }
    match std::env::var("GEMINI_API_KEY") {
        Ok(key) if !key.is_empty() => return Ok(key),
        _ => {}
    }
    match std::env::var("GOOGLE_API_KEY") {
        Ok(key) if !key.is_empty() => Ok(key),
        _ => Err("Gemini API key is required. Set the GEMINI_API_KEY or GOOGLE_API_KEY \
             environment variable or pass `api_key` in `StreamOptions`."
            .to_owned()),
    }
}

// ---------------------------------------------------------------------------
// Utility helpers
// ---------------------------------------------------------------------------

/// Extract plain text from a slice of [`ContentBlock`]s.
fn extract_text(content: &[ContentBlock]) -> String {
    content
        .iter()
        .filter_map(|block| if let ContentBlock::Text(t) = block { Some(t.text.as_str()) } else { None })
        .collect::<Vec<&str>>()
        .join("\n")
}

/// Send an error event and log it.
fn emit_error(tx: &EventStreamSender<StreamEvent>, message: impl Into<String>, code: Option<String>) {
    let msg: String = message.into();
    tracing::error!("{msg}");
    let _ = tx.send(StreamEvent::Error { error: StreamError { message: msg, code, r#type: None } });
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use pi_ai_core::api_registry::{clear_api_providers, register_api_provider};
    use pi_ai_core::event_stream::collect_stream;
    use pi_ai_core::stream;
    use pi_ai_core::types::{ImageContent, KnownProvider, ToolDefinition, ToolResultContent};
    use serial_test::serial;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    // ------------------------------------------------------------------
    // Helpers
    // ------------------------------------------------------------------

    fn test_model() -> Model {
        Model {
            id: "gemini-2.0-flash".into(),
            provider: KnownProvider::Google,
            api: "google-generative-ai".into(),
            name: None,
            base_url: None,
            supports_thinking: true,
            supports_tools: true,
            supports_streaming: true,
            supports_image_input: true,
            max_tokens: None,
            max_input_tokens: None,
            max_output_tokens: Some(8192),
            cost_per_input_token: Some(0.000_000_1),
            cost_per_output_token: Some(0.000_000_4),
            cost_per_cache_read_token: None,
            cost_per_cache_write_token: None,
        }
    }

    async fn setup_provider(mock_server: &MockServer) {
        let provider = GoogleProvider::with_base_url(mock_server.uri().to_string());
        clear_api_providers().await;
        register_api_provider(Box::new(provider)).await;
    }

    /// Ensure GEMINI_API_KEY is set to a known value for all wiremock tests.
    fn ensure_api_key() {} // No-op: all tests pass api_key in options

    /// Mount a mock SSE endpoint for `models/{model}:streamGenerateContent`.
    async fn mount_sse(mock_server: &MockServer, model_id: &str, body: &'static str) {
        Mock::given(method("POST"))
            .and(path(format!("/v1beta/models/{model_id}:streamGenerateContent")))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"))
            .mount(mock_server)
            .await;
    }

    /// Mount a mock that returns an HTTP error.
    async fn mount_error(mock_server: &MockServer, model_id: &str, status: u16, body: &'static str) {
        Mock::given(method("POST"))
            .and(path(format!("/v1beta/models/{model_id}:streamGenerateContent")))
            .respond_with(
                ResponseTemplate::new(status).set_body_string(body).insert_header("content-type", "application/json"),
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
            "gemini-2.0-flash",
            "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"Hello world\"}],\"role\":\"model\"},\"finishReason\":null}],\"usageMetadata\":{\"promptTokenCount\":5,\"candidatesTokenCount\":3,\"totalTokenCount\":8}}\n\
             \n\
             data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"\"}],\"role\":\"model\"},\"finishReason\":\"STOP\"}],\"usageMetadata\":{\"promptTokenCount\":5,\"candidatesTokenCount\":3,\"totalTokenCount\":8}}\n\
             \n",
        )
        .await;
        setup_provider(&mock).await;

        let model = test_model();
        let context =
            Context { messages: vec![Message::user_text("Hi")], system_prompt: None, model: None, tools: vec![] };

        let stream =
            stream::stream(&model, context, StreamOptions { api_key: Some("test-key".into()), ..Default::default() })
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
            "gemini-2.0-flash",
            "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"Hello\"}],\"role\":\"model\"},\"finishReason\":null}]}\n\
             \n\
             data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\" world\"}],\"role\":\"model\"},\"finishReason\":null}]}\n\
             \n\
             data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"!\"}],\"role\":\"model\"},\"finishReason\":\"STOP\"}]}\n\
             \n",
        )
        .await;
        setup_provider(&mock).await;

        let model = test_model();
        let context =
            Context { messages: vec![Message::user_text("Say hi")], system_prompt: None, model: None, tools: vec![] };

        let stream =
            stream::stream(&model, context, StreamOptions { api_key: Some("test-key".into()), ..Default::default() })
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
            "gemini-2.0-flash",
            "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"Sure\"}],\"role\":\"model\"},\"finishReason\":null}],\"usageMetadata\":{\"promptTokenCount\":10,\"candidatesTokenCount\":2,\"totalTokenCount\":12}}\n\
             \n\
             data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"\"}],\"role\":\"model\"},\"finishReason\":\"STOP\"}]}\n\
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

        let stream =
            stream::stream(&model, context, StreamOptions { api_key: Some("test-key".into()), ..Default::default() })
                .await
                .expect("stream() should return a stream");
        let result = collect_stream(stream, &model).await.expect("collect should succeed");

        let text = extract_text(&result.message.content);
        assert_eq!(text, "Sure");
    }

    // ------------------------------------------------------------------
    // Thinking content streaming tests
    // ------------------------------------------------------------------

    #[serial]
    #[tokio::test]
    async fn test_thinking_stream() {
        ensure_api_key();
        let mock = MockServer::start().await;
        mount_sse(
            &mock,
            "gemini-2.0-flash",
            "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"Let me think about this\",\"thought\":true}],\"role\":\"model\"},\"finishReason\":null}]}\n\
             \n\
             data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"The answer is 42\"}],\"role\":\"model\"},\"finishReason\":null}]}\n\
             \n\
             data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"\"}],\"role\":\"model\"},\"finishReason\":\"STOP\"}]}\n\
             \n",
        )
        .await;
        setup_provider(&mock).await;

        let model = test_model();
        let context = Context {
            messages: vec![Message::user_text("What is the meaning?")],
            system_prompt: None,
            model: None,
            tools: vec![],
        };

        let stream =
            stream::stream(&model, context, StreamOptions { api_key: Some("test-key".into()), ..Default::default() })
                .await
                .expect("stream() should return a stream");
        let result = collect_stream(stream, &model).await.expect("collect should succeed");

        let message = &result.message;
        assert!(message.content.len() >= 2, "Expected at least 2 content blocks, got {}", message.content.len());

        // First block should be thinking.
        let thinking_block =
            message.content.iter().find_map(|b| if let ContentBlock::Thinking(th) = b { Some(th) } else { None });
        assert!(thinking_block.is_some(), "Expected a thinking content block");
        assert_eq!(thinking_block.unwrap().thinking, "Let me think about this");

        // Last block should be text.
        let text = extract_text(&message.content);
        assert_eq!(text, "The answer is 42");
    }

    // ------------------------------------------------------------------
    // Tool call streaming tests
    // ------------------------------------------------------------------

    #[serial]
    #[tokio::test]
    async fn test_tool_call_stream() {
        ensure_api_key();
        let mock = MockServer::start().await;
        mount_sse(
            &mock,
            "gemini-2.0-flash",
            "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"Let me check the weather\"}],\"role\":\"model\"},\"finishReason\":null}]}\n\
             \n\
             data: {\"candidates\":[{\"content\":{\"parts\":[{\"functionCall\":{\"name\":\"get_weather\",\"args\":{\"location\":\"NYC\"}}}],\"role\":\"model\"},\"finishReason\":null}]}\n\
             \n\
             data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"\"}],\"role\":\"model\"},\"finishReason\":\"STOP\"}]}\n\
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
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "location": { "type": "string" }
                    },
                    "required": ["location"],
                }),
                strict: Some(false),
            }],
        };

        let stream =
            stream::stream(&model, context, StreamOptions { api_key: Some("test-key".into()), ..Default::default() })
                .await
                .expect("stream() should return a stream");
        let result = collect_stream(stream, &model).await.expect("collect should succeed");

        // Should have text + tool call.
        let has_tool_call = result.message.content.iter().any(|b| matches!(b, ContentBlock::ToolCall(_)));
        assert!(has_tool_call, "Expected a tool call in the result");

        // Verify tool call content.
        for block in &result.message.content {
            if let ContentBlock::ToolCall(tc) = block {
                assert_eq!(tc.name, "get_weather");
                assert_eq!(tc.arguments, serde_json::json!({"location": "NYC"}));
            }
        }

        assert_eq!(result.stop_reason, Some("stop".to_owned()));
    }

    // ------------------------------------------------------------------
    // Error handling tests
    // ------------------------------------------------------------------

    #[serial]
    #[tokio::test]
    async fn test_api_key_error() {
        ensure_api_key();
        // No API key in options or env — should produce an Error event.
        let model = test_model();
        let context =
            Context { messages: vec![Message::user_text("Hi")], system_prompt: None, model: None, tools: vec![] };

        let provider = GoogleProvider::new();
        clear_api_providers().await;
        register_api_provider(Box::new(provider)).await;

        // The provider has no mock and will fail to connect. The error message
        // will be about connection failure rather than API key.
        let stream =
            stream::stream(&model, context, StreamOptions::default()).await.expect("stream() should return a stream");
        let result = collect_stream(stream, &model).await;

        assert!(result.is_err(), "Expected an error when no API key is available");
    }

    #[serial]
    #[tokio::test]
    async fn test_http_error() {
        ensure_api_key();
        let mock = MockServer::start().await;
        mount_error(&mock, "gemini-2.0-flash", 401, r#"{"error":{"message":"Invalid API key","code":"auth_error"}}"#)
            .await;
        setup_provider(&mock).await;

        let model = test_model();
        let context =
            Context { messages: vec![Message::user_text("Hi")], system_prompt: None, model: None, tools: vec![] };

        let stream =
            stream::stream(&model, context, StreamOptions { api_key: Some("test-key".into()), ..Default::default() })
                .await
                .expect("stream() should return a stream");
        let result = collect_stream(stream, &model).await;

        assert!(result.is_err(), "Expected an error for HTTP 401");
    }

    #[serial]
    #[tokio::test]
    async fn test_invalid_sse_chunk_is_skipped() {
        ensure_api_key();
        let mock = MockServer::start().await;
        mount_sse(
            &mock,
            "gemini-2.0-flash",
            "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"Hello\"}],\"role\":\"model\"},\"finishReason\":null}]}\n\
             \n\
             data: NOT_VALID_JSON\n\
             \n\
             data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\" world\"}],\"role\":\"model\"},\"finishReason\":null}]}\n\
             \n\
             data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"\"}],\"role\":\"model\"},\"finishReason\":\"STOP\"}]}\n\
             \n",
        )
        .await;
        setup_provider(&mock).await;

        let model = test_model();
        let context =
            Context { messages: vec![Message::user_text("Hi")], system_prompt: None, model: None, tools: vec![] };

        let stream =
            stream::stream(&model, context, StreamOptions { api_key: Some("test-key".into()), ..Default::default() })
                .await
                .expect("stream() should return a stream");
        let result = collect_stream(stream, &model).await.expect("collect should succeed");

        let text = extract_text(&result.message.content);
        assert_eq!(text, "Hello world", "Invalid SSE chunk should be skipped");
    }

    // ------------------------------------------------------------------
    // Message conversion unit tests
    // ------------------------------------------------------------------

    #[test]
    fn test_convert_user_message_text_only() {
        let msg = Message::user_text("Hello");
        let context = Context { messages: vec![msg], system_prompt: None, model: None, tools: vec![] };
        let contents = convert_messages(&context);
        assert_eq!(contents.len(), 1);
        assert_eq!(contents[0]["role"], "user");
        assert_eq!(contents[0]["parts"][0]["text"], "Hello");
    }

    #[test]
    fn test_convert_user_message_with_image() {
        let msg = Message {
            role: MessageRole::User,
            content: vec![
                ContentBlock::Text(TextContent { text: "What's in this image?".into() }),
                ContentBlock::Image(ImageContent {
                    source: ImageSource::Base64 {
                        media_type: "image/png".into(),
                        data: "iVBORw0KGgoAAAANSUhEUgAAAAEAAAA=".into(),
                    },
                }),
            ],
            id: None,
            name: None,
            usage: None,
            redacted: false,
        };
        let context = Context { messages: vec![msg], system_prompt: None, model: None, tools: vec![] };
        let contents = convert_messages(&context);
        assert_eq!(contents.len(), 1);

        let parts = contents[0]["parts"].as_array().unwrap();
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0]["text"], "What's in this image?");
        assert_eq!(parts[1]["inlineData"]["mimeType"], "image/png");
        assert_eq!(parts[1]["inlineData"]["data"], "iVBORw0KGgoAAAANSUhEUgAAAAEAAAA=");
    }

    #[test]
    fn test_convert_assistant_message_with_tool_call() {
        let msg = Message {
            role: MessageRole::Assistant,
            content: vec![
                ContentBlock::Text(TextContent { text: "I'll look that up.".into() }),
                ContentBlock::ToolCall(ToolCallContent {
                    id: "call_1".into(),
                    name: "search_web".into(),
                    arguments: serde_json::json!({"query": "Rust programming"}),
                }),
            ],
            id: None,
            name: None,
            usage: None,
            redacted: false,
        };
        let context = Context { messages: vec![msg], system_prompt: None, model: None, tools: vec![] };
        let contents = convert_messages(&context);
        assert_eq!(contents.len(), 1);

        assert_eq!(contents[0]["role"], "model");
        let parts = contents[0]["parts"].as_array().unwrap();
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0]["text"], "I'll look that up.");
        assert_eq!(parts[1]["functionCall"]["name"], "search_web");
        assert_eq!(parts[1]["functionCall"]["args"]["query"], "Rust programming");
    }

    #[test]
    fn test_convert_assistant_message_with_thinking() {
        let msg = Message {
            role: MessageRole::Assistant,
            content: vec![
                ContentBlock::Thinking(ThinkingContent {
                    thinking: "Let me reason...".into(),
                    signature: Some("sig123".into()),
                }),
                ContentBlock::Text(TextContent { text: "Here's the answer.".into() }),
            ],
            id: None,
            name: None,
            usage: None,
            redacted: false,
        };
        let context = Context { messages: vec![msg], system_prompt: None, model: None, tools: vec![] };
        let contents = convert_messages(&context);
        assert_eq!(contents.len(), 1);

        assert_eq!(contents[0]["role"], "model");
        let parts = contents[0]["parts"].as_array().unwrap();
        assert_eq!(parts.len(), 2);

        // Thinking block
        assert_eq!(parts[0]["thought"], true);
        assert_eq!(parts[0]["text"], "Let me reason...");
        assert_eq!(parts[0]["thoughtSignature"], "sig123");

        // Text block
        assert_eq!(parts[1]["text"], "Here's the answer.");
    }

    #[test]
    fn test_convert_tool_result() {
        let tool_msg = Message {
            role: MessageRole::Tool,
            content: vec![ContentBlock::ToolResult(ToolResultContent {
                id: "call_1".into(),
                name: "get_weather".into(),
                content: Some(vec![ContentBlock::Text(TextContent { text: "72 degrees".into() })]),
                error: None,
                is_error: false,
            })],
            id: None,
            name: None,
            usage: None,
            redacted: false,
        };
        let context = Context { messages: vec![tool_msg], system_prompt: None, model: None, tools: vec![] };
        let contents = convert_messages(&context);
        assert_eq!(contents.len(), 1);

        assert_eq!(contents[0]["role"], "user");
        let parts = contents[0]["parts"].as_array().unwrap();
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0]["functionResponse"]["name"], "get_weather");
        assert_eq!(parts[0]["functionResponse"]["response"]["output"], "72 degrees");
    }

    #[test]
    fn test_convert_tool_result_with_error() {
        let tool_msg = Message {
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
        let context = Context { messages: vec![tool_msg], system_prompt: None, model: None, tools: vec![] };
        let contents = convert_messages(&context);
        assert_eq!(contents.len(), 1);

        let parts = contents[0]["parts"].as_array().unwrap();
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0]["functionResponse"]["response"]["error"], "Connection refused");
    }

    #[test]
    fn test_convert_tools() {
        let tools = vec![ToolDefinition {
            name: "get_weather".into(),
            description: "Get weather for a location".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "location": { "type": "string" }
                },
                "required": ["location"],
            }),
            strict: Some(false),
        }];
        let converted = convert_tools(&tools);
        assert_eq!(converted.len(), 1);

        let declarations = converted[0]["functionDeclarations"].as_array().unwrap();
        assert_eq!(declarations.len(), 1);
        assert_eq!(declarations[0]["name"], "get_weather");
        assert_eq!(declarations[0]["description"], "Get weather for a location");
        assert!(declarations[0]["parameters"].is_object());
    }

    // ------------------------------------------------------------------
    // Stop reason mapping tests
    // ------------------------------------------------------------------

    #[test]
    fn test_map_stop_reasons() {
        assert_eq!(map_stop_reason("STOP"), "stop");
        assert_eq!(map_stop_reason("MAX_TOKENS"), "length");
        assert_eq!(map_stop_reason("SAFETY"), "error");
        assert_eq!(map_stop_reason("BLOCKLIST"), "error");
        assert_eq!(map_stop_reason("RECITATION"), "error");
        assert_eq!(map_stop_reason("OTHER"), "error");
        assert_eq!(map_stop_reason("MALFORMED_FUNCTION_CALL"), "error");
        assert_eq!(map_stop_reason("UNKNOWN_REASON"), "error:provider_finish_reason:UNKNOWN_REASON");
    }

    // ------------------------------------------------------------------
    // API key resolution tests
    // ------------------------------------------------------------------

    #[test]
    fn test_resolve_api_key_from_options() {
        let opts = StreamOptions { api_key: Some("test-key-from-options".into()), ..Default::default() };
        let result = resolve_api_key(&opts);
        assert_eq!(result.unwrap(), "test-key-from-options");
    }

    #[test]
    fn test_resolve_api_key_prefers_options() {
        let result = resolve_api_key(&StreamOptions { api_key: Some("option-key".into()), ..Default::default() });
        assert_eq!(result.unwrap(), "option-key");
    }

    // ------------------------------------------------------------------
    // Usage parsing tests
    // ------------------------------------------------------------------

    #[test]
    fn test_parse_usage_simple() {
        let raw = UsageMetadata {
            prompt_token_count: Some(100),
            candidates_token_count: Some(50),
            total_token_count: Some(150),
            cached_content_token_count: None,
            thoughts_token_count: None,
        };
        let usage = parse_usage(&raw);
        assert_eq!(usage.input, 100);
        assert_eq!(usage.output, 50);
        assert_eq!(usage.cache_read, Some(0));
        assert_eq!(usage.cache_write, Some(0));
        assert_eq!(usage.total_tokens, Some(150));
    }

    #[test]
    fn test_parse_usage_with_cache_and_thoughts() {
        let raw = UsageMetadata {
            prompt_token_count: Some(200),
            candidates_token_count: Some(30),
            total_token_count: Some(250),
            cached_content_token_count: Some(20),
            thoughts_token_count: Some(10),
        };
        let usage = parse_usage(&raw);
        // input = 200 - 20 = 180
        // output = 30 + 10 = 40
        assert_eq!(usage.input, 180);
        assert_eq!(usage.output, 40);
        assert_eq!(usage.cache_read, Some(20));
    }

    // ------------------------------------------------------------------
    // Cleanup for env-var-dependent tests
    // ------------------------------------------------------------------

    /// Ensure env vars are reset after tests that touch them.
    #[serial]
    #[tokio::test]
    async fn cleanup_env() {
        unsafe {
            std::env::remove_var("GEMINI_API_KEY");
            std::env::remove_var("GOOGLE_API_KEY");
        }
        clear_api_providers().await;
    }
}
