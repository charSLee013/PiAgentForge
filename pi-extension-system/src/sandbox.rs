//! Sandbox configuration for WASM extensions.
//!
//! Controls what capabilities an extension has access to at runtime.

/// Configuration for the WASM sandbox that hosts an extension.
///
/// These settings determine what system resources the extension can access.
/// By default, all capabilities are denied; they must be explicitly granted.
#[derive(Debug, Clone)]
pub struct SandboxConfig {
    /// Allow the extension to read from the local filesystem.
    pub allow_fs_read: bool,
    /// Allow the extension to write to the local filesystem.
    pub allow_fs_write: bool,
    /// Allow the extension to make network requests.
    pub allow_net: bool,
    /// Maximum memory the WASM instance can allocate, in bytes.
    pub max_memory_bytes: u64,
    /// Optional limit on the number of WASM instructions executed.
    pub max_instructions: Option<u64>,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            allow_fs_read: false,
            allow_fs_write: false,
            allow_net: false,
            max_memory_bytes: 10 * 1024 * 1024,
            max_instructions: None,
        }
    }
}
