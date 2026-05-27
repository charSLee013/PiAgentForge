//! Core extension types for the WASM extension system.
//!
//! Extensions are WASM modules that can:
//! - Subscribe to agent lifecycle events
//! - Register LLM-callable tools
//! - Request sandbox capabilities

/// Manifest describing a WASM extension.
#[derive(Debug, Clone)]
pub struct ExtensionManifest {
    pub name: String,
    pub version: String,
    pub description: Option<String>,
    /// Capabilities this extension requires (e.g. "tools", "fs:read", "fs:write", "net").
    pub capabilities: Vec<String>,
}

/// Events that can be sent to an extension lifecycle handler.
#[derive(Debug, Clone)]
pub enum ExtensionEvent {
    Init,
    Shutdown,
    ToolCall {
        name: String,
        arguments: serde_json::Value,
    },
    Message {
        role: String,
        content: String,
    },
}

/// Extension error types.
#[derive(Debug, thiserror::Error)]
pub enum ExtensionError {
    #[error("WASM error: {0}")]
    Wasm(#[from] wasmtime::Error),
    #[error("WASM trap: {0}")]
    Trap(String),
    #[error("Serialization error: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("UTF-8 error: {0}")]
    Utf8(#[from] std::string::FromUtf8Error),
    #[error("Extension not found: {0}")]
    NotFound(String),
    #[error("Missing export: {0}")]
    MissingExport(String),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

/// Convenience result alias for extension operations.
pub type Result<T> = std::result::Result<T, ExtensionError>;

// Re-export ToolDefinition since it is a core part of the extension API.
pub use pi_ai_core::types::ToolDefinition;
