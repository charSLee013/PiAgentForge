//! JSONL session file I/O.
//!
//! Mirrors `jsonl-storage.ts` in the TS source.
//!
//! A JSONL session file looks like:
//!
//! ```jsonl
//! {"type":"session","version":3,"id":"...","timestamp":"...","cwd":"...","parentSession":"..."}
//! {"type":"message","id":"...","parentId":"...","timestamp":"...","message":{...}}
//! {"type":"compaction","id":"...","parentId":"...","timestamp":"...","summary":"...","firstKeptEntryId":"...","tokensBefore":100}
//! ```

use std::path::Path;

use crate::session::types::*;
use tokio::fs::OpenOptions;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

/// Read the session header from the first line of a JSONL file.
///
/// Returns `SessionError::InvalidSession` if the file is empty or the first
/// line is not a valid session header.
pub async fn read_header(path: &Path) -> Result<SessionHeader, SessionError> {
    let file = tokio::fs::File::open(path).await?;
    let mut reader = BufReader::new(file);
    let mut first_line = String::new();
    let bytes_read = reader.read_line(&mut first_line).await?;
    if bytes_read == 0 {
        return Err(SessionError::InvalidSession(format!("Empty session file: {}", path.display())));
    }
    let trimmed = first_line.trim();
    if trimmed.is_empty() {
        return Err(SessionError::InvalidSession(format!("Empty first line in session file: {}", path.display())));
    }
    let header: SessionHeader = serde_json::from_str(trimmed)
        .map_err(|e| SessionError::InvalidSession(format!("Invalid session header in {}: {}", path.display(), e)))?;
    Ok(header)
}

/// Read all entries from a JSONL session file (including the header).
///
/// Returns `(header, entries, leaf_id)` where `leaf_id` is the ID of the last
/// entry in the file, if any.
pub async fn read_all(path: &Path) -> Result<(SessionHeader, Vec<SessionEntry>, Option<EntryId>), SessionError> {
    let file = tokio::fs::File::open(path).await?;
    let mut reader = BufReader::new(file);
    let mut line = String::new();

    // Read header
    let bytes_read = reader.read_line(&mut line).await?;
    if bytes_read == 0 {
        return Err(SessionError::InvalidSession(format!("Empty session file: {}", path.display())));
    }
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Err(SessionError::InvalidSession(format!("Empty first line in session file: {}", path.display())));
    }
    let header: SessionHeader = serde_json::from_str(trimmed)
        .map_err(|e| SessionError::InvalidSession(format!("Invalid session header in {}: {}", path.display(), e)))?;

    // Read entries
    let mut entries = Vec::new();
    let mut leaf_id: Option<EntryId> = None;
    loop {
        line.clear();
        let bytes_read = reader.read_line(&mut line).await?;
        if bytes_read == 0 {
            break;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Ok(entry) = serde_json::from_str::<SessionEntry>(trimmed) {
            leaf_id = Some(entry.id().to_string());
            entries.push(entry);
        }
        // Silently skip malformed lines (matching TS behaviour).
    }

    Ok((header, entries, leaf_id))
}

/// Append a single entry to a JSONL session file.
///
/// Creates the file and parent directories if they do not exist.
pub async fn append(path: &Path, entry: &SessionEntry) -> Result<(), SessionError> {
    // Ensure parent directory exists
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    let mut file = OpenOptions::new().create(true).append(true).open(path).await?;

    let line = serde_json::to_string(entry)?;
    file.write_all(line.as_bytes()).await?;
    file.write_all(b"\n").await?;
    file.flush().await?;

    Ok(())
}

/// Write a new session file with header (overwrites any existing file).
///
/// Creates parent directories if they do not exist.
pub async fn create(path: &Path, header: &SessionHeader) -> Result<(), SessionError> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    let line = serde_json::to_string(header)?;
    tokio::fs::write(path, format!("{line}\n")).await?;
    Ok(())
}

