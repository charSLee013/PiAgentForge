//! Pi Provider — OpenAI Completions API.
//!
//! Maps to `packages/ai/src/providers/openai-completions.ts` in the TS source.
//!
//! This provider implements the [`ApiProvider`] trait for OpenAI's chat
//! completions API, supporting text streaming, reasoning/thinking content,
//! tool calls via SSE (Server-Sent Events), and image input.

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

/// Default OpenAI Chat Completions endpoint.
const OPENAI_CHAT_COMPLETIONS_URL: &str = "https://api.openai.com/v1/chat/completions";

/// SSE data prefix (the part before the actual JSON payload).
const SSE_DATA_PREFIX: &str = "data: ";

/// SSE stream-termination sentinel.
const SSE_DONE_SENTINEL: &str = "[DONE]";

// ---------------------------------------------------------------------------
// Response chunk types (deserialization from SSE `data: ...` lines)
// ---------------------------------------------------------------------------

/// Top-level streaming chunk from the OpenAI Chat Completions API.
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
    /// Some providers (e.g. Moonshot) place usage inside the choice object.
    usage: Option<ChunkUsage>,
}

/// Delta content within a choice.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Delta {
    content: Option<String>,
    tool_calls: Option<Vec<ToolCallDeltaChunk>>,
    /// Non-standard reasoning fields from compatible endpoints.
    reasoning_content: Option<String>,
    reasoning: Option<String>,
    reasoning_text: Option<String>,
}

impl Delta {
    /// Return the first non-empty reasoning delta found among the known field
    /// names (`reasoning_content`, `reasoning`, `reasoning_text`).
    fn get_reasoning(&self) -> Option<&str> {
        self.reasoning_content
            .as_deref()
            .or(self.reasoning.as_deref())
            .or(self.reasoning_text.as_deref())
            .filter(|s| !s.is_empty())
    }
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

/// Token usage reported in the final chunk.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ChunkUsage {
    prompt_tokens: Option<u64>,
    completion_tokens: Option<u64>,
    total_tokens: Option<u64>,
    prompt_tokens_details: Option<PromptTokensDetails>,
    prompt_cache_hit_tokens: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct PromptTokensDetails {
    cached_tokens: Option<u64>,
    cache_write_tokens: Option<u64>,
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
    /// The response model from any chunk where it differs from the request model.
    response_model: Option<String>,
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
// Provider struct
// ---------------------------------------------------------------------------

/// Provider for the OpenAI Chat Completions API (streaming).
///
/// Sends POST requests to `<base_url>/chat/completions` and parses the
/// SSE response stream, emitting [`StreamEvent`] items.
///
/// # Example
///
/// ```ignore
/// use pi_provider_openai::OpenAiCompletionsProvider;
/// use pi_ai_core::api_registry::register_api_provider;
///
/// let provider = OpenAiCompletionsProvider::new();
/// register_api_provider(Box::new(provider)).await;
/// ```
pub struct OpenAiCompletionsProvider {
    base_url: String,
}

impl OpenAiCompletionsProvider {
    /// Create a new provider that targets the standard OpenAI API endpoint.
    pub fn new() -> Self {
        Self {
            base_url: OPENAI_CHAT_COMPLETIONS_URL.to_owned(),
        }
    }

    /// Create a provider with a custom base URL (useful for testing or
    /// OpenAI-compatible backends).
    pub fn with_base_url(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
        }
    }
}

impl Default for OpenAiCompletionsProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl ApiProvider for OpenAiCompletionsProvider {
    fn api_id(&self) -> &str {
        "openai-completions"
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
                tracing::error!("OpenAI completions stream error: {e}");
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

    // 3. Send the HTTP request.
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(
            options.timeout.unwrap_or(120),
        ))
        .build()?;

    let response = client
        .post(base_url)
        .header("Authorization", format!("Bearer {api_key}"))
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| {
            emit_error(
                &tx,
                format!("HTTP request failed: {e}"),
                Some("request_error".to_owned()),
            );
            e
        })?;

    // 4. Check the HTTP status code.
    if !response.status().is_success() {
        let status = response.status();
        let error_text = response.text().await.unwrap_or_else(|_| String::new());
        emit_error(
            &tx,
            format!("OpenAI API error ({}): {error_text}", status.as_u16()),
            Some(status.as_str().to_owned()),
        );
        return Ok(());
    }

    // 5. Emit the Start event.
    let _ = tx.send(StreamEvent::Start);

