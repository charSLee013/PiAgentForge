//! IO abstractions for testability.
//!
//! Defines [`FileSystem`] and [`Shell`] traits that each tool depends on.
//! Default implementations delegate to `tokio::fs` and `tokio::process`.

use std::ffi::OsString;
use std::fmt::Debug;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// IO-layer error.
#[derive(Debug, thiserror::Error)]
pub enum IoError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Not found: {0}")]
    NotFound(String),
    #[error("Cancelled")]
    Cancelled,
}

// ---------------------------------------------------------------------------
// FileSystem trait
// ---------------------------------------------------------------------------

/// Abstract filesystem that tools use instead of calling `tokio::fs` directly.
#[async_trait::async_trait]
pub trait FileSystem: Send + Sync + Debug {
    /// Read the raw bytes of a file.
    async fn read(&self, path: &Path) -> Result<Vec<u8>, IoError>;
    /// Read a file as a UTF-8 string.
    async fn read_to_string(&self, path: &Path) -> Result<String, IoError>;
    /// Write bytes to a file.
    async fn write(&self, path: &Path, content: &[u8]) -> Result<(), IoError>;
    /// Check whether a path exists (returns true/false, never errors).
    async fn exists(&self, path: &Path) -> bool;
    /// Check whether a path is a directory.
    async fn is_dir(&self, path: &Path) -> bool;
    /// Return metadata for a path (errors if the path does not exist).
    async fn metadata(&self, path: &Path) -> Result<DirEntry, IoError>;
    /// List entries in a directory.
    async fn read_dir(&self, path: &Path) -> Result<Vec<DirEntry>, IoError>;
    /// Recursively create directories.
    async fn create_dir_all(&self, path: &Path) -> Result<(), IoError>;
}

/// Metadata for a single directory entry.
#[derive(Debug, Clone)]
pub struct DirEntry {
    pub path: PathBuf,
    pub file_name: OsString,
    pub is_dir: bool,
    pub len: u64,
    pub modified: Option<std::time::SystemTime>,
}

// ---------------------------------------------------------------------------
// DefaultFileSystem
// ---------------------------------------------------------------------------

/// Production filesystem backed by `tokio::fs`.
#[derive(Debug, Clone, Copy)]
pub struct DefaultFileSystem;

#[async_trait::async_trait]
impl FileSystem for DefaultFileSystem {
    async fn read(&self, path: &Path) -> Result<Vec<u8>, IoError> {
        Ok(tokio::fs::read(path).await?)
    }

    async fn read_to_string(&self, path: &Path) -> Result<String, IoError> {
        Ok(tokio::fs::read_to_string(path).await?)
    }

    async fn write(&self, path: &Path, content: &[u8]) -> Result<(), IoError> {
        Ok(tokio::fs::write(path, content).await?)
    }

    async fn exists(&self, path: &Path) -> bool {
        tokio::fs::try_exists(path).await.unwrap_or(false)
    }

    async fn is_dir(&self, path: &Path) -> bool {
        tokio::fs::metadata(path).await.map(|m| m.is_dir()).unwrap_or(false)
    }

    async fn metadata(&self, path: &Path) -> Result<DirEntry, IoError> {
        let meta = tokio::fs::metadata(path).await?;
        Ok(DirEntry {
            path: path.to_path_buf(),
            file_name: path.file_name().map(|s| s.to_os_string()).unwrap_or_default(),
            is_dir: meta.is_dir(),
            len: meta.len(),
            modified: meta.modified().ok(),
        })
    }

    async fn read_dir(&self, path: &Path) -> Result<Vec<DirEntry>, IoError> {
        let mut entries = Vec::new();
        let mut dir = tokio::fs::read_dir(path).await?;
        while let Some(entry) = dir.next_entry().await? {
            let meta = entry.metadata().await.map_err(IoError::Io)?;
            entries.push(DirEntry {
                path: entry.path(),
                file_name: entry.file_name(),
                is_dir: meta.is_dir(),
                len: meta.len(),
                modified: meta.modified().ok(),
            });
        }
        Ok(entries)
    }

