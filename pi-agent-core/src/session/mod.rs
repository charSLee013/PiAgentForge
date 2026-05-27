//! Session management — JSONL persistence, tree-based session manager, and types.
//!
//! This module provides the session persistence and tree management layer for
//! the pi-coding-agent. It mirrors the TS packages:
//!
//! - `packages/agent/src/harness/session/`
//! - `packages/coding-agent/src/core/session-manager.ts`
//!
//! # Modules
//!
//! - [`types`] — Session data types (`SessionHeader`, `SessionEntry`, etc.)
//! - [`storage`] — JSONL file I/O (append, read, rewrite)
//! - [`session_manager`] — Tree-based in-memory session manager

pub mod session_manager;
pub mod storage;
pub mod types;

pub use session_manager::SessionManager;
pub use storage::{append, create, read_all, read_header, rewrite};
pub use types::*;