/// Overwrite a session file with the full contents (header + all entries).
///
/// Used when migration or branching rewrites the file.
pub async fn rewrite(path: &Path, header: &SessionHeader, entries: &[SessionEntry]) -> Result<(), SessionError> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    let mut buf = serde_json::to_string(header)?;
    buf.push('\n');
    for entry in entries {
        buf.push_str(&serde_json::to_string(entry)?);
        buf.push('\n');
    }
    tokio::fs::write(path, buf.as_bytes()).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn make_message_entry(parent_id: Option<&str>) -> SessionEntry {
        let existing: HashSet<String> = HashSet::new();
        let id = generate_entry_id(&existing);
        SessionEntry::Message(MessageEntryData {
            id,
            parent_id: parent_id.map(|s| s.to_string()),
            timestamp: now_timestamp(),
            message: serde_json::json!({
                "role": "user",
                "content": [{"type": "text", "text": "hello"}]
            }),
        })
    }

    #[tokio::test]
    async fn test_round_trip() {
        let dir = std::env::temp_dir().join("session_test_round_trip");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let path = dir.join("session.jsonl");

        // Create header and write it
        let header = SessionHeader::new("/tmp", "test-session-id".to_string());
        create(&path, &header).await.unwrap();

        // Append two entries
        let entry1 = make_message_entry(None);
        let entry1_id = entry1.id().to_string();
        append(&path, &entry1).await.unwrap();

        let entry2 = make_message_entry(Some(&entry1_id));
        append(&path, &entry2).await.unwrap();

        // Read back
        let (rd_header, entries, leaf_id) = read_all(&path).await.unwrap();
        assert_eq!(rd_header.id, "test-session-id");
        assert_eq!(entries.len(), 2);
        assert_eq!(leaf_id.as_deref(), Some(entry2.id()));

        // Verify header-only read
        let h = read_header(&path).await.unwrap();
        assert_eq!(h.id, "test-session-id");

        // Cleanup
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn test_append_and_read_header() {
        let dir = std::env::temp_dir().join("session_test_header");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let path = dir.join("session.jsonl");

        let header = SessionHeader::with_parent("/tmp", "id-1".to_string(), "/old/session.jsonl");
        create(&path, &header).await.unwrap();

        let read_back = read_header(&path).await.unwrap();
        assert_eq!(read_back.id, "id-1");
        assert_eq!(read_back.parent_session.as_deref(), Some("/old/session.jsonl"));
        assert_eq!(read_back.version, Some(3));

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn test_empty_file_error() {
        let dir = std::env::temp_dir().join("session_test_empty");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let path = dir.join("empty.jsonl");
        tokio::fs::write(&path, "").await.unwrap();

        let result = read_all(&path).await;
        assert!(result.is_err());

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn test_rewrite() {
        let dir = std::env::temp_dir().join("session_test_rewrite");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let path = dir.join("session.jsonl");

        let header = SessionHeader::new("/tmp", "rewrite-id".to_string());
        create(&path, &header).await.unwrap();

        let entry = make_message_entry(None);
        append(&path, &entry).await.unwrap();

        // Rewrite with fewer entries
        let new_header = SessionHeader::new("/tmp", "new-id".to_string());
        rewrite(&path, &new_header, &[]).await.unwrap();

        let (h, entries, _) = read_all(&path).await.unwrap();
        assert_eq!(h.id, "new-id");
        assert!(entries.is_empty());

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn test_skips_malformed_lines() {
        let dir = std::env::temp_dir().join("session_test_malformed");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let path = dir.join("session.jsonl");

        let header = SessionHeader::new("/tmp", "malformed-test".to_string());
        let content = format!(
            "{}\n{{\"type\":\"message\",\"id\":\"1\",\"parentId\":null,\"timestamp\":\"now\",\"message\":{{}}}}\nnot-json\n{{\"type\":\"message\",\"id\":\"2\",\"parentId\":\"1\",\"timestamp\":\"now\",\"message\":{{}}}}\n",
            serde_json::to_string(&header).unwrap()
        );
        tokio::fs::write(&path, &content).await.unwrap();

        let (_, entries, leaf_id) = read_all(&path).await.unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(leaf_id.as_deref(), Some("2"));

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }
}