    async fn create_dir_all(&self, path: &Path) -> Result<(), IoError> {
        Ok(tokio::fs::create_dir_all(path).await?)
    }
}

// ---------------------------------------------------------------------------
// Shell trait
// ---------------------------------------------------------------------------

/// Result of executing a shell command.
#[derive(Debug, Clone)]
pub struct ShellOutput {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

/// Abstract shell that tools use to execute commands.
#[async_trait::async_trait]
pub trait Shell: Send + Sync + Debug {
    /// Execute a command, capturing both stdout and stderr.
    async fn execute(
        &self,
        command: &str,
        timeout: Option<Duration>,
        cancel: CancellationToken,
    ) -> Result<ShellOutput, IoError>;
}

/// Read all bytes from an async reader into a `String`.
async fn read_stream_to_string(mut reader: impl tokio::io::AsyncRead + Unpin) -> Result<String, std::io::Error> {
    let mut buf = Vec::new();
    reader.read_to_end(&mut buf).await?;
    Ok(String::from_utf8_lossy(&buf).to_string())
}

// ---------------------------------------------------------------------------
// DefaultShell
// ---------------------------------------------------------------------------

/// Production shell backed by `tokio::process::Command`.
#[derive(Debug, Clone, Copy)]
pub struct DefaultShell;

#[async_trait::async_trait]
impl Shell for DefaultShell {
    async fn execute(
        &self,
        command: &str,
        timeout: Option<Duration>,
        cancel: CancellationToken,
    ) -> Result<ShellOutput, IoError> {
        tracing::debug!(command = %command, timeout = ?timeout, "DefaultShell::execute");

        let mut child = Command::new("bash")
            .arg("-c")
            .arg(command)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(IoError::Io)?;

        let stdout_reader = child.stdout.take().ok_or_else(|| IoError::Io(std::io::Error::other("no stdout")))?;
        let stderr_reader = child.stderr.take().ok_or_else(|| IoError::Io(std::io::Error::other("no stderr")))?;

        let read_stdout = tokio::spawn(async move { read_stream_to_string(stdout_reader).await });
        let read_stderr = tokio::spawn(async move { read_stream_to_string(stderr_reader).await });

        // Wait for the process to finish, with optional timeout and cancellation.
        let wait = child.wait();
        let wait_result = match timeout {
            Some(dur) => {
                tokio::select! {
                    _ = cancel.cancelled() => {
                        let _ = child.kill().await;
                        let _ = child.wait().await;
                        return Err(IoError::Cancelled);
                    }
                    result = tokio::time::timeout(dur, wait) => {
                        match result {
                            Ok(status) => status.map_err(IoError::Io)?,
                            Err(_) => {
                                let _ = child.kill().await;
                                let _ = child.wait().await;
                                return Err(IoError::Io(std::io::Error::new(
                                    std::io::ErrorKind::TimedOut,
                                    format!("Command timed out after {}s", dur.as_secs()),
                                )));
                            }
                        }
                    }
                }
            }
            None => {
                tokio::select! {
                    _ = cancel.cancelled() => {
                        let _ = child.kill().await;
                        let _ = child.wait().await;
                        return Err(IoError::Cancelled);
                    }
                    status = wait => status.map_err(IoError::Io)?,
                }
            }
        };

        let exit_code = wait_result.code().unwrap_or(-1);
        let stdout = read_stdout.await.unwrap_or_else(|_| Ok(String::new())).unwrap_or_default();
        let stderr = read_stderr.await.unwrap_or_else(|_| Ok(String::new())).unwrap_or_default();

        Ok(ShellOutput { exit_code, stdout, stderr })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    // ---- Mock FileSystem for tests ----

    #[derive(Debug, Clone)]
    pub(crate) struct MockFileSystem {
        files: std::collections::HashMap<PathBuf, Vec<u8>>,
    }

    impl MockFileSystem {
        pub fn new() -> Self {
            Self { files: std::collections::HashMap::new() }
        }

        pub fn add_file(&mut self, path: &Path, content: &str) {
            self.files.insert(path.to_path_buf(), content.as_bytes().to_vec());
        }
    }

    #[async_trait::async_trait]
    impl FileSystem for MockFileSystem {
        async fn read(&self, path: &Path) -> Result<Vec<u8>, IoError> {
            self.files.get(path).cloned().ok_or_else(|| IoError::NotFound(path.display().to_string()))
        }

        async fn read_to_string(&self, path: &Path) -> Result<String, IoError> {
            let bytes = self.read(path).await?;
            Ok(String::from_utf8_lossy(&bytes).to_string())
        }

        async fn write(&self, _path: &Path, _content: &[u8]) -> Result<(), IoError> {
            Ok(())
        }

        async fn exists(&self, path: &Path) -> bool {
            self.files.contains_key(path)
        }

        async fn is_dir(&self, path: &Path) -> bool {
            self.files.get(path).map(|_| false).unwrap_or(true)
        }

        async fn metadata(&self, path: &Path) -> Result<DirEntry, IoError> {
            let content = self.files.get(path).ok_or_else(|| IoError::NotFound(path.display().to_string()))?;
            Ok(DirEntry {
                path: path.to_path_buf(),
                file_name: path.file_name().map(|s| s.to_os_string()).unwrap_or_default(),
                is_dir: false,
                len: content.len() as u64,
                modified: None,
            })
        }

        async fn read_dir(&self, _path: &Path) -> Result<Vec<DirEntry>, IoError> {
            Ok(Vec::new())
        }

        async fn create_dir_all(&self, _path: &Path) -> Result<(), IoError> {
            Ok(())
        }
    }

    // ---- Mock Shell ----

    #[derive(Debug, Clone)]
    pub(crate) struct MockShell {
        pub execution_count: Arc<AtomicBool>,
    }

    impl MockShell {
        pub fn new() -> Self {
            Self { execution_count: Arc::new(AtomicBool::new(false)) }
        }
    }

    #[async_trait::async_trait]
    impl Shell for MockShell {
        async fn execute(
            &self,
            _command: &str,
            _timeout: Option<Duration>,
            _cancel: CancellationToken,
        ) -> Result<ShellOutput, IoError> {
            self.execution_count.store(true, Ordering::SeqCst);
            Ok(ShellOutput { exit_code: 0, stdout: "mock output".to_string(), stderr: String::new() })
        }
    }

    #[tokio::test]
    async fn test_default_shell_echo() {
        let shell = DefaultShell;
        let cancel = CancellationToken::new();
        let result = shell.execute("echo hello", None, cancel).await.unwrap();
        assert_eq!(result.exit_code, 0);
        assert!(result.stdout.contains("hello"));
    }

    #[tokio::test]
    async fn test_default_shell_nonzero_exit() {
        let shell = DefaultShell;
        let cancel = CancellationToken::new();
        let result = shell.execute("exit 42", None, cancel).await.unwrap();
        assert_eq!(result.exit_code, 42);
    }

    #[tokio::test]
    async fn test_default_shell_timeout() {
        let shell = DefaultShell;
        let cancel = CancellationToken::new();
        let result = shell.execute("sleep 10", Some(Duration::from_millis(100)), cancel).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_default_shell_cancellation() {
        let shell = DefaultShell;
        let cancel = CancellationToken::new();
        let cancel_clone = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            cancel_clone.cancel();
        });
        let result = shell.execute("sleep 10", None, cancel).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_mock_shell() {
        let shell = MockShell::new();
        let cancel = CancellationToken::new();
        let result = shell.execute("anything", None, cancel).await.unwrap();
        assert_eq!(result.exit_code, 0);
        assert_eq!(result.stdout, "mock output");
    }

    #[tokio::test]
    async fn test_default_fs_write_and_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.txt");
        let fs = DefaultFileSystem;
        fs.write(&path, b"hello world").await.unwrap();
        assert!(fs.exists(&path).await);
        let content = fs.read_to_string(&path).await.unwrap();
        assert_eq!(content, "hello world");
    }

    #[tokio::test]
    async fn test_default_fs_not_found() {
        let fs = DefaultFileSystem;
        let result = fs.read_to_string(Path::new("/nonexistent_file_xyz")).await;
        assert!(result.is_err());
    }
}
