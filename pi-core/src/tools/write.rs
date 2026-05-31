//! Write tool — write content to files.
//! Mirrors `packages/coding-agent/src/core/tools/write.ts`

use crate::io::{DefaultFileSystem, FileSystem, IoError};
use crate::tools::file_mutation_queue::with_file_mutation_queue;
use crate::tools::path_utils::resolve_to_cwd;
use std::path::Path;

// ---------------------------------------------------------------------------
// Input
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct WriteInput {
    /// Path to the file to write (relative or absolute).
    pub path: String,
    /// Content to write.
    pub content: String,
}

// ---------------------------------------------------------------------------
// Result
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct WriteResult {
    /// Confirmation message.
    pub message: String,
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum WriteError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Write error: {0}")]
    Other(String),
}

impl From<IoError> for WriteError {
    fn from(e: IoError) -> Self {
        match e {
            IoError::Io(io_err) => WriteError::Io(io_err),
            IoError::Cancelled => WriteError::Other("Operation aborted".to_string()),
            IoError::NotFound(msg) => WriteError::Other(msg),
        }
    }
}

// ---------------------------------------------------------------------------
// execute
// ---------------------------------------------------------------------------

/// Write content to a file using the default filesystem.
pub async fn execute_write(input: &WriteInput, cwd: &Path) -> Result<WriteResult, WriteError> {
    let fs = DefaultFileSystem;
    execute_write_with(input, cwd, &fs).await
}

/// Write content to a file with a custom filesystem (for testing).
pub async fn execute_write_with(
    input: &WriteInput,
    cwd: &Path,
    fs: &dyn FileSystem,
) -> Result<WriteResult, WriteError> {
    let absolute_path = resolve_to_cwd(&input.path, cwd);

    tracing::debug!(path = %absolute_path.display(), bytes = input.content.len(), "write::execute");

    // Create parent directories via the mutation queue.
    with_file_mutation_queue(&absolute_path, async {
        if let Some(parent) = absolute_path.parent() {
            fs.create_dir_all(parent).await?;
        }

        fs.write(&absolute_path, input.content.as_bytes()).await?;

        Ok::<_, WriteError>(WriteResult {
            message: format!("Successfully wrote {} bytes to {}", input.content.len(), input.path),
        })
    })
    .await
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::tests::MockFileSystem;
    use std::path::{Path, PathBuf};

    fn test_cwd() -> PathBuf {
        PathBuf::from("/test/cwd")
    }

    #[tokio::test]
    async fn test_write_new_file() {
        let mut mock = MockFileSystem::new();
        mock.add_file(Path::new("/test/cwd"), "");
        let cwd = test_cwd();
        let result = execute_write_with(
            &WriteInput { path: "new_file.txt".to_string(), content: "hello world".to_string() },
            &cwd,
            &mock,
        )
        .await
        .unwrap();
        assert!(result.message.contains("11 bytes"));
    }

    #[tokio::test]
    async fn test_write_empty_content() {
        let mut mock = MockFileSystem::new();
        mock.add_file(Path::new("/test/cwd"), "");
        let cwd = test_cwd();
        let result =
            execute_write_with(&WriteInput { path: "empty.txt".to_string(), content: String::new() }, &cwd, &mock)
                .await
                .unwrap();
        assert!(result.message.contains("0 bytes"));
    }

    #[tokio::test]
    async fn test_write_creates_dirs() {
        // Should not error even with deep paths (MockFileSystem create_dir_all is a no-op).
        let mock = MockFileSystem::new();
        let cwd = test_cwd();
        let result = execute_write_with(
            &WriteInput { path: "deep/nested/file.txt".to_string(), content: "data".to_string() },
            &cwd,
            &mock,
        )
        .await;
        assert!(result.is_ok());
    }
}
