//! Session file workflows used by CLI and TUI entrypoints.
//!
//! This module intentionally stays small and focused on M1 session flows:
//! listing/resolving session files, cloning/forking active paths, and
//! exporting a session to simple HTML.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use super::session_manager::SessionManager;
use super::storage;
use super::types::{SessionEntry, SessionError, SessionHeader, create_session_id, now_timestamp};

/// Summary metadata for a session file, suitable for session selection UIs.
#[derive(Debug, Clone)]
pub struct SessionSummary {
    pub path: PathBuf,
    pub id: String,
    pub cwd: String,
    pub name: Option<String>,
    pub modified: SystemTime,
    pub message_count: usize,
    pub first_message: String,
    pub all_messages_text: String,
}

/// Build a timestamped session file path in `session_dir`.
pub fn build_session_file_path(session_dir: &Path, model_id: &str) -> PathBuf {
    let timestamp = chrono::Utc::now().format("%Y%m%d_%H%M%S");
    let safe_model: String =
        model_id.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
    let session_id = create_session_id();
    session_dir.join(format!("{timestamp}-{safe_model}-{session_id}.jsonl"))
}

/// Resolve a session ID prefix within `session_dir`.
pub async fn resolve_session_id_prefix(
    session_dir: &Path,
    session_id_prefix: &str,
) -> Result<Option<PathBuf>, SessionError> {
    let sessions = list_sessions(session_dir).await?;
    Ok(sessions.into_iter().find(|s| s.id.starts_with(session_id_prefix)).map(|s| s.path))
}

/// Return the most recently modified valid session file within `session_dir`.
pub async fn find_most_recent_session(session_dir: &Path) -> Result<Option<PathBuf>, SessionError> {
    let sessions = list_sessions(session_dir).await?;
    Ok(sessions.first().map(|s| s.path.clone()))
}

/// List valid session files within `session_dir`, newest first.
pub async fn list_sessions(session_dir: &Path) -> Result<Vec<SessionSummary>, SessionError> {
    let mut files = Vec::new();
    collect_session_files(session_dir, &mut files)?;

    let mut sessions = Vec::new();
    for path in files {
        if let Some(summary) = build_session_summary(&path).await? {
            sessions.push(summary);
        }
    }

    sessions.sort_by(|a, b| b.modified.cmp(&a.modified));
    Ok(sessions)
}

/// Clone the current active path from `source` into `dest_path`.
pub async fn clone_active_path_to_file(
    source: &SessionManager,
    dest_path: &Path,
    parent_session: Option<&Path>,
) -> Result<(), SessionError> {
    let entries: Vec<SessionEntry> = source.path_to_root(None).into_iter().cloned().collect();
    let header = build_header(source.cwd(), parent_session);
    storage::rewrite(dest_path, &header, &entries).await
}

/// Fork the path up to and including `entry_id` from `source` into `dest_path`.
pub async fn fork_path_to_file(
    source: &SessionManager,
    entry_id: &str,
    dest_path: &Path,
    parent_session: Option<&Path>,
) -> Result<(), SessionError> {
    if source.get_entry(entry_id).is_none() {
        return Err(SessionError::EntryNotFound(entry_id.to_string()));
    }
    let entries: Vec<SessionEntry> = source.path_to_root(Some(entry_id)).into_iter().cloned().collect();
    let header = build_header(source.cwd(), parent_session);
    storage::rewrite(dest_path, &header, &entries).await
}

