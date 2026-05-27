//! ANSI terminal utilities.
//!
//! Mirrors `packages/tui/src/utils.ts` — provides `visible_width`,
//! `truncate_to_width`, `wrap_text_with_ansi`, and helpers for overlay
//! compositing such as `extract_segments`.
//!
//! All functions in this module treat ANSI escape sequences as zero-width.

use unicode_width::UnicodeWidthChar;

// ---------------------------------------------------------------------------
// ANSI extraction
// ---------------------------------------------------------------------------

/// The result of extracting an ANSI escape sequence at a given position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnsiCode {
    /// The full escape-sequence string (including the leading ESC byte).
    pub code: String,
    /// Byte-length of the complete sequence.
    pub length: usize,
}

/// Extract a single ANSI escape sequence starting at byte offset `pos` in `s`.
///
/// Supports CSI (`ESC [` …), OSC (`ESC ]` … BEL / ST), and APC (`ESC _` … BEL / ST)
/// sequences. Returns `None` when no recognised sequence begins at `pos`.
pub fn extract_ansi_code(s: &str, pos: usize) -> Option<AnsiCode> {
    let bytes = s.as_bytes();
    if pos >= bytes.len() || bytes[pos] != 0x1b {
        return None;
    }

    let next = *bytes.get(pos + 1)?;

    // --- CSI: ESC [ ... final byte in [mGKHJ] ---
    if next == b'[' {
        let mut j = pos + 2;
        while j < bytes.len() && !matches!(bytes[j], b'm' | b'G' | b'K' | b'H' | b'J') {
            j += 1;
        }
        if j < bytes.len() {
            return Some(AnsiCode {
                code: s[pos..=j].to_string(),
                length: j + 1 - pos,
            });
        }
        return None;
    }

    // --- OSC: ESC ] ... BEL (0x07) or ST (ESC \\) ---
    if next == b']' {
        let mut j = pos + 2;
        while j < bytes.len() {
            if bytes[j] == 0x07 {
                return Some(AnsiCode {
                    code: s[pos..=j].to_string(),
                    length: j + 1 - pos,
                });
            }
            if bytes[j] == 0x1b && j + 1 < bytes.len() && bytes[j + 1] == b'\\' {
                return Some(AnsiCode {
                    code: s[pos..=j + 1].to_string(),
                    length: j + 2 - pos,
                });
            }
            j += 1;
        }
        return None;
    }

    // --- APC: ESC _ ... BEL (0x07) or ST (ESC \\) ---
    if next == b'_' {
        let mut j = pos + 2;
        while j < bytes.len() {
            if bytes[j] == 0x07 {
                return Some(AnsiCode {
                    code: s[pos..=j].to_string(),
                    length: j + 1 - pos,
                });
            }
            if bytes[j] == 0x1b && j + 1 < bytes.len() && bytes[j + 1] == b'\\' {
                return Some(AnsiCode {
                    code: s[pos..=j + 1].to_string(),
                    length: j + 2 - pos,
                });
            }
            j += 1;
        }
        return None;
    }

    None
}

// ---------------------------------------------------------------------------
// visible_width
// ---------------------------------------------------------------------------

/// Fast-path: return `true` when every byte is an ASCII printable (0x20-0x7e).
fn is_ascii_printable(s: &str) -> bool {
    s.bytes().all(|b| (0x20..=0x7e).contains(&b))
}

/// Calculate the visible column width of a string, ignoring ANSI escape
/// sequences and expanding tabs to 3 columns.
///
/// Uses `unicode-width` to correctly size CJK characters (width 2) and
/// combining characters (width 0).
pub fn visible_width(s: &str) -> usize {
    if s.is_empty() {
        return 0;
    }

    // Fast path: plain ASCII printable chars (no escapes, no tabs, no CJK)
    if is_ascii_printable(s) {
        return s.len();
    }

    let mut width = 0usize;
    let mut i = 0;
    let bytes = s.as_bytes();

    while i < s.len() {
        // Check for ANSI escape
        if bytes[i] == 0x1b {
            if let Some(ansi) = extract_ansi_code(s, i) {
                i += ansi.length;
                continue;
            }
            // Stray ESC with no recognised sequence — skip one byte
            i += 1;
            continue;
        }

        // Tab → 3 columns
        if bytes[i] == b'\t' {
            width += 3;
            i += 1;
            continue;
        }

        // Regular character
        let c = s[i..].chars().next().unwrap_or('\0');
        let cw = UnicodeWidthChar::width(c).unwrap_or(0);
        width += cw;
        i += c.len_utf8();
    }

    width
}

