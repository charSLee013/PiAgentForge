//! Pi Provider — AWS Bedrock ConverseStream API.
//!
//! Maps to `packages/ai/src/providers/amazon-bedrock.ts` in the TS source.
//!
//! This provider implements the [`ApiProvider`] trait for AWS Bedrock's
//! ConverseStream API, supporting text streaming, thinking blocks with
//! signatures, tool calls, and image input.
//!
//! # Authentication
//!
//! This provider uses the standard AWS SDK credential chain (environment
//! variables, ~/.aws/credentials, IAM roles, etc.). No API key is required
//! in `StreamOptions`.
//!
//! # Feature gate
//!
//! This crate is NOT compiled by default. Enable via:
//! ```text
//! cargo build --features feat-bedrock
//! ```

use pi_ai_core::api_registry::ApiProvider;
use pi_ai_core::event_stream::{AssistantMessageEventStream, EventStreamSender};
use pi_ai_core::types::{
    ContentBlock as PiContentBlock, Context, ImageContent, ImageSource, Message as PiMessage,
    MessageRole, Model, StreamError, StreamEvent, StreamOptions, TextContent, ThinkingContent,
    ToolCallContent, ToolResultContent, Usage,
};
use pi_ai_core::types::ToolDefinition;
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// AWS SDK type aliases
// ---------------------------------------------------------------------------

use aws_sdk_bedrockruntime::types::{
    self as bt, ConversationRole, ImageFormat as AwsImageFormat, ImageSource as AwsImageSource,
    StopReason, SystemContentBlock as AwsSystemContentBlock,
    ToolConfiguration as AwsToolConfiguration, ToolResultStatus as AwsToolResultStatus,
};

use aws_sdk_bedrockruntime::primitives::Blob;
use aws_sdk_bedrockruntime::Client;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Default AWS region for Bedrock when none is configured.
const DEFAULT_BEDROCK_REGION: &str = "us-east-1";

/// Known human-readable prefixes for Bedrock SDK error names.
const BEDROCK_ERROR_PREFIXES: &[(&str, &str)] = &[
    ("InternalServerException", "Internal server error"),
    ("ModelStreamErrorException", "Model stream error"),
    ("ValidationException", "Validation error"),
    ("ThrottlingException", "Throttling error"),
    ("ServiceUnavailableException", "Service unavailable"),
];

// ---------------------------------------------------------------------------
// Provider struct
// ---------------------------------------------------------------------------

/// Provider for the AWS Bedrock ConverseStream API (streaming).
///
/// Sends a ConverseStream request to the Bedrock runtime API and processes
/// the event-stream response, emitting [`StreamEvent`] items.
///
/// # Example
///
/// ```ignore
/// use pi_provider_bedrock::BedrockProvider;
/// use pi_ai_core::api_registry::register_api_provider;
///
/// let provider = BedrockProvider::new();
/// register_api_provider(Box::new(provider)).await;
/// ```
pub struct BedrockProvider {
    region: Option<String>,
    base_url: Option<String>,
}

impl BedrockProvider {
    /// Create a new Bedrock provider with default configuration.
    pub fn new() -> Self {
        Self {
            region: None,
            base_url: None,
        }
    }

    /// Create a provider with an explicit AWS region.
    pub fn with_region(region: impl Into<String>) -> Self {
        Self {
            region: Some(region.into()),
            base_url: None,
        }
    }

    /// Create a provider with a custom base URL (for testing or VPC endpoints).
    pub fn with_base_url(base_url: impl Into<String>) -> Self {
        Self {
            region: None,
            base_url: Some(base_url.into()),
        }
    }
}

impl Default for BedrockProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl ApiProvider for BedrockProvider {
    fn api_id(&self) -> &str {
        "bedrock-converse-stream"
    }

    fn stream(
        &self,
        model: &Model,
        context: Context,
        options: StreamOptions,
    ) -> AssistantMessageEventStream {
        let (tx, rx) = AssistantMessageEventStream::new();
        let model = model.clone();
        let region = self.region.clone();
        let base_url = self.base_url.clone();

        tokio::spawn(async move {
            if let Err(e) = process_stream(tx, &model, context, options, region, base_url).await {
                tracing::error!("Bedrock stream error: {e}");
            }
        });

        rx
    }
}

