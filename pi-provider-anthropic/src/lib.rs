//! Pi Provider — Anthropic Messages API.
//!
//! Maps to `packages/ai/src/providers/anthropic.ts` in the TS source.
//!
//! This provider implements the [`ApiProvider`] trait for Anthropic's Messages
//! API, supporting text streaming, thinking blocks with signatures, tool calls,
//! image input, multiple authentication paths, and SSE-based event streaming.

use pi_ai_core::api_registry::ApiProvider;
use pi_ai_core::event_stream::{AssistantMessageEventStream, EventStreamSender};
use pi_ai_core::types::{
    ContentBlock, Context, ImageContent, ImageSource, Message, MessageRole, Model, StreamError, StreamEvent,
    StreamOptions, TextContent, ThinkingContent, ToolCallContent, ToolResultContent, Usage,
};
use serde::Deserialize;
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Default Anthropic Messages API endpoint.
const DEFAULT_ANTHROPIC_API_URL: &str = "https://api.anthropic.com/v1/messages";

/// Minimum number of output tokens to reserve when thinking is active.
const MIN_OUTPUT_TOKENS: u64 = 1024;

/// Default thinking budget tokens for older models.
const DEFAULT_THINKING_BUDGET: u64 = 1024;

/// Beta header for interleaved thinking (deprecated on Opus 4.6+ but harmless).
const INTERLEAVED_THINKING_BETA: &str = "interleaved-thinking-2025-04-15";

// ---------------------------------------------------------------------------
// Anthropic-specific SSE event types
// ---------------------------------------------------------------------------

/// Top-level Anthropic stream event (tagged by the JSON `type` field).
#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum AnthropicStreamPayload {
    #[serde(rename = "message_start")]
    MessageStart { message: AnthropicMessageInfo },
    #[serde(rename = "content_block_start")]
    ContentBlockStart { index: u32, content_block: AnthropicContentBlockInfo },
    #[serde(rename = "content_block_delta")]
    ContentBlockDelta { index: u32, delta: AnthropicDelta },
    #[serde(rename = "content_block_stop")]
    ContentBlockStop { index: u32 },
    #[serde(rename = "message_delta")]
    MessageDelta { delta: AnthropicMessageDelta, usage: Option<AnthropicUsageRaw> },
    #[serde(rename = "message_stop")]
    MessageStop,
    #[serde(rename = "ping")]
    Ping,
    #[serde(rename = "error")]
    Error { error: AnthropicErrorInfo },
}

/// Info about the message extracted from `message_start`.
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct AnthropicMessageInfo {
    #[serde(default)]
    id: String,
    #[serde(default)]
    model: String,
    #[serde(default)]
    stop_reason: Option<String>,
    #[serde(default)]
    stop_sequence: Option<String>,
    #[serde(default)]
    usage: AnthropicUsageRaw,
}

/// A content block within `content_block_start`.
#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum AnthropicContentBlockInfo {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "thinking")]
    Thinking {
        thinking: String,
        #[serde(default)]
        signature: Option<String>,
    },
    #[serde(rename = "redacted_thinking")]
    RedactedThinking {
        /// Opaque encrypted data that must be sent back as-is.
        #[serde(default)]
        data: String,
    },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        #[serde(default)]
        input: serde_json::Value,
    },
}

/// A delta within `content_block_delta`.
#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
#[allow(clippy::enum_variant_names)]
enum AnthropicDelta {
    #[serde(rename = "text_delta")]
    TextDelta { text: String },
    #[serde(rename = "thinking_delta")]
    ThinkingDelta { thinking: String },
    #[serde(rename = "signature_delta")]
    SignatureDelta { signature: String },
    #[serde(rename = "input_json_delta")]
    InputJsonDelta {
        #[serde(rename = "partial_json")]
        partial_json: String,
    },
}

/// The `delta` sub-object within `message_delta`.
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct AnthropicMessageDelta {
    #[serde(default)]
    stop_reason: Option<String>,
    #[serde(default)]
    stop_sequence: Option<String>,
}

/// Raw usage numbers from the Anthropic API response.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct AnthropicUsageRaw {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    #[serde(rename = "cache_creation_input_tokens")]
    cache_write_tokens: Option<u64>,
    #[serde(rename = "cache_read_input_tokens")]
    cache_read_tokens: Option<u64>,
}

/// Error payload from an `error` SSE event.
#[derive(Debug, Deserialize)]
struct AnthropicErrorInfo {
    #[serde(default)]
    r#type: Option<String>,
    #[serde(default)]
    message: Option<String>,
}

// ---------------------------------------------------------------------------
// Stream state
// ---------------------------------------------------------------------------

/// Accumulated state while processing an Anthropic SSE stream.
#[derive(Debug, Default)]
struct AnthropicStreamState {
    /// All text content accumulated across all text blocks.
    text: String,
    /// All thinking text accumulated across all thinking blocks.
    thinking: String,
    /// The most recent thinking signature (for round-tripping).
    thinking_signature: Option<String>,
    /// Whether the most recent thinking block was redacted.
    thinking_redacted: bool,
    /// Tool calls being accumulated, keyed by content block index.
    tool_calls: HashMap<u32, ToolCallBuilder>,
    /// The stop reason from the final `message_delta`.
    stop_reason: Option<String>,
    /// The response ID from `message_start`.
    response_id: Option<String>,
    /// The response model from `message_start`.
    response_model: Option<String>,
    /// Track whether we've seen `message_start`.
    saw_message_start: bool,
    /// Track whether we've seen `message_stop`.
    saw_message_stop: bool,
    /// Token usage.
    input_tokens: u64,
    output_tokens: u64,
    cache_read: u64,
    cache_write: u64,
}

/// Accumulates streamed data for a single tool call.
#[derive(Debug)]
#[allow(dead_code)]
struct ToolCallBuilder {
    index: u32,
    id: String,
    name: String,
    arguments: String,
    /// Whether the `id` has already been emitted via `ToolCallDelta`.
    id_emitted: bool,
    /// Whether the `name` has already been emitted via `ToolCallDelta`.
    name_emitted: bool,
}

impl AnthropicStreamState {
    fn get_or_create_tool_call(&mut self, index: u32) -> &mut ToolCallBuilder {
        self.tool_calls.entry(index).or_insert_with(|| ToolCallBuilder {
            index,
            id: String::new(),
            name: String::new(),
            arguments: String::new(),
            id_emitted: false,
            name_emitted: false,
        })
    }
}