// ---------------------------------------------------------------------------
// truncate_to_width
// ---------------------------------------------------------------------------

/// Truncate `text` so its visible width does not exceed `max_width`.
///
/// An ellipsis (`…`) is appended when truncation occurs. ANSI escape sequences
/// are preserved in the truncated result; only visible glyphs are counted.
pub fn truncate_to_width(text: &str, max_width: usize) -> String {
    if max_width == 0 || text.is_empty() {
        return String::new();
    }

    let text_width = visible_width(text);
    if text_width <= max_width {
        return text.to_string();
    }

    // Reserve one column for the ellipsis. Use the character `…` (U+2026,
    // width 1 in most terminals) for a single‑cell ellipsis.
    let ellipsis = "\u{2026}";
    let ellipsis_w = 1usize; // … is normally width 1
    let target = max_width.saturating_sub(ellipsis_w);

    let mut out = String::with_capacity(text.len());
    let mut cur = 0usize;
    let mut i = 0;

    while i < text.len() && cur < target {
        if text.as_bytes()[i] == 0x1b {
            if let Some(ansi) = extract_ansi_code(text, i) {
                out.push_str(&ansi.code);
                i += ansi.length;
                continue;
            }
            i += 1;
            continue;
        }

        // Tab
        if text.as_bytes()[i] == b'\t' {
            if cur + 3 > target {
                break;
            }
            out.push('\t');
            cur += 3;
            i += 1;
            continue;
        }

        let c = text[i..].chars().next().unwrap_or('\0');
        let cw = UnicodeWidthChar::width(c).unwrap_or(0);
        if cur + cw > target {
            break;
        }
        out.push(c);
        cur += cw;
        i += c.len_utf8();
    }

    out.push_str(ellipsis);
    out
}

// ---------------------------------------------------------------------------
// Simple ANSI style tracker (for wrap_text_with_ansi)
// ---------------------------------------------------------------------------

/// Tracks active ANSI SGR attributes so they can be replayed on continuation
/// lines after a wrap.
#[derive(Clone)]
struct AnsiStyleTracker {
    /// Complete SGR sequences that are currently "open".
    active_styles: Vec<String>,
    /// Whether we have seen a full reset.
    have_reset: bool,
    /// OSC 8 hyperlink open sequence (if active).
    active_hyperlink: Option<String>,
}

impl AnsiStyleTracker {
    fn new() -> Self {
        Self {
            active_styles: Vec::new(),
            have_reset: false,
            active_hyperlink: None,
        }
    }

    /// Process an ANSI code and update internal state.
    fn process(&mut self, code: &str) {
        // OSC 8 hyperlink
        if code.starts_with("\x1b]8;") {
            // Check if this is a close (url is empty) or open
            let body = if code.ends_with('\x07') {
                &code[4..code.len() - 1]
            } else if code.ends_with("\x1b\\") {
                &code[4..code.len() - 2]
            } else {
                return;
            };

            let sep = body.find(';');
            let url = sep.map(|s| &body[s + 1..]).unwrap_or("");
            if url.is_empty() {
                // Close
                self.active_hyperlink = None;
            } else {
                self.active_hyperlink = Some(code.to_string());
            }

            if !code.ends_with('m') {
                return;
            }
        }

        // SGR sequences end with 'm'
        if !code.ends_with('m') {
            return;
        }

        // Full reset: \x1b[0m or \x1b[m
        if code == "\x1b[m" || code == "\x1b[0m" {
            self.active_styles.clear();
            self.have_reset = true;
            return;
        }

        // Style-setting SGR — store it
        self.active_styles.push(code.to_string());
        self.have_reset = false;
    }

    /// Build the ANSI prefix that should be prepended to a continuation line.
    fn wrap_prefix(&self) -> String {
        let mut out = String::new();
        // If we ever reset, replay all active styles
        if self.have_reset {
            for s in &self.active_styles {
                out.push_str(s);
            }
        }
        // Re-open hyperlink if active
        if let Some(h) = &self.active_hyperlink {
            out.push_str(h);
        }
        out
    }

    /// Close attributes that should not bleed across a line break (underline).
    fn line_end_reset(&self) -> String {
        let mut out = String::new();
        // If we have OSC 8 hyperlink active, close it so it doesn't bleed
        // into padding; it will be re-opened on the next line by wrap_prefix.
        if self.active_hyperlink.is_some() {
            out.push_str("\x1b]8;;\x1b\\");
        }
        out
    }
}

// ---------------------------------------------------------------------------
// wrap_text_with_ansi
// ---------------------------------------------------------------------------