// ---------------------------------------------------------------------------
// Top-level stream processing
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
async fn process_stream(
    tx: EventStreamSender<StreamEvent>,
    model: &Model,
    context: Context,
    options: StreamOptions,
    provider_region: Option<String>,
    provider_base_url: Option<String>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // 1. Resolve the region.
    let resolved_region = resolve_region(&provider_region);

    // 2. Build the AWS SDK config.
    let mut config_builder =
        aws_sdk_bedrockruntime::Config::builder()
            .behavior_version(aws_sdk_bedrockruntime::config::BehaviorVersion::latest());

    if let Some(region) = &resolved_region {
        config_builder =
            config_builder.region(aws_sdk_bedrockruntime::config::Region::new(region.clone()));
    }

    // Use the model's base_url or the provider's base_url as a custom endpoint.
    let endpoint = model.base_url.as_ref().or(provider_base_url.as_ref());
    if let Some(endpoint_url) = endpoint {
        config_builder = config_builder.endpoint_url(endpoint_url);
    }

    // 3. Create the client.
    let config = config_builder.build();
    let client = Client::from_conf(config);

    // 4. Build the request.
    let system_prompt = context.system_prompt.clone();
    let tool_config = if !context.tools.is_empty() {
        Some(build_tool_config(&context.tools)?)
    } else {
        None
    };

    let messages = convert_messages(context, model);

    let mut request = client
        .converse_stream()
        .model_id(&model.id)
        .set_messages(Some(messages));

    // System prompt.
    if let Some(ref system_prompt) = system_prompt {
        if !system_prompt.is_empty() {
            request = request.set_system(Some(build_system_blocks(system_prompt)));
        }
    }

    // Inference config: max_tokens.
    if let Some(max_tokens) = options.max_tokens {
        let inference_config = bt::InferenceConfiguration::builder()
            .max_tokens(max_tokens as i32)
            .build();
        request = request.inference_config(inference_config);
    }

    // Tool config.
    if let Some(tool_config) = tool_config {
        request = request.tool_config(tool_config);
    }

    // 5. Send the request.
    let response = request.send().await.inspect_err(|e| {
        let err_msg = format_bedrock_sdk_error(e);
        emit_error(&tx, err_msg, Some("request_error".to_owned()));
    })?;

    // 6. Emit the Start event.
    let _ = tx.send(StreamEvent::Start);

    // 7. Process the event stream.
    let mut state = BedrockStreamState::default();
    if let Err(e) = process_event_stream(&tx, response, &mut state, model).await {
        emit_error(
            &tx,
            format!("Bedrock stream error: {e}"),
            Some("stream_error".to_owned()),
        );
        return Ok(());
    }

    // 8. Build the Done message.
    let usage = build_usage(&state, model);
    let stop_reason = state.stop_reason.clone().unwrap_or_else(|| "stop".to_owned());
    let message = build_done_message(&state, model, usage.clone());

    let _ = tx.send(StreamEvent::Done {
        message: Some(message),
        stop_reason: Some(stop_reason),
    });

    Ok(())
}

// ---------------------------------------------------------------------------
// Event stream processing
// ---------------------------------------------------------------------------

/// Accumulated state while processing a Bedrock ConverseStream event stream.
#[derive(Debug, Default)]
struct BedrockStreamState {
    /// All text content accumulated across all text blocks.
    text: String,
    /// All thinking text accumulated across all thinking blocks.
    thinking: String,
    /// The most recent thinking signature (for round-tripping).
    thinking_signature: Option<String>,
    /// Tool calls being accumulated, keyed by content block index.
    tool_calls: HashMap<i32, ToolCallBuilder>,
    /// The stop reason from `messageStop`.
    stop_reason: Option<String>,
    /// Token usage from `metadata` event.
    input_tokens: u64,
    output_tokens: u64,
    cache_read: u64,
    cache_write: u64,
}

/// Accumulates streamed data for a single tool call.
#[derive(Debug)]
#[allow(dead_code)]
struct ToolCallBuilder {
    tool_call_index: i32,
    id: String,
    name: String,
    arguments: String,
    /// Whether the `id` has already been emitted via `ToolCallDelta`.
    id_emitted: bool,
    /// Whether the `name` has already been emitted via `ToolCallDelta`.
    name_emitted: bool,
}

impl BedrockStreamState {
    fn get_or_create_tool_call(&mut self, tool_call_index: i32) -> &mut ToolCallBuilder {
        self.tool_calls
            .entry(tool_call_index)
            .or_insert_with(|| ToolCallBuilder {
                tool_call_index,
                id: String::new(),
                name: String::new(),
                arguments: String::new(),
                id_emitted: false,
                name_emitted: false,
            })
    }
}

/// Process the ConverseStream event stream, emitting [`StreamEvent`] items.
async fn process_event_stream(
    tx: &EventStreamSender<StreamEvent>,
    mut response: aws_sdk_bedrockruntime::operation::converse_stream::ConverseStreamOutput,
    state: &mut BedrockStreamState,
    model: &Model,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    loop {
        match response.stream.recv().await {
            Ok(Some(output)) => {
                handle_converse_stream_output(tx, output, state, model)?;
            }
            Ok(None) => {
                // Stream ended normally.
                return Ok(());
            }
            Err(e) => {
                let err_msg = format!("Bedrock event stream error: {e}");
                emit_error(tx, err_msg, Some("stream_error".to_owned()));
                return Ok(());
            }
        }
    }
}

