//! RPC mode — JSONL-over-stdio protocol for headless agent control.
//!
//! # Modules
//!
//! - [`types`] — Protocol type definitions (commands, responses, states)
//! - [`jsonl`] — JSONL framing helpers (serialize/deserialize)
//! - [`server`] — RPC server loop (stdin/stdout protocol dispatcher)

pub mod jsonl;
mod runtime;
pub mod server;
pub mod types;