fn wrap_single_line(line: &str, width: usize, result: &mut Vec<String>, tracker: &mut AnsiStyleTracker) {
    // Prepend active codes from previous lines
    let prefix = if result.is_empty() {
        String::new()
    } else {
        tracker.wrap_prefix()
    };
    let prefixed = format!("{}{}", prefix, line);

    if visible_width(&prefixed) <= width {
        result.push(prefixed);
        // Update tracker with codes from this line
        update_tracker(line, tracker);
        return;
    }

    // Word-wrap: split into tokens (words and whitespace)
    let tokens = tokenize_with_ansi(&prefixed);
    let mut current_line = String::new();
    let mut current_width = 0usize;

    for token in &tokens {
        let tw = visible_width(token);
        let is_ws = token.chars().all(|c| c.is_ascii_whitespace());

        // Long unbreakable token — character-break it
        if tw > width && !is_ws {
            if !current_line.is_empty() {
                let reset = tracker.line_end_reset();
                if !reset.is_empty() {
                    current_line.push_str(&reset);
                }
                result.push(current_line);
            }
            let broke = break_long_word(token, width, tracker);
            for b in &broke[..broke.len() - 1] {
                result.push(b.clone());
            }
            current_line = broke.last().cloned().unwrap_or_default();
            current_width = visible_width(&current_line);
            continue;
        }

        if current_width + tw > width && current_width > 0 {
            let reset = tracker.line_end_reset();
            if !reset.is_empty() {
                current_line.push_str(&reset);
            }
            result.push(current_line);

            if is_ws {
                // Don't start a new line with whitespace
                let wp = tracker.wrap_prefix();
                current_line = wp;
                current_width = visible_width(&current_line);
            } else {
                let wp = tracker.wrap_prefix();
                current_line = format!("{}{}", wp, token);
                current_width = tw;
            }
        } else {
            current_line.push_str(token);
            current_width += tw;
        }
    }

    if !current_line.is_empty() {
        result.push(current_line);
    }

    // Update tracker for the *original* line so state is correct for next line
    update_tracker(line, tracker);
}

/// Split text into tokens: alternating whitespace and non-whitespace groups,
/// with ANSI codes attached to the following visible content.
fn tokenize_with_ansi(text: &str) -> Vec<String> {
    let mut tokens: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut pending_ansi = String::new();
    let mut in_ws = false;
    let mut i = 0;

    while i < text.len() {
        if text.as_bytes()[i] == 0x1b {
            if let Some(ansi) = extract_ansi_code(text, i) {
                pending_ansi.push_str(&ansi.code);
                i += ansi.length;
                continue;
            }
        }

        let c = text[i..].chars().next().unwrap_or('\0');
        let char_is_ws = c.is_ascii_whitespace();

        if char_is_ws != in_ws && !current.is_empty() {
            tokens.push(current);
            current = String::new();
        }

        // Attach pending ANSI to this visible char
        if !pending_ansi.is_empty() {
            current.push_str(&pending_ansi);
            pending_ansi.clear();
        }

        in_ws = char_is_ws;
        current.push(c);
        i += c.len_utf8();
    }

    // Flush remaining pending ANSI
    if !pending_ansi.is_empty() {
        current.push_str(&pending_ansi);
    }

    if !current.is_empty() {
        tokens.push(current);
    }

    tokens
}

/// Break a single unbreakable token (long word) character by character.
fn break_long_word(word: &str, width: usize, tracker: &AnsiStyleTracker) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut current = tracker.wrap_prefix();
    let mut cw = 0usize;
    let mut i = 0;

    // Collect segments: alternating ANSI and grapheme-ish chunks
    // (we approximate each visible char as one grapheme for simplicity)
    struct Segment {
        text: String,
        width: usize,
    }

    let mut segs: Vec<Segment> = Vec::new();
    while i < word.len() {
        if word.as_bytes()[i] == 0x1b {
            if let Some(ansi) = extract_ansi_code(word, i) {
                segs.push(Segment {
                    text: ansi.code,
                    width: 0,
                });
                i += ansi.length;
                continue;
            }
        }
        if word.as_bytes()[i] == b'\t' {
            segs.push(Segment {
                text: "\t".to_string(),
                width: 3,
            });
            i += 1;
            continue;
        }
        let c = word[i..].chars().next().unwrap_or('\0');
        let seg_w = UnicodeWidthChar::width(c).unwrap_or(0);
        segs.push(Segment {
            text: c.to_string(),
            width: seg_w,
        });
        i += c.len_utf8();
    }

    for seg in &segs {
        if seg.width == 0 {
            // ANSI code or zero-width — just append
            current.push_str(&seg.text);
            if seg.text.ends_with('m') {
                // Process SGR
                if seg.text == "\x1b[m" || seg.text == "\x1b[0m" {
                    // Hmm, this is getting complex. For simplicity in break_long_word,
                    // just append and let the outer tracker handle it.
                }
            }
            continue;
        }

        if cw + seg.width > width && cw > 0 {
            let reset = tracker.line_end_reset();
            if !reset.is_empty() {
                current.push_str(&reset);
            }
            lines.push(current);
            current = tracker.wrap_prefix();
            cw = 0;
        }

        current.push_str(&seg.text);
        cw += seg.width;
    }

    if !current.is_empty() {
        lines.push(current);
    }

    lines
}

