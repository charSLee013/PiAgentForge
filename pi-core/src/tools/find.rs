//! Find tool — search for files by glob pattern.
//! Mirrors `packages/coding-agent/src/core/tools/find.ts`
//!
//! Uses the `ignore` crate for .gitignore-aware file walking.

use crate::io::{DefaultFileSystem, FileSystem, IoError};
use crate::tools::path_utils::resolve_to_cwd;
use crate::tools::truncate::{self, format_size, TruncationOptions, TruncationResult, DEFAULT_MAX_BYTES};
use globset::{Glob, GlobSet, GlobSetBuilder};
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Input
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct FindInput {
    /// Glob pattern to match files.
    pub pattern: String,
    /// Directory to search in (default: current directory).
    pub path: Option<String>,
    /// Maximum number of results (default: 1000).
    pub limit: Option<usize>,
}

// ---------------------------------------------------------------------------
// Result / Details
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct FindResult {
    /// Matching file paths, one per line.
    pub output: String,
    /// Truncation info.
    pub truncation: Option<TruncationResult>,
    /// Whether the result limit was reached.
    pub result_limit_reached: Option<usize>,
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum FindError {
    #[error("Path not found: {0}")]
    PathNotFound(String),
    #[error("Find error: {0}")]
    Other(String),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

impl From<IoError> for FindError {
    fn from(e: IoError) -> Self {
        match e {
            IoError::Io(io_err) => FindError::Io(io_err),
            IoError::NotFound(msg) => FindError::PathNotFound(msg),
            IoError::Cancelled => FindError::Other("Operation aborted".to_string()),
        }
    }
}

// ---------------------------------------------------------------------------
// Default constants
// ---------------------------------------------------------------------------

const DEFAULT_LIMIT: usize = 1000;

// ---------------------------------------------------------------------------
// execute
// ---------------------------------------------------------------------------

/// Find files by glob pattern using the default filesystem.
pub async fn execute_find(input: &FindInput, cwd: &Path) -> Result<FindResult, FindError> {
    let fs = DefaultFileSystem;
    execute_find_with(input, cwd, &fs).await
}

/// Find files by glob pattern with a custom filesystem (for testing).
pub async fn execute_find_with(
    input: &FindInput,
    cwd: &Path,
    fs: &dyn FileSystem,
) -> Result<FindResult, FindError> {
    let search_path = resolve_to_cwd(input.path.as_deref().unwrap_or("."), cwd);

    tracing::debug!(
        pattern = %input.pattern,
        search_path = %search_path.display(),
        "find::execute"
    );

    if !fs.exists(&search_path).await {
        return Err(FindError::PathNotFound(search_path.display().to_string()));
    }

    let effective_limit = input.limit.unwrap_or(DEFAULT_LIMIT);

    // Use the ignore crate for file walking with .gitignore support.
    let results = find_files_with_glob(&search_path, &input.pattern, effective_limit).await?;

    if results.is_empty() {
        return Ok(FindResult {
            output: "No files found matching pattern".to_string(),
            truncation: None,
            result_limit_reached: None,
        });
    }

    // Relativize paths.
    let relativized: Vec<String> = results
        .iter()
        .map(|p| {
            p.strip_prefix(&search_path)
                .unwrap_or(p)
                .display()
                .to_string()
        })
        .collect();

    let raw_output = relativized.join("\n");
    let result_limit_reached = relativized.len() >= effective_limit;

    // Apply byte truncation.
    let trunc_opts = TruncationOptions {
        max_lines: usize::MAX,
        max_bytes: DEFAULT_MAX_BYTES,
    };
    let trunc = truncate::truncate_head(&raw_output, trunc_opts);

    let mut final_output = trunc.content.clone();
    let details_truncation: Option<TruncationResult> = if trunc.truncated { Some(trunc) } else { None };

    let mut notices: Vec<String> = Vec::new();
    if result_limit_reached {
        notices.push(format!(
            "{} results limit reached. Use limit={} for more, or refine pattern",
            effective_limit,
            effective_limit * 2
        ));
    }
    if details_truncation.is_some() {
        notices.push(format!("{} limit reached", format_size(DEFAULT_MAX_BYTES)));
    }
    if !notices.is_empty() {
        final_output.push_str(&format!("\n\n[{}]", notices.join(". ")));
    }

    Ok(FindResult {
        output: final_output,
        truncation: details_truncation,
        result_limit_reached: if result_limit_reached {
            Some(effective_limit)
        } else {
            None
        },
    })
}

/// Walk the filesystem using `ignore` and filter by glob pattern.
async fn find_files_with_glob(dir: &Path, pattern: &str, limit: usize) -> Result<Vec<PathBuf>, FindError> {
    let dir = dir.to_path_buf();
    let pattern = pattern.to_string();

    tokio::task::spawn_blocking(move || {
        let walk = ignore::WalkBuilder::new(&dir)
            .hidden(false) // Include hidden files
            .git_ignore(true) // Respect .gitignore
            .build();

        let mut results = Vec::new();

        // Build GlobSet from the pattern.
        let glob_set = build_glob_set(&pattern);

        for result in walk {
            if results.len() >= limit {
                break;
            }

            match result {
                Ok(entry) => {
                    let path = entry.path();
                    if path.is_dir() {
                        continue;
                    }

                    // Apply glob matching.
                    if let Some(ref gs) = glob_set {
                        if gs.is_match(path) {
                            results.push(path.to_path_buf());
                        }
                    } else {
                        // If glob parsing failed, include all files.
                        results.push(path.to_path_buf());
                    }
                }
                Err(_) => continue,
            }
        }

        results.sort();
        Ok(results)
    })
    .await
    .map_err(|e| FindError::Other(format!("Task join error: {}", e)))?
}

/// Build a GlobSet from a pattern string.
fn build_glob_set(pattern: &str) -> Option<GlobSet> {
    let pattern = if pattern.contains('/') && !pattern.starts_with('/') && !pattern.starts_with("**/") {
        format!("**/{}", pattern)
    } else {
        pattern.to_string()
    };

    let mut builder = GlobSetBuilder::new();
    match Glob::new(&pattern) {
        Ok(glob) => {
            builder.add(glob);
            builder.build().ok()
        }
        Err(_) => None,
    }
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
    async fn test_find_by_glob_with_tempdir() {
        let fs = DefaultFileSystem;
        let dir = tempfile::tempdir().unwrap();
        let dir_path = dir.path().to_path_buf();

        // Create some files.
        fs.write(&dir_path.join("a.rs"), b"fn a() {}").await.unwrap();
        fs.write(&dir_path.join("b.rs"), b"fn b() {}").await.unwrap();
        fs.write(&dir_path.join("c.txt"), b"text").await.unwrap();

        let result = execute_find(
            &FindInput {
                pattern: "*.rs".to_string(),
                path: Some(".".to_string()),
                limit: None,
            },
            &dir_path,
        )
        .await
        .unwrap();

        assert!(result.output.contains("a.rs"));
        assert!(result.output.contains("b.rs"));
        assert!(!result.output.contains("c.txt"));
    }

    #[tokio::test]
    async fn test_find_no_matches_with_tempdir() {
        let dir = tempfile::tempdir().unwrap();
        let result = execute_find(
            &FindInput {
                pattern: "*.py".to_string(),
                path: None,
                limit: None,
            },
            dir.path(),
        )
        .await
        .unwrap();
        assert_eq!(result.output, "No files found matching pattern");
    }

    #[tokio::test]
    async fn test_find_path_not_found() {
        let mock = MockFileSystem::new();
        let cwd = PathBuf::from("/test/cwd");
        let result = execute_find_with(
            &FindInput {
                pattern: "*.rs".to_string(),
                path: Some("/nonexistent".to_string()),
                limit: None,
            },
            &cwd,
            &mock,
        )
        .await;
        assert!(result.is_err());
    }
}
