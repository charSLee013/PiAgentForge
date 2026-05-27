//! Grep tool — search file contents for patterns.
//! Mirrors `packages/coding-agent/src/core/tools/grep.ts`
//!
//! Uses the `ignore` crate for .gitignore-aware file walking and `regex` for
//! pattern matching.

use crate::io::{DefaultFileSystem, FileSystem, IoError};
use crate::tools::path_utils::resolve_to_cwd;
use crate::tools::truncate::{
    self, format_size, truncate_line, TruncationOptions, TruncationResult, DEFAULT_MAX_BYTES,
    GREP_MAX_LINE_LENGTH,
};
use globset::{Glob, GlobSetBuilder};
use regex::Regex;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Input
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct GrepInput {
    /// Search pattern (regex or literal string).
    pub pattern: String,
    /// Directory or file to search (default: current directory).
    pub path: Option<String>,
    /// Glob filter (e.g., "*.rs" or "**/*.spec.ts").
    pub glob: Option<String>,
    /// Case-insensitive search.
    pub ignore_case: Option<bool>,
    /// Treat pattern as literal string instead of regex.
    pub literal: Option<bool>,
    /// Lines of context before and after each match.
    pub context: Option<usize>,
    /// Maximum number of matches.
    pub limit: Option<usize>,
}