// ---------------------------------------------------------------------------
// Provider struct
// ---------------------------------------------------------------------------

/// Provider for the Anthropic Messages API (streaming).
///
/// Sends POST requests to the Messages API endpoint and parses the
/// SSE response stream, emitting [`StreamEvent`] items.
///
/// # Example
///
/// ```ignore
/// use pi_provider_anthropic::AnthropicProvider;
/// use pi_ai_core::api_registry::register_api_provider;
///
/// let provider = AnthropicProvider::new();
/// register_api_provider(Box::new(provider)).await;
/// ```
pub struct AnthropicProvider {
    base_url: String,
}

impl AnthropicProvider {
    /// Create a new provider that targets the standard Anthropic Messages API.
    /// If `ANTHROPIC_BASE_URL` is set in the environment, it uses that instead.
    pub fn new() -> Self {
        let base_url = std::env::var("ANTHROPIC_BASE_URL")
            .ok()
            .filter(|u| !u.is_empty())
            .unwrap_or_else(|| DEFAULT_ANTHROPIC_API_URL.to_owned());
        Self { base_url }
    }

    /// Create a provider with a custom base URL (useful for testing or
    /// Anthropic-compatible backends).
    pub fn with_base_url(base_url: impl Into<String>) -> Self {
        Self { base_url: base_url.into() }
    }
}

impl Default for AnthropicProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl ApiProvider for AnthropicProvider {
    fn api_id(&self) -> &str {
        "anthropic-messages"
    }

