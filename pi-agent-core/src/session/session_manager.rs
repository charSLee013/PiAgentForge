//! Tree-based session manager.
//!
//! Mirrors `SessionManager` from `packages/coding-agent/src/core/session-manager.ts`
//! and `Session` from `packages/agent/src/harness/session/session.ts`.
//!
//! A `SessionManager` maintains:
//! - An ordered list of all entries (append-only, tree-structured via `id`/`parent_id`)
//! - A `by_id` index for O(1) lookups
//! - A `leaf_id` tracking the current position in the tree
//! - A label cache for resolved label lookups
//!
//! # Tree semantics
//!
//! - **Append**: creates a new entry whose `parent_id` is the current `leaf_id`.
//!   The new entry becomes the leaf.
//! - **Branch**: moves `leaf_id` to an existing entry. The next append creates a
//!   child of that entry, forming a branch.
//! - **Path to root**: walks from a given entry up through `parent_id` links,
//!   producing the sequence of entries from root to that entry.

use std::collections::{HashMap, HashSet};

use crate::session::types::*;

/// A tree-based session manager.
///
/// Provides the core tree traversal and mutation operations without I/O.
/// The caller is responsible for persisting changes (e.g. via `storage::append`).
#[derive(Debug, Clone)]
pub struct SessionManager {
    /// The session header.
    header: SessionHeader,
    /// All entries in insertion order.
    entries: Vec<SessionEntry>,
    /// Index: entry ID → index in `entries`.
    by_id: HashMap<EntryId, usize>,
    /// Resolved labels: target entry ID → label text.
    labels_by_id: HashMap<EntryId, String>,
    /// Current leaf position; `None` means "before the first entry".
    leaf_id: Option<EntryId>,
}

impl SessionManager {
    /// Create a new empty session manager with the given header.
    pub fn new(header: SessionHeader) -> Self {
        Self { header, entries: Vec::new(), by_id: HashMap::new(), labels_by_id: HashMap::new(), leaf_id: None }
    }

    /// Create a session manager from existing entries.
    ///
    /// Rebuilds the internal index. Use this after reading entries from a file.
    pub fn from_entries(header: SessionHeader, entries: Vec<SessionEntry>) -> Self {
        let mut sm = Self::new(header);
        for entry in entries {
            sm._index_entry(entry);
        }
        sm
    }

    /// Create an in-memory-only session (no header persistence).
    pub fn in_memory(cwd: impl Into<String>) -> Self {
        let id = create_session_id();
        let header = SessionHeader::new(cwd, id);
        Self::new(header)
    }

    // ── Read access ──────────────────────────────────────────────────────

    /// The session header.
    pub fn header(&self) -> &SessionHeader {
        &self.header
    }

    /// All entries in insertion order (shallow clone).
    pub fn entries(&self) -> Vec<SessionEntry> {
        self.entries.clone()
    }

    /// The current leaf ID, if any.
    pub fn leaf_id(&self) -> Option<&EntryId> {
        self.leaf_id.as_ref()
    }

    /// Look up an entry by ID.
    pub fn get_entry(&self, id: &str) -> Option<&SessionEntry> {
        self.by_id.get(id).map(|&idx| &self.entries[idx])
    }

    /// Get a mutable reference to an entry by ID.
    pub fn get_entry_mut(&mut self, id: &str) -> Option<&mut SessionEntry> {
        self.by_id.get(id).map(|&idx| &mut self.entries[idx])
    }

    /// The number of entries (excluding header).
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the session has no entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The currently selected leaf entry, if any.
    pub fn leaf_entry(&self) -> Option<&SessionEntry> {
        self.leaf_id.as_ref().and_then(|id| self.get_entry(id))
    }

    /// Get the session ID.
    pub fn session_id(&self) -> &str {
        &self.header.id
    }

    /// Get the session cwd.
    pub fn cwd(&self) -> &str {
        &self.header.cwd
    }

    /// Get all entries of a specific type.
    pub fn find_entries(&self, entry_type: &str) -> Vec<&SessionEntry> {
        self.entries.iter().filter(|e| e.variant_name() == entry_type).collect()
    }

    // ── Labels ───────────────────────────────────────────────────────────

    /// Get the resolved label for an entry, if any.
    pub fn get_label(&self, id: &str) -> Option<&str> {
        self.labels_by_id.get(id).map(|s| s.as_str())
    }

    /// Get all resolved labels.
    pub fn labels(&self) -> &HashMap<EntryId, String> {
        &self.labels_by_id
    }

    // ── Tree traversal ───────────────────────────────────────────────────

    /// Walk from the given entry (or current leaf) to root, returning entries
    /// in path order (root first, leaf last).
    ///
    /// Mirrors `getBranch()` / `getPathToRoot()` in the TS source.
    pub fn path_to_root(&self, from_id: Option<&str>) -> Vec<&SessionEntry> {
        let start_id = from_id.or(self.leaf_id.as_deref());
        let Some(sid) = start_id else {
            return Vec::new();
        };
        let mut path: Vec<&SessionEntry> = Vec::new();
        let mut current_id: Option<&str> = Some(sid);
        while let Some(cid) = current_id {
            if let Some(entry) = self.get_entry(cid) {
                path.push(entry);
                current_id = entry.parent_id();
            } else {
                break;
            }
        }
        path.reverse();
        path
    }