/// Update a style tracker by scanning `text` for ANSI codes.
fn update_tracker(text: &str, tracker: &mut AnsiStyleTracker) {
    let mut i = 0;
    while i < text.len() {
        if text.as_bytes()[i] == 0x1b {
            if let Some(ansi) = extract_ansi_code(text, i) {
                tracker.process(&ansi.code);
                i += ansi.length;
                continue;
            }
        }
        i += text[i..].chars().next().map(|c| c.len_utf8()).unwrap_or(1);
    }
}

/// Wrap text to fit within a given visible `width`, respecting ANSI escape
/// sequences and word boundaries.
///
/// Newlines in the input produce explicit line breaks. Lines output are NOT
/// padded to `width` — the caller is responsible for any padding.
pub fn wrap_text_with_ansi(text: &str, width: usize) -> Vec<String> {
    if text.is_empty() {
        return vec![String::new()];
    }

    let mut tracker = AnsiStyleTracker::new();
    let mut result: Vec<String> = Vec::new();

    for input_line in text.split('\n') {
        wrap_single_line(input_line, width, &mut result, &mut tracker);
    }

    if result.is_empty() {
        vec![String::new()]
    } else {
        result
    }
}

// ---------------------------------------------------------------------------
// extract_segments — for overlay compositing
// ---------------------------------------------------------------------------

/// The result of `extract_segments`: content from before and after an overlay
/// region in a line.
#[derive(Debug, Clone, Default)]
pub struct Segments {
    /// Text content before the overlay region (columns `0..before_end`).
    pub before: String,
    /// Visible width of `before`.
    pub before_width: usize,
    /// Text content after the overlay region (columns `after_start .. after_start+after_len`).
    pub after: String,
    /// Visible width of `after`.
    pub after_width: usize,
}