    fn stream(&self, model: &Model, context: Context, options: StreamOptions) -> AssistantMessageEventStream {
        let (tx, rx) = AssistantMessageEventStream::new();
        let model = model.clone();
        let base_url = self.base_url.clone();

        tokio::spawn(async move {
            if let Err(e) = process_stream(tx, &base_url, &model, context, options).await {
                tracing::error!("Anthropic stream error: {e}");
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

    // 2. Determine whether the key is an OAuth token (starts with "sk-ant-oat").
    let is_oauth = is_oauth_token(&api_key);

    // 3. Build the JSON request body.
    let body = build_request_body(model, &context, &options, is_oauth);

    // 4. Send the HTTP request.
    let client =
        reqwest::Client::builder().timeout(std::time::Duration::from_secs(options.timeout.unwrap_or(120))).build()?;

    let mut req =
        client.post(base_url).header("Content-Type", "application/json").header("anthropic-version", "2023-06-01");

    // Set auth header based on token type.
    if is_oauth {
        req = req.header("Authorization", format!("Bearer {api_key}"));
        req = req.header("anthropic-dangerous-direct-browser-access", "true");
        // Add beta headers for OAuth.
        req = req.header("anthropic-beta", {
            let betas = [INTERLEAVED_THINKING_BETA];
            betas.join(",")
        });
    } else {
        req = req.header("x-api-key", &api_key);
        req = req.header("anthropic-beta", INTERLEAVED_THINKING_BETA);
    }

    let response = req.json(&body).send().await.map_err(|e| {
        emit_error(&tx, format!("HTTP request failed: {e}"), Some("request_error".to_owned()));
        e
    })?;

    // 5. Check the HTTP status code.
    if !response.status().is_success() {
        let status = response.status();
        let error_text = response.text().await.unwrap_or_else(|_| String::new());
        emit_error(
            &tx,
            format!("Anthropic API error ({}): {error_text}", status.as_u16()),
            Some(status.as_str().to_owned()),
        );
        return Ok(());
    }

    // Ensure it's a SSE response.
    let content_type =
        response.headers().get(reqwest::header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("");
    if !content_type.contains("text/event-stream") {
        tracing::warn!("Expected text/event-stream, got: {}", content_type);
    }

    // 6. Emit the Start event.
    let _ = tx.send(StreamEvent::Start);

    // 7. Process the SSE response body.
    let mut state = AnthropicStreamState::default();
    if let Err(e) = process_sse_stream(&tx, response, &mut state).await {
        emit_error(&tx, format!("SSE stream error: {e}"), Some("stream_error".to_owned()));
        return Ok(());
    }

    // Validate that we saw both start and stop.
    if state.saw_message_start && !state.saw_message_stop {
        emit_error(&tx, "Anthropic stream ended before message_stop".to_owned(), Some("incomplete_stream".to_owned()));
        return Ok(());
    }

    // 8. Build the usage and emit Done.
    let usage = build_usage(&state);
    let stop_reason = state.stop_reason.clone().unwrap_or_else(|| "stop".to_owned());
    let message = build_done_message(&state, model, usage.clone());

    let _ = tx.send(StreamEvent::Done { message: Some(message), stop_reason: Some(stop_reason) });

    Ok(())
}

// ---------------------------------------------------------------------------
// SSE stream parsing (event:/data: format)
// ---------------------------------------------------------------------------

/// Known Anthropic message event types (SSE `event:` names).
/// Maps to `ANTHROPIC_MESSAGE_EVENTS` in the TS source.
const ANTHROPIC_MESSAGE_EVENTS: &[&str] = &[
    "message_start",
    "message_delta",
    "message_stop",
    "content_block_start",
    "content_block_delta",
    "content_block_stop",
];

/// Read an SSE text/event-stream from the HTTP response body, splitting on
/// newlines, tracking `event:` and `data:` lines, and dispatching complete
/// events on blank-line boundaries.
async fn process_sse_stream(
    tx: &EventStreamSender<StreamEvent>,
    response: reqwest::Response,
    state: &mut AnthropicStreamState,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use tokio_stream::StreamExt;

    let byte_stream = response.bytes_stream();
    tokio::pin!(byte_stream);

    let mut buffer: Vec<u8> = Vec::new();
    let mut current_event: Option<String> = None;
    let mut current_data: Vec<String> = Vec::new();

    while let Some(chunk_result) = byte_stream.next().await {
        let chunk = chunk_result?;
        buffer.extend_from_slice(&chunk);

        // Process as many complete lines as possible.
        while let Some(newline_pos) = buffer.iter().position(|&b| b == b'\n') {
            let raw_line: Vec<u8> = buffer.drain(..=newline_pos).collect();
            let line_str = String::from_utf8_lossy(strip_line_ending(&raw_line));

            // Empty line: flush the accumulated event.
            if line_str.is_empty() {
                if let Some(event_type) = current_event.take() {
                    let data = current_data.join("\n");
                    current_data.clear();

                    // Filter to known Anthropic message events.
                    if ANTHROPIC_MESSAGE_EVENTS.contains(&event_type.as_str()) {
                        if let Err(e) = dispatch_event(tx, &event_type, &data, state) {
                            tracing::warn!("Error processing event {event_type}: {e}");
                        }
                    } else if event_type == "error" {
                        // Error events are reported as exceptions in the TS code.
                        if !data.is_empty() {
                            return Err(format!("Anthropic API error: {data}").into());
                        }
                    }
                    // "ping" and other events are silently ignored.
                }
                continue;
            }

            // Parse SSE field lines.
            if let Some(value) = line_str.strip_prefix("event: ") {
                // Strip leading space is part of the SSE spec.
                current_event = Some(value.to_owned());
            } else if let Some(value) = line_str.strip_prefix("data: ") {
                current_data.push(value.to_owned());
            }
            // Lines that start with ':' are comments and are ignored.
            // Lines without a recognized prefix are ignored per the SSE spec.
        }
    }

    // Flush any remaining event data (no trailing blank line).
    if let Some(event_type) = current_event.take() {
        let data = current_data.join("\n");
        if ANTHROPIC_MESSAGE_EVENTS.contains(&event_type.as_str()) {
            if let Err(e) = dispatch_event(tx, &event_type, &data, state) {
                tracing::warn!("Error processing trailing event {event_type}: {e}");
            }
        } else if event_type == "error" && !data.is_empty() {
            return Err(format!("Anthropic API error: {data}").into());
        }
    }

    Ok(())
}

/// Strip trailing `\r` or `\r\n` from a line ending byte sequence.
fn strip_line_ending(bytes: &[u8]) -> &[u8] {
    let bytes = if bytes.ends_with(b"\n") { &bytes[..bytes.len() - 1] } else { bytes };
    if bytes.ends_with(b"\r") { &bytes[..bytes.len() - 1] } else { bytes }
}

// ---------------------------------------------------------------------------
// Event dispatching
// ---------------------------------------------------------------------------

/// Dispatch a single parsed SSE event to the appropriate handler.
fn dispatch_event(
    tx: &EventStreamSender<StreamEvent>,
    _event_type: &str,
    data: &str,
    state: &mut AnthropicStreamState,
) -> Result<(), String> {
    // Parse the JSON payload using the tagged `type` field.
    let payload: AnthropicStreamPayload = serde_json::from_str(data)
        .map_err(|e| format!("Could not parse Anthropic SSE event {}: {}; data={}", _event_type, e, data))?;

    match payload {
        AnthropicStreamPayload::MessageStart { message } => {
            state.saw_message_start = true;
            state.response_id = Some(message.id);
            state.response_model = Some(message.model);

            // Capture initial usage.
            let raw = message.usage;
            state.input_tokens = raw.input_tokens.unwrap_or(0);
            state.output_tokens = raw.output_tokens.unwrap_or(0);
            state.cache_read = raw.cache_read_tokens.unwrap_or(0);
            state.cache_write = raw.cache_write_tokens.unwrap_or(0);

            // Emit usage from message_start.
            let usage = build_usage(state);
            let _ = tx.send(StreamEvent::Usage(usage));
        }

        AnthropicStreamPayload::ContentBlockStart { index, content_block } => {
            match content_block {
                AnthropicContentBlockInfo::Text { text } => {
                    // Initialize text accumulation for this block; the zero-length
                    // text will be filled by subsequent delta events.
                    if !text.is_empty() {
                        state.text.push_str(&text);
                        let _ = tx.send(StreamEvent::TextDelta { delta: text });
                    }
                }
                AnthropicContentBlockInfo::Thinking { thinking, signature: _sig } => {
                    state.thinking.push_str(&thinking);
                    state.thinking_redacted = false;
                    // The signature from content_block_start is the initial one;
                    // signature_delta events will append to it.
                    let _ = tx.send(StreamEvent::ThinkingDelta { delta: thinking });
                }
                AnthropicContentBlockInfo::RedactedThinking { data } => {
                    state.thinking.push_str("[Reasoning redacted]");
                    state.thinking_signature = Some(data);
                    state.thinking_redacted = true;
                    let _ = tx.send(StreamEvent::ThinkingDelta { delta: "[Reasoning redacted]".to_owned() });
                }
                AnthropicContentBlockInfo::ToolUse { id, name, input } => {
                    let builder = state.get_or_create_tool_call(index);
                    builder.id = id.clone();
                    builder.name = name.clone();

                    // Tool use may carry pre-filled input (non-streaming tools).
                    // Empty object `{}` means streaming mode — skip serialization
                    // to avoid duplicating with input_json_delta events.
                    if !input.is_null() {
                        let is_empty = input.as_object().map(|obj| obj.is_empty()).unwrap_or(false);
                        if !is_empty {
                            let args_str = serde_json::to_string(&input).unwrap_or_else(|_| "{}".to_owned());
                            builder.arguments = args_str;
                        }
                    }

                    // Emit the initial tool call event with id and name.
                    let _ =
                        tx.send(StreamEvent::ToolCallDelta { index, id: Some(id), name: Some(name), arguments: None });
                    builder.id_emitted = true;
                    builder.name_emitted = true;
                }
            }
        }

        AnthropicStreamPayload::ContentBlockDelta { index, delta } => {
            match delta {
                AnthropicDelta::TextDelta { text } => {
                    state.text.push_str(&text);
                    let _ = tx.send(StreamEvent::TextDelta { delta: text });
                }
                AnthropicDelta::ThinkingDelta { thinking } => {
                    state.thinking.push_str(&thinking);
                    let _ = tx.send(StreamEvent::ThinkingDelta { delta: thinking });
                }
                AnthropicDelta::SignatureDelta { signature } => {
                    // Accumulate signature for thinking round-trip.
                    let current = state.thinking_signature.take().unwrap_or_default();
                    state.thinking_signature = Some(current + &signature);
                }
                AnthropicDelta::InputJsonDelta { partial_json } => {
                    let builder = state.get_or_create_tool_call(index);
                    builder.arguments.push_str(&partial_json);

                    let _ = tx.send(StreamEvent::ToolCallDelta {
                        index,
                        id: if builder.id_emitted {
                            None
                        } else {
                            builder.id_emitted = true;
                            Some(builder.id.clone())
                        },
                        name: if builder.name_emitted {
                            None
                        } else {
                            builder.name_emitted = true;
                            Some(builder.name.clone())
                        },
                        arguments: Some(partial_json),
                    });
                }
            }
        }

        AnthropicStreamPayload::ContentBlockStop { index: _index } => {
            // Nothing to emit here for the flat event model.
            // All deltas have already been pushed.
        }

        AnthropicStreamPayload::MessageDelta { delta, usage } => {
            if let Some(ref reason) = delta.stop_reason {
                state.stop_reason = Some(map_stop_reason(reason));
            }

            // Update usage from message_delta (preserving input_tokens if not present).
            if let Some(raw) = usage {
                if let Some(val) = raw.input_tokens {
                    state.input_tokens = val;
                }
                if let Some(val) = raw.output_tokens {
                    state.output_tokens = val;
                }
                if let Some(val) = raw.cache_read_tokens {
                    state.cache_read = val;
                }
                if let Some(val) = raw.cache_write_tokens {
                    state.cache_write = val;
                }

                let usage = build_usage(state);
                let _ = tx.send(StreamEvent::Usage(usage));
            }
        }

        AnthropicStreamPayload::MessageStop => {
            state.saw_message_stop = true;
        }

        AnthropicStreamPayload::Ping => {
            // Pings are health-check events; silently ignore.
        }

        AnthropicStreamPayload::Error { error } => {
            let msg = error.message.unwrap_or_else(|| "Unknown error".to_owned());
            let code = error.r#type;
            let _ = tx.send(StreamEvent::Error { error: StreamError { message: msg, code, r#type: None } });
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Final message construction
// ---------------------------------------------------------------------------

/// Build the final [`Message`] from the accumulated streaming state.
fn build_done_message(state: &AnthropicStreamState, _model: &Model, usage: Usage) -> Message {
    let mut content: Vec<ContentBlock> = Vec::new();

    // Add thinking block if we have thinking content.
    if !state.thinking.is_empty() || state.thinking_redacted {
        content.push(ContentBlock::Thinking(ThinkingContent {
            thinking: state.thinking.clone(),
            signature: state.thinking_signature.clone(),
        }));
    }

    // Add text block if we have text content.
    if !state.text.is_empty() {
        content.push(ContentBlock::Text(TextContent { text: state.text.clone() }));
    }

    // Add tool call blocks.
    let mut tool_indices: Vec<u32> = state.tool_calls.keys().copied().collect();
    tool_indices.sort();
    for idx in tool_indices {
        if let Some(builder) = state.tool_calls.get(&idx) {
            let parsed_args: serde_json::Value = serde_json::from_str(&builder.arguments)
                .unwrap_or_else(|_| serde_json::Value::String(builder.arguments.clone()));

            content.push(ContentBlock::ToolCall(ToolCallContent {
                id: builder.id.clone(),
                name: builder.name.clone(),
                arguments: parsed_args,
            }));
        }
    }

    Message {
        role: pi_ai_core::types::MessageRole::Assistant,
        content,
        id: state.response_id.clone(),
        name: state.response_model.clone(),
        usage: Some(usage),
        redacted: false,
    }
}

/// Build a [`Usage`] struct from the current state.
fn build_usage(state: &AnthropicStreamState) -> Usage {
    let total = state
        .input_tokens
        .saturating_add(state.output_tokens)
        .saturating_add(state.cache_read)
        .saturating_add(state.cache_write);

    Usage {
        input: state.input_tokens,
        output: state.output_tokens,
        cache_read: Some(state.cache_read),
        cache_write: Some(state.cache_write),
        total_tokens: Some(total),
    }
}

// ---------------------------------------------------------------------------
// Request body construction
// ---------------------------------------------------------------------------

/// Build the JSON request body for the Anthropic Messages API.
fn build_request_body(model: &Model, context: &Context, options: &StreamOptions, is_oauth: bool) -> serde_json::Value {
    let messages = convert_messages(context, is_oauth);
    let tools = convert_tools(context, is_oauth);

    let max_tokens = options.max_tokens.unwrap_or_else(|| model.max_tokens.unwrap_or(4096).max(MIN_OUTPUT_TOKENS));

    let mut body = serde_json::json!({
        "model": model.id,
        "messages": messages,
        "max_tokens": max_tokens,
        "stream": true,
    });

    // System prompt (separate parameter, not in messages array).
    if let Some(ref system_prompt) = context.system_prompt {
        if !system_prompt.is_empty() {
            body["system"] = serde_json::json!(system_prompt);
        }
    }

    // Tools.
    if !tools.is_empty() {
        body["tools"] = serde_json::json!(tools);
    }

    // Thinking mode.
    if options.thinking.unwrap_or(false) && model.supports_thinking {
        body["thinking"] = serde_json::json!({
            "type": "enabled",
            "budget_tokens": DEFAULT_THINKING_BUDGET,
        });
    }

    body
}

// ---------------------------------------------------------------------------
// Message conversion (pi-ai-core -> Anthropic messages format)
// ---------------------------------------------------------------------------

/// Normalize tool call IDs to match Anthropic's required pattern and length.
fn normalize_tool_call_id(id: &str) -> String {
    id.chars()
        .map(|c| if c.is_alphanumeric() || c == '_' || c == '-' { c } else { '_' })
        .collect::<String>()
        .chars()
        .take(64)
        .collect()
}

/// Convert [`Context`] messages to Anthropic-compatible messages array.
fn convert_messages(context: &Context, _is_oauth: bool) -> Vec<serde_json::Value> {
    let mut result: Vec<serde_json::Value> = Vec::new();
    let mut i = 0;

    while i < context.messages.len() {
        let msg = &context.messages[i];

        match msg.role {
            MessageRole::System => {
                // System messages are handled via the `system` parameter;
                // skip them in the messages array.
                i += 1;
                continue;
            }
            MessageRole::User => {
                let converted = convert_user_message(msg);
                if let Some(val) = converted {
                    result.push(val);
                }
                i += 1;
            }
            MessageRole::Assistant => {
                let converted = convert_assistant_message(msg);
                if let Some(val) = converted {
                    result.push(val);
                }
                i += 1;
            }
            MessageRole::Tool => {
                // Collect consecutive tool result messages into a single user message.
                let mut tool_results: Vec<serde_json::Value> = Vec::new();

                while i < context.messages.len() && context.messages[i].role == MessageRole::Tool {
                    let tool_msg = &context.messages[i];
                    for block in &tool_msg.content {
                        if let ContentBlock::ToolResult(tr) = block {
                            let content = convert_tool_result_content(tr);
                            tool_results.push(serde_json::json!({
                                "type": "tool_result",
                                "tool_use_id": normalize_tool_call_id(&tr.id),
                                "content": content,
                                "is_error": tr.is_error,
                            }));
                        }
                    }
                    i += 1;
                }

                if !tool_results.is_empty() {
                    result.push(serde_json::json!({
                        "role": "user",
                        "content": tool_results,
                    }));
                }
            }
        }
    }

    result
}

/// Convert a user message to Anthropic format.
fn convert_user_message(msg: &Message) -> Option<serde_json::Value> {
    let has_images = msg.content.iter().any(|b| matches!(b, ContentBlock::Image(_)));

    if !has_images {
        // Simple text-only message.
        let text = extract_text(&msg.content);
        if text.trim().is_empty() {
            return None;
        }
        return Some(serde_json::json!({
            "role": "user",
            "content": text,
        }));
    }

    // Mixed content: build content block array.
    let mut blocks: Vec<serde_json::Value> = Vec::new();
    let mut has_text = false;

    for block in &msg.content {
        match block {
            ContentBlock::Text(t) => {
                if !t.text.trim().is_empty() {
                    blocks.push(serde_json::json!({
                        "type": "text",
                        "text": t.text,
                    }));
                    has_text = true;
                }
            }
            ContentBlock::Image(img) => {
                if let Some(image_block) = convert_image_block(img) {
                    blocks.push(image_block);
                }
            }
            _ => {}
        }
    }

    // If only images (no text), add placeholder text block.
    if !has_text && !blocks.is_empty() {
        blocks.insert(
            0,
            serde_json::json!({
                "type": "text",
                "text": "(see attached image)",
            }),
        );
    }

    if blocks.is_empty() {
        return None;
    }

    Some(serde_json::json!({
        "role": "user",
        "content": blocks,
    }))
}

/// Convert an assistant message to Anthropic format.
fn convert_assistant_message(msg: &Message) -> Option<serde_json::Value> {
    let mut blocks: Vec<serde_json::Value> = Vec::new();

    for block in &msg.content {
        match block {
            ContentBlock::Text(t) => {
                if t.text.trim().is_empty() {
                    continue;
                }
                blocks.push(serde_json::json!({
                    "type": "text",
                    "text": t.text,
                }));
            }
            ContentBlock::Thinking(th) => {
                if th.thinking.trim().is_empty() {
                    continue;
                }
                // If we have a signature, send as a proper thinking block.
                if let Some(ref sig) = th.signature {
                    if !sig.trim().is_empty() {
                        blocks.push(serde_json::json!({
                            "type": "thinking",
                            "thinking": th.thinking,
                            "signature": sig,
                        }));
                    } else {
                        // No valid signature: degrade to text block.
                        blocks.push(serde_json::json!({
                            "type": "text",
                            "text": th.thinking,
                        }));
                    }
                } else {
                    // No signature: degrade to text block.
                    blocks.push(serde_json::json!({
                        "type": "text",
                        "text": th.thinking,
                    }));
                }
            }
            ContentBlock::ToolCall(tc) => {
                blocks.push(serde_json::json!({
                    "type": "tool_use",
                    "id": normalize_tool_call_id(&tc.id),
                    "name": tc.name,
                    "input": tc.arguments,
                }));
            }
            ContentBlock::ToolResult(_) => {
                // Tool results should not appear in assistant messages.
                // They are handled separately via MessageRole::Tool.
            }
            ContentBlock::Image(_) => {
                // Images are not expected in assistant messages.
            }
        }
    }

    if blocks.is_empty() {
        return None;
    }

    Some(serde_json::json!({
        "role": "assistant",
        "content": blocks,
    }))
}

/// Convert tool result content blocks to Anthropic format.
fn convert_tool_result_content(tr: &ToolResultContent) -> serde_json::Value {
    // If there's an error, return it as a simple string.
    if tr.is_error {
        if let Some(ref error) = tr.error {
            return serde_json::json!(format!("Error: {error}"));
        }
        return serde_json::json!("Error");
    }

    // If there's nested content, convert it.
    if let Some(ref content) = tr.content {
        let has_images = content.iter().any(|b| matches!(b, ContentBlock::Image(_)));
        if !has_images {
            let text = extract_text(content);
            return serde_json::json!(text);
        }

        // Mixed content with images: build block array.
        let blocks: Vec<serde_json::Value> = content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text(t) => Some(serde_json::json!({
                    "type": "text",
                    "text": t.text,
                })),
                ContentBlock::Image(img) => convert_image_block(img),
                _ => None,
            })
            .collect();

        return serde_json::json!(blocks);
    }

    serde_json::json!("")
}

/// Convert an image content block to Anthropic's image format.
fn convert_image_block(img: &ImageContent) -> Option<serde_json::Value> {
    match &img.source {
        ImageSource::Base64 { media_type, data } => Some(serde_json::json!({
            "type": "image",
            "source": {
                "type": "base64",
                "media_type": media_type,
                "data": data,
            },
        })),
        ImageSource::Url { .. } => {
            // Anthropic's API only supports base64 images directly.
            // For URLs, we'd need to fetch and re-encode. Skip for now.
            tracing::warn!("Anthropic API does not support URL-based images directly; skipping image");
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Tool definition conversion
// ---------------------------------------------------------------------------

/// Convert [`ToolDefinition`]s to Anthropic tool format.
fn convert_tools(context: &Context, _is_oauth: bool) -> Vec<serde_json::Value> {
    context
        .tools
        .iter()
        .map(|tool| {
            let properties = tool.parameters.get("properties").cloned().unwrap_or_default();
            let required = tool.parameters.get("required").and_then(|v| v.as_array()).cloned().unwrap_or_default();

            serde_json::json!({
                "name": tool.name,
                "description": tool.description,
                "input_schema": {
                    "type": "object",
                    "properties": properties,
                    "required": required,
                },
            })
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Stop reason mapping
// ---------------------------------------------------------------------------

/// Map an Anthropic `stop_reason` to the canonical stop-reason string.
fn map_stop_reason(reason: &str) -> String {
    match reason {
        "end_turn" | "stop_sequence" | "pause_turn" => "stop".to_owned(),
        "max_tokens" => "length".to_owned(),
        "tool_use" => "toolUse".to_owned(),
        "refusal" | "sensitive" => "error".to_owned(),
        other => format!("error:provider_finish_reason:{other}"),
    }
}

// ---------------------------------------------------------------------------
// Auth / API key resolution
// ---------------------------------------------------------------------------

/// Determine if an API key is an OAuth token.
fn is_oauth_token(api_key: &str) -> bool {
    api_key.contains("sk-ant-oat")
}

/// Resolve the API key from options or environment.
fn resolve_api_key(options: &StreamOptions) -> Result<String, String> {
    if let Some(ref key) = options.api_key {
        if !key.is_empty() {
            return Ok(key.clone());
        }
    }
    match std::env::var("ANTHROPIC_API_KEY") {
        Ok(key) if !key.is_empty() => Ok(key),
        _ => Err("Anthropic API key is required. Set the ANTHROPIC_API_KEY environment \
             variable or pass `api_key` in `StreamOptions`."
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
    use pi_ai_core::types::{KnownProvider, ToolDefinition};
    use serial_test::serial;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    // ------------------------------------------------------------------
    // Helpers
    // ------------------------------------------------------------------

    fn test_model() -> Model {
        Model {
            id: "claude-sonnet-4-20250514".into(),
            provider: KnownProvider::Anthropic,
            api: "anthropic-messages".into(),
            name: None,
            base_url: None,
            supports_thinking: true,
            supports_tools: true,
            supports_streaming: true,
            supports_image_input: true,
            max_tokens: None,
            max_input_tokens: None,
            max_output_tokens: Some(8192),
            cost_per_input_token: Some(0.000_003),
            cost_per_output_token: Some(0.000_015),
            cost_per_cache_read_token: Some(0.000_000_3),
            cost_per_cache_write_token: Some(0.000_003_75),
        }
    }

    async fn setup_provider(mock_server: &MockServer) {
        let provider = AnthropicProvider::with_base_url(format!("{}/v1/messages", mock_server.uri()));
        clear_api_providers().await;
        register_api_provider(Box::new(provider)).await;
    }

    /// Ensure ANTHROPIC_API_KEY is set to a known value for all wiremock tests.
    fn ensure_api_key() {
        unsafe {
            std::env::set_var("ANTHROPIC_API_KEY", "sk-ant-test-placeholder");
        }
    }

    /// Mount a mock SSE endpoint that returns the given body.
    async fn mount_sse(mock_server: &MockServer, body: &'static str) {
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"))
            .mount(mock_server)
            .await;
    }

    /// Mount a mock that returns an HTTP error.
    async fn mount_error(mock_server: &MockServer, status: u16, body: &'static str) {
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
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
    async fn test_anthropic_text_stream() {
        ensure_api_key();
        let mock = MockServer::start().await;
        mount_sse(
            &mock,
            "event: message_start\n\
             data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"model\":\"claude-sonnet-4-20250514\",\"stop_reason\":null,\"usage\":{\"input_tokens\":10,\"output_tokens\":0}}}\n\
             \n\
             event: content_block_start\n\
             data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\
             \n\
             event: content_block_delta\n\
             data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}\n\
             \n\
             event: content_block_delta\n\
             data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\" world\"}}\n\
             \n\
             event: content_block_stop\n\
             data: {\"type\":\"content_block_stop\",\"index\":0}\n\
             \n\
             event: message_delta\n\
             data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\",\"stop_sequence\":null},\"usage\":{\"input_tokens\":10,\"output_tokens\":12}}\n\
             \n\
             event: message_stop\n\
             data: {\"type\":\"message_stop\"}\n\
             \n",
        )
        .await;
        setup_provider(&mock).await;

        let model = test_model();
        let context =
            Context { messages: vec![Message::user_text("Hi")], system_prompt: None, model: None, tools: vec![] };

        let stream = stream::stream(
            &model,
            context,
            StreamOptions { api_key: Some("sk-ant-test-key".into()), ..Default::default() },
        )
        .await
        .expect("stream() should return a stream");
        let result = collect_stream(stream, &model).await.expect("collect should succeed");

        let text = extract_text(&result.message.content);
        assert_eq!(text, "Hello world");
        assert_eq!(result.stop_reason, Some("stop".to_owned()));
    }

    #[serial]
    #[tokio::test]
    async fn test_anthropic_text_with_system_prompt() {
        ensure_api_key();
        let mock = MockServer::start().await;
        mount_sse(
            &mock,
            "event: message_start\n\
             data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_2\",\"model\":\"claude-sonnet-4-20250514\",\"stop_reason\":null,\"usage\":{\"input_tokens\":15,\"output_tokens\":0}}}\n\
             \n\
             event: content_block_start\n\
             data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\
             \n\
             event: content_block_delta\n\
             data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Sure\"}}\n\
             \n\
             event: content_block_stop\n\
             data: {\"type\":\"content_block_stop\",\"index\":0}\n\
             \n\
             event: message_delta\n\
             data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\",\"stop_sequence\":null},\"usage\":{\"input_tokens\":15,\"output_tokens\":4}}\n\
             \n\
             event: message_stop\n\
             data: {\"type\":\"message_stop\"}\n\
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

        let stream = stream::stream(
            &model,
            context,
            StreamOptions { api_key: Some("sk-ant-test-key".into()), ..Default::default() },
        )
        .await
        .expect("stream() should return a stream");
        let result = collect_stream(stream, &model).await.expect("collect should succeed");

        let text = extract_text(&result.message.content);
        assert_eq!(text, "Sure");
    }

    // ------------------------------------------------------------------
    // Thinking block streaming tests
    // ------------------------------------------------------------------

    #[serial]
    #[tokio::test]
    async fn test_anthropic_thinking_stream() {
        ensure_api_key();
        let mock = MockServer::start().await;
        mount_sse(
            &mock,
            "event: message_start\n\
             data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_3\",\"model\":\"claude-sonnet-4-20250514\",\"stop_reason\":null,\"usage\":{\"input_tokens\":10,\"output_tokens\":0}}}\n\
             \n\
             event: content_block_start\n\
             data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"\",\"signature\":null}}\n\
             \n\
             event: content_block_delta\n\
             data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"Let me think about this\"}}\n\
             \n\
             event: content_block_delta\n\
             data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"signature_delta\",\"signature\":\"EqoBCkgIARAhGAAyDwoN\"}}\n\
             \n\
             event: content_block_stop\n\
             data: {\"type\":\"content_block_stop\",\"index\":0}\n\
             \n\
             event: content_block_start\n\
             data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\
             \n\
             event: content_block_delta\n\
             data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\"The answer is 42\"}}\n\
             \n\
             event: content_block_stop\n\
             data: {\"type\":\"content_block_stop\",\"index\":1}\n\
             \n\
             event: message_delta\n\
             data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\",\"stop_sequence\":null},\"usage\":{\"input_tokens\":10,\"output_tokens\":33}}\n\
             \n\
             event: message_stop\n\
             data: {\"type\":\"message_stop\"}\n\
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

        let stream = stream::stream(
            &model,
            context,
            StreamOptions { api_key: Some("sk-ant-test-key".into()), thinking: Some(true), ..Default::default() },
        )
        .await
        .expect("stream() should return a stream");
        let result = collect_stream(stream, &model).await.expect("collect should succeed");

        let message = &result.message;
        // Should have at least 2 blocks: thinking + text
        assert!(message.content.len() >= 2, "Expected at least 2 content blocks, got {}", message.content.len());

        // Check thinking block
        let thinking_block =
            message.content.iter().find_map(|b| if let ContentBlock::Thinking(th) = b { Some(th) } else { None });
        assert!(thinking_block.is_some(), "Expected a thinking content block");
        let thinking = thinking_block.unwrap();
        assert_eq!(thinking.thinking, "Let me think about this");
        assert_eq!(thinking.signature, Some("EqoBCkgIARAhGAAyDwoN".to_owned()));

        // Check text block
        let text = extract_text(&message.content);
        assert_eq!(text, "The answer is 42");
    }

    #[serial]
    #[tokio::test]
    async fn test_anthropic_redacted_thinking() {
        ensure_api_key();
        let mock = MockServer::start().await;
        mount_sse(
            &mock,
            "event: message_start\n\
             data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_4\",\"model\":\"claude-sonnet-4-20250514\",\"stop_reason\":null,\"usage\":{\"input_tokens\":10,\"output_tokens\":0}}}\n\
             \n\
             event: content_block_start\n\
             data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"redacted_thinking\",\"data\":\"encrypted_blob\"}}\n\
             \n\
             event: content_block_stop\n\
             data: {\"type\":\"content_block_stop\",\"index\":0}\n\
             \n\
             event: content_block_start\n\
             data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\
             \n\
             event: content_block_delta\n\
             data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}\n\
             \n\
             event: content_block_stop\n\
             data: {\"type\":\"content_block_stop\",\"index\":1}\n\
             \n\
             event: message_delta\n\
             data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\",\"stop_sequence\":null},\"usage\":{\"input_tokens\":10,\"output_tokens\":5}}\n\
             \n\
             event: message_stop\n\
             data: {\"type\":\"message_stop\"}\n\
             \n",
        )
        .await;
        setup_provider(&mock).await;

        let model = test_model();
        let context =
            Context { messages: vec![Message::user_text("Hi")], system_prompt: None, model: None, tools: vec![] };

        let stream = stream::stream(
            &model,
            context,
            StreamOptions { api_key: Some("sk-ant-test-key".into()), ..Default::default() },
        )
        .await
        .expect("stream() should return a stream");
        let result = collect_stream(stream, &model).await.expect("collect should succeed");

        let message = &result.message;
        let thinking_block =
            message.content.iter().find_map(|b| if let ContentBlock::Thinking(th) = b { Some(th) } else { None });
        assert!(thinking_block.is_some(), "Expected a thinking content block for redacted thinking");
        let thinking = thinking_block.unwrap();
        assert_eq!(thinking.thinking, "[Reasoning redacted]");
        assert_eq!(thinking.signature, Some("encrypted_blob".to_owned()));
    }

    // ------------------------------------------------------------------
    // Tool call streaming tests
    // ------------------------------------------------------------------

    #[serial]
    #[tokio::test]
    async fn test_anthropic_tool_call() {
        ensure_api_key();
        let mock = MockServer::start().await;
        mount_sse(
            &mock,
            "event: message_start\n\
             data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_5\",\"model\":\"claude-sonnet-4-20250514\",\"stop_reason\":null,\"usage\":{\"input_tokens\":20,\"output_tokens\":0}}}\n\
             \n\
             event: content_block_start\n\
             data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\
             \n\
             event: content_block_delta\n\
             data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Let me check the weather\"}}\n\
             \n\
             event: content_block_stop\n\
             data: {\"type\":\"content_block_stop\",\"index\":0}\n\
             \n\
             event: content_block_start\n\
             data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"get_weather\",\"input\":{}}}\n\
             \n\
             event: content_block_delta\n\
             data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"location\\\":\\\"NYC\\\"}\"}}\n\
             \n\
             event: content_block_stop\n\
             data: {\"type\":\"content_block_stop\",\"index\":1}\n\
             \n\
             event: message_delta\n\
             data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\",\"stop_sequence\":null},\"usage\":{\"input_tokens\":20,\"output_tokens\":15}}\n\
             \n\
             event: message_stop\n\
             data: {\"type\":\"message_stop\"}\n\
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

        let stream = stream::stream(
            &model,
            context,
            StreamOptions { api_key: Some("sk-ant-test-key".into()), ..Default::default() },
        )
        .await
        .expect("stream() should return a stream");
        let result = collect_stream(stream, &model).await.expect("collect should succeed");

        // Should have text + tool call.
        let has_tool_call = result.message.content.iter().any(|b| matches!(b, ContentBlock::ToolCall(_)));
        assert!(has_tool_call, "Expected a tool call in the result");

        // Verify tool call content.
        for block in &result.message.content {
            if let ContentBlock::ToolCall(tc) = block {
                assert_eq!(tc.id, "toolu_1");
                assert_eq!(tc.name, "get_weather");
                assert_eq!(tc.arguments, serde_json::json!({"location": "NYC"}));
            }
        }

        assert_eq!(result.stop_reason, Some("toolUse".to_owned()));
    }

    // ------------------------------------------------------------------
    // Error handling tests
    // ------------------------------------------------------------------

    #[serial]
    #[tokio::test]
    async fn test_anthropic_api_key_error() {
        ensure_api_key();
        // Use a provider with no mock — should error on connection failure.
        let model = test_model();
        let context =
            Context { messages: vec![Message::user_text("Hi")], system_prompt: None, model: None, tools: vec![] };

        let provider = AnthropicProvider::new();
        clear_api_providers().await;
        register_api_provider(Box::new(provider)).await;

        let stream =
            stream::stream(&model, context, StreamOptions::default()).await.expect("stream() should return a stream");
        let result = collect_stream(stream, &model).await;

        assert!(result.is_err(), "Expected an error when no API key is available");
    }

    #[serial]
    #[tokio::test]
    async fn test_anthropic_http_error() {
        ensure_api_key();
        let mock = MockServer::start().await;
        mount_error(&mock, 401, r#"{"error":{"message":"Invalid API key","type":"auth_error"}}"#).await;
        setup_provider(&mock).await;

        let model = test_model();
        let context =
            Context { messages: vec![Message::user_text("Hi")], system_prompt: None, model: None, tools: vec![] };

        let stream = stream::stream(
            &model,
            context,
            StreamOptions { api_key: Some("sk-ant-test-key".into()), ..Default::default() },
        )
        .await
        .expect("stream() should return a stream");
        let result = collect_stream(stream, &model).await;

        assert!(result.is_err(), "Expected an error for HTTP 401");
    }

    // ------------------------------------------------------------------
    // Message conversion tests
    // ------------------------------------------------------------------

    #[test]
    fn test_convert_user_message_text_only() {
        let msg = Message::user_text("Hello");
        let context = Context { messages: vec![msg], system_prompt: None, model: None, tools: vec![] };
        let messages = convert_messages(&context, false);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[0]["content"], "Hello");
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
        let messages = convert_messages(&context, false);
        assert_eq!(messages.len(), 1);

        let content = messages[0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[1]["type"], "image");
        assert_eq!(content[1]["source"]["type"], "base64");
        assert_eq!(content[1]["source"]["media_type"], "image/png");
    }

    #[test]
    fn test_convert_assistant_message_with_thinking_and_tool() {
        let msg = Message {
            role: MessageRole::Assistant,
            content: vec![
                ContentBlock::Thinking(ThinkingContent {
                    thinking: "Let me reason...".into(),
                    signature: Some("sig123".into()),
                }),
                ContentBlock::Text(TextContent { text: "I'll look that up.".into() }),
                ContentBlock::ToolCall(ToolCallContent {
                    id: "toolu_abc".into(),
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
        let messages = convert_messages(&context, false);
        assert_eq!(messages.len(), 1);

        assert_eq!(messages[0]["role"], "assistant");
        let content = messages[0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 3);

        // Thinking block
        assert_eq!(content[0]["type"], "thinking");
        assert_eq!(content[0]["thinking"], "Let me reason...");
        assert_eq!(content[0]["signature"], "sig123");

        // Text block
        assert_eq!(content[1]["type"], "text");
        assert_eq!(content[1]["text"], "I'll look that up.");

        // Tool use block
        assert_eq!(content[2]["type"], "tool_use");
        assert_eq!(content[2]["id"], "toolu_abc");
        assert_eq!(content[2]["name"], "search_web");
    }

    #[test]
    fn test_convert_tool_results() {
        let tool_msg = Message {
            role: MessageRole::Tool,
            content: vec![ContentBlock::ToolResult(ToolResultContent {
                id: "toolu_1".into(),
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
        let messages = convert_messages(&context, false);
        assert_eq!(messages.len(), 1);

        assert_eq!(messages[0]["role"], "user");
        let content = messages[0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 1);
        assert_eq!(content[0]["type"], "tool_result");
        assert_eq!(content[0]["tool_use_id"], "toolu_1");
        assert_eq!(content[0]["content"], "72 degrees");
        assert_eq!(content[0]["is_error"], false);
    }

    #[test]
    fn test_convert_consecutive_tool_results_collapsed() {
        let tool_msg_1 = Message {
            role: MessageRole::Tool,
            content: vec![ContentBlock::ToolResult(ToolResultContent {
                id: "toolu_1".into(),
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
        let tool_msg_2 = Message {
            role: MessageRole::Tool,
            content: vec![ContentBlock::ToolResult(ToolResultContent {
                id: "toolu_2".into(),
                name: "get_time".into(),
                content: Some(vec![ContentBlock::Text(TextContent { text: "12:00 PM".into() })]),
                error: None,
                is_error: false,
            })],
            id: None,
            name: None,
            usage: None,
            redacted: false,
        };
        let context =
            Context { messages: vec![tool_msg_1, tool_msg_2], system_prompt: None, model: None, tools: vec![] };
        let messages = convert_messages(&context, false);
        assert_eq!(messages.len(), 1);

        assert_eq!(messages[0]["role"], "user");
        let content = messages[0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["tool_use_id"], "toolu_1");
        assert_eq!(content[1]["tool_use_id"], "toolu_2");
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
        let context = Context { messages: vec![], system_prompt: None, model: None, tools };
        let converted = convert_tools(&context, false);
        assert_eq!(converted.len(), 1);
        assert_eq!(converted[0]["name"], "get_weather");
        assert_eq!(converted[0]["input_schema"]["type"], "object");
        assert_eq!(converted[0]["input_schema"]["required"], serde_json::json!(["location"]));
    }

    // ------------------------------------------------------------------
    // Stop reason mapping tests
    // ------------------------------------------------------------------

    #[test]
    fn test_map_stop_reasons() {
        assert_eq!(map_stop_reason("end_turn"), "stop");
        assert_eq!(map_stop_reason("stop_sequence"), "stop");
        assert_eq!(map_stop_reason("pause_turn"), "stop");
        assert_eq!(map_stop_reason("max_tokens"), "length");
        assert_eq!(map_stop_reason("tool_use"), "toolUse");
        assert_eq!(map_stop_reason("refusal"), "error");
        assert_eq!(map_stop_reason("sensitive"), "error");
        assert!(map_stop_reason("unknown").contains("error"));
    }

    // ------------------------------------------------------------------
    // OAuth token detection tests
    // ------------------------------------------------------------------

    #[test]
    fn test_is_oauth_token() {
        assert!(is_oauth_token("sk-ant-oat-abc123"));
        assert!(!is_oauth_token("sk-ant-api03-abc123"));
        assert!(!is_oauth_token(""));
    }

    // ------------------------------------------------------------------
    // Tool call ID normalization tests
    // ------------------------------------------------------------------

    #[test]
    fn test_normalize_tool_call_id() {
        assert_eq!(normalize_tool_call_id("simple_id"), "simple_id");
        assert_eq!(normalize_tool_call_id("id|with|pipes"), "id_with_pipes");
        assert_eq!(normalize_tool_call_id("id_with_special_chars!@#"), "id_with_special_chars___");
        // Test truncation
        let long_id = "a".repeat(100);
        assert_eq!(normalize_tool_call_id(&long_id).len(), 64);
    }

    // ------------------------------------------------------------------
    // Cleanup
    // ------------------------------------------------------------------

    /// Ensure env vars are reset after tests.
    #[serial]
    #[tokio::test]
    async fn cleanup_env() {
        unsafe {
            std::env::remove_var("ANTHROPIC_API_KEY");
        }
        clear_api_providers().await;
    }
}
