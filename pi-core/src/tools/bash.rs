//! Bash tool — execute shell commands.
//! Mirrors `packages/coding-agent/src/core/tools/bash.ts`

use crate::io::{DefaultShell, IoError, Shell, ShellOutput};
use crate::tools::truncate::{self, DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES, TruncationOptions, TruncationResult};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------------
// Input
// ---------------------------------------------------------------------------

/// Parameters accepted by the bash tool.
#[derive(Debug, Clone)]
pub struct BashInput {
    /// The shell command to execute.
    pub command: String,
    /// Optional timeout in seconds.
    pub timeout: Option<u64>,
}

// ---------------------------------------------------------------------------
// Result
// ---------------------------------------------------------------------------

/// Successful bash execution output.
#[derive(Debug, Clone)]
pub struct BashResult {
    /// Exit code of the command (0 for success).
    pub exit_code: i32,
    /// Combined stdout + stderr (same order: stderr follows stdout).
    pub output: String,
    /// Whether the output was truncated.
    pub truncated: bool,
    /// Detailed truncation info (if truncated).
    pub truncation: Option<TruncationResult>,
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum BashError {
    #[error("Command exited with code {0}")]
    NonZeroExit(i32),
    #[error("Command timed out after {0}s")]
    Timeout(u64),
    #[error("Command aborted")]
    Aborted,
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Shell error: {0}")]
    Shell(String),
}

impl From<IoError> for BashError {
    fn from(e: IoError) -> Self {
        match e {
            IoError::Cancelled => BashError::Aborted,
            IoError::Io(io_err) => BashError::Io(io_err),
            IoError::NotFound(msg) => BashError::Shell(msg),
        }
    }
}

// ---------------------------------------------------------------------------
// execute
// ---------------------------------------------------------------------------

/// Execute a bash command using the default shell.
pub async fn execute_bash(input: &BashInput, cancel: CancellationToken) -> Result<BashResult, BashError> {
    let shell = DefaultShell;
    execute_bash_with(input, &shell, cancel).await
}

/// Execute a bash command with a custom shell implementation (for testing).
pub async fn execute_bash_with(
    input: &BashInput,
    shell: &dyn Shell,
    cancel: CancellationToken,
) -> Result<BashResult, BashError> {
    tracing::debug!(
        command = %input.command,
        timeout = ?input.timeout,
        "bash::execute"
    );

    // Convert u64 seconds to Duration.
    let timeout_dur = input.timeout.map(Duration::from_secs);

    let ShellOutput { exit_code, stdout, stderr } = shell.execute(&input.command, timeout_dur, cancel).await?;

    // Combine stdout and stderr (matching TS behavior: both go to onData).
    let combined = if stderr.is_empty() {
        stdout
    } else if stdout.is_empty() {
        stderr
    } else {
        format!("{}{}", stdout, stderr)
    };

    // Apply tail truncation (bash shows the end of output).
    let trunc_opts = TruncationOptions { max_lines: DEFAULT_MAX_LINES, max_bytes: DEFAULT_MAX_BYTES };
    let trunc = truncate::truncate_tail(&combined, trunc_opts);
    let output = if trunc.truncated { trunc.content.clone() } else { combined.clone() };

    Ok(BashResult {
        exit_code,
        output,
        truncated: trunc.truncated,
        truncation: if trunc.truncated { Some(trunc) } else { None },
    })
}

// ---------------------------------------------------------------------------
// Convenience helper
// ---------------------------------------------------------------------------

/// Combine the output text with an error status suffix (matching TS appendStatus).
pub fn append_status(text: &str, status: &str) -> String {
    if text.is_empty() { status.to_string() } else { format!("{}\n\n{}", text, status) }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::tests::MockShell;

    #[tokio::test]
    async fn test_bash_success() {
        let result =
            execute_bash(&BashInput { command: "echo hello".to_string(), timeout: None }, CancellationToken::new())
                .await
                .unwrap();
        assert_eq!(result.exit_code, 0);
        assert!(result.output.contains("hello"));
        assert!(!result.truncated);
    }

    #[tokio::test]
    async fn test_bash_error_exit() {
        let result =
            execute_bash(&BashInput { command: "exit 42".to_string(), timeout: None }, CancellationToken::new()).await;
        // Non-zero exit is not an error in the tool — it still returns Ok
        assert!(result.is_ok());
        assert_eq!(result.unwrap().exit_code, 42);
    }

    #[tokio::test]
    async fn test_bash_timeout() {
        let result =
            execute_bash(&BashInput { command: "sleep 10".to_string(), timeout: Some(1) }, CancellationToken::new())
                .await;
        assert!(result.is_err());
        // The timeout may surface as Io(TimedOut) because DefaultShell converts
        // it before BashError can wrap it as Timeout.
        match result.unwrap_err() {
            BashError::Timeout(secs) => assert_eq!(secs, 1),
            BashError::Io(_) => {} // also valid — inner shell timed out
            e => panic!("Expected Timeout or Io, got: {:?}", e),
        }
    }

    #[tokio::test]
    async fn test_bash_cancellation() {
        let cancel = CancellationToken::new();
        let cancel_clone = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            cancel_clone.cancel();
        });
        let result = execute_bash(&BashInput { command: "sleep 10".to_string(), timeout: None }, cancel).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            BashError::Aborted => {} // expected
            e => panic!("Expected Aborted, got: {:?}", e),
        }
    }

    #[tokio::test]
    async fn test_bash_with_mock_shell() {
        let mock = MockShell::new();
        let cancel = CancellationToken::new();
        let result = execute_bash_with(&BashInput { command: "anything".to_string(), timeout: None }, &mock, cancel)
            .await
            .unwrap();
        assert_eq!(result.exit_code, 0);
        assert_eq!(result.output, "mock output");
    }

    #[tokio::test]
    async fn test_bash_stderr() {
        let result = execute_bash(
            &BashInput { command: "echo stderr_output >&2".to_string(), timeout: None },
            CancellationToken::new(),
        )
        .await
        .unwrap();
        // Stderr should be captured in output.
        assert!(result.output.contains("stderr_output"));
    }

    #[test]
    fn test_append_status() {
        assert_eq!(append_status("", "error"), "error");
        assert_eq!(append_status("output", "error"), "output\n\nerror");
    }
}
