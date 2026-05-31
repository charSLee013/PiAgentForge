//! Serialized file mutation queue.
//!
//! Ensures write/edit operations targeting the same file are serialized, while
//! operations on different files can run concurrently.

use std::collections::HashMap;
use std::future::Future;
use std::path::Path;
use std::sync::Arc;

static QUEUES: std::sync::LazyLock<tokio::sync::Mutex<HashMap<std::path::PathBuf, Arc<tokio::sync::Semaphore>>>> =
    std::sync::LazyLock::new(|| tokio::sync::Mutex::new(HashMap::new()));

/// Run `op` exclusively for the given `file_path`.
///
/// If another mutation is in progress for the same (canonical) path, this call
/// waits until the previous one finishes before starting.
pub async fn with_file_mutation_queue<T, F, E>(file_path: &Path, op: F) -> Result<T, E>
where
    F: Future<Output = Result<T, E>>,
{
    let canonical = std::fs::canonicalize(file_path).unwrap_or_else(|_| file_path.to_path_buf());

    let semaphore = {
        let mut queues = QUEUES.lock().await;
        queues.entry(canonical).or_insert_with(|| Arc::new(tokio::sync::Semaphore::new(1))).clone()
    };

    let _permit = semaphore.acquire().await.expect("Semaphore should not be closed");

    op.await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn test_mutation_queue_serializes_access() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.txt");

        let counter = Arc::new(AtomicUsize::new(0));
        let counter_clone = counter.clone();

        // Run two operations concurrently on the same file.
        let path1 = path.clone();
        let path2 = path.clone();
        let (r1, r2) = tokio::join!(
            with_file_mutation_queue(&path1, async {
                let val = counter_clone.load(Ordering::SeqCst);
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                counter_clone.store(val + 1, Ordering::SeqCst);
                Ok::<_, String>(val)
            }),
            with_file_mutation_queue(&path2, async {
                let val = counter.load(Ordering::SeqCst);
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                counter.store(val + 1, Ordering::SeqCst);
                Ok::<_, String>(val)
            }),
        );

        // Because of serialization, r1 should get 0 and r2 should see 1 (or vice versa).
        // But they should NOT both see 0.
        let v1 = r1.unwrap();
        let v2 = r2.unwrap();
        assert!((v1 == 0 && v2 == 1) || (v1 == 1 && v2 == 0), "Values should be sequential: got {} and {}", v1, v2);
    }
}
