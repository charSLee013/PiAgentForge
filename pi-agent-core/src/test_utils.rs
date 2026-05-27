//! Test utilities for agent-loop tests.
//!
//! Provides assertion helpers and default state factories that complement
//! the mock stream factories in `pi_ai_core::test_utils`.

use crate::types::{AgentContext, AgentEvent, AgentState, AgentToolResult};
use pi_ai_core::types::{ContentBlock, Message, ToolDefinition};

/// Assert that captured [`AgentEvent`]s match an expected sequence.
///
/// Each entry in `expected_patterns` is a string that must appear as a
/// substring of the corresponding event's variant name (Debug output).
///
/// # Panics
///
/// Panics if the event count differs or any event's Debug output doesn't
/// contain the expected substring.
pub fn assert_event_sequence(events: &[AgentEvent], expected_patterns: &[&str]) {
    assert_eq!(
        events.len(),
        expected_patterns.len(),
        "Event count mismatch.\n  actual:   {}\n  expected: {}\n  actual events: {:#?}",
        events.len(),
        expected_patterns.len(),
        events,
    );

    for (i, (event, expected)) in events.iter().zip(expected_patterns.iter()).enumerate() {
        let debug = format!("{event:?}");
        assert!(
            debug.contains(expected),
            "Event {i}: expected pattern `{expected}` not found in `{debug}`"
        );
    }
}

/// Create an [`AgentState`] with a single user message and default context.
///
/// The context has `max_turns` set to `max_turns` and uses a faux model.
pub fn default_state(max_turns: u32) -> AgentState {
    AgentState {
        messages: vec![Message::user_text("test prompt")],
        context: AgentContext {
            messages: vec![],
            system_prompt: None,
            tools: vec![],
            model: Some("faux-model".to_string()),
            max_turns,
            current_turn: 0,
        },
        pending_tool_calls: vec![],
    }
}

/// A [`ToolDefinition`] with the given name and a `{"type": "object"}` schema.
///
/// Useful for registering a placeholder tool in test contexts.
pub fn dummy_tool_definition(name: &str) -> ToolDefinition {
    ToolDefinition {
        name: name.to_string(),
        description: String::new(),
        parameters: serde_json::json!({"type": "object"}),
        strict: None,
    }
}

/// Tool executor that always returns a successful result with `"ok"` as text.
pub fn ok_tool_executor(
    _name: &str,
    _id: &str,
    _args: &serde_json::Value,
) -> Result<AgentToolResult, String> {
    Ok(AgentToolResult {
        tool_call_id: _id.to_string(),
        content: vec![ContentBlock::Text(pi_ai_core::types::TextContent {
            text: "ok".to_string(),
        })],
        is_error: false,
        details: Some(serde_json::Value::Null),
    })
}

/// Tool executor that always returns an error.
pub fn failing_tool_executor(
    _name: &str,
    _id: &str,
    _args: &serde_json::Value,
) -> Result<AgentToolResult, String> {
    Err("mock failure".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_assert_event_sequence_matching() {
        let ctx = AgentContext {
            messages: vec![],
            system_prompt: None,
            tools: vec![],
            model: None,
            max_turns: 1,
            current_turn: 0,
        };
        let events = vec![
            AgentEvent::AgentStart { context: ctx },
            AgentEvent::AgentEnd {
                finish_reason: "end_turn".to_string(),
                messages: vec![],
            },
        ];
        assert_event_sequence(&events, &["AgentStart", "AgentEnd"]);
    }

    #[test]
    fn test_default_state_has_known_model() {
        let state = default_state(42);
        assert_eq!(state.context.max_turns, 42);
        assert_eq!(state.context.messages.len(), 0);
        assert!(state.context.model.is_some());
    }

    #[test]
    fn test_ok_tool_executor_returns_success() {
        let result = ok_tool_executor("read", "call_1", &serde_json::json!({"path": "x"}));
        assert!(result.is_ok());
    }

    #[test]
    fn test_failing_tool_executor_returns_error() {
        let result = failing_tool_executor("read", "call_1", &serde_json::json!({}));
        assert!(result.is_err());
    }
}
