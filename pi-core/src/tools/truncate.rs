//! Output truncation utilities.
//!
//! Truncation is based on two independent limits — whichever is hit first wins:
//! - Line limit (default: 2000 lines)
//! - Byte limit (default: 50 KB)
//!
//! Never returns partial lines (except tail-truncation edge case).

/// Default maximum number of lines before truncation.
pub const DEFAULT_MAX_LINES: usize = 2000;

/// Default maximum number of bytes before truncation.
pub const DEFAULT_MAX_BYTES: usize = 50 * 1024; // 50 KB

/// Maximum characters per grep match line.
pub const GREP_MAX_LINE_LENGTH: usize = 500;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Which limit was hit during truncation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TruncatedBy {
    Lines,
    Bytes,
    None,
}

/// Detailed result of a truncation operation.
#[derive(Debug, Clone)]
pub struct TruncationResult {
    /// The (possibly truncated) content.
    pub content: String,
    /// Whether truncation actually occurred.
    pub truncated: bool,
    /// Which limit was hit.
    pub truncated_by: TruncatedBy,
    /// Total lines in the original content.
    pub total_lines: usize,
    /// Total bytes in the original content.
    pub total_bytes: usize,
    /// Number of complete lines in the output.
    pub output_lines: usize,
    /// Number of bytes in the output.
    pub output_bytes: usize,
    /// Whether the last line was partially truncated (tail-only edge case).
    pub last_line_partial: bool,
    /// Whether the first line alone exceeded the byte limit (head-only).
    pub first_line_exceeds_limit: bool,
    /// The max-lines limit that was applied.
    pub max_lines: usize,
    /// The max-bytes limit that was applied.
    pub max_bytes: usize,
}

/// Options passed to truncation functions.
#[derive(Debug, Clone, Copy)]
pub struct TruncationOptions {
    pub max_lines: usize,
    pub max_bytes: usize,
}

impl Default for TruncationOptions {
    fn default() -> Self {
        Self { max_lines: DEFAULT_MAX_LINES, max_bytes: DEFAULT_MAX_BYTES }
    }
}

// ---------------------------------------------------------------------------
// format_size
// ---------------------------------------------------------------------------

