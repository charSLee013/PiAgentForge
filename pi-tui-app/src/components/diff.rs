//! Diff component — renders diff output with color-coded additions/removals.
//!
//! Parses unified-diff format lines and applies per-line colours:
//! - Context lines: `tool_diff_context` (dim/gray)
//! - Removed lines: `tool_diff_removed` (red)
//! - Added lines: `tool_diff_added` (green)
//!
//! Mirrors `packages/coding-agent/src/modes/interactive/components/diff.ts`

use pi_tui_core::Component;
use crate::Theme;

/// Parse a single diff line into its prefix, line number, and content parts.
///
/// Format: `+<line-num> <content>` or `-<line-num> <content>` or ` <line-num> <content>`.
fn parse_diff_line(line: &str) -> Option<(char, String, String)> {
    let line = line.replace('\t', "   ");
    let mut chars = line.chars();
    let prefix = chars.next()?;
    if prefix != '+' && prefix != '-' && prefix != ' ' {
        return None;
    }
    let rest = line[1..].to_string();
    let bytes = rest.as_bytes();
    let mut i = 0;

    // Skip leading whitespace before line number
    while i < bytes.len() && bytes[i] == b' ' {
        i += 1;
    }

    // Skip digit line number
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }

    // Skip the space separator
    if i < bytes.len() && bytes[i] == b' ' {
        i += 1;
    }

    let line_num = rest[..i].trim().to_string();
    let content = rest[i..].to_string();
    Some((prefix, line_num, content))
}

/// Renders a diff with colour-coded context, removed, and added lines.
pub struct Diff {
    /// Pre-computed styled diff lines (computed in constructor).
    styled_lines: Vec<String>,
}

impl Diff {
    /// Create a new diff component from a unified-diff formatted string.
    pub fn new(diff_text: String, theme: &Theme) -> Self {
        let styled_lines = render_diff_internal(&diff_text, theme);
        Self { styled_lines }
    }
}

impl Component for Diff {
    fn render(&self, _width: u16) -> Vec<String> {
        self.styled_lines.clone()
    }

    fn invalidate(&mut self) {}
}

/// Internal: render diff text with ANSI colouring.
fn render_diff_internal(diff_text: &str, theme: &Theme) -> Vec<String> {
    if diff_text.is_empty() {
        return Vec::new();
    }
    let lines: Vec<&str> = diff_text.split('\n').collect();
    let mut result: Vec<String> = Vec::new();
    let mut i = 0;

    while i < lines.len() {
        let line = lines[i];
        let parsed = parse_diff_line(line);

        match parsed {
            Some((prefix, ref line_num, ref content)) => {
                match prefix {
                    '-' => {
                        // Collect consecutive removed lines
                        let mut removed: Vec<(String, String)> = Vec::new();
                        removed.push((line_num.clone(), content.clone()));
                        i += 1;
                        while i < lines.len() {
                            if let Some(('-', ln, c)) = parse_diff_line(lines[i]) {
                                removed.push((ln, c));
                                i += 1;
                            } else {
                                break;
                            }
                        }

                        // Collect consecutive added lines
                        let mut added: Vec<(String, String)> = Vec::new();
                        while i < lines.len() {
                            if let Some(('+', ln, c)) = parse_diff_line(lines[i]) {
                                added.push((ln, c));
                                i += 1;
                            } else {
                                break;
                            }
                        }

                        // Render all removed then all added
                        for (ln, c) in &removed {
                            let styled =
                                theme.ansi(&theme.tool_diff_removed, &format!("-{ln} {c}"));
                            result.push(styled);
                        }
                        for (ln, c) in &added {
                            let styled =
                                theme.ansi(&theme.tool_diff_added, &format!("+{ln} {c}"));
                            result.push(styled);
                        }
                    }
                    '+' => {
                        // Standalone added line
                        let styled =
                            theme.ansi(&theme.tool_diff_added, &format!("+{line_num} {content}"));
                        result.push(styled);
                        i += 1;
                    }
                    ' ' => {
                        // Context line
                        let styled =
                            theme.ansi(&theme.tool_diff_context, &format!(" {line_num} {content}"));
                        result.push(styled);
                        i += 1;
                    }
                    _ => {
                        // Unexpected prefix — pass through as-is
                        result.push(line.to_string());
                        i += 1;
                    }
                }
            }
            None => {
                // Unrecognized line — pass through as-is
                result.push(line.to_string());
                i += 1;
            }
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_diff_empty() {
        let theme = Theme::dark();
        let diff = Diff::new(String::new(), &theme);
        let lines = diff.render(80);
        assert!(lines.is_empty());
    }

    #[test]
    fn test_diff_context_line() {
        let theme = Theme::dark();
        let diff = Diff::new(" 1 unchanged".into(), &theme);
        let lines = diff.render(80);
        assert!(!lines.is_empty());
        let line = &lines[0];
        assert!(line.contains("unchanged"));
        // Context lines use tool_diff_context color
        assert!(line.contains("\x1b["));
    }

    #[test]
    fn test_diff_added_line() {
        let theme = Theme::dark();
        let diff = Diff::new("+1 new line".into(), &theme);
        let lines = diff.render(80);
        assert!(!lines.is_empty());
        assert!(lines[0].contains("new line"));
        assert!(lines[0].contains("+1"));
    }

    #[test]
    fn test_diff_removed_line() {
        let theme = Theme::dark();
        let diff = Diff::new("-1 old line".into(), &theme);
        let lines = diff.render(80);
        assert!(!lines.is_empty());
        assert!(lines[0].contains("old line"));
        assert!(lines[0].contains("-1"));
    }

    #[test]
    fn test_diff_combined() {
        let theme = Theme::dark();
        let input = " 1 context\n-2 removed\n+2 added\n 3 more context\n";
        let diff = Diff::new(input.into(), &theme);
        let lines = diff.render(80);
        assert!(lines.iter().any(|l| l.contains("context")), "context should appear");
        assert!(lines.iter().any(|l| l.contains("removed")), "removed should appear");
        assert!(lines.iter().any(|l| l.contains("added")), "added should appear");
        // Removed should come before added
        let removed_idx = lines.iter().position(|l| l.contains("removed"));
        let added_idx = lines.iter().position(|l| l.contains("added"));
        if let (Some(ri), Some(ai)) = (removed_idx, added_idx) {
            assert!(ri < ai, "removed should appear before added");
        }
    }

    #[test]
    fn test_diff_colors_distinct() {
        let theme = Theme::dark();
        // Added lines use tool_diff_added (green)
        let added_diff = Diff::new("+1 hello".into(), &theme);
        let added = added_diff.render(80)[0].clone();
        // Removed lines use tool_diff_removed (red)
        let removed_diff = Diff::new("-1 hello".into(), &theme);
        let removed = removed_diff.render(80)[0].clone();
        // They should produce different ANSI codes
        assert_ne!(added, removed, "added and removed lines should be styled differently");
    }
}