    /// Get all direct children of an entry.
    ///
    /// Mirrors `getChildren()` in the TS source.
    pub fn get_children(&self, parent_id: &str) -> Vec<&SessionEntry> {
        self.entries.iter().filter(|e| e.parent_id() == Some(parent_id)).collect()
    }

    /// Build the session tree structure.
    ///
    /// Returns a list of root nodes. Each node has its children nested.
    /// Orphaned entries (broken parent chain) are also returned as roots.
    /// Children are sorted by timestamp (oldest first).
    ///
    /// Mirrors `getTree()` in the TS source.
    pub fn get_tree(&self) -> Vec<SessionTreeNode> {
        // Phase 1: Create all nodes and mark every entry as a potential root.
        let mut node_map: HashMap<String, SessionTreeNode> = HashMap::new();
        let mut root_candidates: HashSet<String> = HashSet::new();

        for entry in &self.entries {
            let label = self.labels_by_id.get(entry.id()).cloned();
            root_candidates.insert(entry.id().to_string());
            node_map
                .insert(entry.id().to_string(), SessionTreeNode { entry: entry.clone(), children: Vec::new(), label });
        }

        // Phase 2: Link children to parents.
        // We keep every node in node_map so that later children can still find
        // their parent.  A child node is cloned into the parent's children list;
        // the original stays in node_map for its own children to reference.
        for entry in &self.entries {
            let eid = entry.id().to_string();
            let Some(pid) = entry.parent_id().map(|s| s.to_string()) else {
                continue; // root (null parent)
            };
            if pid == eid {
                continue; // self-referencing root
            }

            // This entry is a child → remove from root candidates
            root_candidates.remove(&eid);

            // Clone the child node into the parent's children list.
            // The original stays in node_map so grandchildren can still find it.
            if let Some(child_node) = node_map.get(&eid).cloned() {
                if let Some(parent_node) = node_map.get_mut(&pid) {
                    parent_node.children.push(child_node);
                }
            }
        }

        // Phase 3: Collect roots (everything still in root_candidates).
        let mut roots: Vec<SessionTreeNode> = root_candidates.iter().filter_map(|id| node_map.remove(id)).collect();

        sort_tree_nodes(&mut roots);
        roots
    }

    /// Get the latest session name from session_info entries.
    ///
    /// Mirrors `getSessionName()` in the TS source.
    pub fn get_session_name(&self) -> Option<&str> {
        for entry in self.entries.iter().rev() {
            if let SessionEntry::SessionInfo(info) = entry {
                return info.name.as_deref().and_then(|n| {
                    let trimmed = n.trim();
                    if trimmed.is_empty() { None } else { Some(trimmed) }
                });
            }
        }
        None
    }

    // ── Mutation ─────────────────────────────────────────────────────────

    /// Append a session entry to the tree.
    ///
    /// Sets the entry's parent to the current leaf (unless it already has one),
    /// updates the index, and advances the leaf pointer.
    ///
    /// Returns the entry ID.
    pub fn append_entry(&mut self, entry: SessionEntry) -> EntryId {
        let id = entry.id().to_string();
        self._index_entry(entry);
        id
    }

    /// Append a message entry under the current leaf. Returns the entry ID.
    pub fn append_message(&mut self, message: serde_json::Value) -> EntryId {
        let id = generate_entry_id(&self._existing_ids());
        let entry = SessionEntry::Message(MessageEntryData {
            id: id.clone(),
            parent_id: self.leaf_id.clone(),
            timestamp: now_timestamp(),
            message,
        });
        self.append_entry(entry);
        id
    }

    /// Append a compaction entry. Returns the entry ID.
    pub fn append_compaction(
        &mut self,
        summary: impl Into<String>,
        first_kept_entry_id: impl Into<EntryId>,
        tokens_before: u64,
    ) -> EntryId {
        let id = generate_entry_id(&self._existing_ids());
        let entry = SessionEntry::Compaction(CompactionEntryData {
            id: id.clone(),
            parent_id: self.leaf_id.clone(),
            timestamp: now_timestamp(),
            summary: summary.into(),
            first_kept_entry_id: first_kept_entry_id.into(),
            tokens_before,
            details: None,
            from_hook: None,
        });
        self.append_entry(entry);
        id
    }

    /// Append a thinking level change. Returns the entry ID.
    pub fn append_thinking_level_change(&mut self, thinking_level: impl Into<String>) -> EntryId {
        let id = generate_entry_id(&self._existing_ids());
        let entry = SessionEntry::ThinkingLevelChange(ThinkingLevelChangeEntryData {
            id: id.clone(),
            parent_id: self.leaf_id.clone(),
            timestamp: now_timestamp(),
            thinking_level: thinking_level.into(),
        });
        self.append_entry(entry);
        id
    }

