//! Read tool — read file contents.
//! Mirrors `packages/coding-agent/src/core/tools/read.ts`

use crate::io::{DefaultFileSystem, FileSystem, IoError};
use crate::tools::path_utils::resolve_to_cwd;
use crate::tools::truncate::{
    self, DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES, TruncationOptions, TruncationResult, format_size,
};
use std::path::Path;

// ---------------------------------------------------------------------------
// Input
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct ReadInput {
    /// Path to the file to read (relative or absolute).
    pub path: String,
    /// Line number to start reading from (1-indexed).
    pub offset: Option<usize>,
    /// Maximum number of lines to read.
    pub limit: Option<usize>,
}

// ---------------------------------------------------------------------------
// Result / Details
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct ReadResult {
    /// File contents as text (for text files).
    pub content: String,
    /// Detailed truncation info.
    pub truncation: Option<TruncationResult>,
    /// Whether the file was detected as an image.
    pub is_image: bool,
    /// MIME type if image.
    pub image_mime_type: Option<String>,
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum ReadError {
    #[error("File not found: {0}")]
    NotFound(String),
    #[error("Offset {0} is beyond end of file ({1} lines total)")]
    OffsetBeyondEnd(usize, usize),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Read error: {0}")]
    Other(String),
}

impl From<IoError> for ReadError {
    fn from(e: IoError) -> Self {
        match e {
            IoError::NotFound(p) => ReadError::NotFound(p),
            IoError::Io(io_err) => ReadError::Io(io_err),
            IoError::Cancelled => ReadError::Other("Operation aborted".to_string()),
        }
    }
}

// ---------------------------------------------------------------------------
// execute
// ---------------------------------------------------------------------------

/// Read a file using the default filesystem.
pub async fn execute_read(input: &ReadInput, cwd: &Path) -> Result<ReadResult, ReadError> {
    let fs = DefaultFileSystem;
    execute_read_with(input, cwd, &fs).await
}

/// Read a file with a custom filesystem (for testing).
pub async fn execute_read_with(input: &ReadInput, cwd: &Path, fs: &dyn FileSystem) -> Result<ReadResult, ReadError> {
    let absolute_path = resolve_to_cwd(&input.path, cwd);

    tracing::debug!(path = %absolute_path.display(), offset = ?input.offset, limit = ?input.limit, "read::execute");

    // Check existence.
    if !fs.exists(&absolute_path).await {
        return Err(ReadError::NotFound(absolute_path.display().to_string()));
    }

    // Detect image by extension.
    let mime_type = guess_image_mime_type(&absolute_path);
    if let Some(mime) = mime_type {
        // Read as binary, return base64-like info (actual encoding deferred to caller).
        let _bytes = fs.read(&absolute_path).await?;
        return Ok(ReadResult {
            content: format!("Read image file [{}]", mime),
            truncation: None,
            is_image: true,
            image_mime_type: Some(mime.to_string()),
        });
    }

    // Read text content.
    let text_content = match fs.read_to_string(&absolute_path).await {
        Ok(c) => c,
        Err(e) => {
            return Err(match e {
                IoError::Io(io_err) => ReadError::Io(io_err),
                IoError::NotFound(p) => ReadError::NotFound(p),
                IoError::Cancelled => ReadError::Other("Operation aborted".to_string()),
            });
        }
    };

    let all_lines: Vec<&str> = text_content.split('\n').collect();
    let total_file_lines = all_lines.len();

    // Apply offset (1-indexed → 0-indexed).
    let start_line = input.offset.map(|o| o.saturating_sub(1)).unwrap_or(0);
    let start_line_display = start_line + 1;

    if start_line >= all_lines.len() {
        return Err(ReadError::OffsetBeyondEnd(input.offset.unwrap_or(1), all_lines.len()));
    }

    let selected_content: String = if let Some(limit) = input.limit {
        let end_line = std::cmp::min(start_line + limit, all_lines.len());
        all_lines[start_line..end_line].join("\n")
    } else {
        all_lines[start_line..].join("\n")
    };

    // Apply truncation.
    let trunc_opts = TruncationOptions { max_lines: DEFAULT_MAX_LINES, max_bytes: DEFAULT_MAX_BYTES };
    let trunc = truncate::truncate_head(&selected_content, trunc_opts);

    let output_text = if trunc.first_line_exceeds_limit {
        let first_line_size = format_size(all_lines[start_line].len());
        format!(
            "[Line {} is {}, exceeds {} limit. Use bash: sed -n '{}p' {} | head -c {}]",
            start_line_display,
            first_line_size,
            format_size(DEFAULT_MAX_BYTES),
            start_line_display,
            input.path,
            DEFAULT_MAX_BYTES,
        )
    } else if trunc.truncated {
        let end_line_display = start_line_display + trunc.output_lines.saturating_sub(1);
        let next_offset = end_line_display + 1;
        let mut text = trunc.content.clone();
        match trunc.truncated_by {
            crate::tools::truncate::TruncatedBy::Lines => {
                text.push_str(&format!(
                    "\n\n[Showing lines {}-{} of {}. Use offset={} to continue.]",
                    start_line_display, end_line_display, total_file_lines, next_offset
                ));
            }
            _ => {
                text.push_str(&format!(
                    "\n\n[Showing lines {}-{} of {} ({} limit). Use offset={} to continue.]",
                    start_line_display,
                    end_line_display,
                    total_file_lines,
                    format_size(DEFAULT_MAX_BYTES),
                    next_offset
                ));
            }
        }
        text
    } else if let Some(user_limit) = input.limit {
        if start_line + user_limit < all_lines.len() {
            let remaining = all_lines.len() - (start_line + user_limit);
            let next_offset = start_line + user_limit + 1;
            format!("{}\n\n[{} more lines in file. Use offset={} to continue.]", trunc.content, remaining, next_offset)
        } else {
            trunc.content.clone()
        }
    } else {
        trunc.content.clone()
    };

    Ok(ReadResult {
        content: output_text,
        truncation: if trunc.truncated { Some(trunc) } else { None },
        is_image: false,
        image_mime_type: None,
    })
}

/// Guess image MIME type from file extension.
fn guess_image_mime_type(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_lowercase();
    match ext.as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        "bmp" => Some("image/bmp"),
        "svg" => Some("image/svg+xml"),
        _ => None,
    }
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
    async fn test_read_existing_file() {
        let mut mock = MockFileSystem::new();
        mock.add_file(Path::new("/test/cwd/file.txt"), "hello\nworld\nfoo\nbar");
        let cwd = test_cwd();
        let result =
            execute_read_with(&ReadInput { path: "file.txt".to_string(), offset: None, limit: None }, &cwd, &mock)
                .await
                .unwrap();
        assert!(result.content.contains("hello"));
        assert!(!result.is_image);
    }

    #[tokio::test]
    async fn test_read_nonexistent_file() {
        let mock = MockFileSystem::new();
        let cwd = test_cwd();
        let result = execute_read_with(
            &ReadInput { path: "nonexistent.txt".to_string(), offset: None, limit: None },
            &cwd,
            &mock,
        )
        .await;
        assert!(result.is_err());
        match result.unwrap_err() {
            ReadError::NotFound(_) => {} // expected
            e => panic!("Expected NotFound, got: {:?}", e),
        }
    }

    #[tokio::test]
    async fn test_read_image_file() {
        let mut mock = MockFileSystem::new();
        mock.add_file(Path::new("/test/cwd/image.png"), "fake-png-data");
        let cwd = test_cwd();
        let result =
            execute_read_with(&ReadInput { path: "image.png".to_string(), offset: None, limit: None }, &cwd, &mock)
                .await
                .unwrap();
        assert!(result.is_image);
        assert_eq!(result.image_mime_type.unwrap(), "image/png");
    }

    #[tokio::test]
    async fn test_read_with_offset() {
        let mut mock = MockFileSystem::new();
        mock.add_file(Path::new("/test/cwd/lines.txt"), "line1\nline2\nline3\nline4\nline5");
        let cwd = test_cwd();
        let result = execute_read_with(
            &ReadInput { path: "lines.txt".to_string(), offset: Some(3), limit: Some(2) },
            &cwd,
            &mock,
        )
        .await
        .unwrap();
        assert!(result.content.contains("line3"));
        assert!(result.content.contains("line4"));
        assert!(!result.content.contains("line1"));
    }

    #[tokio::test]
    async fn test_read_offset_beyond_end() {
        let mut mock = MockFileSystem::new();
        mock.add_file(Path::new("/test/cwd/short.txt"), "a\nb");
        let cwd = test_cwd();
        let result =
            execute_read_with(&ReadInput { path: "short.txt".to_string(), offset: Some(10), limit: None }, &cwd, &mock)
                .await;
        assert!(result.is_err());
    }
}
