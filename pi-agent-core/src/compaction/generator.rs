//! LLM-driven summary generation for context compaction.
//!
//! The shared `call_llm_for_text` function is used by both compaction
//! and branch-summary generation. It accepts a generic stream function
//! so the caller controls which model/provider is used.

use pi_ai_core::event_stream::AssistantMessageEventStream;
use pi_ai_core::stream::StreamError;
use pi_ai_core::types::{Context, Message, StreamEvent};
use std::future::Future;
use tokio_stream::StreamExt;

/// Error type for compaction operations.
#[derive(Debug, thiserror::Error)]
pub enum CompactionError {
    #[error("Stream error: {0}")]
    Stream(#[from] StreamError),
    #[error("Serialization error: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("Empty response from LLM")]
    EmptyResponse,
}

/// Result of a compaction operation.
#[derive(Debug, Clone)]
pub struct CompactionResult {
    pub summary: String,
    pub first_kept_entry_id: String,
    pub tokens_before: u64,
}

/// Shared `SUMMARIZATION_PROMPT` template.
///
/// Mirrors the TS prompt in packages/coding-agent/src/core/compaction/utils.ts.
pub static SUMMARIZATION_PROMPT: &str = r#"Summarize the following conversation context.

Focus on:
- Goal: What was the user trying to accomplish?
- Progress: What has been done so far?
- Key Decisions: What important choices were made?
- Current State: What is the current status?
- Critical Context: Any important details needed to continue.

Keep the summary concise (2-4 paragraphs) and focused on actionable information.
Do not include generic greetings or introductory remarks."#;

/// Call an LLM with a prompt and system prompt, returning the text response.
///
/// This is the shared low-level function used by:
/// - `generate_summary()` (context compaction)
/// - `generate_branch_summary()` (branch summarization)
///
/// The `stream_fn` parameter is the same pattern as `agent_loop`'s stream
/// function — it takes a `Context` and returns a stream of events.
pub async fn call_llm_for_text<F, Fut>(
    prompt: &str,
    system_prompt: &str,
    stream_fn: F,
) -> Result<String, CompactionError>
where
    F: Fn(Context) -> Fut,
    Fut: Future<Output = Result<AssistantMessageEventStream, StreamError>>,
{
    let ctx = Context {
        system_prompt: Some(system_prompt.to_string()),
        messages: vec![Message::user_text(prompt)],
        model: None,
        tools: vec![],
    };

    let mut stream = stream_fn(ctx).await?;
    let mut text_parts: Vec<String> = Vec::new();

    while let Some(event) = stream.next().await {
        match event {
            StreamEvent::TextDelta { delta } => text_parts.push(delta),
            StreamEvent::Error { error } => {
                return Err(CompactionError::Stream(StreamError::ProviderError(
                    error.message,
                )));
            }
            _ => {} // ignore Start, ThinkingDelta, ToolCallDelta, Done, Usage
        }
    }

    let text = text_parts.concat();
    if text.is_empty() {
        return Err(CompactionError::EmptyResponse);
    }
    Ok(text)
}

/// Serialize a list of session entries into a text representation for the LLM.
pub fn serialize_conversation(entries_text: &[String]) -> String {
    let mut output = String::new();
    for (i, text) in entries_text.iter().enumerate() {
        output.push_str(&format!("--- Entry {} ---\n{}\n", i + 1, text));
    }
    output
}

/// Generate a compaction summary from a set of conversation entries.
///
/// Calls the LLM via `call_llm_for_text` with the `SUMMARIZATION_PROMPT`
/// and the serialized conversation entries.
pub async fn generate_summary<F, Fut>(
    entries_text: &[String],
    stream_fn: F,
) -> Result<String, CompactionError>
where
    F: Fn(Context) -> Fut,
    Fut: Future<Output = Result<AssistantMessageEventStream, StreamError>>,
{
    let serialized = serialize_conversation(entries_text);
    let prompt = format!("{}\n\nConversation to summarize:\n\n{}", SUMMARIZATION_PROMPT, serialized);
    call_llm_for_text(&prompt, "You are a helpful assistant that summarizes conversations.", stream_fn).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_ai_core::test_utils::mock_stream_fixed;

    #[tokio::test]
    async fn test_call_llm_for_text_with_mock() {
        let stream_fn = mock_stream_fixed("test summary output", "end_turn");
        let result = call_llm_for_text("hello", "be helpful", stream_fn).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "test summary output");
    }

    #[tokio::test]
    async fn test_call_llm_for_text_empty_error() {
        let stream_fn = mock_stream_fixed("", "end_turn");
        let result = call_llm_for_text("hello", "be helpful", stream_fn).await;
        assert!(result.is_err());
        match result {
            Err(CompactionError::EmptyResponse) => {} // expected
            _ => panic!("Expected EmptyResponse error"),
        }
    }

    #[test]
    fn test_serialize_conversation() {
        let entries = vec!["hello".to_string(), "world".to_string()];
        let result = serialize_conversation(&entries);
        assert!(result.contains("Entry 1"));
        assert!(result.contains("hello"));
        assert!(result.contains("Entry 2"));
        assert!(result.contains("world"));
    }
}
