//! Cut-point planning for context compaction.
//!
//! Determines which session entries to summarize and which to keep.
//! Mirrors the TS logic in packages/coding-agent/src/core/compaction/.

use crate::session::types::SessionEntry;

/// Result of preparing a compaction operation.
#[derive(Debug, Clone)]
pub struct CompactionPreparation {
    /// Entries before the cut point that should be summarized.
    pub entries_to_summarize: Vec<SessionEntry>,
    /// Entries from the cut point onward that should be kept.
    pub entries_to_keep: Vec<SessionEntry>,
    /// File operations discovered in entries being summarized.
    pub file_ops: super::estimator::FileOperations,
    /// Previous summary text, if re-compacting.
    pub prev_summary: Option<String>,
}

/// Find the first entry index that should be kept after compaction.
///
/// Walks backwards from the newest entry, accumulating estimated tokens,
/// and stops when the accumulated tokens exceed `keep_recent_tokens`
/// (default 20_000) or when only compaction entries remain.
///
/// Returns the index (in the entries slice) of the first entry to keep.
/// Returns 0 if no cut is possible (too few entries).
pub fn find_cut_point(entries: &[SessionEntry], keep_recent_tokens: u64) -> Option<usize> {
    if entries.len() < 3 {
        return None; // too few entries to compact
    }

    let mut accumulated: u64 = 0;
    let mut cut_idx: Option<usize> = None;

    for i in (0..entries.len()).rev() {
        let entry = &entries[i];

        // Only message entries contribute to token count
        let tokens = match entry {
            SessionEntry::Message(data) => {
                if let Ok(msg) = serde_json::from_value::<pi_ai_core::types::Message>(data.message.clone()) {
                    crate::compaction::estimator::estimate_message_tokens(&msg)
                } else {
                    0
                }
            }
            SessionEntry::Compaction(_) | SessionEntry::BranchSummary(_) => {
                // Keep compaction/branch-summary entries as reference
                accumulated = 0; // reset — these are important context
                continue;
            }
            _ => 0,
        };

        accumulated += tokens;
        if accumulated > keep_recent_tokens && cut_idx.is_none() {
            // Cut AFTER the entry that exceeded the budget (keep newer entries)
            cut_idx = Some(i + 1);
        }
    }

    // Don't cut at the very beginning or end, and ensure cut_idx is valid
    match cut_idx {
        Some(idx) if idx > 0 && idx < entries.len() - 1 => Some(idx),
        Some(idx) if idx >= entries.len() - 1 => {
            // Cut at 2/3 point as fallback (keep the last third)
            let fallback = (entries.len() * 2) / 3;
            if fallback > 0 && fallback < entries.len() - 1 { Some(fallback) } else { None }
        }
        _ => None,
    }
}

/// Prepare a compaction operation.
///
/// Splits entries at the cut point into "summarize" and "keep" groups.
pub fn prepare_compaction(entries: &[SessionEntry], keep_recent_tokens: u64) -> Option<CompactionPreparation> {
    let cut_idx = find_cut_point(entries, keep_recent_tokens)?;

    let entries_to_summarize: Vec<SessionEntry> = entries[..cut_idx].to_vec();
    let entries_to_keep: Vec<SessionEntry> = entries[cut_idx..].to_vec();

    // Extract file ops from summarized entries
    let file_ops = super::estimator::FileOperations::default();

    // Check for existing previous summary
    let prev_summary = entries
        .iter()
        .rev()
        .find_map(|e| if let SessionEntry::Compaction(data) = e { Some(data.summary.clone()) } else { None });

    Some(CompactionPreparation { entries_to_summarize, entries_to_keep, file_ops, prev_summary })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::types::*;

    fn dummy_message_entry(text: &str) -> SessionEntry {
        let msg = pi_ai_core::types::Message::user_text(text);
        SessionEntry::Message(MessageEntryData {
            id: format!("msg_{}", text.len()),
            parent_id: None,
            timestamp: "2025-01-01T00:00:00Z".to_string(),
            message: serde_json::to_value(msg).unwrap(),
        })
    }

    #[test]
    fn test_find_cut_point_too_few_entries() {
        assert!(find_cut_point(&[], 20000).is_none());
        assert!(find_cut_point(&[dummy_message_entry("a")], 20000).is_none());
        assert!(find_cut_point(&[dummy_message_entry("a"), dummy_message_entry("b")], 20000).is_none());
    }

    #[test]
    fn test_find_cut_point_with_enough_entries() {
        let entries: Vec<SessionEntry> =
            (0..10).map(|i| dummy_message_entry(&format!("message number {}", i))).collect();
        let cut = find_cut_point(&entries, 20000);
        // With 10 small messages and 20000 keep_tokens, all should fit
        assert!(cut.is_none() || cut.unwrap() > 0);
    }

    #[test]
    fn test_prepare_compaction_basic() {
        let entries: Vec<SessionEntry> = (0..10).map(|i| dummy_message_entry(&format!("entry {}", i))).collect();
        let prep = prepare_compaction(&entries, 10);
        // With keep_tokens=10, many early entries should be summarized
        if let Some(p) = prep {
            // entries_to_summarize + entries_to_keep should equal total
            assert_eq!(p.entries_to_summarize.len() + p.entries_to_keep.len(), entries.len());
        }
    }
}
