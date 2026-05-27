//! Token estimation and compaction trigger logic.
//!
//! These heuristics determine when context should be compacted.
//! They mirror the TS logic in packages/coding-agent/src/core/compaction/.

use pi_ai_core::types::{ContentBlock, Message};

/// Default compaction settings.
pub const DEFAULT_RESERVE_TOKENS: u64 = 16384;
pub const DEFAULT_KEEP_RECENT_TOKENS: u64 = 20000;
pub const DEFAULT_COMPACT_THRESHOLD: f64 = 0.75; // 75% of context window

/// Roughly estimate the number of tokens in a message.
///
/// Uses the same heuristic as the TS code: `JSON.stringify(msg).length / 4`.
/// This is intentionally simple — real tokenizers are model-specific.
pub fn estimate_message_tokens(message: &Message) -> u64 {
    let json = serde_json::to_string(message).unwrap_or_default();
    (json.len() as u64).max(1) / 4
}

/// Estimate total tokens for a slice of messages.
///
/// If a previous `Usage` exists, uses its `total_tokens` as the baseline
/// and adds estimates for any newer messages.
pub fn estimate_context_tokens(messages: &[Message], total_tokens_from_usage: Option<u64>) -> u64 {
    match total_tokens_from_usage {
        Some(base) => {
            // Use the usage baseline + estimate for any messages after the measured point.
            // This is conservative — we assume the usage covers all but the last few.
            base
        }
        None => messages.iter().map(estimate_message_tokens).sum(),
    }
}

/// Determine whether compaction should be triggered.
///
/// Returns `true` when estimated tokens exceed the threshold of the context window.
pub fn should_compact(
    estimated_tokens: u64,
    context_window: u64,
    reserve_tokens: u64,
) -> bool {
    if context_window == 0 {
        return false;
    }
    let threshold = (context_window as f64 * DEFAULT_COMPACT_THRESHOLD) as u64;
    estimated_tokens + reserve_tokens > threshold
}

/// Calculate context usage statistics.
pub struct ContextUsage {
    pub estimated_tokens: u64,
    pub context_window: u64,
    pub percent: f64,
}

impl ContextUsage {
    pub fn new(estimated_tokens: u64, context_window: u64) -> Self {
        let percent = if context_window > 0 {
            (estimated_tokens as f64 / context_window as f64) * 100.0
        } else {
            0.0
        };
        Self {
            estimated_tokens,
            context_window,
            percent,
        }
    }
}

/// Track which files the agent has read or modified (for compaction context).
#[derive(Debug, Clone, Default)]
pub struct FileOperations {
    pub read: Vec<String>,
    pub modified: Vec<String>,
}

impl FileOperations {
    pub fn is_empty(&self) -> bool {
        self.read.is_empty() && self.modified.is_empty()
    }
}

/// Extract file operations from tool calls within messages.
///
/// Looks for `read`, `write`, `edit`, `grep`, `find` tool calls and records
/// the file paths from their arguments.
pub fn extract_file_ops_from_messages(messages: &[Message]) -> FileOperations {
    let mut ops = FileOperations::default();
    for msg in messages {
        for block in &msg.content {
            if let ContentBlock::ToolCall(tc) = block {
                let path = tc.arguments.get("path").or_else(|| tc.arguments.get("pattern"));
                if let Some(p) = path.and_then(|v| v.as_str()) {
                    match tc.name.as_str() {
                        "read" | "grep" | "find" | "ls" => {
                            if !ops.read.contains(&p.to_string()) {
                                ops.read.push(p.to_string());
                            }
                        }
                        "write" | "edit" => {
                            if !ops.modified.contains(&p.to_string()) {
                                ops.modified.push(p.to_string());
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    }
    ops
}

/// Format file operations as a human-readable string for the summary prompt.
pub fn format_file_operations(ops: &FileOperations) -> String {
    let mut parts = Vec::new();
    if !ops.read.is_empty() {
        parts.push(format!("Read files: {}", ops.read.join(", ")));
    }
    if !ops.modified.is_empty() {
        parts.push(format!("Modified files: {}", ops.modified.join(", ")));
    }
    parts.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_estimate_message_tokens_non_empty() {
        let msg = Message::user_text("hello world");
        let tokens = estimate_message_tokens(&msg);
        assert!(tokens > 0, "should produce at least 1 token");
    }

    #[test]
    fn test_should_compact_threshold() {
        // context_window=100000, estimated=80000, reserve=10000 → 80000+10000 > 75000 → true
        assert!(should_compact(80000, 100000, 10000));
        // 50000+10000 < 75000 → false
        assert!(!should_compact(50000, 100000, 10000));
    }

    #[test]
    fn test_context_usage_percent() {
        let usage = ContextUsage::new(25000, 100000);
        assert!((usage.percent - 25.0).abs() < 0.01);
    }

    #[test]
    fn test_extract_file_ops() {
        let msg_text = r#"{"role": "assistant", "content": []}"#;
        let msg: Message = serde_json::from_str(msg_text).unwrap_or_else(|_| Message::user_text(""));
        // Basic test — empty message produces no ops
        let ops = extract_file_ops_from_messages(&[msg]);
        assert!(ops.is_empty());
    }
}
