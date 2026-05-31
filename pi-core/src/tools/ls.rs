//! Ls tool — list directory contents.
//! Mirrors `packages/coding-agent/src/core/tools/ls.ts`

use crate::io::{DefaultFileSystem, DirEntry, FileSystem, IoError};
use crate::tools::path_utils::resolve_to_cwd;
use crate::tools::truncate::{self, DEFAULT_MAX_BYTES, TruncationOptions, TruncationResult, format_size};
use std::path::Path;

// ---------------------------------------------------------------------------
// Input
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct LsInput {
    /// Directory to list (default: current directory).
    pub path: Option<String>,
    /// Maximum number of entries (default: 500).
    pub limit: Option<usize>,
}

// ---------------------------------------------------------------------------
// Result / Details
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct LsResult {
    /// Directory listing output.
    pub output: String,
    /// Truncation info.
    pub truncation: Option<TruncationResult>,
    /// Whether the entry limit was reached.
    pub entry_limit_reached: Option<usize>,
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum LsError {
    #[error("Path not found: {0}")]
    NotFound(String),
    #[error("Not a directory: {0}")]
    NotADirectory(String),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Ls error: {0}")]
    Other(String),
}

impl From<IoError> for LsError {
    fn from(e: IoError) -> Self {
        match e {
            IoError::Io(io_err) => LsError::Io(io_err),
            IoError::NotFound(msg) => LsError::NotFound(msg),
            IoError::Cancelled => LsError::Other("Operation aborted".to_string()),
        }
    }
}

// ---------------------------------------------------------------------------
// Default constants
// ---------------------------------------------------------------------------

const DEFAULT_LIMIT: usize = 500;

// ---------------------------------------------------------------------------
// execute
// ---------------------------------------------------------------------------

/// List directory contents using the default filesystem.
pub async fn execute_ls(input: &LsInput, cwd: &Path) -> Result<LsResult, LsError> {
    let fs = DefaultFileSystem;
    execute_ls_with(input, cwd, &fs).await
}

/// List directory contents with a custom filesystem (for testing).
pub async fn execute_ls_with(input: &LsInput, cwd: &Path, fs: &dyn FileSystem) -> Result<LsResult, LsError> {
    let dir_path = resolve_to_cwd(input.path.as_deref().unwrap_or("."), cwd);

    tracing::debug!(dir = %dir_path.display(), "ls::execute");

    // Check if path exists.
    if !fs.exists(&dir_path).await {
        return Err(LsError::NotFound(dir_path.display().to_string()));
    }

    // Check if directory.
    let metadata = fs.metadata(&dir_path).await?;
    if !metadata.is_dir {
        return Err(LsError::NotADirectory(dir_path.display().to_string()));
    }

    // Read entries.
    let entries = fs.read_dir(&dir_path).await?;

    let effective_limit = input.limit.unwrap_or(DEFAULT_LIMIT);

    // Sort alphabetically, case-insensitive.
    let mut sorted_entries: Vec<DirEntry> = entries;
    sorted_entries.sort_by(|a, b| {
        let a_lower = a.file_name.to_ascii_lowercase();
        let b_lower = b.file_name.to_ascii_lowercase();
        a_lower.cmp(&b_lower)
    });

    // Format entries with directory indicator.
    let mut results: Vec<String> = Vec::new();
    let mut entry_limit_reached = false;

    for entry in &sorted_entries {
        if results.len() >= effective_limit {
            entry_limit_reached = true;
            break;
        }
        let suffix = if entry.is_dir { "/" } else { "" };
        results.push(format!("{}{}", entry.file_name.to_string_lossy(), suffix));
    }

    if results.is_empty() {
        return Ok(LsResult { output: "(empty directory)".to_string(), truncation: None, entry_limit_reached: None });
    }

    let raw_output = results.join("\n");

    // Apply byte truncation.
    let trunc_opts = TruncationOptions { max_lines: usize::MAX, max_bytes: DEFAULT_MAX_BYTES };
    let trunc = truncate::truncate_head(&raw_output, trunc_opts);

    let mut final_output = trunc.content.clone();
    let details_truncation: Option<TruncationResult> = if trunc.truncated { Some(trunc) } else { None };

    let mut notices: Vec<String> = Vec::new();
    if entry_limit_reached {
        notices.push(format!("{} entries limit reached. Use limit={} for more", effective_limit, effective_limit * 2));
    }
    if details_truncation.is_some() {
        notices.push(format!("{} limit reached", format_size(DEFAULT_MAX_BYTES)));
    }
    if !notices.is_empty() {
        final_output.push_str(&format!("\n\n[{}]", notices.join(". ")));
    }

    Ok(LsResult {
        output: final_output,
        truncation: details_truncation,
        entry_limit_reached: if entry_limit_reached { Some(effective_limit) } else { None },
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::tests::MockFileSystem;
    use std::path::PathBuf;

    fn test_cwd() -> PathBuf {
        PathBuf::from("/test/cwd")
    }

    #[tokio::test]
    async fn test_ls_not_found() {
        let mock = MockFileSystem::new();
        let cwd = test_cwd();
        let result =
            execute_ls_with(&LsInput { path: Some("/nonexistent".to_string()), limit: None }, &cwd, &mock).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_ls_with_tempdir() {
        let fs = DefaultFileSystem;
        let dir = tempfile::tempdir().unwrap();
        let dir_path = dir.path().to_path_buf();

        // Create some files.
        fs.write(&dir_path.join("a.txt"), b"aaa").await.unwrap();
        fs.write(&dir_path.join("b.txt"), b"bbb").await.unwrap();

        let result = execute_ls_with(&LsInput { path: None, limit: None }, &dir_path, &fs).await.unwrap();

        assert!(result.output.contains("a.txt"));
        assert!(result.output.contains("b.txt"));
    }

    #[tokio::test]
    async fn test_ls_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let result = execute_ls(&LsInput { path: None, limit: None }, dir.path()).await.unwrap();
        assert_eq!(result.output, "(empty directory)");
    }
}
