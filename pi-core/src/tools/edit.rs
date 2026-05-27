//! Edit tool — make precise file edits with exact text replacement.
//! Mirrors `packages/coding-agent/src/core/tools/edit.ts`

use crate::io::{DefaultFileSystem, FileSystem, IoError};
use crate::tools::edit_diff::{
    apply_edits_to_normalized_content, detect_line_ending, generate_diff_string, normalize_to_lf,
    restore_line_endings, strip_bom, DiffResult, Edit,
};
use crate::tools::file_mutation_queue::with_file_mutation_queue;
use crate::tools::path_utils::resolve_to_cwd;
use std::path::Path;

// ---------------------------------------------------------------------------
// Input
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct EditInput {
    /// Path to the file to edit (relative or absolute).
    pub path: String,
    /// One or more targeted replacements.
    pub edits: Vec<Edit>,
}

// ---------------------------------------------------------------------------
// Result / Details
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct EditResult {
    /// Confirmation message.
    pub message: String,
    /// Unified diff of the changes made.
    pub diff: String,
    /// Line number of the first change in the new file (for editor navigation).
    pub first_changed_line: Option<usize>,
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum EditError {
    #[error("Edit failed: {0}")]
    Edit(String),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

impl From<IoError> for EditError {
    fn from(e: IoError) -> Self {
        match e {
            IoError::Io(io_err) => EditError::Io(io_err),
            IoError::Cancelled => EditError::Edit("Operation aborted".to_string()),
            IoError::NotFound(msg) => EditError::Edit(format!("File not found: {}", msg)),
        }
    }
}

// ---------------------------------------------------------------------------
// execute
// ---------------------------------------------------------------------------

/// Edit a file using the default filesystem.
pub async fn execute_edit(input: &EditInput, cwd: &Path) -> Result<EditResult, EditError> {
    let fs = DefaultFileSystem;
    execute_edit_with(input, cwd, &fs).await
}

/// Edit a file with a custom filesystem (for testing).
pub async fn execute_edit_with(
    input: &EditInput,
    cwd: &Path,
    fs: &dyn FileSystem,
) -> Result<EditResult, EditError> {
    let absolute_path = resolve_to_cwd(&input.path, cwd);

    tracing::debug!(
        path = %absolute_path.display(),
        edit_count = input.edits.len(),
        "edit::execute"
    );

    if input.edits.is_empty() {
        return Err(EditError::Edit(
            "Edit tool input is invalid. edits must contain at least one replacement.".to_string(),
        ));
    }

    with_file_mutation_queue(
        &absolute_path,
        async {
            // Check existence.
            if !fs.exists(&absolute_path).await {
                return Err(EditError::Edit(format!(
                    "Could not edit file: {}. File not found.",
                    input.path
                )));
            }

            // Read the file.
            let raw_content = fs.read_to_string(&absolute_path).await?;

            // Strip BOM.
            let (bom, text) = strip_bom(&raw_content);
            let original_ending = detect_line_ending(&text);
            let normalized_content = normalize_to_lf(&text);

            // Apply edits.
            let applied = apply_edits_to_normalized_content(
                &normalized_content,
                &input.edits,
                &input.path,
            )
            .map_err(EditError::Edit)?;

            // Restore line endings and prepend BOM.
            let final_content = bom + &restore_line_endings(&applied.new_content, original_ending);

            // Write via the mutation queue's inner operation.
            fs.write(&absolute_path, final_content.as_bytes()).await?;

            // Generate diff.
            let DiffResult {
                diff,
                first_changed_line,
            } = generate_diff_string(&applied.base_content, &applied.new_content, 4);

            Ok(EditResult {
                message: format!(
                    "Successfully replaced {} block(s) in {}.",
                    input.edits.len(),
                    input.path
                ),
                diff,
                first_changed_line,
            })
        },
    )
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
    async fn test_edit_single_match() {
        let mut mock = MockFileSystem::new();
        mock.add_file(Path::new("/test/cwd/test.txt"), "hello world\nfoo bar");
        let cwd = test_cwd();

        let result = execute_edit_with(
            &EditInput {
                path: "test.txt".to_string(),
                edits: vec![Edit {
                    old_text: "world".to_string(),
                    new_text: "there".to_string(),
                }],
            },
            &cwd,
            &mock,
        )
        .await
        .unwrap();

        assert!(result.message.contains("1 block(s)"));
        assert!(result.diff.contains("-1 hello world"));
        assert!(result.diff.contains("+1 hello there"));
    }

    #[tokio::test]
    async fn test_edit_multiple_disjoint_matches() {
        let mut mock = MockFileSystem::new();
        mock.add_file(Path::new("/test/cwd/test.txt"), "aaa\nbbb\nccc\nddd");
        let cwd = test_cwd();

        let result = execute_edit_with(
            &EditInput {
                path: "test.txt".to_string(),
                edits: vec![
                    Edit {
                        old_text: "aaa".to_string(),
                        new_text: "111".to_string(),
                    },
                    Edit {
                        old_text: "ddd".to_string(),
                        new_text: "999".to_string(),
                    },
                ],
            },
            &cwd,
            &mock,
        )
        .await
        .unwrap();

        assert!(result.message.contains("2 block(s)"));
    }

    #[tokio::test]
    async fn test_edit_no_match() {
        let mut mock = MockFileSystem::new();
        mock.add_file(Path::new("/test/cwd/test.txt"), "hello world");
        let cwd = test_cwd();

        let result = execute_edit_with(
            &EditInput {
                path: "test.txt".to_string(),
                edits: vec![Edit {
                    old_text: "nonexistent".to_string(),
                    new_text: "replacement".to_string(),
                }],
            },
            &cwd,
            &mock,
        )
        .await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_edit_nonexistent_file() {
        let mock = MockFileSystem::new();
        let cwd = test_cwd();

        let result = execute_edit_with(
            &EditInput {
                path: "nonexistent.txt".to_string(),
                edits: vec![Edit {
                    old_text: "hello".to_string(),
                    new_text: "hi".to_string(),
                }],
            },
            &cwd,
            &mock,
        )
        .await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_edit_empty_edits() {
        let cwd = test_cwd();
        let mock = MockFileSystem::new();

        let result = execute_edit_with(
            &EditInput {
                path: "test.txt".to_string(),
                edits: vec![],
            },
            &cwd,
            &mock,
        )
        .await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_edit_overlapping() {
        let mut mock = MockFileSystem::new();
        mock.add_file(Path::new("/test/cwd/test.txt"), "hello world foo");
        let cwd = test_cwd();

        let result = execute_edit_with(
            &EditInput {
                path: "test.txt".to_string(),
                edits: vec![
                    Edit {
                        old_text: "hello world".to_string(),
                        new_text: "hi".to_string(),
                    },
                    Edit {
                        old_text: "world foo".to_string(),
                        new_text: "there".to_string(),
                    },
                ],
            },
            &cwd,
            &mock,
        )
        .await;

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("overlap"));
    }
}