/// Handle a single [`aws_sdk_bedrockruntime::types::ConverseStreamOutput`] event.
fn handle_converse_stream_output(
    tx: &EventStreamSender<StreamEvent>,
    output: aws_sdk_bedrockruntime::types::ConverseStreamOutput,
    state: &mut BedrockStreamState,
    model: &Model,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use aws_sdk_bedrockruntime::types::ConverseStreamOutput;

    match output {
        ConverseStreamOutput::MessageStart(ev) => {
            if ev.role() != &ConversationRole::Assistant {
                let err = "Unexpected message start: expected assistant role";
                emit_error(tx, err.to_owned(), Some("protocol_error".to_owned()));
            }
        }

        ConverseStreamOutput::ContentBlockStart(ev) => {
            let index = ev.content_block_index();
            if let Some(start) = ev.start() {
                handle_content_block_start(tx, index, start, state);
            }
        }

        ConverseStreamOutput::ContentBlockDelta(ev) => {
            let index = ev.content_block_index();
            if let Some(delta) = ev.delta() {
                handle_content_block_delta(tx, index, delta, state);
            }
        }

        ConverseStreamOutput::ContentBlockStop(_ev) => {
            // Content block stop events are handled by delta accumulation;
            // no specific action needed at block boundaries.
        }

        ConverseStreamOutput::MessageStop(ev) => {
            state.stop_reason = Some(map_stop_reason(ev.stop_reason()));
        }

        ConverseStreamOutput::Metadata(ev) => {
            if let Some(usage) = ev.usage() {
                handle_metadata_usage(tx, usage, model, state);
            }
        }

        _ => {
            // Unknown variants are ignored.
        }
    }

    Ok(())
}

/// Handle a `contentBlockStart` event.
fn handle_content_block_start(
    tx: &EventStreamSender<StreamEvent>,
    index: i32,
    start: &bt::ContentBlockStart,
    state: &mut BedrockStreamState,
) {
    use bt::ContentBlockStart;
    if let ContentBlockStart::ToolUse(tool_use_start) = start {
        let id = tool_use_start.tool_use_id().to_owned();
        let name = tool_use_start.name().to_owned();
        let builder = state.get_or_create_tool_call(index);
        builder.id = id.clone();
        builder.name = name.clone();

        let _ = tx.send(StreamEvent::ToolCallDelta {
            index: index as u32,
            id: Some(id),
            name: Some(name),
            arguments: None,
        });
        builder.id_emitted = true;
        builder.name_emitted = true;
    }
}

/// Handle a `contentBlockDelta` event.
fn handle_content_block_delta(
    tx: &EventStreamSender<StreamEvent>,
    index: i32,
    delta: &bt::ContentBlockDelta,
    state: &mut BedrockStreamState,
) {
    use bt::ContentBlockDelta;
    match delta {
        ContentBlockDelta::Text(text) => {
            if !text.is_empty() {
                state.text.push_str(text);
                let _ = tx.send(StreamEvent::TextDelta {
                    delta: text.clone(),
                });
            }
        }

        ContentBlockDelta::ToolUse(tool_use_delta) => {
            let input = tool_use_delta.input().to_owned();
            if !input.is_empty() {
                let builder = state.get_or_create_tool_call(index);
                builder.arguments.push_str(&input);

                let emit_id = if builder.id_emitted {
                    None
                } else {
                    builder.id_emitted = true;
                    Some(builder.id.clone())
                };
                let emit_name = if builder.name_emitted {
                    None
                } else {
                    builder.name_emitted = true;
                    Some(builder.name.clone())
                };

                let _ = tx.send(StreamEvent::ToolCallDelta {
                    index: index as u32,
                    id: emit_id,
                    name: emit_name,
                    arguments: Some(input),
                });
            }
        }

        ContentBlockDelta::ReasoningContent(reasoning_delta) => {
            match reasoning_delta {
                bt::ReasoningContentBlockDelta::Text(text) => {
                    if !text.is_empty() {
                        state.thinking.push_str(text);
                        let _ = tx.send(StreamEvent::ThinkingDelta {
                            delta: text.clone(),
                        });
                    }
                }
                bt::ReasoningContentBlockDelta::Signature(sig) => {
                    if !sig.is_empty() {
                        let current = state.thinking_signature.take().unwrap_or_default();
                        state.thinking_signature = Some(current + sig);
                    }
                }
                bt::ReasoningContentBlockDelta::RedactedContent(_blob) => {
                    // Redacted thinking — we don't have access to the content.
                    // The TS code would accumulate "[Reasoning redacted]" here
                    // but we simply skip redacted blocks since the content is encrypted.
                }
                _ => {}
            }
        }

        _ => {}
    }
}

/// Handle usage from a `metadata` event.
fn handle_metadata_usage(
    tx: &EventStreamSender<StreamEvent>,
    usage: &bt::TokenUsage,
    model: &Model,
    state: &mut BedrockStreamState,
) {
    state.input_tokens = usage.input_tokens() as u64;
    state.output_tokens = usage.output_tokens() as u64;

    if let Some(cache_read) = usage.cache_read_input_tokens() {
        state.cache_read = cache_read as u64;
    }
    if let Some(cache_write) = usage.cache_write_input_tokens() {
        state.cache_write = cache_write as u64;
    }

    let usage_info = build_usage(state, model);
    let _ = tx.send(StreamEvent::Usage(usage_info));
}

// ---------------------------------------------------------------------------
// Message conversion (pi-ai-core -> Bedrock ConverseStream format)
// ---------------------------------------------------------------------------

