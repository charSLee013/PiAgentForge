//! Core type definitions for the pi AI layer.
//! Mirrors packages/ai/src/types.ts

#![expect(missing_docs)]

use serde::{Deserialize, Serialize};

/// Unique identifier for a message or session entry.
pub type EntryId = String;

/// Provider API identifier.
pub type ApiId = String;

/// Known provider names.
pub type ProviderName = String;

/// Model identifier string.
pub type ModelId = String;

// ── Usage / Token Accounting ──────────────────────────────────────────

/// Token usage statistics for an assistant message.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<u64>,
}

// ── Content Blocks ────────────────────────────────────────────────────

/// A block of content within a message.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type")]
pub enum ContentBlock {
    #[serde(rename = "text")]
    Text(TextContent),
    #[serde(rename = "thinking")]
    Thinking(ThinkingContent),
    #[serde(rename = "tool_call")]
    ToolCall(ToolCallContent),
    #[serde(rename = "tool_result")]
    ToolResult(ToolResultContent),
    #[serde(rename = "image")]
    Image(ImageContent),
}

/// Plain text content.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TextContent {
    pub text: String,
}

/// Thinking/reasoning block.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ThinkingContent {
    pub thinking: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
}

/// A tool call requested by the assistant.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolCallContent {
    pub id: String,
    pub name: String,
    /// JSON-encoded arguments for the tool.
    pub arguments: serde_json::Value,
}

/// The result of a tool execution.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolResultContent {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<Vec<ContentBlock>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default)]
    pub is_error: bool,
}

/// Image content block (base64-encoded or URL).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ImageContent {
    pub source: ImageSource,
}

/// Source of an image.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type")]
pub enum ImageSource {
    #[serde(rename = "base64")]
    Base64 { media_type: String, data: String },
    #[serde(rename = "url")]
    Url { url: String },
}

// ── Messages ──────────────────────────────────────────────────────────

/// Role of a message participant.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum MessageRole {
    #[serde(rename = "user")]
    User,
    #[serde(rename = "assistant")]
    Assistant,
    #[serde(rename = "system")]
    System,
    #[serde(rename = "tool")]
    Tool,
}

/// A single message in the conversation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Message {
    pub role: MessageRole,
    pub content: Vec<ContentBlock>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<EntryId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    #[serde(default)]
    pub redacted: bool,
}

impl Message {
    /// Create a new user message with text content.
    pub fn user_text(text: impl Into<String>) -> Self {
        Self {
            role: MessageRole::User,
            content: vec![ContentBlock::Text(TextContent { text: text.into() })],
            id: None,
            name: None,
            usage: None,
            redacted: false,
        }
    }

    /// Create a new assistant message.
    pub fn assistant(content: Vec<ContentBlock>) -> Self {
        Self { role: MessageRole::Assistant, content, id: None, name: None, usage: None, redacted: false }
    }

    /// Create a new system message.
    pub fn system(text: impl Into<String>) -> Self {
        Self {
            role: MessageRole::System,
            content: vec![ContentBlock::Text(TextContent { text: text.into() })],
            id: None,
            name: None,
            usage: None,
            redacted: false,
        }
    }
}

// ── Context ───────────────────────────────────────────────────────────

/// The full conversation context sent to the LLM.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Context {
    pub messages: Vec<Message>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelId>,
    #[serde(default)]
    pub tools: Vec<ToolDefinition>,
}

/// Definition of a tool that the LLM can call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    /// JSON Schema for the tool's parameters.
    pub parameters: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strict: Option<bool>,
}

// ── Model ─────────────────────────────────────────────────────────────

/// Known provider identifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum KnownProvider {
    #[serde(rename = "anthropic")]
    Anthropic,
    #[serde(rename = "openai")]
    OpenAi,
    #[serde(rename = "google")]
    Google,
    #[serde(rename = "mistral")]
    Mistral,
    #[serde(rename = "bedrock")]
    Bedrock,
    #[serde(rename = "faux")]
    Faux,
}

/// A model entry in the catalog.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Model {
    pub id: ModelId,
    pub provider: KnownProvider,
    pub api: ApiId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(default)]
    pub supports_thinking: bool,
    #[serde(default)]
    pub supports_tools: bool,
    #[serde(default)]
    pub supports_streaming: bool,
    #[serde(default)]
    pub supports_image_input: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_per_input_token: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_per_output_token: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_per_cache_read_token: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_per_cache_write_token: Option<f64>,
}