/// Render a session to simple standalone HTML.
pub fn export_session_as_html(header: &SessionHeader, entries: &[SessionEntry]) -> String {
    let mut body = String::new();

    body.push_str("<h1>Pi Session Export</h1>\n");
    body.push_str(&format!(
        "<div class=\"meta\"><strong>Session:</strong> {}<br /><strong>CWD:</strong> {}<br /><strong>Created:</strong> {}</div>\n",
        escape_html(&header.id),
        escape_html(&header.cwd),
        escape_html(&header.timestamp)
    ));

    for entry in entries {
        match entry {
            SessionEntry::Message(msg) => {
                let role = msg.message.get("role").and_then(|v| v.as_str()).unwrap_or("unknown");
                let text = extract_message_text(&msg.message);
                if text.is_empty() {
                    continue;
                }
                body.push_str(&format!(
                    "<section class=\"entry role-{role}\"><div class=\"entry-meta\">{timestamp}</div><pre>{text}</pre></section>\n",
                    role = escape_html(role),
                    timestamp = escape_html(&msg.timestamp),
                    text = escape_html(&text),
                ));
            }
            SessionEntry::Compaction(comp) => {
                body.push_str(&format!(
                    "<section class=\"entry role-system\"><div class=\"entry-meta\">{}</div><pre>[compaction {} tokens]\n{}</pre></section>\n",
                    escape_html(&comp.timestamp),
                    comp.tokens_before,
                    escape_html(&comp.summary),
                ));
            }
            SessionEntry::BranchSummary(summary) => {
                body.push_str(&format!(
                    "<section class=\"entry role-system\"><div class=\"entry-meta\">{}</div><pre>[branch from {}]\n{}</pre></section>\n",
                    escape_html(&summary.timestamp),
                    escape_html(&summary.from_id),
                    escape_html(&summary.summary),
                ));
            }
            SessionEntry::SessionInfo(info) => {
                if let Some(name) = &info.name {
                    if !name.trim().is_empty() {
                        body.push_str(&format!(
                            "<section class=\"entry role-system\"><div class=\"entry-meta\">{}</div><pre>[session name] {}</pre></section>\n",
                            escape_html(&info.timestamp),
                            escape_html(name),
                        ));
                    }
                }
            }
            _ => {}
        }
    }

    format!(
        "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\" />\n<title>Pi Session Export</title>\n<style>\nbody {{ font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; max-width: 980px; margin: 2rem auto; padding: 0 1rem; background: #0f1115; color: #e6e6e6; }}\npre {{ white-space: pre-wrap; word-break: break-word; margin: 0; }}\n.entry {{ border: 1px solid #2b313a; border-radius: 6px; padding: 0.75rem; margin: 0.75rem 0; background: #161a21; }}\n.entry-meta {{ color: #8b949e; font-size: 0.9rem; margin-bottom: 0.5rem; }}\n.role-user {{ border-left: 4px solid #58a6ff; }}\n.role-assistant {{ border-left: 4px solid #3fb950; }}\n.role-system {{ border-left: 4px solid #d29922; }}\n.meta {{ color: #c9d1d9; margin-bottom: 1rem; line-height: 1.5; }}\nh1 {{ margin-bottom: 0.5rem; }}\n</style>\n</head>\n<body>\n{body}</body>\n</html>\n"
    )
}

fn build_header(cwd: &str, parent_session: Option<&Path>) -> SessionHeader {
    match parent_session {
        Some(path) => SessionHeader {
            r#type: "session".to_string(),
            version: Some(3),
            id: create_session_id(),
            timestamp: now_timestamp(),
            cwd: cwd.to_string(),
            parent_session: Some(path.to_string_lossy().to_string()),
        },
        None => SessionHeader::new(cwd.to_string(), create_session_id()),
    }
}

fn collect_session_files(dir: &Path, files: &mut Vec<PathBuf>) -> Result<(), SessionError> {
    if !dir.exists() {
        return Ok(());
    }

    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            collect_session_files(&path, files)?;
        } else if path.extension().and_then(|s| s.to_str()) == Some("jsonl") {
            files.push(path);
        }
    }

    Ok(())
}

async fn build_session_summary(path: &Path) -> Result<Option<SessionSummary>, SessionError> {
    let (header, entries, _) = match storage::read_all(path).await {
        Ok(value) => value,
        Err(SessionError::InvalidSession(_)) => return Ok(None),
        Err(err) => return Err(err),
    };

    let metadata = fs::metadata(path)?;
    let modified = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);

    let mut message_count = 0usize;
    let mut first_message = String::new();
    let mut all_messages = Vec::new();
    let mut name = None;

    for entry in &entries {
        match entry {
            SessionEntry::SessionInfo(info) => {
                name = info.name.clone().filter(|v| !v.trim().is_empty());
            }
            SessionEntry::Message(msg) => {
                message_count += 1;
                let text = extract_message_text(&msg.message);
                if !text.is_empty() {
                    if first_message.is_empty() && msg.message.get("role").and_then(|v| v.as_str()) == Some("user") {
                        first_message = text.clone();
                    }
                    all_messages.push(text);
                }
            }
            _ => {}
        }
    }

    let first_message = if first_message.is_empty() { "(no messages)".to_string() } else { first_message };

    let all_messages_text = all_messages.join(" ");

    Ok(Some(SessionSummary {
        path: path.to_path_buf(),
        id: header.id,
        cwd: header.cwd,
        name,
        modified,
        message_count,
        first_message,
        all_messages_text,
    }))
}

