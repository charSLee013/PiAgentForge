//! Pi Agent Core — Agent runtime with tool calling and state management.
//! Mirrors packages/agent/src/
//!
//! This crate provides the heart of the pi-coding-agent: a state-machine-based
//! agent loop that streams LLM responses, executes tool calls, and emits
//! lifecycle events.
//!
//! # Architecture
//!
//! - **`types`**: Core types — `AgentEvent`, `AgentState`, `AgentContext`,
//!   `AgentToolResult`, `PendingToolCall`.
//! - **`agent_loop`**: The main `agent_loop()` async function and the
//!   `AgentError` enum.
//!
//! # Usage
//!
//! ```rust,ignore
//! use pi_agent_core::{agent_loop::agent_loop, types::*};
//!
//! let mut state = AgentState { /* ... */ };
//! agent_loop(
//!     &mut state,
//!     |ctx| Box::pin(async move { stream_simple(&model, ctx).await }),
//!     |name, id, args| execute_tool(name, id, args),
//!     |event| handle_event(event),
//!     cancel_token,
//! ).await;
//! ```

pub mod agent;
pub mod agent_loop;
/// Context compaction: token estimation, cut-point planning, LLM summary generation.
pub mod compaction;
pub mod queue;
pub mod session;
pub mod types;

/// Hook types and the `AgentLoopConfig` struct.
///
/// These are optional callbacks that the `Agent` struct wires into the
/// `agent_loop` at runtime. They mirror the TS `AgentOptions` hooks
/// (packages/agent/src/agent.ts:104-115).
pub mod hook;

#[cfg(test)]
pub mod test_utils;

// Re-export the most important types at the crate root for convenience.
pub use agent::Agent;
pub use agent_loop::{AgentError, agent_loop};
pub use hook::AgentLoopConfig;
pub use queue::{MessageQueue, QueueMode, QueuePriority};
pub use session::SessionManager;
pub use types::*;

/// Re-export compaction types at the crate root for convenience.
pub use compaction::estimator::{estimate_message_tokens, should_compact};
pub use compaction::generator::{CompactionResult, call_llm_for_text, generate_summary};
pub use compaction::planner::{CompactionPreparation, find_cut_point, prepare_compaction};
