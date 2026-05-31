//! Diff computation and edit-application utilities.
//! Mirrors `packages/coding-agent/src/core/tools/edit-diff.ts`

use similar::{DiffOp, TextDiff};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// A single edit operation: replace `old_text` with `new_text`.
#[derive(Debug, Clone)]
pub struct Edit {
    pub old_text: String,
    pub new_text: String,
}

/// Result of applying edits to normalized content.
#[derive(Debug, Clone)]
pub struct AppliedEdits {
    /// The content used as the base for diffing (possibly fuzzy-normalized).
    pub base_content: String,
    /// The content after applying all edits.
    pub new_content: String,
}

/// Result of diff generation.
#[derive(Debug, Clone)]
pub struct DiffResult {
    /// Unified diff string with line numbers.
    pub diff: String,
    /// The first changed line number in the new file (1-indexed), if any.
    pub first_changed_line: Option<usize>,
}

// ---------------------------------------------------------------------------
// Line-ending helpers
// ---------------------------------------------------------------------------

/// Detect whether the content uses `\r\n` or `\n`.
pub fn detect_line_ending(content: &str) -> &str {
    let crlf_idx = content.find("\r\n");
    let lf_idx = content.find('\n');
    match (crlf_idx, lf_idx) {
        (Some(crlf), Some(lf)) => {
            if crlf < lf {
                "\r\n"
            } else {
                "\n"
            }
        }
        (Some(_), None) => "\r\n",
        _ => "\n",
    }
}

