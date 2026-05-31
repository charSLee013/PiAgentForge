//! Context compaction for long sessions.
//!
//! Pure functions for compaction logic. The session manager handles I/O,
//! and after compaction the session is reloaded.
//!
//! Mirrors packages/coding-agent/src/core/compaction/ from the TS codebase.

pub mod estimator;
pub mod generator;
pub mod planner;

pub use estimator::{ContextUsage, FileOperations, estimate_message_tokens, should_compact};
pub use generator::{CompactionError, CompactionResult, call_llm_for_text, generate_summary};
pub use planner::{CompactionPreparation, find_cut_point, prepare_compaction};