    /// Append a model change. Returns the entry ID.
    pub fn append_model_change(&mut self, provider: impl Into<String>, model_id: impl Into<String>) -> EntryId {
        let id = generate_entry_id(&self._existing_ids());
        let entry = SessionEntry::ModelChange(ModelChangeEntryData {
            id: id.clone(),
            parent_id: self.leaf_id.clone(),
            timestamp: now_timestamp(),
            provider: provider.into(),
            model_id: model_id.into(),
        });
        self.append_entry(entry);
        id
    }

    /// Append a label entry. Throws if `target_id` does not exist.
    ///
    /// Pass `None` or empty string to clear the label.
    pub fn append_label(
        &mut self,
        target_id: impl Into<EntryId>,
        label: Option<&str>,
    ) -> Result<EntryId, SessionError> {
        let target_id = target_id.into();
        if !self.by_id.contains_key(&target_id) {
            return Err(SessionError::EntryNotFound(target_id));
        }

        let id = generate_entry_id(&self._existing_ids());
        let trimmed = label.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());

        let entry = SessionEntry::Label(LabelEntryData {
            id: id.clone(),
            parent_id: self.leaf_id.clone(),
            timestamp: now_timestamp(),
            target_id: target_id.clone(),
            label: trimmed.clone(),
        });
        self._index_entry(entry);

        // Update label cache
        match trimmed {
            Some(l) => {
                self.labels_by_id.insert(target_id.clone(), l);
            }
            None => {
                self.labels_by_id.remove(&target_id);
            }
        }