/// Normalize all line endings to `\n`.
pub fn normalize_to_lf(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// Restore line endings from `\n` back to the original ending.
pub fn restore_line_endings(text: &str, ending: &str) -> String {
    if ending == "\r\n" { text.replace('\n', "\r\n") } else { text.to_string() }
}

// ---------------------------------------------------------------------------
// BOM
// ---------------------------------------------------------------------------

/// Strip UTF-8 BOM if present, returning the BOM and the text without it.
/// The BOM is \u{FEFF} which is 3 bytes in UTF-8.
pub fn strip_bom(content: &str) -> (String, String) {
    if let Some(stripped) = content.strip_prefix('\u{FEFF}') {
        ("\u{FEFF}".to_string(), stripped.to_string())
    } else {
        (String::new(), content.to_string())
    }
}

// ---------------------------------------------------------------------------
// Fuzzy matching
// ---------------------------------------------------------------------------

/// Normalize text for fuzzy matching.
///
/// Strips trailing whitespace per line, normalizes smart quotes and dashes
/// to ASCII equivalents, and normalizes special spaces.
pub fn normalize_for_fuzzy_match(text: &str) -> String {
    let mut result = text.lines().map(|line| line.trim_end()).collect::<Vec<&str>>().join("\n");

    // Normalize Unicode characters.
    result = result
        .replace(['\u{2018}', '\u{2019}', '\u{201A}', '\u{201B}'], "'")
        .replace(['\u{201C}', '\u{201D}', '\u{201E}', '\u{201F}'], "\"")
        .replace(['\u{2010}', '\u{2011}', '\u{2012}', '\u{2013}', '\u{2014}', '\u{2015}', '\u{2212}'], "-")
        .replace(
            [
                '\u{00A0}', '\u{2002}', '\u{2003}', '\u{2004}', '\u{2005}', '\u{2006}', '\u{2007}', '\u{2008}',
                '\u{2009}', '\u{200A}', '\u{202F}', '\u{205F}', '\u{3000}',
            ],
            " ",
        );

    result
}

// ---------------------------------------------------------------------------
// Fuzzy find
// ---------------------------------------------------------------------------

/// Result of a fuzzy find operation.
#[derive(Debug, Clone)]
pub struct FuzzyMatchResult {
    pub found: bool,
    pub index: usize,
    pub match_length: usize,
    pub used_fuzzy_match: bool,
    pub content_for_replacement: String,
}

/// Find `old_text` in `content`, trying exact match first, then fuzzy match.
pub fn fuzzy_find_text(content: &str, old_text: &str) -> FuzzyMatchResult {
    // Try exact match first.
    if let Some(idx) = content.find(old_text) {
        return FuzzyMatchResult {
            found: true,
            index: idx,
            match_length: old_text.len(),
            used_fuzzy_match: false,
            content_for_replacement: content.to_string(),
        };
    }

    // Try fuzzy match.
    let fuzzy_content = normalize_for_fuzzy_match(content);
    let fuzzy_old_text = normalize_for_fuzzy_match(old_text);

    if let Some(idx) = fuzzy_content.find(&fuzzy_old_text) {
        return FuzzyMatchResult {
            found: true,
            index: idx,
            match_length: fuzzy_old_text.len(),
            used_fuzzy_match: true,
            content_for_replacement: fuzzy_content,
        };
    }

    FuzzyMatchResult {
        found: false,
        index: 0,
        match_length: 0,
        used_fuzzy_match: false,
        content_for_replacement: content.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Apply edits
// ---------------------------------------------------------------------------

/// Apply one or more exact-text replacements to LF-normalized content.
///
/// All edits are matched against the same original content. Replacements are
/// applied in reverse order so offsets remain stable. If any edit requires
/// fuzzy matching, the operation runs in normalized content space.
pub fn apply_edits_to_normalized_content(
    normalized_content: &str,
    edits: &[Edit],
    file_path: &str,
) -> Result<AppliedEdits, String> {
    if edits.is_empty() {
        return Err("edits must contain at least one replacement.".to_string());
    }

    // Validate and normalize edits.
    let mut normalized_edits: Vec<Edit> = Vec::new();
    for (i, edit) in edits.iter().enumerate() {
        let old_text = normalize_to_lf(&edit.old_text);
        let new_text = normalize_to_lf(&edit.new_text);
        if old_text.is_empty() {
            return Err(if edits.len() == 1 {
                format!("oldText must not be empty in {}.", file_path)
            } else {
                format!("edits[{}].oldText must not be empty in {}.", i, file_path)
            });
        }
        normalized_edits.push(Edit { old_text, new_text });
    }

    // Find all matches.
    let initial_matches: Vec<FuzzyMatchResult> =
        normalized_edits.iter().map(|edit| fuzzy_find_text(normalized_content, &edit.old_text)).collect();

    let any_fuzzy = initial_matches.iter().any(|m| m.used_fuzzy_match);
    let base_content =
        if any_fuzzy { normalize_for_fuzzy_match(normalized_content) } else { normalized_content.to_string() };

    // Find matches again in the base content.
    struct MatchedEdit {
        edit_index: usize,
        match_index: usize,
        match_length: usize,
        new_text: String,
    }

    let mut matched_edits: Vec<MatchedEdit> = Vec::new();
    for (i, edit) in normalized_edits.iter().enumerate() {
        let match_result = fuzzy_find_text(&base_content, &edit.old_text);
        if !match_result.found {
            return Err(if normalized_edits.len() == 1 {
                format!(
                    "Could not find the exact text in {}. The old text must match exactly including all whitespace and newlines.",
                    file_path
                )
            } else {
                format!(
                    "Could not find edits[{}] in {}. The oldText must match exactly including all whitespace and newlines.",
                    i, file_path
                )
            });
        }

        // Count occurrences to check uniqueness.
        let occurrences = count_occurrences(&base_content, &edit.old_text);
        if occurrences > 1 {
            return Err(if normalized_edits.len() == 1 {
                format!(
                    "Found {} occurrences of the text in {}. The text must be unique. Please provide more context to make it unique.",
                    occurrences, file_path
                )
            } else {
                format!(
                    "Found {} occurrences of edits[{}] in {}. Each oldText must be unique. Please provide more context to make it unique.",
                    occurrences, i, file_path
                )
            });
        }

        matched_edits.push(MatchedEdit {
            edit_index: i,
            match_index: match_result.index,
            match_length: match_result.match_length,
            new_text: edit.new_text.clone(),
        });
    }

    // Sort by match index and check for overlaps.
    matched_edits.sort_by_key(|m| m.match_index);
    for i in 1..matched_edits.len() {
        let prev = &matched_edits[i - 1];
        let curr = &matched_edits[i];
        if prev.match_index + prev.match_length > curr.match_index {
            return Err(format!(
                "edits[{}] and edits[{}] overlap in {}. Merge them into one edit or target disjoint regions.",
                prev.edit_index, curr.edit_index, file_path
            ));
        }
    }

    // Apply edits in reverse order.
    let mut new_content = base_content.clone();
    for edit in matched_edits.into_iter().rev() {
        let before = &new_content[..edit.match_index];
        let after = &new_content[edit.match_index + edit.match_length..];
        new_content = format!("{}{}{}", before, edit.new_text, after);
    }

    if base_content == new_content {
        return Err(if normalized_edits.len() == 1 {
            format!(
                "No changes made to {}. The replacement produced identical content. This might indicate an issue with special characters or the text not existing as expected.",
                file_path
            )
        } else {
            format!("No changes made to {}. The replacements produced identical content.", file_path)
        });
    }

    Ok(AppliedEdits { base_content, new_content })
}

/// Count occurrences of `old_text` in `content` (fuzzy).
fn count_occurrences(content: &str, old_text: &str) -> usize {
    let fuzzy_content = normalize_for_fuzzy_match(content);
    let fuzzy_old = normalize_for_fuzzy_match(old_text);
    fuzzy_content.split(&fuzzy_old).count().saturating_sub(1)
}

// ---------------------------------------------------------------------------
// Diff generation
// ---------------------------------------------------------------------------

/// Generate a unified diff string with line numbers and context.
///
/// Returns both the diff string and the first changed line number (in the new file).
pub fn generate_diff_string(old_content: &str, new_content: &str, context_lines: usize) -> DiffResult {
    let diff = TextDiff::from_lines(old_content, new_content);

    let old_lines: Vec<&str> = old_content.split('\n').collect();
    let new_lines: Vec<&str> = new_content.split('\n').collect();
    let max_line_num = old_lines.len().max(new_lines.len());
    let line_num_width = format!("{}", max_line_num).len();

    let mut output: Vec<String> = Vec::new();
    let mut old_line_num: usize = 1;
    let mut new_line_num: usize = 1;
    let mut last_was_change = false;
    let mut first_changed_line: Option<usize> = None;

    let ops = diff.ops();
    for (op_idx, op) in ops.iter().enumerate() {
        let next_is_change = op_idx + 1 < ops.len() && !matches!(ops[op_idx + 1], DiffOp::Equal { .. });

        match op {
            DiffOp::Equal { len, .. } => {
                let count = *len;
                let has_leading_change = last_was_change;
                let has_trailing_change = next_is_change;

                if has_leading_change && has_trailing_change {
                    if count <= context_lines * 2 {
                        for _ in 0..count {
                            let line = old_lines.get(old_line_num - 1).unwrap_or(&"");
                            output.push(format!(" {:>width$} {}", old_line_num, line, width = line_num_width));
                            old_line_num += 1;
                            new_line_num += 1;
                        }
                    } else {
                        let shown = context_lines.min(count);
                        for _ in 0..shown {
                            let line = old_lines.get(old_line_num - 1).unwrap_or(&"");
                            output.push(format!(" {:>width$} {}", old_line_num, line, width = line_num_width));
                            old_line_num += 1;
                            new_line_num += 1;
                        }
                        let skipped = count - shown * 2;
                        let padding: String = (0..line_num_width).map(|_| ' ').collect();
                        output.push(format!(" {} ...", padding));
                        old_line_num += skipped;
                        new_line_num += skipped;
                        for _ in 0..shown {
                            let line = old_lines.get(old_line_num - 1).unwrap_or(&"");
                            output.push(format!(" {:>width$} {}", old_line_num, line, width = line_num_width));
                            old_line_num += 1;
                            new_line_num += 1;
                        }
                    }
                } else if has_leading_change {
                    let shown = context_lines.min(count);
                    for _ in 0..shown {
                        let line = old_lines.get(old_line_num - 1).unwrap_or(&"");
                        output.push(format!(" {:>width$} {}", old_line_num, line, width = line_num_width));
                        old_line_num += 1;
                        new_line_num += 1;
                    }
                    if shown < count {
                        let padding: String = (0..line_num_width).map(|_| ' ').collect();
                        output.push(format!(" {} ...", padding));
                        old_line_num += count - shown;
                        new_line_num += count - shown;
                    }
                } else if has_trailing_change {
                    let skipped = count.saturating_sub(context_lines);
                    if skipped > 0 {
                        let padding: String = (0..line_num_width).map(|_| ' ').collect();
                        output.push(format!(" {} ...", padding));
                        old_line_num += skipped;
                        new_line_num += skipped;
                    }
                    let shown = count - skipped;
                    for _ in 0..shown {
                        let line = old_lines.get(old_line_num - 1).unwrap_or(&"");
                        output.push(format!(" {:>width$} {}", old_line_num, line, width = line_num_width));
                        old_line_num += 1;
                        new_line_num += 1;
                    }
                } else {
                    old_line_num += count;
                    new_line_num += count;
                }

                last_was_change = false;
            }
            DiffOp::Delete { old_len, .. } => {
                if first_changed_line.is_none() {
                    first_changed_line = Some(new_line_num);
                }
                for _ in 0..*old_len {
                    let line = old_lines.get(old_line_num - 1).unwrap_or(&"");
                    output.push(format!("-{:>width$} {}", old_line_num, line, width = line_num_width));
                    old_line_num += 1;
                }
                last_was_change = true;
            }
            DiffOp::Insert { new_len, .. } => {
                if first_changed_line.is_none() {
                    first_changed_line = Some(new_line_num);
                }
                for _ in 0..*new_len {
                    let line = new_lines.get(new_line_num - 1).unwrap_or(&"");
                    output.push(format!("+{:>width$} {}", new_line_num, line, width = line_num_width));
                    new_line_num += 1;
                }
                last_was_change = true;
            }
            DiffOp::Replace { old_len, new_len, .. } => {
                if first_changed_line.is_none() {
                    first_changed_line = Some(new_line_num);
                }
                for _ in 0..*old_len {
                    let line = old_lines.get(old_line_num - 1).unwrap_or(&"");
                    output.push(format!("-{:>width$} {}", old_line_num, line, width = line_num_width));
                    old_line_num += 1;
                }
                for _ in 0..*new_len {
                    let line = new_lines.get(new_line_num - 1).unwrap_or(&"");
                    output.push(format!("+{:>width$} {}", new_line_num, line, width = line_num_width));
                    new_line_num += 1;
                }
                last_was_change = true;
            }
        }
    }

    DiffResult { diff: output.join("\n"), first_changed_line }
}

// ---------------------------------------------------------------------------
// Convenience: apply edits and produce diff
// ---------------------------------------------------------------------------

/// Read the file at `absolute_path`, apply the given edits, write the result,
/// and return the diff.
pub async fn apply_edits_and_diff(
    absolute_path: &std::path::Path,
    edits: &[Edit],
    file_path: &str,
) -> Result<DiffResult, String> {
    let content = tokio::fs::read_to_string(absolute_path).await.map_err(|e| format!("Could not read file: {}", e))?;

    // Strip BOM.
    let (_bom, text) = strip_bom(&content);
    let normalized = normalize_to_lf(&text);

    let applied = apply_edits_to_normalized_content(&normalized, edits, file_path)?;

    let diff = generate_diff_string(&applied.base_content, &applied.new_content, 4);

    Ok(diff)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_line_ending_lf() {
        assert_eq!(detect_line_ending("a\nb\nc"), "\n");
    }

    #[test]
    fn test_detect_line_ending_crlf() {
        assert_eq!(detect_line_ending("a\r\nb\r\nc"), "\r\n");
    }

    #[test]
    fn test_normalize_to_lf() {
        assert_eq!(normalize_to_lf("a\r\nb\r\nc"), "a\nb\nc");
        assert_eq!(normalize_to_lf("a\nb\nc"), "a\nb\nc");
    }

    #[test]
    fn test_strip_bom() {
        let (bom, text) = strip_bom("\u{FEFF}hello");
        assert_eq!(bom, "\u{FEFF}");
        assert_eq!(text, "hello");

        let (bom, text) = strip_bom("hello");
        assert_eq!(bom, "");
        assert_eq!(text, "hello");
    }

    #[test]
    fn test_fuzzy_find_exact_match() {
        let result = fuzzy_find_text("hello world", "world");
        assert!(result.found);
        assert!(!result.used_fuzzy_match);
        assert_eq!(result.index, 6);
    }

    #[test]
    fn test_fuzzy_find_fuzzy_match() {
        // Smart quotes vs straight quotes.
        let result = fuzzy_find_text("hello \u{2018}world\u{2019}", "'world'");
        assert!(result.found);
        assert!(result.used_fuzzy_match);
    }

    #[test]
    fn test_fuzzy_find_no_match() {
        let result = fuzzy_find_text("hello world", "xyz");
        assert!(!result.found);
    }

    #[test]
    fn test_apply_edits_single() {
        let content = "hello\nworld\nfoo";
        let edits = vec![Edit { old_text: "world".to_string(), new_text: "there".to_string() }];
        let result = apply_edits_to_normalized_content(content, &edits, "test.txt").unwrap();
        assert_eq!(result.new_content, "hello\nthere\nfoo");
    }

    #[test]
    fn test_apply_edits_multiple_disjoint() {
        let content = "aaa\nbbb\nccc\nddd";
        let edits = vec![
            Edit { old_text: "aaa".to_string(), new_text: "111".to_string() },
            Edit { old_text: "ddd".to_string(), new_text: "999".to_string() },
        ];
        let result = apply_edits_to_normalized_content(content, &edits, "test.txt").unwrap();
        assert_eq!(result.new_content, "111\nbbb\nccc\n999");
    }

    #[test]
    fn test_apply_edits_no_match() {
        let content = "hello world";
        let edits = vec![Edit { old_text: "nope".to_string(), new_text: "xxx".to_string() }];
        let result = apply_edits_to_normalized_content(content, &edits, "test.txt");
        assert!(result.is_err());
    }

    #[test]
    fn test_apply_edits_overlap() {
        let content = "hello world foo";
        let edits = vec![
            Edit { old_text: "hello world".to_string(), new_text: "hi".to_string() },
            Edit { old_text: "world foo".to_string(), new_text: "there".to_string() },
        ];
        let result = apply_edits_to_normalized_content(content, &edits, "test.txt");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("overlap"));
    }

    #[test]
    fn test_generate_diff_string() {
        let old = "hello\nworld\nfoo\nbar\nbaz";
        let new = "hello\nworld\nqux\nbar\nbaz";
        let diff = generate_diff_string(old, new, 4);
        assert!(diff.diff.contains("-3 foo"));
        assert!(diff.diff.contains("+3 qux"));
        assert_eq!(diff.first_changed_line, Some(3));
    }

    #[test]
    fn test_normalize_for_fuzzy_match() {
        let input = "hello \u{201C}world\u{201D}";
        let result = normalize_for_fuzzy_match(input);
        assert_eq!(result, "hello \"world\"");
    }

    #[test]
    fn test_apply_edits_identical_content() {
        let content = "hello world";
        let edits = vec![Edit { old_text: "hello".to_string(), new_text: "hello".to_string() }];
        let result = apply_edits_to_normalized_content(content, &edits, "test.txt");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("No changes"));
    }
}