/// Convert [`Context`] messages to Bedrock ConverseStream messages.
fn convert_messages(context: Context, model: &Model) -> Vec<bt::Message> {
    let mut result: Vec<bt::Message> = Vec::new();
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
                if let Some(val) = convert_user_message(msg) {
                    result.push(val);
                }
                i += 1;
            }
            MessageRole::Assistant => {
                if let Some(val) = convert_assistant_message(msg, model) {
                    result.push(val);
                }
                i += 1;
            }
            MessageRole::Tool => {
                // Collect consecutive tool result messages into a single user message.
                let mut content_blocks: Vec<bt::ContentBlock> = Vec::new();

                while i < context.messages.len() && context.messages[i].role == MessageRole::Tool {
                    let tool_msg = &context.messages[i];
                    for block in &tool_msg.content {
                        if let PiContentBlock::ToolResult(tr) = block {
                            let tr_block = convert_tool_result(tr);
                            content_blocks.push(bt::ContentBlock::ToolResult(tr_block));
                        }
                    }
                    i += 1;
                }

                if !content_blocks.is_empty() {
                    result.push(
                        bt::Message::builder()
                            .role(ConversationRole::User)
                            .set_content(Some(content_blocks))
                            .build()
                            .expect("tool result user message should build"),
                    );
                }
            }
        }
    }

    result
}

/// Convert a user message to Bedrock format.
fn convert_user_message(msg: &PiMessage) -> Option<bt::Message> {
    let has_images = msg
        .content
        .iter()
        .any(|b| matches!(b, PiContentBlock::Image(_)));

    if !has_images {
        // Simple text-only message.
        let text = extract_text(&msg.content);
        if text.trim().is_empty() {
            return None;
        }
        return Some(
            bt::Message::builder()
                .role(ConversationRole::User)
                .content(bt::ContentBlock::Text(text))
                .build()
                .expect("user message should build"),
        );
    }

    // Mixed content: build content block array.
    let mut blocks: Vec<bt::ContentBlock> = Vec::new();
    let mut has_text = false;

    for block in &msg.content {
        match block {
            PiContentBlock::Text(t) => {
                if !t.text.trim().is_empty() {
                    blocks.push(bt::ContentBlock::Text(t.text.clone()));
                    has_text = true;
                }
            }
            PiContentBlock::Image(img) => {
                if let Some(image_block) = convert_image_to_bedrock(img) {
                    blocks.push(bt::ContentBlock::Image(image_block));
                }
            }
            _ => {}
        }
    }

    // If only images (no text), add placeholder text block.
    if !has_text && !blocks.is_empty() {
        blocks.insert(0, bt::ContentBlock::Text("(see attached image)".to_owned()));
    }

    if blocks.is_empty() {
        return None;
    }

    Some(
        bt::Message::builder()
            .role(ConversationRole::User)
            .set_content(Some(blocks))
            .build()
            .expect("user message with images should build"),
    )
}

/// Convert an assistant message to Bedrock format.
fn convert_assistant_message(msg: &PiMessage, _model: &Model) -> Option<bt::Message> {
    let mut blocks: Vec<bt::ContentBlock> = Vec::new();

    for block in &msg.content {
        match block {
            PiContentBlock::Text(t) => {
                if t.text.trim().is_empty() {
                    continue;
                }
                blocks.push(bt::ContentBlock::Text(t.text.clone()));
            }
            PiContentBlock::Thinking(th) => {
                if th.thinking.trim().is_empty() {
                    continue;
                }
                // If we have a valid signature, send as a reasoning content block.
                if let Some(sig) = &th.signature {
                    if !sig.trim().is_empty() {
                        let reasoning_text = bt::ReasoningTextBlock::builder()
                            .text(th.thinking.clone())
                            .signature(sig.clone())
                            .build()
                            .expect("reasoning text block should build");
                        blocks.push(bt::ContentBlock::ReasoningContent(
                            bt::ReasoningContentBlock::ReasoningText(reasoning_text),
                        ));
                    } else {
                        // No valid signature: degrade to text block.
                        blocks.push(bt::ContentBlock::Text(th.thinking.clone()));
                    }
                } else {
                    // No signature: degrade to text block.
                    blocks.push(bt::ContentBlock::Text(th.thinking.clone()));
                }
            }
            PiContentBlock::ToolCall(tc) => {
                let tool_use_block = bt::ToolUseBlock::builder()
                    .tool_use_id(normalize_tool_call_id(&tc.id))
                    .name(&tc.name)
                    .input(serde_json_value_to_document(&tc.arguments))
                    .build()
                    .expect("tool use block should build");
                blocks.push(bt::ContentBlock::ToolUse(tool_use_block));
            }
            PiContentBlock::ToolResult(_) | PiContentBlock::Image(_) => {
                // Tool results should not appear in assistant messages.
                // Images are not expected in assistant messages.
            }
        }
    }

    if blocks.is_empty() {
        return None;
    }

    Some(
        bt::Message::builder()
            .role(ConversationRole::Assistant)
            .set_content(Some(blocks))
            .build()
            .expect("assistant message should build"),
    )
}