    // 6. Process the SSE response body.
    let mut state = StreamState::default();
    if let Err(e) = process_sse_stream(&tx, response, &mut state).await {
        emit_error(
            &tx,
            format!("SSE stream error: {e}"),
            Some("stream_error".to_owned()),
        );
        return Ok(());
    }

    // 7. Emit the final Done event.
    let stop_reason = state.finish_reason.clone().unwrap_or_else(|| "stop".to_owned());
    let message = build_done_message(&state, model);
    let _ = tx.send(StreamEvent::Done {
        message: Some(message),
        stop_reason: Some(stop_reason),
    });

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
                        tracing::warn!("Failed to parse SSE chunk JSON: {e} — data: {data}");
                        // Non-fatal: skip malformed chunks.
                    }
                }
            }
        }
        // Lines that do not start with `data: ` are ignored per the SSE spec
        // (they may be comments or event-type lines).
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Chunk processing
// ---------------------------------------------------------------------------

/// Process a single parsed [`Chunk`] and emit the corresponding
/// [`StreamEvent`]s.
fn process_chunk(
    tx: &EventStreamSender<StreamEvent>,
    chunk: Chunk,
    state: &mut StreamState,
) {
    // Track response metadata.
    if state.response_id.is_none() {
        state.response_id = chunk.id.clone();
    }
    if let Some(ref model) = chunk.model {
        if !model.is_empty() && state.response_model.is_none() {
            state.response_model = Some(model.clone());
        }
    }

    // Usage chunk (choices array is empty, usage is present).
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
        // Fallback choice-level usage (some providers like Moonshot).
        if !state.usage_emitted {
            if let Some(ref choice_usage) = choice.usage {
                let parsed = parse_usage(choice_usage);
                let _ = tx.send(StreamEvent::Usage(parsed));
                state.usage_emitted = true;
            }
        }

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
    // --- Text content ---
    if let Some(ref content) = delta.content {
        if !content.is_empty() {
            state.text.push_str(content);
            let _ = tx.send(StreamEvent::TextDelta {
                delta: content.clone(),
            });
        }
    }

    // --- Reasoning / thinking content ---
    if let Some(reasoning) = delta.get_reasoning() {
        if !reasoning.is_empty() {
            state.thinking.push_str(reasoning);
            let _ = tx.send(StreamEvent::ThinkingDelta {
                delta: reasoning.to_owned(),
            });
        }
    }

    // --- Tool call deltas ---
    if let Some(ref tool_calls) = delta.tool_calls {
        for tc in tool_calls {
            let builder = state.get_or_create_tool_call(tc.index);

            // Determine if id or name should be emitted.
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
                // If the accumulated arguments string is not valid JSON, wrap it as a raw string.
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
        name: state.response_model.clone(),
        usage: None,
        redacted: false,
    }
}

// ---------------------------------------------------------------------------
// Request body construction
// ---------------------------------------------------------------------------

/// Build the JSON request body for the Chat Completions API.
fn build_request_body(model: &Model, context: &Context, options: &StreamOptions) -> serde_json::Value {
    let messages = convert_messages(context);
    let tools = convert_tools(&context.tools);

    let mut body = serde_json::json!({
        "model": model.id,
        "messages": messages,
        "stream": true,
        "stream_options": { "include_usage": true },
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
// Message conversion (pi-ai-core → OpenAI Chat Completions format)
// ---------------------------------------------------------------------------

/// Convert [`Context`] messages to OpenAI-compatible JSON array.
fn convert_messages(context: &Context) -> Vec<serde_json::Value> {
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
        let converted = match msg.role {
            MessageRole::System => convert_system_message(msg),
            MessageRole::User => convert_user_message(msg),
            MessageRole::Assistant => convert_assistant_message(msg),
            MessageRole::Tool => None, // handled below via ToolResult blocks
        };

        if let Some(val) = converted {
            result.push(val);
        }

        // Tool results are extracted from ToolRole messages and handled separately
        // as OpenAI "tool" role messages.
        if msg.role == MessageRole::Tool {
            let tool_msgs = convert_tool_result_messages(msg);
            result.extend(tool_msgs);
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

/// Convert an Assistant message (text + tool_calls).
fn convert_assistant_message(msg: &Message) -> Option<serde_json::Value> {
    let text = extract_text(&msg.content);
    let tool_calls = extract_tool_calls(&msg.content);
    let reasoning = extract_reasoning(&msg.content);

    // Only skip if text, tool_calls, AND reasoning are all empty.
    if text.is_empty() && tool_calls.is_empty() && reasoning.is_none() {
        return None;
    }

    let content_value = if text.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::Value::String(text)
    };

    let mut assistant_msg = serde_json::json!({
        "role": "assistant",
        "content": content_value,
    });

    if !tool_calls.is_empty() {
        assistant_msg["tool_calls"] = serde_json::json!(tool_calls);
    }

    // Include reasoning/thinking content for DeepSeek and compatible providers.
    // DeepSeek requires `reasoning_content` to be passed back on follow-up calls.
    if let Some(ref r) = reasoning {
        assistant_msg["reasoning_content"] = serde_json::json!(r);
    }

    Some(assistant_msg)
}

/// Extract tool call objects from assistant message content.
fn extract_tool_calls(content: &[ContentBlock]) -> Vec<serde_json::Value> {
    content
        .iter()
        .filter_map(|block| {
            if let ContentBlock::ToolCall(tc) = block {
                Some(serde_json::json!({
                    "id": tc.id,
                    "type": "function",
                    "function": {
                        "name": tc.name,
                        "arguments": serialize_arguments(&tc.arguments),
                    },
                }))
            } else {
                None
            }
        })
        .collect()
}

/// Serialize tool arguments to a JSON string for the OpenAI API.
fn serialize_arguments(args: &serde_json::Value) -> String {
    match args {
        serde_json::Value::String(s) => s.clone(),
        other => serde_json::to_string(other).unwrap_or_else(|_| "{}".to_owned()),
    }
}

/// Convert Tool-role messages (tool results) to OpenAI "tool" role messages.
fn convert_tool_result_messages(msg: &Message) -> Vec<serde_json::Value> {
    msg.content
        .iter()
        .filter_map(|block| {
            if let ContentBlock::ToolResult(tr) = block {
                let text = if tr.is_error {
                    if let Some(ref error) = tr.error {
                        format!("Error: {error}")
                    } else {
                        "Error".to_owned()
                    }
                } else if let Some(ref content) = tr.content {
                    extract_text(content)
                } else {
                    String::new()
                };

                let mut tool_msg = serde_json::json!({
                    "role": "tool",
                    "tool_call_id": tr.id,
                    "content": text,
                });

                // Include optional tool name for providers that require it.
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

/// Convert [`ToolDefinition`]s to OpenAI tool format.
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

/// Parse OpenAI chunk usage into the pi-ai-core [`Usage`] struct.
fn parse_usage(raw: &ChunkUsage) -> Usage {
    let prompt_tokens = raw.prompt_tokens.unwrap_or(0);
    let completion_tokens = raw.completion_tokens.unwrap_or(0);

    let reported_cached = raw
        .prompt_tokens_details
        .as_ref()
        .and_then(|d| d.cached_tokens)
        .or(raw.prompt_cache_hit_tokens)
        .unwrap_or(0);

    let cache_write = raw
        .prompt_tokens_details
        .as_ref()
        .and_then(|d| d.cache_write_tokens)
        .unwrap_or(0);

    // Normalize: some providers report cached_tokens as (previous hits +
    // current writes). Deduplicate.
    let cache_read = if cache_write > 0 {
        reported_cached.saturating_sub(cache_write)
    } else {
        reported_cached
    };

    let input = prompt_tokens.saturating_sub(cache_read + cache_write);

    Usage {
        input,
        output: completion_tokens,
        cache_read: Some(cache_read),
        cache_write: Some(cache_write),
        total_tokens: raw.total_tokens,
    }
}

// ---------------------------------------------------------------------------
// Stop reason mapping
// ---------------------------------------------------------------------------

/// Map an OpenAI `finish_reason` to the canonical stop-reason string used by pi.
fn map_stop_reason(reason: &str) -> String {
    match reason {
        "stop" | "end" => "stop".to_owned(),
        "length" => "length".to_owned(),
        "function_call" | "tool_calls" => "toolUse".to_owned(),
        "content_filter" => format!("error:provider_finish_reason:{reason}"),
        "network_error" => format!("error:provider_finish_reason:{reason}"),
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

/// Extract reasoning/thinking text from content blocks.
fn extract_reasoning(content: &[ContentBlock]) -> Option<String> {
    for block in content {
        if let ContentBlock::Thinking(th) = block {
            if !th.thinking.is_empty() {
                return Some(th.thinking.clone());
            }
        }
    }
    None
}

/// Resolve the API key from options or environment.
fn resolve_api_key(options: &StreamOptions) -> Result<String, String> {
    if let Some(ref key) = options.api_key {
        if !key.is_empty() {
            return Ok(key.clone());
        }
    }
    match std::env::var("OPENAI_API_KEY") {
        Ok(key) if !key.is_empty() => Ok(key),
        _ => Err(
            "OpenAI API key is required. Set the OPENAI_API_KEY environment \
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
    use pi_ai_core::types::{ImageContent, ImageSource, KnownProvider, ToolCallContent, ToolResultContent};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    // ------------------------------------------------------------------
    // Helpers
    // ------------------------------------------------------------------

    fn test_model() -> Model {
        Model {
            id: "gpt-4o-mini".into(),
            provider: KnownProvider::OpenAi,
            api: "openai-completions".into(),
            name: None,
            base_url: None,
            supports_thinking: false,
            supports_tools: true,
            supports_streaming: true,
            supports_image_input: true,
            max_tokens: None,
            max_input_tokens: None,
            max_output_tokens: Some(16384),
            cost_per_input_token: Some(0.00015),
            cost_per_output_token: Some(0.0006),
            cost_per_cache_read_token: None,
            cost_per_cache_write_token: None,
        }
    }

    async fn setup_provider(mock_server: &MockServer) {
        let provider =
            OpenAiCompletionsProvider::with_base_url(format!("{}/v1/chat/completions", mock_server.uri()));
        clear_api_providers().await;
        register_api_provider(Box::new(provider)).await;
    }

    /// Ensure OPENAI_API_KEY is set to a known value for all wiremock tests.
    fn ensure_api_key() {
        use std::sync::OnceLock;
        static INIT: OnceLock<()> = OnceLock::new();
        INIT.get_or_init(|| {
            // Only set if not already set (defensive for concurrent test threads)
            if std::env::var("OPENAI_API_KEY").is_err() {
                unsafe { std::env::set_var("OPENAI_API_KEY", "sk-test-placeholder"); }
            }
        });
    }

    /// Mount a mock SSE endpoint that returns the given body bytes.
    async fn mount_sse(mock_server: &MockServer, body: &'static str) {
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"))
            .mount(mock_server)
            .await;
    }

    /// Mount a mock that returns an HTTP error.
    async fn mount_error(mock_server: &MockServer, status: u16, body: &'static str) {
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
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
             data: {\"id\":\"ch1\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"!\"},\"finish_reason\":\"stop\"}]}\n\
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
    // Reasoning / thinking streaming tests
    // ------------------------------------------------------------------

    #[serial]
#[tokio::test]
    async fn test_text_stream_with_reasoning() {
        ensure_api_key();
        let mock = MockServer::start().await;
        mount_sse(
            &mock,
            "data: {\"id\":\"ch1\",\"choices\":[{\"index\":0,\"delta\":{\"reasoning_content\":\"Let me think...\"},\"finish_reason\":null}]}\n\
             \n\
             data: {\"id\":\"ch1\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"The answer is 42\"},\"finish_reason\":null}]}\n\
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
            messages: vec![Message::user_text("What is the meaning?")],
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

        let message = &result.message;
        assert!(message.content.len() >= 2);
        // First block should be thinking.
        match &message.content[0] {
            ContentBlock::Thinking(th) => assert_eq!(th.thinking, "Let me think..."),
            _ => panic!("Expected thinking block first"),
        }
        // Last block should be text.
        match &message.content.last().unwrap() {
            ContentBlock::Text(t) => assert_eq!(t.text, "The answer is 42"),
            _ => panic!("Expected text block last"),
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
            "data: {\"id\":\"ch1\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"\"},\"finish_reason\":null}]}\n\
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
                assert_eq!(tc.id, "call_1");
                assert_eq!(tc.name, "get_weather");
                assert_eq!(
                    tc.arguments,
                    serde_json::json!({"location": "NYC"})
                );
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
        ensure_api_key();
        // No API key set, no options key — should produce an Error event.
        let model = test_model();
        let context = Context {
            messages: vec![Message::user_text("Hi")],
            system_prompt: None,
            model: None,
            tools: vec![],
        };

        // Use a provider with no API key — should error on first HTTP request.
        let provider = OpenAiCompletionsProvider::with_base_url("http://0.0.0.0:1/v1/chat/completions");
        clear_api_providers().await;
        register_api_provider(Box::new(provider)).await;

        let stream = stream::stream(&model, context, StreamOptions {
            api_key: Some(String::new()),
            ..Default::default()
        })
            .await
            .expect("stream() should return a stream");
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

    #[serial]
#[tokio::test]
    async fn test_invalid_sse_chunk_is_skipped() {
        ensure_api_key();
        let mock = MockServer::start().await;
        mount_sse(
            &mock,
            "data: {\"id\":\"ch1\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hello\"},\"finish_reason\":null}]}\n\
             \n\
             data: NOT_VALID_JSON\n\
             \n\
             data: {\"id\":\"ch1\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\" world\"},\"finish_reason\":null}]}\n\
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
        assert_eq!(text, "Hello world", "Invalid SSE chunk should be skipped");
    }

    // ------------------------------------------------------------------
    // Message conversion unit tests
    // ------------------------------------------------------------------

    #[test]
    fn test_convert_user_message_text_only() {
        let context = Context {
            messages: vec![Message::user_text("Hello")],
            system_prompt: None,
            model: None,
            tools: vec![],
        };
        let messages = convert_messages(&context);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[0]["content"][0]["type"], "text");
        assert_eq!(messages[0]["content"][0]["text"], "Hello");
    }

    #[test]
    fn test_convert_user_message_with_image() {
        let msg = Message {
            role: MessageRole::User,
            content: vec![
                ContentBlock::Text(TextContent {
                    text: "What's in this image?".into(),
                }),
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
        let context = Context {
            messages: vec![msg],
            system_prompt: None,
            model: None,
            tools: vec![],
        };
        let messages = convert_messages(&context);
        assert_eq!(messages.len(), 1);

        let content = messages[0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[1]["type"], "image_url");
        assert!(
            content[1]["image_url"]["url"]
                .as_str()
                .unwrap()
                .starts_with("data:image/png;base64,")
        );
    }

    #[test]
    fn test_convert_assistant_message_with_tool_calls() {
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
        let messages = convert_messages(&context);
        assert_eq!(messages.len(), 1);

        assert_eq!(messages[0]["role"], "assistant");
        assert_eq!(messages[0]["content"], "I'll look that up.");
        let tool_calls = messages[0]["tool_calls"].as_array().unwrap();
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0]["id"], "call_abc");
        assert_eq!(tool_calls[0]["function"]["name"], "search_web");
        assert_eq!(tool_calls[0]["function"]["arguments"], r#"{"query":"Rust programming"}"#);
    }

    #[test]
    fn test_convert_tool_result_messages() {
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
        let messages = convert_messages(&context);
        assert_eq!(messages.len(), 1);

        assert_eq!(messages[0]["role"], "tool");
        assert_eq!(messages[0]["tool_call_id"], "call_def");
        assert_eq!(messages[0]["content"], "12:00 PM");
    }

    #[test]
    fn test_convert_tool_result_with_error() {
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
        let messages = convert_messages(&context);
        assert_eq!(messages.len(), 1);

        assert_eq!(messages[0]["role"], "tool");
        assert_eq!(messages[0]["tool_call_id"], "call_err");
        assert!(messages[0]["content"].as_str().unwrap().contains("Error"));
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
            prompt_tokens_details: None,
            prompt_cache_hit_tokens: None,
        };
        let usage = parse_usage(&raw);
        assert_eq!(usage.input, 100);
        assert_eq!(usage.output, 50);
        assert_eq!(usage.cache_read, Some(0));
        assert_eq!(usage.cache_write, Some(0));
        assert_eq!(usage.total_tokens, Some(150));
    }

    #[test]
    fn test_parse_usage_with_cache() {
        let raw = ChunkUsage {
            prompt_tokens: Some(200),
            completion_tokens: Some(30),
            total_tokens: Some(230),
            prompt_tokens_details: Some(PromptTokensDetails {
                cached_tokens: Some(50),
                cache_write_tokens: Some(10),
            }),
            prompt_cache_hit_tokens: None,
        };
        let usage = parse_usage(&raw);
        // cache_read = reported(50) - cache_write(10) = 40
        // input = prompt(200) - cache_read(40) - cache_write(10) = 150
        assert_eq!(usage.input, 150);
        assert_eq!(usage.cache_read, Some(40));
        assert_eq!(usage.cache_write, Some(10));
    }

    // ------------------------------------------------------------------
    // Stop reason mapping unit tests
    // ------------------------------------------------------------------

    #[test]
    fn test_map_stop_reasons() {
        assert_eq!(map_stop_reason("stop"), "stop");
        assert_eq!(map_stop_reason("end"), "stop");
        assert_eq!(map_stop_reason("length"), "length");
        assert_eq!(map_stop_reason("tool_calls"), "toolUse");
        assert_eq!(map_stop_reason("function_call"), "toolUse");
        assert!(map_stop_reason("content_filter").contains("error"));
        assert!(map_stop_reason("network_error").contains("error"));
        assert!(map_stop_reason("unknown").contains("error"));
    }

    // ------------------------------------------------------------------
    // Cleanup for env-var-dependent tests
    // ------------------------------------------------------------------

}