        Ok(id)
    }

    /// Append a branch summary entry with an automatic leaf move.
    ///
    /// Moves the leaf to `branch_from_id` (or `None` to reset to before
    /// the first entry), then appends the summary entry.
    ///
    /// Mirrors `branchWithSummary()` in the TS source.
    pub fn branch_with_summary(
        &mut self,
        branch_from_id: Option<&str>,
        summary: impl Into<String>,
    ) -> Result<EntryId, SessionError> {
        if let Some(bid) = branch_from_id {
            if !self.by_id.contains_key(bid) {
                return Err(SessionError::EntryNotFound(bid.to_string()));
            }
        }
        self.leaf_id = branch_from_id.map(|s| s.to_string());

        let id = generate_entry_id(&self._existing_ids());
        let entry = SessionEntry::BranchSummary(BranchSummaryEntryData {
            id: id.clone(),
            parent_id: self.leaf_id.clone(),
            timestamp: now_timestamp(),
            from_id: branch_from_id.unwrap_or("root").to_string(),
            summary: summary.into(),
            details: None,
            from_hook: None,
        });
        self.append_entry(entry);
        Ok(id)
    }

    /// Append a custom entry (extension data, not sent to LLM).
    pub fn append_custom(&mut self, custom_type: impl Into<String>, data: Option<serde_json::Value>) -> EntryId {
        let id = generate_entry_id(&self._existing_ids());
        let entry = SessionEntry::Custom(CustomEntryData {
            id: id.clone(),
            parent_id: self.leaf_id.clone(),
            timestamp: now_timestamp(),
            custom_type: custom_type.into(),
            data,
        });
        self.append_entry(entry);
        id
    }

    /// Append a custom message entry (extension data, sent to LLM).
    #[allow(clippy::too_many_arguments)]
    pub fn append_custom_message(
        &mut self,
        custom_type: impl Into<String>,
        content: serde_json::Value,
        display: bool,
        details: Option<serde_json::Value>,
    ) -> EntryId {
        let id = generate_entry_id(&self._existing_ids());
        let entry = SessionEntry::CustomMessage(CustomMessageEntryData {
            id: id.clone(),
            parent_id: self.leaf_id.clone(),
            timestamp: now_timestamp(),
            custom_type: custom_type.into(),
            content,
            display,
            details,
        });
        self.append_entry(entry);
        id
    }

    /// Append a session info entry (display name).
    pub fn append_session_info(&mut self, name: impl Into<String>) -> EntryId {
        let id = generate_entry_id(&self._existing_ids());
        let entry = SessionEntry::SessionInfo(SessionInfoEntryData {
            id: id.clone(),
            parent_id: self.leaf_id.clone(),
            timestamp: now_timestamp(),
            name: Some(name.into()),
        });
        self.append_entry(entry);
        id
    }

    /// Append a branch_summary entry at the current leaf position (without moving the leaf).
    /// This is a convenience wrapper for `branch_with_summary` that doesn't move the leaf first.
    pub fn append_branch_summary(&mut self, from_id: impl Into<String>, summary: impl Into<String>) -> EntryId {
        let id = generate_entry_id(&self._existing_ids());
        let entry = SessionEntry::BranchSummary(BranchSummaryEntryData {
            id: id.clone(),
            parent_id: self.leaf_id.clone(),
            timestamp: now_timestamp(),
            from_id: from_id.into(),
            summary: summary.into(),
            details: None,
            from_hook: None,
        });
        self.append_entry(entry);
        id
    }

    // ── Branching ───────────────────────────────────────────────────────

    /// Move the leaf pointer to an existing entry.
    ///
    /// The next `append_*()` call will create a child of that entry, forming
    /// a new branch. Existing entries are not modified or deleted.
    ///
    /// Mirrors `branch()` in the TS source.
    pub fn branch(&mut self, target_id: &str) -> Result<(), SessionError> {
        if target_id.is_empty() {
            self.leaf_id = None;
            return Ok(());
        }
        if !self.by_id.contains_key(target_id) {
            return Err(SessionError::EntryNotFound(target_id.to_string()));
        }
        self.leaf_id = Some(target_id.to_string());
        Ok(())
    }

    /// Reset the leaf pointer to `None` (before any entry).
    ///
    /// Mirrors `resetLeaf()` in the TS source.
    pub fn reset_leaf(&mut self) {
        self.leaf_id = None;
    }

    // ── Context building ────────────────────────────────────────────────

    /// Build the session context (what gets sent to the LLM).
    ///
    /// Walks from the current leaf to root, resolving messages and handling:
    /// - Compaction summaries (insert summary message, include kept entries)
    /// - Thinking level changes (track current level)
    /// - Model changes (track current model)
    /// - Branch summaries (insert as text messages)
    /// - Custom messages (insert as text messages)
    ///
    /// Mirrors `buildSessionContext()` from session-manager.ts.
    pub fn build_context(&self) -> SessionContext {
        let path = self.path_to_root(None);
        if path.is_empty() {
            return SessionContext::default();
        }

        let mut thinking_level = "off".to_string();
        let mut model: Option<(String, String)> = None;
        let mut compaction: Option<(&CompactionEntryData, usize)> = None;

        for (i, entry) in path.iter().enumerate() {
            match entry {
                SessionEntry::ThinkingLevelChange(e) => {
                    thinking_level = e.thinking_level.clone();
                }
                SessionEntry::ModelChange(e) => {
                    model = Some((e.provider.clone(), e.model_id.clone()));
                }
                SessionEntry::Message(e) => {
                    if let Some(role) = e.message.get("role").and_then(|r| r.as_str()) {
                        if role == "assistant" {
                            if let (Some(provider), Some(mid)) = (
                                e.message.get("provider").and_then(|v| v.as_str()),
                                e.message.get("model").and_then(|v| v.as_str()),
                            ) {
                                model = Some((provider.to_string(), mid.to_string()));
                            }
                        }
                    }
                }
                SessionEntry::Compaction(e) => {
                    compaction = Some((e, i));
                }
                _ => {}
            }
        }

        // Build messages list
        let mut messages: Vec<Message> = Vec::new();

        match compaction {
            Some((comp, comp_idx)) => {
                // 1. Add compaction summary message
                messages.push(Message::user_text(format!(
                    "[Compaction: {} tokens compacted] {}",
                    comp.tokens_before, comp.summary
                )));

                // 2. Emit kept entries before compaction (from firstKeptEntryId to compaction)
                let mut found_first_kept = false;
                for entry in path.iter().take(comp_idx) {
                    if entry.id() == comp.first_kept_entry_id {
                        found_first_kept = true;
                    }
                    if found_first_kept {
                        append_entry_to_messages(entry, &mut messages);
                    }
                }

                // 3. Emit entries after compaction
                for entry in path.iter().skip(comp_idx + 1) {
                    append_entry_to_messages(entry, &mut messages);
                }
            }
            None => {
                // No compaction — emit all entries
                for entry in &path {
                    append_entry_to_messages(entry, &mut messages);
                }
            }
        }

        SessionContext { messages, thinking_level, model }
    }

    // ── Internal helpers ─────────────────────────────────────────────────

    /// Index a single entry: add to `entries` vec, update `by_id`, update
    /// leaf pointer, update label cache.
    fn _index_entry(&mut self, entry: SessionEntry) {
        let id = entry.id().to_string();

        // Update label cache
        if let SessionEntry::Label(label) = &entry {
            if let Some(ref l) = label.label {
                let trimmed = l.trim();
                if !trimmed.is_empty() {
                    self.labels_by_id.insert(label.target_id.clone(), trimmed.to_string());
                } else {
                    self.labels_by_id.remove(&label.target_id);
                }
            } else {
                self.labels_by_id.remove(&label.target_id);
            }
        }

        self.entries.push(entry.clone());
        self.by_id.insert(id.clone(), self.entries.len() - 1);
        self.leaf_id = Some(id);
    }

    /// Build a `HashSet` of all existing entry IDs (for collision checking
    /// during ID generation).
    fn _existing_ids(&self) -> HashSet<String> {
        self.by_id.keys().cloned().collect()
    }
}