// ---------------------------------------------------------------------------
// Result / Details
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct GrepResult {
    /// Output text with matches.
    pub output: String,
    /// Truncation info.
    pub truncation: Option<TruncationResult>,
    /// Whether the match limit was reached.
    pub match_limit_reached: Option<usize>,
    /// Whether some lines were truncated.
    pub lines_truncated: bool,
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum GrepError {
    #[error("Path not found: {0}")]
    PathNotFound(String),
    #[error("Regex error: {0}")]
    Regex(#[from] regex::Error),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Grep error: {0}")]
    Other(String),
}

impl From<IoError> for GrepError {
    fn from(e: IoError) -> Self {
        match e {
            IoError::Io(io_err) => GrepError::Io(io_err),
            IoError::NotFound(msg) => GrepError::PathNotFound(msg),
            IoError::Cancelled => GrepError::Other("Operation aborted".to_string()),
        }
    }
}

// ---------------------------------------------------------------------------
// Default constants
// ---------------------------------------------------------------------------

const DEFAULT_LIMIT: usize = 100;

// ---------------------------------------------------------------------------
// execute
// ---------------------------------------------------------------------------

/// Search file contents using the default filesystem.
pub async fn execute_grep(input: &GrepInput, cwd: &Path) -> Result<GrepResult, GrepError> {
    let fs = DefaultFileSystem;
    execute_grep_with(input, cwd, &fs).await
}

/// Search file contents with a custom filesystem (for testing).
pub async fn execute_grep_with(
    input: &GrepInput,
    cwd: &Path,
    fs: &dyn FileSystem,
) -> Result<GrepResult, GrepError> {
    let search_path = resolve_to_cwd(input.path.as_deref().unwrap_or("."), cwd);

    tracing::debug!(
        pattern = %input.pattern,
        search_path = %search_path.display(),
        "grep::execute"
    );

    // Check if path exists.
    if !fs.exists(&search_path).await {
        return Err(GrepError::PathNotFound(search_path.display().to_string()));
    }

    let is_directory = search_path.is_dir() || fs.is_dir(&search_path).await;

    // Build regex.
    let mut regex_str = input.pattern.clone();
    if input.literal == Some(true) {
        regex_str = regex::escape(&regex_str);
    }

    let re = if input.ignore_case == Some(true) {
        Regex::new(&format!("(?i){}", regex_str))?
    } else {
        Regex::new(&regex_str)?
    };

    let context_value = input.context.unwrap_or(0);
    let effective_limit = input.limit.unwrap_or(DEFAULT_LIMIT).max(1);

    // Collect matching files.
    let mut match_count = 0;
    let mut lines_truncated = false;
    let mut match_limit_reached = false;
    let mut output_lines: Vec<String> = Vec::new();

    // Walk files.
    let files = if is_directory {
        collect_files(&search_path, input.glob.as_deref()).await?
    } else {
        vec![search_path.clone()]
    };

    for file_path in &files {
        if match_limit_reached {
            break;
        }

        // Read file content.
        let content = match fs.read_to_string(file_path).await {
            Ok(c) => c,
            Err(_) => continue, // Skip unreadable files.
        };

        let lines: Vec<&str> = content.split('\n').collect();

        // Get relative path for display.
        let relative_path = if is_directory {
            file_path
                .strip_prefix(&search_path)
                .unwrap_or(file_path)
                .display()
                .to_string()
        } else {
            file_path
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default()
        };

        // Search each line.
        for (line_idx, line_text) in lines.iter().enumerate() {
            if match_count >= effective_limit {
                match_limit_reached = true;
                break;
            }

            if !re.is_match(line_text) {
                continue;
            }

            match_count += 1;

            if context_value > 0 {
                // Show context lines.
                let start = (line_idx.saturating_sub(context_value)).max(0);
                let end = (line_idx + context_value).min(lines.len() - 1);

                for (ctx_idx, ctx_line) in lines.iter().enumerate().take(end + 1).skip(start) {
                    let (truncated_text, was_truncated) = truncate_line(ctx_line, GREP_MAX_LINE_LENGTH);
                    if was_truncated {
                        lines_truncated = true;
                    }

                    if ctx_idx == line_idx {
                        output_lines.push(format!("{}:{}: {}", relative_path, ctx_idx + 1, truncated_text));
                    } else {
                        output_lines.push(format!("{}-{}- {}", relative_path, ctx_idx + 1, truncated_text));
                    }
                }
            } else {
                let (truncated_text, was_truncated) = truncate_line(line_text, GREP_MAX_LINE_LENGTH);
                if was_truncated {
                    lines_truncated = true;
                }
                output_lines.push(format!("{}:{}: {}", relative_path, line_idx + 1, truncated_text));
            }
        }
    }

    if match_count == 0 {
        return Ok(GrepResult {
            output: "No matches found".to_string(),
            truncation: None,
            match_limit_reached: None,
            lines_truncated: false,
        });
    }

    let raw_output = output_lines.join("\n");

    // Apply byte truncation.
    let trunc_opts = TruncationOptions {
        max_lines: usize::MAX,
        max_bytes: DEFAULT_MAX_BYTES,
    };
    let trunc = truncate::truncate_head(&raw_output, trunc_opts);

    let mut final_output = trunc.content.clone();
    let details_truncation: Option<TruncationResult> = if trunc.truncated { Some(trunc) } else { None };

    // Build notices.
    let mut notices: Vec<String> = Vec::new();
    if match_limit_reached {
        notices.push(format!(
            "{} matches limit reached. Use limit={} for more, or refine pattern",
            effective_limit,
            effective_limit * 2
        ));
    }
    if details_truncation.is_some() {
        notices.push(format!("{} limit reached", format_size(DEFAULT_MAX_BYTES)));
    }
    if lines_truncated {
        notices.push(format!(
            "Some lines truncated to {} chars. Use read tool to see full lines",
            GREP_MAX_LINE_LENGTH
        ));
    }
    if !notices.is_empty() {
        final_output.push_str(&format!("\n\n[{}]", notices.join(". ")));
    }

    Ok(GrepResult {
        output: final_output,
        truncation: details_truncation,
        match_limit_reached: if match_limit_reached {
            Some(effective_limit)
        } else {
            None
        },
        lines_truncated,
    })
}

/// Collect files matching an optional glob pattern using `ignore::WalkBuilder`.
async fn collect_files(dir: &Path, glob_pattern: Option<&str>) -> Result<Vec<PathBuf>, GrepError> {
    let tokio_dir = dir.to_path_buf();
    let tokio_glob = glob_pattern.map(|s| s.to_string());

    // Run the sync WalkBuilder on a blocking thread so we don't block the async runtime.
    tokio::task::spawn_blocking(move || {
        let walk = ignore::WalkBuilder::new(&tokio_dir)
            .hidden(false) // Search hidden files (matching rg --hidden)
            .git_ignore(true) // Respect .gitignore
            .build();

        let mut files = Vec::new();

        // If glob pattern is set, build a GlobSet for filtering.
        let glob_set = tokio_glob.as_ref().and_then(|glob_str| {
            let mut builder = GlobSetBuilder::new();
            match Glob::new(glob_str) {
                Ok(glob) => {
                    builder.add(glob);
                    builder.build().ok()
                }
                Err(_) => None,
            }
        });

        for result in walk {
            match result {
                Ok(entry) => {
                    let path = entry.path();
                    if path.is_dir() {
                        continue;
                    }

                    // Apply glob filter if set.
                    if let Some(ref gs) = glob_set {
                        if !gs.is_match(path) {
                            continue;
                        }
                    }

                    files.push(path.to_path_buf());
                }
                Err(_) => continue,
            }
        }

        // Sort for deterministic output.
        files.sort();
        Ok(files)
    })
    .await
    .map_err(|e| GrepError::Other(format!("Task join error: {}", e)))?
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::tests::MockFileSystem;
    use std::path::PathBuf;

    #[tokio::test]
    async fn test_grep_with_matches() {
        let fs = DefaultFileSystem;
        let dir = tempfile::tempdir().unwrap();
        let dir_path = dir.path().to_path_buf();

        fs.write(&dir_path.join("file1.txt"), b"hello world\nfoo bar\nbaz qux")
            .await
            .unwrap();
        fs.write(&dir_path.join("file2.txt"), b"abc hello\ndef ghi\njkl mno")
            .await
            .unwrap();

        let result = execute_grep(
            &GrepInput {
                pattern: "hello".to_string(),
                path: Some(dir_path.to_string_lossy().to_string()),
                glob: None,
                ignore_case: None,
                literal: None,
                context: None,
                limit: None,
            },
            &dir_path,
        )
        .await
        .unwrap();

        assert!(result.output.contains("hello"));
        assert!(!result.output.contains("No matches"));
    }

    #[tokio::test]
    async fn test_grep_no_matches() {
        let fs = DefaultFileSystem;
        let dir = tempfile::tempdir().unwrap();
        fs.write(&dir.path().join("test.txt"), b"hello world")
            .await
            .unwrap();

        let result = execute_grep(
            &GrepInput {
                pattern: "zzzzz".to_string(),
                path: None,
                glob: None,
                ignore_case: None,
                literal: None,
                context: None,
                limit: None,
            },
            dir.path(),
        )
        .await
        .unwrap();
        assert_eq!(result.output, "No matches found");
    }

    #[tokio::test]
    async fn test_grep_path_not_found() {
        let mock = MockFileSystem::new();
        let cwd = PathBuf::from("/test/cwd");
        let result = execute_grep_with(
            &GrepInput {
                pattern: "hello".to_string(),
                path: Some("/nonexistent".to_string()),
                glob: None,
                ignore_case: None,
                literal: None,
                context: None,
                limit: None,
            },
            &cwd,
            &mock,
        )
        .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_grep_case_insensitive() {
        let fs = DefaultFileSystem;
        let dir = tempfile::tempdir().unwrap();
        fs.write(&dir.path().join("test.txt"), b"HELLO world")
            .await
            .unwrap();

        let result = execute_grep(
            &GrepInput {
                pattern: "hello".to_string(),
                path: None,
                glob: None,
                ignore_case: Some(true),
                literal: None,
                context: None,
                limit: None,
            },
            dir.path(),
        )
        .await
        .unwrap();

        assert!(result.output.contains("HELLO"));
    }

    #[tokio::test]
    async fn test_grep_literal_pattern() {
        let fs = DefaultFileSystem;
        let dir = tempfile::tempdir().unwrap();
        fs.write(&dir.path().join("test.txt"), b"hello.world")
            .await
            .unwrap();

        let result = execute_grep(
            &GrepInput {
                pattern: "hello.world".to_string(),
                path: None,
                glob: None,
                ignore_case: None,
                literal: Some(true),
                context: None,
                limit: None,
            },
            dir.path(),
        )
        .await
        .unwrap();

        assert!(result.output.contains("hello.world"));
    }
}
