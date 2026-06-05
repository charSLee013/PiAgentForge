//! Agent runtime types.
//! Mirrors packages/agent/src/types.ts

use pi_ai_core::types::{ContentBlock, Message, ToolDefinition};
use std::sync::Arc;

/// Agent event types emitted during the agent loop lifecycle.
///
/// Maps to the TS `AgentEvent` union type. Events follow a strict emission order:
///
/// 1. `AgentStart` — emitted once when the loop begins
/// 2. `TurnStart` — at the beginning of each turn
/// 3. `MessageStart` / `MessageDelta` / `MessageEnd` — for the assistant response
/// 4. `ToolExecutionStart` / `ToolExecutionEnd` — for each tool call (if any)
/// 5. `TurnEnd` — when the turn completes
/// 6. `AgentEnd` — emitted once when the loop finishes
#[derive(Debug, Clone)]
pub enum AgentEvent {
    /// A new message (assistant response) starts streaming.
    MessageStart {
        /// Unique identifier for this message.
        message_id: String,
    },
    /// A text or thinking delta during streaming.
    MessageDelta {
        /// The message being streamed.
        message_id: String,
        /// The delta text content.
        delta: String,
    },
    /// Streaming of a message has completed.
    MessageEnd {
        /// The final content blocks of the message.
        message: Vec<ContentBlock>,
        /// The message identifier.
        message_id: String,
    },
    /// A tool call is about to be executed.
    ToolExecutionStart { tool_call_id: String, tool_name: String, arguments: serde_json::Value },
    /// A tool call has finished execution.
    ToolExecutionEnd { tool_call_id: String, tool_name: String, result: AgentToolResult },
    /// Partial/streaming update during a tool execution.
    ToolExecutionUpdate { tool_call_id: String, tool_name: String, partial_result: serde_json::Value },
    /// The agent loop has started with the given context.
    AgentStart { context: AgentContext },
    /// The agent loop has finished.
    AgentEnd {
        /// Reason the loop finished (e.g., "end_turn", "max_turns", "error").
        finish_reason: String,
        /// Final message transcript at loop completion.
        messages: Vec<Message>,
    },
    /// A new turn has started.
    TurnStart { turn_number: u32 },
    /// A turn has ended.
    TurnEnd { turn_number: u32 },
}

/// Callback for reporting partial/streaming tool output during execution.
pub type ToolUpdateCallback = Arc<dyn Fn(serde_json::Value) + Send + Sync>;

/// The result of executing a single tool call.
///
/// Corresponds to `AgentToolResult` in the TS types.
#[derive(Debug, Clone)]
pub struct AgentToolResult {
    /// The tool call identifier this result is for.
    pub tool_call_id: String,
    /// Content blocks returned by the tool (typically text, images).
    pub content: Vec<ContentBlock>,
    /// Whether the tool execution resulted in an error.
    pub is_error: bool,
    /// Optional structured details for logging or UI.
    pub details: Option<serde_json::Value>,
}

/// Context snapshot provided at the start of the agent loop.
///
/// Corresponds to `AgentContext` in the TS types — the state needed
/// by the low-level loop to produce LLM requests.
#[derive(Debug, Clone)]
pub struct AgentContext {
    /// Messages in the conversation so far.
    pub messages: Vec<Message>,
    /// Optional system prompt.
    pub system_prompt: Option<String>,
    /// Tool definitions available to the LLM.
    pub tools: Vec<ToolDefinition>,
    /// Model identifier string.
    pub model: Option<String>,
    /// Maximum number of turns before the loop terminates with `MaxTurnsReached`.
    pub max_turns: u32,
    /// Current turn number (0 before the loop begins).
    pub current_turn: u32,
}

/// Full mutable state of the agent loop.
///
/// This is the primary input/output of `agent_loop`. The caller owns this
/// struct and can inspect it after the loop finishes to see the final transcript.
#[derive(Debug, Clone)]
pub struct AgentState {
    /// All messages accumulated during the loop (user, assistant, tool results).
    pub messages: Vec<Message>,
    /// Context parameters that persist across turns.
    pub context: AgentContext,
    /// Tool calls that are currently waiting to be executed.
    pub pending_tool_calls: Vec<PendingToolCall>,
}

/// A tool call that has been requested by the LLM but not yet executed.
#[derive(Debug, Clone)]
pub struct PendingToolCall {
    /// The unique identifier of the tool call.
    pub id: String,
    /// The name of the tool to execute.
    pub name: String,
    /// JSON arguments for the tool call.
    pub arguments: serde_json::Value,
}
