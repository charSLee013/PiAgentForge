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
//! - [`workflows`] — session file listing, cloning/forking, and HTML export helpers

pub mod session_manager;
pub mod storage;
pub mod types;
pub mod workflows;

pub use session_manager::SessionManager;
pub use storage::{append, create, read_all, read_header, rewrite};
pub use types::*;
pub use workflows::{
    SessionSummary, build_session_file_path, clone_active_path_to_file, export_session_as_html,
    find_most_recent_session, fork_path_to_file, list_sessions, resolve_session_id_prefix,
};