/// Convert a tool result to Bedrock format.
fn convert_tool_result(tr: &ToolResultContent) -> bt::ToolResultBlock {
    let mut builder = bt::ToolResultBlock::builder()
        .tool_use_id(normalize_tool_call_id(&tr.id))
        .status(if tr.is_error {
            AwsToolResultStatus::Error
        } else {
            AwsToolResultStatus::Success
        });

    if tr.is_error {
        let error_text = if let Some(ref error) = tr.error {
            format!("Error: {error}")
        } else {
            "Error".to_owned()
        };
        builder = builder
            .content(bt::ToolResultContentBlock::Text(error_text));
    } else if let Some(ref content) = tr.content {
        for block in content {
            match block {
                PiContentBlock::Text(t) => {
                    builder = builder
                        .content(bt::ToolResultContentBlock::Text(t.text.clone()));
                }
                PiContentBlock::Image(img) => {
                    if let Some(image_block) = convert_image_to_bedrock(img) {
                        builder = builder
                            .content(bt::ToolResultContentBlock::Image(image_block));
                    }
                }
                _ => {}
            }
        }
    }

    builder
        .build()
        .expect("tool result block should build")
}

/// Convert an image content block to Bedrock format.
fn convert_image_to_bedrock(img: &ImageContent) -> Option<bt::ImageBlock> {
    match &img.source {
        ImageSource::Base64 { media_type, data } => {
            let format = match media_type.as_str() {
                "image/jpeg" | "image/jpg" => AwsImageFormat::Jpeg,
                "image/png" => AwsImageFormat::Png,
                "image/gif" => AwsImageFormat::Gif,
                "image/webp" => AwsImageFormat::Webp,
                _ => {
                    tracing::warn!("Unknown image media type: {media_type}");
                    return None;
                }
            };

            let bytes = match base64::Engine::decode(
                &base64::engine::general_purpose::STANDARD,
                data,
            ) {
                Ok(b) => b,
                Err(e) => {
                    tracing::warn!("Failed to decode base64 image data: {e}");
                    return None;
                }
            };

            Some(
                bt::ImageBlock::builder()
                    .format(format)
                    .source(AwsImageSource::Bytes(Blob::new(bytes)))
                    .build()
                    .expect("image block should build"),
            )
        }
        ImageSource::Url { .. } => {
            tracing::warn!(
                "Bedrock API does not support URL-based images directly; skipping image"
            );
            None
        }
    }
}

// ---------------------------------------------------------------------------
// System prompt conversion
// ---------------------------------------------------------------------------

/// Build system content blocks for the Bedrock API.
fn build_system_blocks(system_prompt: &str) -> Vec<AwsSystemContentBlock> {
    vec![AwsSystemContentBlock::Text(system_prompt.to_owned())]
}

// ---------------------------------------------------------------------------
// Tool configuration conversion
// ---------------------------------------------------------------------------

/// Convert [`ToolDefinition`]s to Bedrock tool configuration.
fn build_tool_config(
    tools: &[ToolDefinition],
) -> Result<AwsToolConfiguration, Box<dyn std::error::Error + Send + Sync>> {
    let bedrock_tools: Vec<bt::Tool> = tools
        .iter()
        .map(|tool| {
            let input_schema_doc = serde_json_value_to_document(&tool.parameters);
            let tool_spec = bt::ToolSpecification::builder()
                .name(&tool.name)
                .description(&tool.description)
                .input_schema(bt::ToolInputSchema::Json(input_schema_doc))
                .build()
                .expect("tool spec should build");
            bt::Tool::ToolSpec(tool_spec)
        })
        .collect();

    Ok(
        AwsToolConfiguration::builder()
            .set_tools(Some(bedrock_tools))
            .build()
            .expect("tool config should build"),
    )
}

// ---------------------------------------------------------------------------
// Document conversion (serde_json::Value -> aws_smithy_types::Document)
// ---------------------------------------------------------------------------

/// Convert a `serde_json::Value` to an `aws_smithy_types::Document` for use
/// in `ToolInputSchema::Json`.
fn serde_json_value_to_document(value: &serde_json::Value) -> aws_smithy_types::Document {
    use aws_smithy_types::Document;

    match value {
        serde_json::Value::Null => Document::Null,
        serde_json::Value::Bool(b) => Document::Bool(*b),
        serde_json::Value::Number(n) => {
            if let Some(f) = n.as_f64() {
                Document::Number(aws_smithy_types::Number::Float(f))
            } else if let Some(i) = n.as_i64() {
                Document::Number(if i >= 0 {
                    aws_smithy_types::Number::PosInt(i as u64)
                } else {
                    aws_smithy_types::Number::NegInt(i)
                })
            } else {
                Document::Number(aws_smithy_types::Number::Float(0.0))
            }
        }
        serde_json::Value::String(s) => Document::String(s.clone()),
        serde_json::Value::Array(arr) => {
            Document::Array(arr.iter().map(serde_json_value_to_document).collect())
        }
        serde_json::Value::Object(obj) => {
            Document::Object(
                obj.iter()
                    .map(|(k, v)| (k.clone(), serde_json_value_to_document(v)))
                    .collect(),
            )
        }
    }
}

// ---------------------------------------------------------------------------
// Final message construction
// ---------------------------------------------------------------------------

