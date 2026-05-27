//! Hook types and `AgentLoopConfig` for the agent loop.
//!
//! Mirrors the TS `AgentOptions` hooks in packages/agent/src/agent.ts.

use pi_ai_core::types::{ContentBlock, Message};
use std::sync::{Arc, Mutex};

// ---------------------------------------------------------------------------
// Hook context types
// ---------------------------------------------------------------------------

/// Context passed to a [`BeforeToolHook`].
#[derive(Debug, Clone)]
pub struct BeforeToolCallContext {
    pub message: Message,
    pub tool_name: String,
    pub tool_call_id: String,
    pub args: serde_json::Value,
}

/// Result returned from a [`BeforeToolHook`].
///
/// Returning `block: true` prevents the tool from executing.
/// `reason` becomes the error text shown in the result.
#[derive(Debug, Clone)]
pub struct BeforeToolCallResult {
    pub block: bool,
    pub reason: Option<String>,
}

/// Context passed to an [`AfterToolHook`].
#[derive(Debug, Clone)]
pub struct AfterToolCallContext {
    pub message: Message,
    pub tool_name: String,
    pub tool_call_id: String,
    pub args: serde_json::Value,
    pub result: Vec<ContentBlock>,
    pub is_error: bool,
}

/// Partial overrides returned from an [`AfterToolHook`].
///
/// Omitted fields keep the original executed values.
#[derive(Debug, Clone)]
pub struct AfterToolCallResult {
    pub content: Option<Vec<ContentBlock>>,
    pub is_error: Option<bool>,
    /// Hint the agent should stop after this batch.
    pub terminate: Option<bool>,
}

// ---------------------------------------------------------------------------
// Hook type aliases
// ---------------------------------------------------------------------------

/// Hook invoked before a tool call is executed.
///
/// `Arc<Mutex<dyn FnMut>>` allows cloning across closure boundaries,
/// making the closure `Fn` (satisfying `agent_loop`'s `G: Fn(...)` bound).
///
/// Return `Some(BeforeToolCallResult { block: true, .. })` to prevent execution.
pub type BeforeToolHook =
    Arc<Mutex<dyn FnMut(BeforeToolCallContext) -> Result<Option<BeforeToolCallResult>, String> + Send>>;

/// Hook invoked after a tool call completes.
pub type AfterToolHook =
    Arc<Mutex<dyn FnMut(AfterToolCallContext) -> Result<Option<AfterToolCallResult>, String> + Send>>;

// ---------------------------------------------------------------------------
// AgentLoopConfig
// ---------------------------------------------------------------------------

/// Configuration for the agent loop — optional hooks.
///
/// Currently wired: `before_tool_call`, `after_tool_call`.
/// `prepare_next_turn`, `should_stop_after_turn`, and `tool_execution` will
/// be added when the runtime supports per-turn lifecycle hooks and parallel
/// execution (Phases 1+).
#[derive(Default)]
pub struct AgentLoopConfig {
    /// Called before a tool executes. Return `{ block: true }` to skip.
    pub before_tool_call: Option<BeforeToolHook>,

    /// Called after a tool executes. Can override content / is_error.
    pub after_tool_call: Option<AfterToolHook>,
}