fn extract_message_text(message: &serde_json::Value) -> String {
    if let Some(content) = message.get("content") {
        if let Some(text) = content.as_str() {
            return text.to_string();
        }
        if let Some(blocks) = content.as_array() {
            return blocks
                .iter()
                .filter_map(|block| {
                    if block.get("type").and_then(|v| v.as_str()) == Some("text") {
                        block.get("text").and_then(|v| v.as_str()).map(str::to_string)
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
                .join(" ");
        }
    }
    String::new()
}

fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&#39;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::types::{MessageEntryData, SessionEntry};

    fn make_msg(id: &str, parent_id: Option<&str>, role: &str, text: &str) -> SessionEntry {
        SessionEntry::Message(MessageEntryData {
            id: id.to_string(),
            parent_id: parent_id.map(str::to_string),
            timestamp: now_timestamp(),
            message: serde_json::json!({
                "role": role,
                "content": [{"type": "text", "text": text}]
            }),
        })
    }

    #[tokio::test]
    async fn test_find_most_recent_session() {
        let dir = tempfile::tempdir().unwrap();
        let path1 = dir.path().join("a.jsonl");
        let path2 = dir.path().join("b.jsonl");
        let header1 = SessionHeader::new("/tmp", "id-a".to_string());
        let header2 = SessionHeader::new("/tmp", "id-b".to_string());
        storage::create(&path1, &header1).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        storage::create(&path2, &header2).await.unwrap();

        let latest = find_most_recent_session(dir.path()).await.unwrap().unwrap();
        assert_eq!(latest, path2);
    }

    #[tokio::test]
    async fn test_resolve_session_id_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.jsonl");
        let header = SessionHeader::new("/tmp", "abcdef12".to_string());
        storage::create(&path, &header).await.unwrap();

        let resolved = resolve_session_id_prefix(dir.path(), "abcd").await.unwrap();
        assert_eq!(resolved.as_deref(), Some(path.as_path()));
    }

    #[tokio::test]
    async fn test_clone_active_path_to_file() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("clone.jsonl");
        let mut sm = SessionManager::in_memory("/tmp");
        sm.append_entry(make_msg("u1", None, "user", "hello"));
        sm.append_entry(make_msg("a1", Some("u1"), "assistant", "hi"));

        clone_active_path_to_file(&sm, &dest, None).await.unwrap();
        let (_header, entries, _leaf) = storage::read_all(&dest).await.unwrap();
        assert_eq!(entries.len(), 2);
    }

    #[tokio::test]
    async fn test_fork_path_to_file() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("fork.jsonl");
        let mut sm = SessionManager::in_memory("/tmp");
        sm.append_entry(make_msg("u1", None, "user", "one"));
        sm.append_entry(make_msg("a1", Some("u1"), "assistant", "two"));
        sm.append_entry(make_msg("u2", Some("a1"), "user", "three"));

        fork_path_to_file(&sm, "a1", &dest, None).await.unwrap();
        let (_header, entries, _leaf) = storage::read_all(&dest).await.unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries.last().unwrap().id(), "a1");
    }

    #[tokio::test]
    async fn test_list_sessions_summary_contains_name() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("named.jsonl");
        let header = SessionHeader::new("/tmp/project", "id-1".to_string());
        let entries = vec![
            make_msg("u1", None, "user", "hello world"),
            SessionEntry::SessionInfo(super::super::types::SessionInfoEntryData {
                id: "s1".to_string(),
                parent_id: Some("u1".to_string()),
                timestamp: now_timestamp(),
                name: Some("Named Session".to_string()),
            }),
        ];
        storage::rewrite(&path, &header, &entries).await.unwrap();

        let sessions = list_sessions(dir.path()).await.unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].name.as_deref(), Some("Named Session"));
        assert!(sessions[0].all_messages_text.contains("hello world"));
    }

    #[test]
    fn test_export_session_as_html_contains_messages() {
        let header = SessionHeader::new("/tmp", "session-id".to_string());
        let entries = vec![make_msg("u1", None, "user", "hello"), make_msg("a1", Some("u1"), "assistant", "hi")];
        let html = export_session_as_html(&header, &entries);
        assert!(html.contains("Pi Session Export"));
        assert!(html.contains("hello"));
        assert!(html.contains("hi"));
        assert!(html.contains("<html"));
    }

    #[test]
    fn test_build_session_file_path_is_unique() {
        let dir = Path::new("/tmp/pi-session-tests");
        let first = build_session_file_path(dir, "gpt-4o");
        let second = build_session_file_path(dir, "gpt-4o");
        assert_ne!(first, second);
        assert_eq!(first.extension().and_then(|ext| ext.to_str()), Some("jsonl"));
        assert_eq!(second.extension().and_then(|ext| ext.to_str()), Some("jsonl"));
    }
}