/// Format a byte count as a human-readable string.
pub fn format_size(bytes: usize) -> String {
    if bytes < 1024 {
        format!("{}B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{:.1}KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1}MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

// ---------------------------------------------------------------------------
// truncate_head
// ---------------------------------------------------------------------------

/// Keep the first N lines/bytes from the beginning of `content`.
///
/// Never returns partial lines.  If the first line alone exceeds the byte
/// limit, returns empty content with `first_line_exceeds_limit = true`.
pub fn truncate_head(content: &str, options: TruncationOptions) -> TruncationResult {
    let max_lines = options.max_lines;
    let max_bytes = options.max_bytes;

    let total_bytes = content.len();
    let lines: Vec<&str> = content.split('\n').collect();
    let total_lines = lines.len();

    // No truncation needed?
    if total_lines <= max_lines && total_bytes <= max_bytes {
        return TruncationResult {
            content: content.to_string(),
            truncated: false,
            truncated_by: TruncatedBy::None,
            total_lines,
            total_bytes,
            output_lines: total_lines,
            output_bytes: total_bytes,
            last_line_partial: false,
            first_line_exceeds_limit: false,
            max_lines,
            max_bytes,
        };
    }

    // Check if the first line alone exceeds the byte limit.
    let first_line_bytes = lines.first().map(|l| l.len()).unwrap_or(0);
    if first_line_bytes > max_bytes {
        return TruncationResult {
            content: String::new(),
            truncated: true,
            truncated_by: TruncatedBy::Bytes,
            total_lines,
            total_bytes,
            output_lines: 0,
            output_bytes: 0,
            last_line_partial: false,
            first_line_exceeds_limit: true,
            max_lines,
            max_bytes,
        };
    }

    // Collect complete lines that fit.
    let mut output_lines_vec: Vec<&str> = Vec::new();
    let mut output_bytes_count = 0;
    let mut truncated_by = TruncatedBy::Lines;

    for (i, line) in lines.iter().enumerate() {
        if i >= max_lines {
            truncated_by = TruncatedBy::Lines;
            break;
        }
        // Account for the newline separator (except for the first entry).
        let line_bytes = line.len() + if i > 0 { 1 } else { 0 };
        if output_bytes_count + line_bytes > max_bytes {
            truncated_by = TruncatedBy::Bytes;
            break;
        }
        output_lines_vec.push(line);
        output_bytes_count += line_bytes;
    }

    let output_content = output_lines_vec.join("\n");
    let final_output_bytes = output_content.len();

    TruncationResult {
        content: output_content,
        truncated: true,
        truncated_by,
        total_lines,
        total_bytes,
        output_lines: output_lines_vec.len(),
        output_bytes: final_output_bytes,
        last_line_partial: false,
        first_line_exceeds_limit: false,
        max_lines,
        max_bytes,
    }
}

// ---------------------------------------------------------------------------
// truncate_tail
// ---------------------------------------------------------------------------

/// Keep the last N lines/bytes from the end of `content`.
///
/// May return a partial first line if the last line of the original content
/// exceeds the byte limit (keeps the end of that line).
pub fn truncate_tail(content: &str, options: TruncationOptions) -> TruncationResult {
    let max_lines = options.max_lines;
    let max_bytes = options.max_bytes;

    let total_bytes = content.len();
    let lines: Vec<&str> = content.split('\n').collect();
    let total_lines = lines.len();

    // No truncation needed?
    if total_lines <= max_lines && total_bytes <= max_bytes {
        return TruncationResult {
            content: content.to_string(),
            truncated: false,
            truncated_by: TruncatedBy::None,
            total_lines,
            total_bytes,
            output_lines: total_lines,
            output_bytes: total_bytes,
            last_line_partial: false,
            first_line_exceeds_limit: false,
            max_lines,
            max_bytes,
        };
    }

    // Work backwards from the end, collecting owned strings.
    let mut output_lines: Vec<String> = Vec::new();
    let mut byte_count: usize = 0;
    let mut truncated_by = TruncatedBy::Lines;
    let mut last_line_partial = false;

    for i in (0..lines.len()).rev() {
        if output_lines.len() >= max_lines {
            truncated_by = TruncatedBy::Lines;
            break;
        }
        let line = lines[i];
        // Newline cost: no cost for the first (rightmost) entry.
        let line_bytes = line.len() + if output_lines.is_empty() { 0 } else { 1 };

        if byte_count + line_bytes > max_bytes {
            truncated_by = TruncatedBy::Bytes;
            if output_lines.is_empty() {
                // Take the end of this line as a partial line.
                let truncated = truncate_string_from_end(line, max_bytes);
                output_lines.push(truncated);
                last_line_partial = true;
            }
            break;
        }

        output_lines.push(line.to_string());
        byte_count += line_bytes;
    }

    output_lines.reverse();
    let output_content = output_lines.join("\n");
    let final_output_bytes = output_content.len();

    TruncationResult {
        content: output_content,
        truncated: true,
        truncated_by,
        total_lines,
        total_bytes,
        output_lines: output_lines.len(),
        output_bytes: final_output_bytes,
        last_line_partial,
        first_line_exceeds_limit: false,
        max_lines,
        max_bytes,
    }
}

/// Truncate a string to fit within a byte limit, taking from the end.
/// Handles multi-byte UTF-8 characters correctly.
fn truncate_string_from_end(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let start = s.len() - max_bytes;
    // Find a valid UTF-8 boundary (start of a character).
    let mut adjusted_start = start;
    while adjusted_start < s.len() && s.as_bytes()[adjusted_start] & 0xC0 == 0x80 {
        adjusted_start += 1;
    }
    s[adjusted_start..].to_string()
}

// ---------------------------------------------------------------------------
// truncate_line
// ---------------------------------------------------------------------------

/// Truncate a single line to `max_chars` characters, adding a `[truncated]` suffix.
pub fn truncate_line(line: &str, max_chars: usize) -> (String, bool) {
    let chars: Vec<char> = line.chars().collect();
    if chars.len() <= max_chars {
        return (line.to_string(), false);
    }
    let truncated: String = chars.into_iter().take(max_chars).collect();
    (format!("{}... [truncated]", truncated), true)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_size() {
        assert_eq!(format_size(500), "500B");
        assert_eq!(format_size(2048), "2.0KB");
        assert_eq!(format_size(1_048_576), "1.0MB");
    }

    #[test]
    fn test_truncate_head_no_truncation() {
        let opts = TruncationOptions { max_lines: 10, max_bytes: 10_000 };
        let result = truncate_head("hello\nworld", opts);
        assert!(!result.truncated);
        assert_eq!(result.content, "hello\nworld");
    }

    #[test]
    fn test_truncate_head_line_limit() {
        let opts = TruncationOptions { max_lines: 2, max_bytes: 10_000 };
        let result = truncate_head("a\nb\nc\nd\ne", opts);
        assert!(result.truncated);
        assert_eq!(result.truncated_by, TruncatedBy::Lines);
        assert_eq!(result.content, "a\nb");
        assert_eq!(result.output_lines, 2);
        assert_eq!(result.total_lines, 5);
    }

    #[test]
    fn test_truncate_head_byte_limit() {
        let opts = TruncationOptions { max_lines: 100, max_bytes: 5 };
        let result = truncate_head("hello\nworld", opts);
        assert!(result.truncated);
        assert_eq!(result.truncated_by, TruncatedBy::Bytes);
        assert_eq!(result.content, "hello");
    }

    #[test]
    fn test_truncate_head_first_line_exceeds_limit() {
        let opts = TruncationOptions { max_lines: 100, max_bytes: 3 };
        let result = truncate_head("hello\nworld", opts);
        assert!(result.truncated);
        assert!(result.first_line_exceeds_limit);
        assert_eq!(result.content, "");
    }

    #[test]
    fn test_truncate_tail_no_truncation() {
        let opts = TruncationOptions { max_lines: 10, max_bytes: 10_000 };
        let result = truncate_tail("hello\nworld", opts);
        assert!(!result.truncated);
        assert_eq!(result.content, "hello\nworld");
    }

    #[test]
    fn test_truncate_tail_line_limit() {
        let opts = TruncationOptions { max_lines: 2, max_bytes: 10_000 };
        let result = truncate_tail("a\nb\nc\nd\ne", opts);
        assert!(result.truncated);
        assert_eq!(result.truncated_by, TruncatedBy::Lines);
        assert_eq!(result.content, "d\ne");
        assert_eq!(result.output_lines, 2);
    }

    #[test]
    fn test_truncate_tail_byte_limit() {
        let opts = TruncationOptions { max_lines: 100, max_bytes: 10 };
        let result = truncate_tail("hello\nworld\nfoo", opts);
        assert!(result.truncated);
        assert_eq!(result.truncated_by, TruncatedBy::Bytes);
    }

    #[test]
    fn test_truncate_line_short() {
        let (text, truncated) = truncate_line("hello", 100);
        assert!(!truncated);
        assert_eq!(text, "hello");
    }

    #[test]
    fn test_truncate_line_long() {
        let (text, truncated) = truncate_line("hello world", 5);
        assert!(truncated);
        assert!(text.contains("[truncated]"));
    }
}