// ── Stream Events ─────────────────────────────────────────────────────

/// Events emitted during an LLM stream.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum StreamEvent {
    #[serde(rename = "start")]
    Start,
    #[serde(rename = "text_delta")]
    TextDelta { delta: String },
    #[serde(rename = "thinking_delta")]
    ThinkingDelta { delta: String },
    #[serde(rename = "tool_call_delta")]
    ToolCallDelta { index: u32, id: Option<String>, name: Option<String>, arguments: Option<String> },
    #[serde(rename = "usage")]
    Usage(Usage),
    #[serde(rename = "done")]
    Done {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<Message>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stop_reason: Option<String>,
    },
    #[serde(rename = "error")]
    Error { error: StreamError },
}

/// An error that occurred during streaming.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamError {
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r#type: Option<String>,
}

// ── Assistant Message Event Stream Result ─────────────────────────────

/// The final result produced by an assistant message stream.
/// Mirrors the TS `AssistantMessage` interface.
#[derive(Debug, Clone)]
pub struct StreamResult {
    pub message: Message,
    pub usage: Usage,
    pub stop_reason: Option<String>,
    pub api: String,
    pub provider: KnownProvider,
    pub model: String,
    pub error_message: Option<String>,
    pub timestamp: i64,
}

// ── Stream Options ────────────────────────────────────────────────────

/// Options for streaming LLM responses.
/// Mirrors the TS `StreamOptions` interface (subset for Phase A).
#[derive(Debug, Clone, Default)]
pub struct StreamOptions {
    /// Request timeout in seconds.
    pub timeout: Option<u64>,
    /// Maximum tokens to generate.
    pub max_tokens: Option<u64>,
    /// Whether to enable thinking/reasoning.
    pub thinking: Option<bool>,
    /// API key override (overrides env var).
    pub api_key: Option<String>,
}

// ── Cost Calculation ──────────────────────────────────────────────────

/// Calculate the cost of a single usage block for a given model.
pub fn calculate_cost(model: &Model, usage: &Usage) -> f64 {
    let input = usage.input as f64 * model.cost_per_input_token.unwrap_or(0.0);
    let output = usage.output as f64 * model.cost_per_output_token.unwrap_or(0.0);
    let cache_read = usage.cache_read.unwrap_or(0) as f64 * model.cost_per_cache_read_token.unwrap_or(0.0);
    let cache_write = usage.cache_write.unwrap_or(0) as f64 * model.cost_per_cache_write_token.unwrap_or(0.0);
    input + output + cache_read + cache_write
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_message_constructors() {
        let user = Message::user_text("hello");
        assert_eq!(user.role, MessageRole::User);
        assert_eq!(user.content.len(), 1);

        let sys = Message::system("be helpful");
        assert_eq!(sys.role, MessageRole::System);
    }

    #[test]
    fn test_content_block_serde() {
        let text = ContentBlock::Text(TextContent { text: "hello".into() });
        let json = serde_json::to_string(&text).unwrap();
        assert!(json.contains("\"type\":\"text\""));
        let back: ContentBlock = serde_json::from_str(&json).unwrap();
        assert_eq!(text, back);
    }

    #[test]
    fn test_calculate_cost() {
        let model = Model {
            id: "test-model".into(),
            provider: KnownProvider::OpenAi,
            api: "openai-completions".into(),
            name: None,
            base_url: None,
            supports_thinking: false,
            supports_tools: true,
            supports_streaming: true,
            supports_image_input: false,
            max_tokens: None,
            max_input_tokens: None,
            max_output_tokens: None,
            cost_per_input_token: Some(0.01),
            cost_per_output_token: Some(0.03),
            cost_per_cache_read_token: None,
            cost_per_cache_write_token: None,
        };
        let usage = Usage { input: 100, output: 50, cache_read: None, cache_write: None, total_tokens: None };
        let cost = calculate_cost(&model, &usage);
        assert!((cost - 2.5).abs() < f64::EPSILON);
    }

    #[test]
    fn test_stream_event_serde() {
        let event = StreamEvent::TextDelta { delta: "Hello".into() };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"type\":\"text_delta\""));
        let back: StreamEvent = serde_json::from_str(&json).unwrap();
        match back {
            StreamEvent::TextDelta { delta } => assert_eq!(delta, "Hello"),
            _ => panic!("wrong variant"),
        }
    }
}