/// Build the final [`PiMessage`] from the accumulated streaming state.
fn build_done_message(state: &BedrockStreamState, _model: &Model, usage: Usage) -> PiMessage {
    let mut content: Vec<PiContentBlock> = Vec::new();

    // Add thinking block if we have thinking content.
    if !state.thinking.is_empty() {
        content.push(PiContentBlock::Thinking(ThinkingContent {
            thinking: state.thinking.clone(),
            signature: state.thinking_signature.clone(),
        }));
    }

    // Add text block if we have text content.
    if !state.text.is_empty() {
        content.push(PiContentBlock::Text(TextContent {
            text: state.text.clone(),
        }));
    }

    // Add tool call blocks.
    let mut tool_indices: Vec<i32> = state.tool_calls.keys().copied().collect();
    tool_indices.sort();
    for idx in tool_indices {
        if let Some(builder) = state.tool_calls.get(&idx) {
            let parsed_args: serde_json::Value =
                serde_json::from_str(&builder.arguments).unwrap_or_else(|_| {
                    serde_json::Value::String(builder.arguments.clone())
                });

            content.push(PiContentBlock::ToolCall(ToolCallContent {
                id: builder.id.clone(),
                name: builder.name.clone(),
                arguments: parsed_args,
            }));
        }
    }

    PiMessage {
        role: MessageRole::Assistant,
        content,
        id: None,
        name: None,
        usage: Some(usage),
        redacted: false,
    }
}