/// Extract "before" and "after" segments from a line in a single pass.
///
/// Used for overlay compositing: given a base line, extracts the content that
/// lives before the overlay (columns 0..before_end) and the content that lives
/// after the overlay (columns after_start .. after_start+after_len).
///
/// ANSI styling from before the overlay is inherited by the "after" segment
/// so that colours and attributes flow correctly around the overlaid region.
///
/// When `strict_after` is `true`, wide characters that cross the column boundary
/// are excluded from the "after" segment.
pub fn extract_segments(
    line: &str,
    before_end: usize,
    after_start: usize,
    after_len: usize,
    strict_after: bool,
) -> Segments {
    let mut before = String::new();
    let mut before_width = 0usize;
    let mut after = String::new();
    let mut after_width = 0usize;
    let mut current_col = 0usize;
    let mut i = 0;
    let mut pending_ansi = String::new();
    let mut after_started = false;
    let after_end = after_start + after_len;

    let bytes = line.as_bytes();

    while i < line.len() {
        if bytes[i] == 0x1b {
            if let Some(ansi) = extract_ansi_code(line, i) {
                // Collect ANSI into the appropriate region
                if current_col < before_end {
                    pending_ansi.push_str(&ansi.code);
                } else if current_col >= after_start && current_col < after_end && after_started {
                    // Only include once "after" has been started (styling prepended)
                    after.push_str(&ansi.code);
                }
                i += ansi.length;
                continue;
            }
        }

        // Get the next grapheme (single char for our purposes; ANSI skipped above)
        let c = line[i..].chars().next().unwrap_or('\0');
        let cw = UnicodeWidthChar::width(c).unwrap_or(0);
        // Tab → 3 cols
        let cw_effective = if c == '\t' { 3usize } else { cw };

        if current_col < before_end {
            // Collect into "before"
            if !pending_ansi.is_empty() {
                before.push_str(&pending_ansi);
                pending_ansi.clear();
            }
            before.push(c);
            before_width += cw_effective;
        } else if current_col >= after_start && current_col < after_end {
            let fits = !strict_after || current_col + cw_effective <= after_end;
            if fits {
                if !after_started {
                    // First "after" grapheme: prepend any pending ANSI
                    after.push_str(&pending_ansi);
                    pending_ansi.clear();
                    after_started = true;
                }
                after.push(c);
                after_width += cw_effective;
            }
        }

        current_col += cw_effective;
        if (after_len == 0 && current_col >= before_end)
            || (after_len > 0 && current_col >= after_end)
        {
            break;
        }
        i += c.len_utf8();
    }

    Segments {
        before,
        before_width,
        after,
        after_width,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // --- visible_width ---

    #[test]
    fn test_visible_width_plain() {
        assert_eq!(visible_width("hello"), 5);
        assert_eq!(visible_width(""), 0);
        assert_eq!(visible_width(" "), 1);
    }

    #[test]
    fn test_visible_width_with_ansi() {
        let red_text = "\x1b[31mhello\x1b[0m";
        assert_eq!(visible_width(red_text), 5);
    }

    #[test]
    fn test_visible_width_with_cjk() {
        assert_eq!(visible_width("中文"), 4); // each CJK char is width 2
        assert_eq!(visible_width("hello中文"), 9); // 5 + 4
    }

    #[test]
    fn test_visible_width_tabs() {
        assert_eq!(visible_width("\t"), 3);
        assert_eq!(visible_width("a\tb"), 5); // 1 + 3 + 1
    }

    #[test]
    fn test_visible_width_mixed() {
        // Red bold CJK
        let s = "\x1b[1;31m你好\x1b[0m";
        assert_eq!(visible_width(s), 4);
    }

    #[test]
    fn test_visible_width_osc8_hyperlink() {
        let link = "\x1b]8;;https://example.com\x1b\\click\x1b]8;;\x1b\\";
        assert_eq!(visible_width(link), 5);
    }

    // --- truncate_to_width ---

    #[test]
    fn test_truncate_too_short() {
        assert_eq!(truncate_to_width("hi", 10), "hi");
    }

    #[test]
    fn test_truncate_exact() {
        assert_eq!(truncate_to_width("hello", 5), "hello");
    }

    #[test]
    fn test_truncate_with_ellipsis() {
        let t = truncate_to_width("hello world", 8);
        assert!(t.ends_with('\u{2026}'));
        assert_eq!(visible_width(&t), 8);
    }

    #[test]
    fn test_truncate_empty() {
        assert_eq!(truncate_to_width("", 10), "");
    }

    #[test]
    fn test_truncate_zero_width() {
        assert_eq!(truncate_to_width("hello", 0), "");
    }

    #[test]
    fn test_truncate_with_ansi() {
        let s = truncate_to_width("\x1b[31mhello\x1b[0m", 3);
        assert_eq!(visible_width(&s), 3);
        assert!(s.contains("\x1b[31m")); // ANSI preserved
    }

    // --- wrap_text_with_ansi ---

    #[test]
    fn test_wrap_no_wrap_needed() {
        let lines = wrap_text_with_ansi("hello", 80);
        assert_eq!(lines, vec!["hello"]);
    }

    #[test]
    fn test_wrap_empty() {
        let lines = wrap_text_with_ansi("", 80);
        assert_eq!(lines, vec![""]);
    }

    #[test]
    fn test_wrap_word_boundary() {
        let lines = wrap_text_with_ansi("hello world foo bar", 10);
        // "hello" fits in 10, "world" fits, "foo bar" fits — or wraps at words
        for line in &lines {
            assert!(visible_width(line) <= 10, "line {:?} exceeds width", line);
        }
        assert!(lines.len() >= 2);
    }

    #[test]
    fn test_wrap_newline_preserved() {
        let lines = wrap_text_with_ansi("hello\nworld", 80);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].trim_end(), "hello");
        assert_eq!(lines[1].trim_end(), "world");
    }

    // --- extract_segments ---

    #[test]
    fn test_extract_segments_plain() {
        let segs = extract_segments("abcdefghij", 3, 6, 2, false);
        assert_eq!(segs.before, "abc");
        assert_eq!(segs.before_width, 3);
        assert_eq!(segs.after, "gh");
        assert_eq!(segs.after_width, 2);
    }

    #[test]
    fn test_extract_segments_with_ansi() {
        let segs = extract_segments("\x1b[31mabc\x1b[0mdef", 3, 4, 2, false);
        assert_eq!(visible_width(&segs.before), 3);
        assert_eq!(visible_width(&segs.after), 2);
    }

    #[test]
    fn test_extract_segments_no_after() {
        let segs = extract_segments("hello", 5, 5, 0, false);
        assert_eq!(segs.before, "hello");
        assert_eq!(segs.after, "");
    }
}