// ── Helper functions ───────────────────────────────────────────────────────

/// Convert a session entry into one or more `Message` values and append
/// them to the given vector.
fn append_entry_to_messages(entry: &SessionEntry, messages: &mut Vec<Message>) {
    match entry {
        SessionEntry::Message(e) => {
            if let Ok(msg) = serde_json::from_value::<Message>(e.message.clone()) {
                messages.push(msg);
            }
        }
        SessionEntry::BranchSummary(e) => {
            if !e.summary.is_empty() {
                messages.push(Message::user_text(format!("[Branch summary from {}] {}", e.from_id, e.summary)));
            }
        }
        SessionEntry::CustomMessage(e) => {
            // Inject custom message content as a user message
            let text = match &e.content {
                serde_json::Value::String(s) => s.clone(),
                _ => e.content.to_string(),
            };
            messages.push(Message::user_text(format!("[{}] {}", e.custom_type, text)));
        }
        _ => {
            // Compaction, ThinkingLevelChange, ModelChange, Label, Custom,
            // and SessionInfo are not included as messages
        }
    }
}

/// Sort tree nodes' children by timestamp (oldest first), recursively.
fn sort_tree_nodes(nodes: &mut [SessionTreeNode]) {
    nodes.sort_by(|a, b| a.entry.timestamp().cmp(b.entry.timestamp()));
    for node in nodes.iter_mut() {
        sort_tree_nodes(&mut node.children);
    }
}

/// Extension trait to get the variant name of a `SessionEntry`.
trait SessionEntryVariant {
    fn variant_name(&self) -> &'static str;
}

impl SessionEntryVariant for SessionEntry {
    fn variant_name(&self) -> &'static str {
        match self {
            Self::Message(_) => "message",
            Self::Compaction(_) => "compaction",
            Self::ThinkingLevelChange(_) => "thinking_level_change",
            Self::ModelChange(_) => "model_change",
            Self::BranchSummary(_) => "branch_summary",
            Self::Label(_) => "label",
            Self::Custom(_) => "custom",
            Self::CustomMessage(_) => "custom_message",
            Self::SessionInfo(_) => "session_info",
        }
    }
}