/// Build a [`Usage`] struct from the current state.
fn build_usage(state: &BedrockStreamState, _model: &Model) -> Usage {
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
// Region resolution
// ---------------------------------------------------------------------------

/// Resolve the AWS region for Bedrock.
///
/// Priority:
/// 1. Provider's configured region
/// 2. `AWS_REGION` environment variable
/// 3. `AWS_DEFAULT_REGION` environment variable
/// 4. Default to `us-east-1`
fn resolve_region(provider_region: &Option<String>) -> Option<String> {
    // Check provider region first.
    if let Some(region) = provider_region {
        if !region.is_empty() {
            return Some(region.clone());
        }
    }

    // Check environment variables.
    if let Ok(region) = std::env::var("AWS_REGION") {
        if !region.is_empty() {
            return Some(region);
        }
    }
    if let Ok(region) = std::env::var("AWS_DEFAULT_REGION") {
        if !region.is_empty() {
            return Some(region);
        }
    }

    // Default.
    Some(DEFAULT_BEDROCK_REGION.to_owned())
}

// ---------------------------------------------------------------------------
// Stop reason mapping
// ---------------------------------------------------------------------------

/// Map a Bedrock `StopReason` to the canonical stop-reason string used by pi.
fn map_stop_reason(reason: &StopReason) -> String {
    match reason {
        StopReason::EndTurn => "stop".to_owned(),
        StopReason::StopSequence => "stop".to_owned(),
        StopReason::MaxTokens => "length".to_owned(),
        StopReason::ContentFiltered => "error".to_owned(),
        StopReason::GuardrailIntervened => "error".to_owned(),
        StopReason::ModelContextWindowExceeded => "length".to_owned(),
        StopReason::ToolUse => "toolUse".to_owned(),
        _ => format!("error:provider_finish_reason:{reason:?}"),
    }
}

// ---------------------------------------------------------------------------
// Tool call ID normalization
// ---------------------------------------------------------------------------

/// Normalize tool call IDs to match Bedrock's required pattern (alphanumeric,
/// underscore, hyphen; max 64 chars).
fn normalize_tool_call_id(id: &str) -> String {
    id.chars()
        .map(|c| if c.is_alphanumeric() || c == '_' || c == '-' { c } else { '_' })
        .collect::<String>()
        .chars()
        .take(64)
        .collect()
}

// ---------------------------------------------------------------------------
// Error formatting
// ---------------------------------------------------------------------------

/// Format a Bedrock SDK error with a human-readable prefix.
fn format_bedrock_sdk_error(e: &(dyn std::error::Error + 'static)) -> String {
    let message = e.to_string();
    for (name, prefix) in BEDROCK_ERROR_PREFIXES {
        if message.contains(name) {
            return format!("{prefix}: {message}");
        }
    }
    message
}

/// Format a Bedrock exception with a human-readable prefix.
#[allow(dead_code)]
fn format_bedrock_exception(name: &str, message: &str) -> String {
    for (n, prefix) in BEDROCK_ERROR_PREFIXES {
        if *n == name {
            return format!("{prefix}: {message}");
        }
    }
    format!("{name}: {message}")
}

// ---------------------------------------------------------------------------
// Utility helpers
// ---------------------------------------------------------------------------

/// Extract plain text from a slice of [`PiContentBlock`]s.
fn extract_text(content: &[PiContentBlock]) -> String {
    content
        .iter()
        .filter_map(|block| {
            if let PiContentBlock::Text(t) = block {
                Some(t.text.as_str())
            } else {
                None
            }
        })
        .collect::<Vec<&str>>()
        .join("\n")
}

/// Send an error event and log it.
fn emit_error(
    tx: &EventStreamSender<StreamEvent>,
    message: impl Into<String>,
    code: Option<String>,
) {
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
    use pi_ai_core::types::{
        ContentBlock as PiContentBlock, ImageContent, ImageSource, KnownProvider, MessageRole,
        TextContent, ThinkingContent, ToolCallContent, ToolResultContent,
    };
    use aws_sdk_bedrockruntime::types::StopReason;

    // ------------------------------------------------------------------
    // Stop reason mapping tests
    // ------------------------------------------------------------------

    #[test]
    fn test_map_stop_reasons() {
        assert_eq!(map_stop_reason(&StopReason::EndTurn), "stop");
        assert_eq!(map_stop_reason(&StopReason::StopSequence), "stop");
        assert_eq!(map_stop_reason(&StopReason::MaxTokens), "length");
        assert_eq!(
            map_stop_reason(&StopReason::ModelContextWindowExceeded),
            "length"
        );
        assert_eq!(map_stop_reason(&StopReason::ToolUse), "toolUse");
        assert_eq!(map_stop_reason(&StopReason::ContentFiltered), "error");
    }

    // ------------------------------------------------------------------
    // Region resolution tests
    // ------------------------------------------------------------------

    #[test]
    fn test_resolve_region_uses_provider_region() {
        let provider_region = Some("us-west-2".to_owned());
        let region = resolve_region(&provider_region);
        assert_eq!(region, Some("us-west-2".to_owned()));
    }

    #[test]
    fn test_resolve_region_falls_back_to_default() {
        unsafe {
            std::env::remove_var("AWS_REGION");
            std::env::remove_var("AWS_DEFAULT_REGION");
        }
        let region = resolve_region(&None);
        assert_eq!(region, Some(DEFAULT_BEDROCK_REGION.to_owned()));
    }

    // ------------------------------------------------------------------
    // Tool call ID normalization tests
    // ------------------------------------------------------------------

    #[test]
    fn test_normalize_tool_call_id() {
        assert_eq!(normalize_tool_call_id("simple_id"), "simple_id");
        assert_eq!(normalize_tool_call_id("id|with|pipes"), "id_with_pipes");
        assert_eq!(
            normalize_tool_call_id("id_with_special_chars!@#"),
            "id_with_special_chars___"
        );
        let long_id = "a".repeat(100);
        assert_eq!(normalize_tool_call_id(&long_id).len(), 64);
    }

    // ------------------------------------------------------------------
    // Document conversion tests
    // ------------------------------------------------------------------

    #[test]
    fn test_serde_json_value_to_document_null() {
        let doc = serde_json_value_to_document(&serde_json::Value::Null);
        assert!(matches!(doc, aws_smithy_types::Document::Null));
    }

    #[test]
    fn test_serde_json_value_to_document_bool() {
        let doc = serde_json_value_to_document(&serde_json::Value::Bool(true));
        assert!(matches!(doc, aws_smithy_types::Document::Bool(true)));
    }

    #[test]
    fn test_serde_json_value_to_document_string() {
        let doc =
            serde_json_value_to_document(&serde_json::Value::String("hello".to_owned()));
        assert_eq!(
            doc,
            aws_smithy_types::Document::String("hello".to_owned())
        );
    }

    #[test]
    fn test_serde_json_value_to_document_array() {
        let arr = serde_json::json!([1, 2, 3]);
        let doc = serde_json_value_to_document(&arr);
        match &doc {
            aws_smithy_types::Document::Array(items) => {
                assert_eq!(items.len(), 3);
            }
            _ => panic!("expected Array"),
        }
    }

    #[test]
    fn test_serde_json_value_to_document_object() {
        let obj = serde_json::json!({"key": "value", "num": 42});
        let doc = serde_json_value_to_document(&obj);
        match &doc {
            aws_smithy_types::Document::Object(entries) => {
                assert_eq!(entries.len(), 2);
                assert!(entries.contains_key("key"));
                assert!(entries.contains_key("num"));
            }
            _ => panic!("expected Object"),
        }
    }

    // ------------------------------------------------------------------
    // Message conversion unit tests
    // ------------------------------------------------------------------

    #[test]
    fn test_convert_user_message_text_only() {
        let msg = PiMessage::user_text("Hello");
        let context = Context {
            messages: vec![msg],
            system_prompt: None,
            model: None,
            tools: vec![],
        };
        let model = test_model();
        let messages = convert_messages(context, &model);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role(), &ConversationRole::User);
    }

    #[test]
    fn test_convert_user_message_empty_content_skipped() {
        let msg = PiMessage {
            role: MessageRole::User,
            content: vec![PiContentBlock::Text(TextContent {
                text: "   ".to_owned(),
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
        let model = test_model();
        let messages = convert_messages(context, &model);
        assert_eq!(messages.len(), 0);
    }

    #[test]
    fn test_convert_assistant_message_with_thinking_and_tool() {
        let msg = PiMessage {
            role: MessageRole::Assistant,
            content: vec![
                PiContentBlock::Thinking(ThinkingContent {
                    thinking: "Let me reason...".into(),
                    signature: Some("sig123".into()),
                }),
                PiContentBlock::Text(TextContent {
                    text: "I'll look that up.".into(),
                }),
                PiContentBlock::ToolCall(ToolCallContent {
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
        let context = Context {
            messages: vec![msg],
            system_prompt: None,
            model: None,
            tools: vec![],
        };
        let model = test_model();
        let messages = convert_messages(context, &model);
        assert_eq!(messages.len(), 1);

        assert_eq!(messages[0].role(), &ConversationRole::Assistant);

        let content = messages[0].content();
        assert_eq!(content.len(), 3);
    }

    #[test]
    fn test_convert_tool_results() {
        let tool_msg = PiMessage {
            role: MessageRole::Tool,
            content: vec![PiContentBlock::ToolResult(ToolResultContent {
                id: "toolu_1".into(),
                name: "get_weather".into(),
                content: Some(vec![PiContentBlock::Text(TextContent {
                    text: "72 degrees".into(),
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
            messages: vec![tool_msg],
            system_prompt: None,
            model: None,
            tools: vec![],
        };
        let model = test_model();
        let messages = convert_messages(context, &model);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role(), &ConversationRole::User);
    }

    #[test]
    fn test_convert_consecutive_tool_results_collapsed() {
        let tool_msg_1 = PiMessage {
            role: MessageRole::Tool,
            content: vec![PiContentBlock::ToolResult(ToolResultContent {
                id: "toolu_1".into(),
                name: "get_weather".into(),
                content: Some(vec![PiContentBlock::Text(TextContent {
                    text: "72 degrees".into(),
                })]),
                error: None,
                is_error: false,
            })],
            id: None,
            name: None,
            usage: None,
            redacted: false,
        };
        let tool_msg_2 = PiMessage {
            role: MessageRole::Tool,
            content: vec![PiContentBlock::ToolResult(ToolResultContent {
                id: "toolu_2".into(),
                name: "get_time".into(),
                content: Some(vec![PiContentBlock::Text(TextContent {
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
            messages: vec![tool_msg_1, tool_msg_2],
            system_prompt: None,
            model: None,
            tools: vec![],
        };
        let model = test_model();
        let messages = convert_messages(context, &model);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role(), &ConversationRole::User);
        // Should have 2 tool result content blocks.
        assert_eq!(messages[0].content().len(), 2);
    }

    // ------------------------------------------------------------------
    // System prompt conversion tests
    // ------------------------------------------------------------------

    #[test]
    fn test_build_system_blocks() {
        let blocks = build_system_blocks("You are helpful.");
        assert_eq!(blocks.len(), 1);
        let text = match &blocks[0] {
            AwsSystemContentBlock::Text(t) => t.as_str(),
            _ => "",
        };
        assert_eq!(text, "You are helpful.");
    }

    #[test]
    fn test_build_system_blocks_empty() {
        let blocks = build_system_blocks("");
        assert_eq!(blocks.len(), 1);
        let text = match &blocks[0] {
            AwsSystemContentBlock::Text(t) => t.as_str(),
            _ => "",
        };
        assert!(text.is_empty());
    }

    // ------------------------------------------------------------------
    // Image conversion tests
    // ------------------------------------------------------------------

    #[test]
    fn test_convert_image_to_bedrock_png() {
        let img = ImageContent {
            source: ImageSource::Base64 {
                media_type: "image/png".into(),
                data: base64::Engine::encode(
                    &base64::engine::general_purpose::STANDARD,
                    "fake-png-bytes",
                ),
            },
        };
        let result = convert_image_to_bedrock(&img);
        assert!(result.is_some());
        let block = result.unwrap();
        assert_eq!(block.format(), &AwsImageFormat::Png);
    }

    #[test]
    fn test_convert_image_to_bedrock_unknown_format() {
        let img = ImageContent {
            source: ImageSource::Base64 {
                media_type: "image/bmp".into(),
                data: "AAAA".into(),
            },
        };
        let result = convert_image_to_bedrock(&img);
        assert!(result.is_none());
    }

    #[test]
    fn test_convert_image_to_bedrock_url_source() {
        let img = ImageContent {
            source: ImageSource::Url {
                url: "https://example.com/img.png".into(),
            },
        };
        let result = convert_image_to_bedrock(&img);
        assert!(result.is_none());
    }

    // ------------------------------------------------------------------
    // Build usage tests
    // ------------------------------------------------------------------

    #[test]
    fn test_build_usage() {
        let state = BedrockStreamState {
            input_tokens: 100,
            output_tokens: 50,
            cache_read: 10,
            cache_write: 5,
            ..Default::default()
        };

        let model = test_model();
        let usage = build_usage(&state, &model);
        assert_eq!(usage.input, 100);
        assert_eq!(usage.output, 50);
        assert_eq!(usage.cache_read, Some(10));
        assert_eq!(usage.cache_write, Some(5));
        assert_eq!(usage.total_tokens, Some(165));
    }

    // ------------------------------------------------------------------
    // Error formatting tests
    // ------------------------------------------------------------------

    #[test]
    fn test_format_bedrock_exception_known() {
        let msg = format_bedrock_exception("InternalServerException", "something broke");
        assert_eq!(msg, "Internal server error: something broke");
    }

    #[test]
    fn test_format_bedrock_exception_unknown() {
        let msg = format_bedrock_exception("UnknownException", "weird error");
        assert_eq!(msg, "UnknownException: weird error");
    }

    // ------------------------------------------------------------------
    // Helpers
    // ------------------------------------------------------------------

    fn test_model() -> Model {
        Model {
            id: "anthropic.claude-sonnet-4-20250514".into(),
            provider: KnownProvider::Bedrock,
            api: "bedrock-converse-stream".into(),
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
}