/// Get the latest compaction entry from a slice of entries.
///
/// Mirrors `getLatestCompactionEntry()` in the TS source.
pub fn get_latest_compaction_entry(entries: &[SessionEntry]) -> Option<&CompactionEntryData> {
    for entry in entries.iter().rev() {
        if let SessionEntry::Compaction(comp) = entry {
            return Some(comp);
        }
    }
    None
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use pi_ai_core::types::ContentBlock;

    fn make_msg_entry(id: &str, parent_id: Option<&str>, text: &str, role: &str) -> SessionEntry {
        SessionEntry::Message(MessageEntryData {
            id: id.to_string(),
            parent_id: parent_id.map(|s| s.to_string()),
            timestamp: now_timestamp(),
            message: serde_json::json!({
                "role": role,
                "content": [{"type": "text", "text": text}]
            }),
        })
    }

    fn make_compaction_entry(
        id: &str,
        parent_id: Option<&str>,
        summary: &str,
        first_kept: &str,
        tokens_before: u64,
    ) -> SessionEntry {
        SessionEntry::Compaction(CompactionEntryData {
            id: id.to_string(),
            parent_id: parent_id.map(|s| s.to_string()),
            timestamp: now_timestamp(),
            summary: summary.to_string(),
            first_kept_entry_id: first_kept.to_string(),
            tokens_before,
            details: None,
            from_hook: None,
        })
    }

    #[test]
    fn test_append_and_path_to_root() {
        let mut sm = SessionManager::in_memory("/tmp");
        assert!(sm.is_empty());

        let m1 = sm.append_message(serde_json::json!({"role": "user", "content": [{"type": "text", "text": "hello"}]}));
        let m2 =
            sm.append_message(serde_json::json!({"role": "assistant", "content": [{"type": "text", "text": "hi"}]}));
        let m3 = sm.append_message(serde_json::json!({"role": "user", "content": [{"type": "text", "text": "again"}]}));

        assert_eq!(sm.len(), 3);

        // Path from leaf to root should be [m1, m2, m3]
        let path = sm.path_to_root(None);
        assert_eq!(path.len(), 3);
        assert_eq!(path[0].id(), m1);
        assert_eq!(path[1].id(), m2);
        assert_eq!(path[2].id(), m3);

        // Path from m2 to root should be [m1, m2]
        let path = sm.path_to_root(Some(&m2));
        assert_eq!(path.len(), 2);
        assert_eq!(path[0].id(), m1);
        assert_eq!(path[1].id(), m2);
    }

    #[test]
    fn test_append_sets_parent_id() {
        let mut sm = SessionManager::in_memory("/tmp");
        let m1 = sm.append_message(serde_json::json!({"role": "user", "content": []}));
        let m2 = sm.append_message(serde_json::json!({"role": "assistant", "content": []}));

        let e1 = sm.get_entry(&m1).unwrap();
        assert_eq!(e1.parent_id(), None);

        let e2 = sm.get_entry(&m2).unwrap();
        assert_eq!(e2.parent_id(), Some(m1.as_str()));
    }

    #[test]
    fn test_leaf_id_updates() {
        let mut sm = SessionManager::in_memory("/tmp");
        assert_eq!(sm.leaf_id(), None);

        let id = sm.append_message(serde_json::json!({"role": "user", "content": []}));
        assert_eq!(sm.leaf_id(), Some(&id));
    }

    #[test]
    fn test_branching() {
        let mut sm = SessionManager::in_memory("/tmp");

        // Linear chain: m1 -> m2 -> m3
        let m1 = sm.append_message(serde_json::json!({"role": "user", "content": []}));
        let m2 = sm.append_message(serde_json::json!({"role": "assistant", "content": []}));
        let _m3 = sm.append_message(serde_json::json!({"role": "user", "content": []}));

        // Branch: jump back to m2
        sm.branch(&m2).unwrap();
        assert_eq!(sm.leaf_id(), Some(&m2));

        // Append under m2 (forming a branch)
        let m4 = sm.append_message(serde_json::json!({"role": "assistant", "content": []}));

        // Path should be [m1, m2, m4]
        let path = sm.path_to_root(None);
        assert_eq!(path.len(), 3);
        assert_eq!(path[0].id(), m1);
        assert_eq!(path[1].id(), m2);
        assert_eq!(path[2].id(), m4);

        // m4's parent should be m2
        assert_eq!(sm.get_entry(&m4).unwrap().parent_id(), Some(m2.as_str()));
    }

    #[test]
    fn test_branch_to_nonexistent_entry() {
        let mut sm = SessionManager::in_memory("/tmp");
        let result = sm.branch("nonexistent");
        assert!(result.is_err());
    }

    #[test]
    fn test_reset_leaf() {
        let mut sm = SessionManager::in_memory("/tmp");
        sm.append_message(serde_json::json!({"role": "user", "content": []}));
        sm.reset_leaf();
        assert_eq!(sm.leaf_id(), None);

        // Appending after reset creates a root entry (parent_id = None)
        let id = sm.append_message(serde_json::json!({"role": "user", "content": []}));
        assert_eq!(sm.get_entry(&id).unwrap().parent_id(), None);
    }

    #[test]
    fn test_get_children() {
        let mut sm = SessionManager::in_memory("/tmp");

        let m1 = sm.append_message(serde_json::json!({"role": "user", "content": []}));
        let _m2 = sm.append_message(serde_json::json!({"role": "assistant", "content": []}));

        // Branch back to m1 and append another child
        sm.branch(&m1).unwrap();
        let m3 = sm.append_message(serde_json::json!({"role": "user", "content": []}));

        let children = sm.get_children(&m1);
        assert_eq!(children.len(), 2);
        let child_ids: Vec<&str> = children.iter().map(|e| e.id()).collect();
        assert!(child_ids.contains(&m3.as_str()));
    }

    #[test]
    fn test_labels() {
        let mut sm = SessionManager::in_memory("/tmp");
        let m1 = sm.append_message(serde_json::json!({"role": "user", "content": []}));
        let _m2 = sm.append_message(serde_json::json!({"role": "assistant", "content": []}));

        // Add label
        sm.append_label(&m1, Some("important")).unwrap();
        assert_eq!(sm.get_label(&m1), Some("important"));

        // Clear label
        sm.append_label(&m1, None).unwrap();
        assert_eq!(sm.get_label(&m1), None);

        // Label non-existent entry
        let result = sm.append_label("nonexistent", Some("x"));
        assert!(result.is_err());
    }

    #[test]
    fn test_label_on_nonexistent_target() {
        let mut sm = SessionManager::in_memory("/tmp");
        let result = sm.append_label("does-not-exist", Some("test"));
        assert!(result.is_err());
    }

    #[test]
    fn test_build_context_linear() {
        let mut sm = SessionManager::in_memory("/tmp");
        sm.append_message(serde_json::json!({"role": "user", "content": [{"type": "text", "text": "hello"}]}));
        sm.append_message(serde_json::json!({"role": "assistant", "content": [{"type": "text", "text": "world"}]}));

        let ctx = sm.build_context();
        assert_eq!(ctx.messages.len(), 2);
        assert_eq!(ctx.thinking_level, "off");
    }

    #[test]
    fn test_build_context_with_thinking_and_model() {
        let mut sm = SessionManager::in_memory("/tmp");
        sm.append_message(serde_json::json!({"role": "user", "content": []}));
        sm.append_thinking_level_change("high");
        sm.append_model_change("anthropic", "claude-3-opus");
        sm.append_message(
            serde_json::json!({"role": "assistant", "content": [], "provider": "anthropic", "model": "claude-3-opus"}),
        );

        let ctx = sm.build_context();
        assert_eq!(ctx.thinking_level, "high");
        assert_eq!(ctx.model, Some(("anthropic".to_string(), "claude-3-opus".to_string())));
    }

    #[test]
    fn test_compaction_handling() {
        let mut sm = SessionManager::in_memory("/tmp");

        let _m1 =
            sm.append_message(serde_json::json!({"role": "user", "content": [{"type": "text", "text": "first"}]}));
        let m2 = sm
            .append_message(serde_json::json!({"role": "assistant", "content": [{"type": "text", "text": "second"}]}));
        let _m3 =
            sm.append_message(serde_json::json!({"role": "user", "content": [{"type": "text", "text": "third"}]}));

        // Compact from m2 onward
        sm.append_compaction("summarized", &m2, 100);

        let _m4 = sm
            .append_message(serde_json::json!({"role": "assistant", "content": [{"type": "text", "text": "fourth"}]}));

        let ctx = sm.build_context();
        // Should have: compaction summary message (1), kept entries from m2 (2: m2, m3), post-compaction (1: m4) = 4 messages
        assert_eq!(ctx.messages.len(), 4, "compaction + m2 + m3 + m4 = 4 messages");
        // First message should be the compaction summary
        match &ctx.messages[0].content[0] {
            ContentBlock::Text(t) => assert!(t.text.contains("summarized")),
            _ => panic!("expected Text content in first message"),
        }
    }

    #[test]
    fn test_build_context_no_messages() {
        let sm = SessionManager::in_memory("/tmp");
        let ctx = sm.build_context();
        assert!(ctx.messages.is_empty());
        assert_eq!(ctx.thinking_level, "off");
    }

    #[test]
    fn test_get_tree_structure() {
        let mut sm = SessionManager::in_memory("/tmp");

        let m1 = sm.append_message(serde_json::json!({"role": "user", "content": []}));
        let m2 = sm.append_message(serde_json::json!({"role": "assistant", "content": []}));
        sm.branch(&m1).unwrap();
        let m3 = sm.append_message(serde_json::json!({"role": "user", "content": []}));

        let tree = sm.get_tree();
        // m1 should be root with two children: m2 and m3
        assert_eq!(tree.len(), 1);
        assert_eq!(tree[0].entry.id(), m1);
        assert_eq!(tree[0].children.len(), 2);

        let child_ids: HashSet<&str> = tree[0].children.iter().map(|n| n.entry.id()).collect();
        assert!(child_ids.contains(m2.as_str()));
        assert!(child_ids.contains(m3.as_str()));
    }

    #[test]
    fn test_get_session_name() {
        let mut sm = SessionManager::in_memory("/tmp");
        assert!(sm.get_session_name().is_none());

        sm.append_session_info("My Session");
        assert_eq!(sm.get_session_name(), Some("My Session"));

        // Later session_info overrides
        sm.append_session_info("Renamed");
        assert_eq!(sm.get_session_name(), Some("Renamed"));
    }

    #[test]
    fn test_entry_id_collision_generation() {
        let mut existing: HashSet<String> = HashSet::new();
        // Fill with many short IDs to potentially trigger collision
        for i in 0..1000 {
            existing.insert(format!("{:08x}", i));
        }
        // This should still generate a valid ID (the fallback is a full UUID)
        let id = generate_entry_id(&existing);
        assert!(!id.is_empty());
    }

    #[test]
    fn test_branch_with_summary() {
        let mut sm = SessionManager::in_memory("/tmp");
        let m1 = sm.append_message(serde_json::json!({"role": "user", "content": []}));
        sm.append_message(serde_json::json!({"role": "assistant", "content": []}));

        // Branch with summary back to m1
        let summary_id = sm.branch_with_summary(Some(&m1), "Changed approach").unwrap();

        let ctx = sm.build_context();
        assert!(ctx.messages.iter().any(|m| {
            m.content.iter().any(|block| {
                if let ContentBlock::Text(t) = block {
                    t.text.contains("Changed approach") || t.text.contains("summary")
                } else {
                    false
                }
            })
        }));

        // Summary entry should be a child of m1
        let summary_entry = sm.get_entry(&summary_id).unwrap();
        assert_eq!(summary_entry.parent_id(), Some(m1.as_str()));
    }

    #[test]
    fn test_branch_with_summary_nonexistent() {
        let mut sm = SessionManager::in_memory("/tmp");
        let result = sm.branch_with_summary(Some("nonexistent"), "summary");
        assert!(result.is_err());
    }

    #[test]
    fn test_get_latest_compaction() {
        let entries: Vec<SessionEntry> = vec![
            make_msg_entry("1", None, "hello", "user"),
            make_compaction_entry("2", Some("1"), "compacted", "1", 50),
            make_msg_entry("3", Some("2"), "world", "user"),
            make_compaction_entry("4", Some("3"), "more compacted", "3", 100),
        ];

        let latest = get_latest_compaction_entry(&entries);
        assert!(latest.is_some());
        assert_eq!(latest.unwrap().summary, "more compacted");
    }

    #[test]
    fn test_append_compaction_entry() {
        let mut sm = SessionManager::in_memory("/tmp");
        let m1 = sm.append_message(serde_json::json!({"role": "user", "content": []}));
        let id = sm.append_compaction("test summary", &m1, 500);
        let entry = sm.get_entry(&id).unwrap();
        match entry {
            SessionEntry::Compaction(c) => {
                assert_eq!(c.summary, "test summary");
                assert_eq!(c.first_kept_entry_id, m1);
                assert_eq!(c.tokens_before, 500);
            }
            _ => panic!("expected Compaction entry"),
        }
    }

    #[test]
    fn test_append_thinking_and_model() {
        let mut sm = SessionManager::in_memory("/tmp");
        let tid = sm.append_thinking_level_change("high");
        let mid = sm.append_model_change("openai", "gpt-4");

        let t_entry = sm.get_entry(&tid).unwrap();
        let m_entry = sm.get_entry(&mid).unwrap();

        match t_entry {
            SessionEntry::ThinkingLevelChange(t) => assert_eq!(t.thinking_level, "high"),
            _ => panic!("expected ThinkingLevelChange"),
        }
        match m_entry {
            SessionEntry::ModelChange(m) => {
                assert_eq!(m.provider, "openai");
                assert_eq!(m.model_id, "gpt-4");
            }
            _ => panic!("expected ModelChange"),
        }
    }

    #[test]
    fn test_append_custom_entries() {
        let mut sm = SessionManager::in_memory("/tmp");
        let cid = sm.append_custom("my-ext", Some(serde_json::json!({"key": "val"})));
        let cmid = sm.append_custom_message("my-ext", serde_json::json!("hello"), true, None);

        match sm.get_entry(&cid).unwrap() {
            SessionEntry::Custom(c) => {
                assert_eq!(c.custom_type, "my-ext");
                assert_eq!(c.data.as_ref().and_then(|d| d.get("key")).and_then(|v| v.as_str()), Some("val"));
            }
            _ => panic!("expected Custom"),
        }

        match sm.get_entry(&cmid).unwrap() {
            SessionEntry::CustomMessage(cm) => {
                assert_eq!(cm.custom_type, "my-ext");
                assert!(cm.display);
            }
            _ => panic!("expected CustomMessage"),
        }
    }

    #[test]
    fn test_tree_sorting() {
        let mut sm = SessionManager::in_memory("/tmp");
        let m1 = sm.append_message(serde_json::json!({"role": "user", "content": []}));
        sm.branch(&m1).unwrap();

        // Create entries with explicit timestamps by manipulating the internal entries
        // Use append which auto-generates timestamps (they'll be in order already).

        // Since timestamps are auto-generated in order, children should already be sorted.
        let m2 = sm.append_message(serde_json::json!({"role": "user", "content": []}));
        sm.branch(&m1).unwrap();
        let m3 = sm.append_message(serde_json::json!({"role": "assistant", "content": []}));

        let tree = sm.get_tree();
        assert_eq!(tree.len(), 1, "should have 1 root");
        assert_eq!(tree[0].children.len(), 2, "root should have 2 children");
        // Children should be sorted by timestamp: first added first
        assert_eq!(tree[0].children[0].entry.id(), m2);
        assert_eq!(tree[0].children[1].entry.id(), m3);
    }

    #[test]
    fn test_from_entries() {
        let header = SessionHeader::new("/tmp", "test-session".to_string());
        let entries =
            vec![make_msg_entry("a", None, "hello", "user"), make_msg_entry("b", Some("a"), "world", "assistant")];

        let sm = SessionManager::from_entries(header, entries);
        assert_eq!(sm.len(), 2);
        assert_eq!(sm.leaf_id(), Some(&"b".to_string()));
        assert!(sm.get_entry("a").is_some());
        assert!(sm.get_entry("b").is_some());
    }

    #[test]
    fn test_find_entries() {
        let mut sm = SessionManager::in_memory("/tmp");
        sm.append_message(serde_json::json!({"role": "user", "content": []}));
        sm.append_thinking_level_change("high");
        sm.append_message(serde_json::json!({"role": "assistant", "content": []}));

        let messages = sm.find_entries("message");
        assert_eq!(messages.len(), 2);

        let tlc = sm.find_entries("thinking_level_change");
        assert_eq!(tlc.len(), 1);
    }

    #[test]
    fn test_append_branch_summary() {
        let mut sm = SessionManager::in_memory("/tmp");
        let m1 = sm.append_message(serde_json::json!({"role": "user", "content": []}));
        let id = sm.append_branch_summary(&m1, "switched path");

        let entry = sm.get_entry(&id).unwrap();
        match entry {
            SessionEntry::BranchSummary(b) => {
                assert_eq!(b.from_id, m1);
                assert_eq!(b.summary, "switched path");
            }
            _ => panic!("expected BranchSummary"),
        }
    }
}
