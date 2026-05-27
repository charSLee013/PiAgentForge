//! Editor component — multi-line text editor with basic Emacs-style editing.
//!
//! Mirrors `packages/tui/src/components/editor.ts` (simplified — omits paste markers,
//! kill ring, autocomplete, history, bracketed paste, word-wrap layout, character
//! jump mode, and page-scroll).
//!
//! Features:
//! - Text buffer stored as `Vec<String>` (one element per logical line)
//! - Cursor movement: arrows, word-wise (Ctrl+arrows), Home/End, Ctrl+A/E
//! - Editing: character insert, backspace, delete, newline (splits line), tab (4 spaces)
//! - Undo (Ctrl+Z) via snapshot stack
//! - Vertical scrolling with `[N more]` scroll indicators
//! - Reverse-video cursor block on the focused line

use std::cell::Cell;

use crate::component::Component;
use crate::keys::{parse_key, KeyCode};
use crate::utils::{truncate_to_width, visible_width};

// ---------------------------------------------------------------------------
// Snapshot for undo
// ---------------------------------------------------------------------------

/// A snapshot of the editor state used for undo.
#[derive(Clone)]
pub struct EditorSnapshot {
    lines: Vec<String>,
    cursor_line: usize,
    cursor_col: usize,
    scroll_offset: usize,
}

// ---------------------------------------------------------------------------
// Editor component
// ---------------------------------------------------------------------------

/// Multi-line text editor component.
pub struct Editor {
    /// All text as logical lines (no trailing newline markers).
    lines: Vec<String>,
    /// Logical cursor position (`cursor_col` is a byte offset into the line).
    cursor_line: usize,
    cursor_col: usize,
    /// Vertical scroll offset in logical lines (interior-mutable for `&self` render).
    scroll_offset: Cell<usize>,
    /// Undo stack. Most recent snapshot is the last element.
    undo_stack: Vec<EditorSnapshot>,
    /// Whether this editor currently has input focus.
    pub focused: bool,
    /// Maximum number of visible lines before clipping (with scroll indicators).
    pub max_visible_lines: usize,
    /// Whether we are accumulating bracketed paste content.
    pasting: bool,
    /// Accumulated paste text while `pasting` is true.
    paste_buffer: String,
}

impl Editor {
    /// Create a new empty editor with a single blank line.
    pub fn new() -> Self {
        Self {
            lines: vec![String::new()],
            cursor_line: 0,
            cursor_col: 0,
            scroll_offset: Cell::new(0),
            undo_stack: Vec::new(),
            focused: false,
            max_visible_lines: 100,
            pasting: false,
            paste_buffer: String::new(),
        }
    }

    /// Create an editor pre-populated with `text`.
    ///
    /// The text is split on `\n`. At least one line is always present.
    pub fn with_text(text: &str) -> Self {
        let mut lines: Vec<String> = text.split('\n').map(|s| s.to_string()).collect();
        if lines.is_empty() {
            lines.push(String::new());
        }
        Self {
            lines,
            cursor_line: 0,
            cursor_col: 0,
            scroll_offset: Cell::new(0),
            undo_stack: Vec::new(),
            focused: false,
            max_visible_lines: 100,
            pasting: false,
            paste_buffer: String::new(),
        }
    }

    // ------------------------------------------------------------------
    // Public accessors
    // ------------------------------------------------------------------

    /// Return the full text content with lines joined by `\n`.
    pub fn get_text(&self) -> String {
        self.lines.join("\n")
    }

    /// Replace the entire text content, resetting the cursor.
    pub fn set_text(&mut self, text: &str) {
        self.push_undo_snapshot();
        self.load_text(text);
    }

    /// Number of logical lines in the buffer.
    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    /// Current cursor position as `(line, col)` where `col` is a byte offset.
    pub fn cursor_position(&self) -> (usize, usize) {
        (self.cursor_line, self.cursor_col)
    }

    // ------------------------------------------------------------------
    // Internal state helpers
    // ------------------------------------------------------------------

    /// Load text without pushing an undo snapshot.
    fn load_text(&mut self, text: &str) {
        let mut lines: Vec<String> = text.split('\n').map(|s| s.to_string()).collect();
        if lines.is_empty() {
            lines.push(String::new());
        }
        self.lines = lines;
        self.cursor_line = 0;
        self.cursor_col = 0;
        self.scroll_offset.set(0);
    }

    fn push_undo_snapshot(&mut self) {
        self.undo_stack.push(EditorSnapshot {
            lines: self.lines.clone(),
            cursor_line: self.cursor_line,
            cursor_col: self.cursor_col,
            scroll_offset: self.scroll_offset.get(),
        });
    }

    fn restore_snapshot(&mut self, snap: &EditorSnapshot) {
        self.lines = snap.lines.clone();
        self.cursor_line = snap.cursor_line;
        self.cursor_col = snap.cursor_col;
        self.scroll_offset.set(snap.scroll_offset);
    }

    /// Clamp `cursor_col` to the length of the current line.
    fn clamp_cursor_col(&mut self) {
        let max = self
            .lines
            .get(self.cursor_line)
            .map(|l| l.len())
            .unwrap_or(0);
        if self.cursor_col > max {
            self.cursor_col = max;
        }
    }

    // ------------------------------------------------------------------
    // Editing operations
    // ------------------------------------------------------------------

    /// Insert a single character at the cursor position.
    fn insert_char(&mut self, c: char) {
        let line = &mut self.lines[self.cursor_line];
        line.insert(self.cursor_col, c);
        self.cursor_col += c.len_utf8();
    }

    /// Insert a string at the cursor position.
    fn insert_str(&mut self, s: &str) {
        let line = &mut self.lines[self.cursor_line];
        line.insert_str(self.cursor_col, s);
        self.cursor_col += s.len();
    }

    /// Delete the character before the cursor (backspace).
    ///
    /// When at column 0 on a non-first line, merges the current line with the
    /// previous line.
    fn backspace(&mut self) {
        if self.cursor_col > 0 {
            let line = &mut self.lines[self.cursor_line];
            let prev = prev_char_boundary(line, self.cursor_col - 1);
            line.drain(prev..self.cursor_col);
            self.cursor_col = prev;
        } else if self.cursor_line > 0 {
            let current = self.lines.remove(self.cursor_line);
            self.cursor_line -= 1;
            self.cursor_col = self.lines[self.cursor_line].len();
            self.lines[self.cursor_line].push_str(&current);
        }
    }

    /// Delete the character at the cursor (forward delete).
    ///
    /// When at the end of a non-last line, merges the next line into the
    /// current one.
    fn delete(&mut self) {
        let line_len = self.lines[self.cursor_line].len();
        if self.cursor_col < line_len {
            let line = &mut self.lines[self.cursor_line];
            let next = next_char_boundary(line, self.cursor_col + 1);
            line.drain(self.cursor_col..next);
        } else if self.cursor_line + 1 < self.lines.len() {
            let next = self.lines.remove(self.cursor_line + 1);
            self.lines[self.cursor_line].push_str(&next);
        }
    }

    /// Split the current line at the cursor, creating a new line.
    ///
    /// Cursor moves to the start of the new line.
    fn newline(&mut self) {
        let after = self.lines[self.cursor_line][self.cursor_col..].to_string();
        self.lines[self.cursor_line].truncate(self.cursor_col);
        self.cursor_line += 1;
        self.lines.insert(self.cursor_line, after);
        self.cursor_col = 0;
    }

    // ------------------------------------------------------------------
    // Cursor movement
    // ------------------------------------------------------------------

    fn move_up(&mut self) {
        if self.cursor_line > 0 {
            self.cursor_line -= 1;
            self.clamp_cursor_col();
        }
    }

    fn move_down(&mut self) {
        if self.cursor_line + 1 < self.lines.len() {
            self.cursor_line += 1;
            self.clamp_cursor_col();
        }
    }

    fn move_left(&mut self) {
        if self.cursor_col > 0 {
            let line = &self.lines[self.cursor_line];
            self.cursor_col = prev_char_boundary(line, self.cursor_col - 1);
        } else if self.cursor_line > 0 {
            self.cursor_line -= 1;
            self.cursor_col = self.lines[self.cursor_line].len();
        }
    }

    fn move_right(&mut self) {
        let line_len = self.lines[self.cursor_line].len();
        if self.cursor_col < line_len {
            let line = &self.lines[self.cursor_line];
            self.cursor_col = next_char_boundary(line, self.cursor_col + 1);
        } else if self.cursor_line + 1 < self.lines.len() {
            self.cursor_line += 1;
            self.cursor_col = 0;
        }
    }

    fn move_to_line_start(&mut self) {
        self.cursor_col = 0;
    }

    fn move_to_line_end(&mut self) {
        self.cursor_col = self.lines[self.cursor_line].len();
    }

    /// Move cursor to the beginning of the previous word (or the previous line).
    fn move_word_left(&mut self) {
        let line = &self.lines[self.cursor_line];
        if self.cursor_col == 0 {
            if self.cursor_line > 0 {
                self.cursor_line -= 1;
                self.cursor_col = self.lines[self.cursor_line].len();
            }
            return;
        }
        // Move at least one character left
        let mut pos = prev_char_boundary(line, self.cursor_col - 1);
        // Skip trailing whitespace
        while pos > 0 {
            let c = line[pos..].chars().next().unwrap_or(' ');
            if !c.is_whitespace() {
                break;
            }
            pos = prev_char_boundary(line, pos.saturating_sub(1));
        }
        // Skip word characters (non-whitespace, non-punctuation)
        while pos > 0 {
            let prev = prev_char_boundary(line, pos.saturating_sub(1));
            let c = line[prev..pos].chars().next().unwrap_or(' ');
            if c.is_whitespace() || c.is_ascii_punctuation() {
                break;
            }
            pos = prev;
        }
        self.cursor_col = pos;
    }

    /// Move cursor to the beginning of the next word (or next line).
    fn move_word_right(&mut self) {
        let line_len = self.lines[self.cursor_line].len();
        let line = &self.lines[self.cursor_line];
        if self.cursor_col >= line_len {
            if self.cursor_line + 1 < self.lines.len() {
                self.cursor_line += 1;
                self.cursor_col = 0;
            }
            return;
        }
        // Move at least one character right
        let mut pos = next_char_boundary(line, self.cursor_col + 1);
        // Skip leading whitespace
        while pos < line_len {
            let c = line[pos..].chars().next().unwrap_or(' ');
            if !c.is_whitespace() {
                break;
            }
            pos = next_char_boundary(line, pos + c.len_utf8());
        }
        // Skip word characters (non-whitespace, non-punctuation)
        while pos < line_len {
            let c = line[pos..].chars().next().unwrap_or(' ');
            if c.is_whitespace() || c.is_ascii_punctuation() {
                break;
            }
            pos = next_char_boundary(line, pos + c.len_utf8());
        }
        self.cursor_col = pos;
    }

    // ------------------------------------------------------------------
    // Undo
    // ------------------------------------------------------------------

    fn undo(&mut self) {
        if let Some(snapshot) = self.undo_stack.pop() {
            self.restore_snapshot(&snapshot);
        }
    }
}

impl Default for Editor {
    fn default() -> Self {
        Self::new()
    }
}

impl Component for Editor {
    fn render(&self, width: u16) -> Vec<String> {
        let w = width as usize;
        if w == 0 {
            return vec![String::new()];
        }

        let total_lines = self.lines.len();

        // Compute the number of visible lines
        let max_vis = self.max_visible_lines.min(total_lines);

        // Compute scroll offset, keeping the cursor visible.
        let mut scroll = self.scroll_offset.get();
        if self.cursor_line < scroll {
            scroll = self.cursor_line;
        } else if max_vis > 0 && self.cursor_line >= scroll + max_vis {
            scroll = self.cursor_line.saturating_add(1).saturating_sub(max_vis);
        }
        let max_scroll = total_lines.saturating_sub(max_vis);
        if scroll > max_scroll {
            scroll = max_scroll;
        }
        self.scroll_offset.set(scroll);

        // Determine the visible slice
        let start = scroll;
        let end = (scroll + max_vis).min(total_lines);

        let mut result: Vec<String> = Vec::new();

        // Top scroll indicator
        if start > 0 {
            let indicator = format!("\u{2191} {} more", start);
            let indicator_vis = visible_width(&indicator);
            if indicator_vis <= w {
                let pad = w - indicator_vis;
                let mut line = indicator;
                line.push_str(&" ".repeat(pad));
                result.push(line);
            } else {
                result.push(truncate_to_width(&indicator, w));
            }
        }

        // Visible lines
        for i in start..end {
            if self.focused && i == self.cursor_line {
                result.push(self.render_cursor_line(&self.lines[i], self.cursor_col, w));
            } else {
                result.push(self.render_plain_line(&self.lines[i], w));
            }
        }

        // Bottom scroll indicator
        let lines_below = total_lines.saturating_sub(end);
        if lines_below > 0 {
            let indicator = format!("\u{2193} {} more", lines_below);
            let indicator_vis = visible_width(&indicator);
            if indicator_vis <= w {
                let pad = w - indicator_vis;
                let mut line = indicator;
                line.push_str(&" ".repeat(pad));
                result.push(line);
            } else {
                result.push(truncate_to_width(&indicator, w));
            }
        }

        result
    }

    fn handle_input(&mut self, data: &str) {
        // ── Bracketed paste markers (defensive: stdin_buffer normally strips these) ──
        if data == "\x1b[200~" {
            self.pasting = true;
            self.paste_buffer.clear();
            return;
        }
        if data == "\x1b[201~" {
            self.pasting = false;
            if !self.paste_buffer.is_empty() {
                self.push_undo_snapshot();
                let text = self.paste_buffer.clone();
				self.insert_text(&text);
                self.paste_buffer.clear();
            }
            return;
        }
        if self.pasting {
            self.paste_buffer.push_str(data);
            return;
        }

        // ── Multi-character paste from stdin_buffer (markers already stripped) ──
        if data.len() > 1 && !data.starts_with('\x1b') {
            self.push_undo_snapshot();
            self.insert_text(data);
            return;
        }

        // ── Normal single-byte / escape-sequence key handling ──
        let event = parse_key(data);

        match (&event.code, event.modifiers.ctrl, event.modifiers.alt) {
            // Printable character (no modifiers) -- insert at cursor
            (KeyCode::Char(c), false, false) if *c >= ' ' => {
                self.push_undo_snapshot();
                self.insert_char(*c);
            }
            // Enter -- split line
            (KeyCode::Enter, _, _) => {
                self.push_undo_snapshot();
                self.newline();
            }
            // Tab -- insert 4 spaces
            (KeyCode::Tab, _, _) => {
                self.push_undo_snapshot();
                self.insert_str("    ");
            }
            // Backspace
            (KeyCode::Backspace, _, _) => {
                self.push_undo_snapshot();
                self.backspace();
            }
            // Forward delete
            (KeyCode::Delete, _, _) => {
                self.push_undo_snapshot();
                self.delete();
            }
            // Arrow keys
            (KeyCode::Left, false, _) => self.move_left(),
            (KeyCode::Right, false, _) => self.move_right(),
            (KeyCode::Left, true, _) => self.move_word_left(),
            (KeyCode::Right, true, _) => self.move_word_right(),
            (KeyCode::Up, _, _) => self.move_up(),
            (KeyCode::Down, _, _) => self.move_down(),
            // Home / End
            (KeyCode::Home, _, _) => self.move_to_line_start(),
            (KeyCode::End, _, _) => self.move_to_line_end(),
            // Ctrl+Z -- undo
            (KeyCode::Char('z'), true, _) => self.undo(),
            // Ctrl+A -- line start (Emacs-style)
            (KeyCode::Char('a'), true, _) => self.move_to_line_start(),
            // Ctrl+E -- line end (Emacs-style)
            (KeyCode::Char('e'), true, _) => self.move_to_line_end(),
            // Everything else is ignored
            _ => {}
        }
    }

    fn invalidate(&mut self) {
        // No cached state to invalidate
    }
}

// ---------------------------------------------------------------------------
// Rendering helpers
// ---------------------------------------------------------------------------

impl Editor {
    /// Insert text at the cursor position, handling newlines.
    ///
    /// For multi-line text, each line segment is inserted on its own line,
    /// splitting at `\n` boundaries. A single undo snapshot covers the entire
    /// insertion. The caller is responsible for pushing the snapshot.
    fn insert_text(&mut self, text: &str) {
        let parts: Vec<&str> = text.split('\n').collect();
        if !parts.is_empty() && !parts[0].is_empty() {
            self.insert_str(parts[0]);
        }
        for &part in &parts[1..] {
            self.newline();
            if !part.is_empty() {
                self.insert_str(part);
            }
        }
    }

    /// Render a single line with a reverse-video cursor block at `cursor_col`.
    ///
    /// If the cursor is within the text, that grapheme is shown in reverse video.
    /// If the cursor is past the end of the line, a reverse-video space is shown.
    fn render_cursor_line(&self, line: &str, cursor_col: usize, width: usize) -> String {
        let cursor_col = cursor_col.min(line.len());

        let (before, rest) = line.split_at(cursor_col);
        let (at_cursor, after) = if let Some(c) = rest.chars().next() {
            let c_len = c.len_utf8();
            (&rest[..c_len], &rest[c_len..])
        } else {
            (" ", "")
        };

        let mut display = String::with_capacity(line.len() + 12);
        display.push_str(before);
        display.push_str("\x1b[7m");
        display.push_str(at_cursor);
        display.push_str("\x1b[27m");
        display.push_str(after);

        let vis = visible_width(&display);
        if vis > width {
            truncate_to_width(&display, width)
        } else {
            let padding = width.saturating_sub(vis);
            if padding > 0 {
                display.push_str(&" ".repeat(padding));
            }
            display
        }
    }

    /// Render a plain line (no cursor), padded or truncated to `width`.
    fn render_plain_line(&self, line: &str, width: usize) -> String {
        let vis = visible_width(line);
        if vis > width {
            truncate_to_width(line, width)
        } else {
            let padding = width.saturating_sub(vis);
            if padding > 0 {
                format!("{}{}", line, " ".repeat(padding))
            } else {
                line.to_string()
            }
        }
    }
}

// ---------------------------------------------------------------------------
// UTF-8 char-boundary helpers
// ---------------------------------------------------------------------------

/// Find the previous char boundary at or before `pos` in `s`.
fn prev_char_boundary(s: &str, pos: usize) -> usize {
    let mut p = pos.min(s.len());
    while p > 0 && !s.is_char_boundary(p) {
        p -= 1;
    }
    p
}

/// Find the next char boundary at or after `pos` in `s`.
fn next_char_boundary(s: &str, pos: usize) -> usize {
    let mut p = pos.min(s.len());
    while p < s.len() && !s.is_char_boundary(p) {
        p += 1;
    }
    p
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ------------------------------------------------------------------
    // Construction
    // ------------------------------------------------------------------

    #[test]
    fn test_new_editor_is_empty() {
        let ed = Editor::new();
        assert_eq!(ed.get_text(), "");
        assert_eq!(ed.line_count(), 1);
        assert_eq!(ed.cursor_position(), (0, 0));
    }

    #[test]
    fn test_with_text() {
        let ed = Editor::with_text("hello\nworld");
        assert_eq!(ed.get_text(), "hello\nworld");
        assert_eq!(ed.line_count(), 2);
        assert_eq!(ed.cursor_position(), (0, 0));
    }

    #[test]
    fn test_with_text_empty_string() {
        let ed = Editor::with_text("");
        assert_eq!(ed.get_text(), "");
        assert_eq!(ed.line_count(), 1);
    }

    #[test]
    fn test_set_text() {
        let mut ed = Editor::new();
        ed.set_text("abc\ndef");
        assert_eq!(ed.get_text(), "abc\ndef");
        assert_eq!(ed.line_count(), 2);
        // Cursor is reset
        assert_eq!(ed.cursor_position(), (0, 0));
    }

    // ------------------------------------------------------------------
    // Insertion
    // ------------------------------------------------------------------

    #[test]
    fn test_insert_characters() {
        let mut ed = Editor::new();
        ed.insert_char('a');
        assert_eq!(ed.get_text(), "a");
        assert_eq!(ed.cursor_col, 1);

        ed.insert_char('b');
        assert_eq!(ed.get_text(), "ab");
        assert_eq!(ed.cursor_col, 2);

        ed.insert_char('c');
        assert_eq!(ed.get_text(), "abc");
        assert_eq!(ed.cursor_col, 3);
    }

    #[test]
    fn test_insert_in_middle_of_line() {
        let mut ed = Editor::with_text("ac");
        ed.cursor_col = 1;
        ed.insert_char('b');
        assert_eq!(ed.get_text(), "abc");
        assert_eq!(ed.cursor_col, 2);
    }

    #[test]
    fn test_insert_via_handle_input() {
        let mut ed = Editor::new();
        ed.handle_input("h");
        ed.handle_input("e");
        ed.handle_input("l");
        ed.handle_input("l");
        ed.handle_input("o");
        assert_eq!(ed.get_text(), "hello");
        assert_eq!(ed.cursor_col, 5);
    }

    // ------------------------------------------------------------------
    // Backspace
    // ------------------------------------------------------------------

    #[test]
    fn test_backspace_deletes_before_cursor() {
        let mut ed = Editor::with_text("hello");
        ed.cursor_col = 5;
        ed.backspace();
        assert_eq!(ed.get_text(), "hell");
        assert_eq!(ed.cursor_col, 4);
    }

    #[test]
    fn test_backspace_at_start_of_line_does_nothing_on_first_line() {
        let mut ed = Editor::new();
        ed.backspace(); // Should not panic
        assert_eq!(ed.get_text(), "");
    }

    #[test]
    fn test_backspace_merges_with_previous_line() {
        let mut ed = Editor::with_text("hello\nworld");
        ed.cursor_line = 1;
        ed.cursor_col = 0;
        ed.backspace();
        assert_eq!(ed.get_text(), "helloworld");
        assert_eq!(ed.cursor_line, 0);
        assert_eq!(ed.cursor_col, 5);
    }

    #[test]
    fn test_backspace_via_handle() {
        let mut ed = Editor::with_text("hello");
        ed.cursor_col = 5;
        ed.handle_input("\x7f"); // Backspace
        assert_eq!(ed.get_text(), "hell");
        assert_eq!(ed.cursor_col, 4);
    }

    // ------------------------------------------------------------------
    // Delete
    // ------------------------------------------------------------------

    #[test]
    fn test_delete_at_cursor() {
        let mut ed = Editor::with_text("hello");
        ed.cursor_col = 2;
        ed.delete();
        assert_eq!(ed.get_text(), "helo");
        assert_eq!(ed.cursor_col, 2);
    }

    #[test]
    fn test_delete_at_end_of_line_merges_with_next_line() {
        let mut ed = Editor::with_text("hello\nworld");
        ed.cursor_line = 0;
        ed.cursor_col = 5;
        ed.delete();
        assert_eq!(ed.get_text(), "helloworld");
        assert_eq!(ed.line_count(), 1);
    }

    #[test]
    fn test_delete_via_handle() {
        let mut ed = Editor::with_text("hello");
        ed.cursor_col = 0;
        ed.handle_input("\x1b[3~"); // Delete key
        assert_eq!(ed.get_text(), "ello");
    }

    // ------------------------------------------------------------------
    // Newline
    // ------------------------------------------------------------------

    #[test]
    fn test_newline_splits_line() {
        let mut ed = Editor::with_text("hello world");
        ed.cursor_col = 5;
        ed.newline();
        assert_eq!(ed.get_text(), "hello\n world");
        assert_eq!(ed.cursor_line, 1);
        assert_eq!(ed.cursor_col, 0);
    }

    #[test]
    fn test_newline_at_start_of_line() {
        let mut ed = Editor::with_text("hello");
        ed.cursor_col = 0;
        ed.newline();
        assert_eq!(ed.get_text(), "\nhello");
        assert_eq!(ed.cursor_line, 1);
        assert_eq!(ed.cursor_col, 0);
    }

    #[test]
    fn test_newline_at_end_of_line() {
        let mut ed = Editor::with_text("hello");
        ed.cursor_col = 5;
        ed.newline();
        assert_eq!(ed.get_text(), "hello\n");
        assert_eq!(ed.cursor_line, 1);
        assert_eq!(ed.cursor_col, 0);
    }

    #[test]
    fn test_enter_inserts_newline() {
        let mut ed = Editor::with_text("abc");
        ed.cursor_col = 1;
        ed.handle_input("\r"); // Enter
        assert_eq!(ed.get_text(), "a\nbc");
        assert_eq!(ed.cursor_line, 1);
        assert_eq!(ed.cursor_col, 0);
    }

    // ------------------------------------------------------------------
    // Tab
    // ------------------------------------------------------------------

    #[test]
    fn test_tab_inserts_four_spaces() {
        let mut ed = Editor::new();
        ed.handle_input("\t");
        assert_eq!(ed.get_text(), "    ");
        assert_eq!(ed.cursor_col, 4);
    }

    // ------------------------------------------------------------------
    // Cursor movement
    // ------------------------------------------------------------------

    #[test]
    fn test_move_left_right() {
        let mut ed = Editor::with_text("hello");
        assert_eq!(ed.cursor_col, 0);

        ed.handle_input("\x1b[C"); // Right
        assert_eq!(ed.cursor_col, 1);

        ed.handle_input("\x1b[C"); // Right
        assert_eq!(ed.cursor_col, 2);

        ed.handle_input("\x1b[D"); // Left
        assert_eq!(ed.cursor_col, 1);
    }

    #[test]
    fn test_move_up_down() {
        let mut ed = Editor::with_text("line1\nline2\nline3");
        assert_eq!(ed.cursor_position(), (0, 0));

        ed.handle_input("\x1b[B"); // Down
        assert_eq!(ed.cursor_line, 1);

        ed.handle_input("\x1b[B"); // Down
        assert_eq!(ed.cursor_line, 2);

        ed.handle_input("\x1b[A"); // Up
        assert_eq!(ed.cursor_line, 1);
    }

    #[test]
    fn test_move_up_clamps_column() {
        let mut ed = Editor::with_text("a\nlonger line");
        ed.cursor_line = 1;
        ed.cursor_col = 11; // end of "longer line"
        ed.handle_input("\x1b[A"); // Up
        assert_eq!(ed.cursor_line, 0);
        // Clamped to length of "a" = 1
        assert_eq!(ed.cursor_col, 1);
    }

    #[test]
    fn test_right_wraps_to_next_line() {
        let mut ed = Editor::with_text("ab\ncd");
        ed.cursor_col = 2; // end of "ab"
        ed.handle_input("\x1b[C"); // Right
        assert_eq!(ed.cursor_line, 1);
        assert_eq!(ed.cursor_col, 0);
    }

    #[test]
    fn test_left_wraps_to_previous_line() {
        let mut ed = Editor::with_text("ab\ncd");
        ed.cursor_line = 1;
        ed.cursor_col = 0;
        ed.handle_input("\x1b[D"); // Left
        assert_eq!(ed.cursor_line, 0);
        assert_eq!(ed.cursor_col, 2);
    }

    #[test]
    fn test_home_moves_to_line_start() {
        let mut ed = Editor::with_text("hello");
        ed.cursor_col = 3;
        ed.handle_input("\x1b[H"); // Home
        assert_eq!(ed.cursor_col, 0);
    }

    #[test]
    fn test_end_moves_to_line_end() {
        let mut ed = Editor::with_text("hello");
        ed.cursor_col = 0;
        ed.handle_input("\x1b[F"); // End
        assert_eq!(ed.cursor_col, 5);
    }

    #[test]
    fn test_ctrl_a_moves_to_line_start() {
        let mut ed = Editor::with_text("hello");
        ed.cursor_col = 3;
        ed.handle_input("\x01"); // Ctrl+A
        assert_eq!(ed.cursor_col, 0);
    }

    #[test]
    fn test_ctrl_e_moves_to_line_end() {
        let mut ed = Editor::with_text("hello");
        ed.cursor_col = 0;
        ed.handle_input("\x05"); // Ctrl+E
        assert_eq!(ed.cursor_col, 5);
    }

    // ------------------------------------------------------------------
    // Word movement
    // ------------------------------------------------------------------

    #[test]
    fn test_word_right_moves_forward() {
        let mut ed = Editor::with_text("hello world foo");
        ed.cursor_col = 0;

        let before = ed.cursor_col;
        ed.move_word_right();
        assert!(
            ed.cursor_col > before,
            "word_right should advance the cursor"
        );
    }

    #[test]
    fn test_word_left_moves_backward() {
        let mut ed = Editor::with_text("hello world foo");
        ed.cursor_col = 15; // end of line

        let before = ed.cursor_col;
        ed.move_word_left();
        assert!(
            ed.cursor_col < before,
            "word_left should retreat the cursor"
        );
    }

    #[test]
    fn test_ctrl_right_advances_word() {
        let mut ed = Editor::with_text("hello world");
        ed.cursor_col = 0;
        ed.handle_input("\x1b[1;5C"); // Ctrl+Right
        // Should move past the first word
        assert!(ed.cursor_col > 0);
    }

    #[test]
    fn test_ctrl_left_retreats_word() {
        let mut ed = Editor::with_text("hello world");
        ed.cursor_col = 6; // at "world"
        ed.handle_input("\x1b[1;5D"); // Ctrl+Left
        // Should move to start of "hello"
        assert!(ed.cursor_col < 6);
    }

    // ------------------------------------------------------------------
    // Undo
    // ------------------------------------------------------------------

    #[test]
    fn test_undo_restores_previous_state() {
        let mut ed = Editor::new();
        ed.handle_input("a");
        ed.handle_input("b");
        ed.handle_input("c");
        assert_eq!(ed.get_text(), "abc");

        ed.handle_input("\x1a"); // Ctrl+Z
        assert_eq!(ed.get_text(), "ab");

        ed.handle_input("\x1a"); // Ctrl+Z
        assert_eq!(ed.get_text(), "a");
    }

    #[test]
    fn test_undo_restores_cursor_position() {
        let mut ed = Editor::with_text("hello");
        ed.cursor_col = 2;
        ed.handle_input("X"); // insert 'X' at position 2 → "heXllo"
        assert_eq!(ed.get_text(), "heXllo");
        assert_eq!(ed.cursor_col, 3);

        ed.handle_input("\x1a"); // Ctrl+Z
        assert_eq!(ed.get_text(), "hello");
        assert_eq!(ed.cursor_col, 2); // restored
    }

    #[test]
    fn test_undo_on_empty_stack_is_noop() {
        let mut ed = Editor::new();
        ed.undo(); // Should not panic
        assert_eq!(ed.get_text(), "");
    }

    // ------------------------------------------------------------------
    // Render
    // ------------------------------------------------------------------

    #[test]
    fn test_render_produces_at_least_line_count_lines() {
        let ed = Editor::with_text("hello\nworld\nfoo");
        let lines = ed.render(80);
        // With max_visible_lines = 100 and total = 3, all 3 lines fit
        // plus no scroll indicators since nothing is clipped
        assert!(lines.len() >= 3, "expected >=3 lines, got {}", lines.len());
    }

    #[test]
    fn test_render_shows_cursor_reverse_video_when_focused() {
        let mut ed = Editor::with_text("hello");
        ed.focused = true;
        let lines = ed.render(80);
        assert!(!lines.is_empty());
        // The cursor is at (0, 0), so the first character should be reversed
        assert!(
            lines[0].contains("\x1b[7m"),
            "render should contain reverse-video ANSI when focused: {:?}",
            lines[0]
        );
    }

    #[test]
    fn test_render_no_cursor_when_not_focused() {
        let mut ed = Editor::with_text("hello");
        ed.focused = false;
        let lines = ed.render(80);
        assert!(!lines[0].contains("\x1b[7m"));
    }

    #[test]
    fn test_render_shows_scroll_indicators_when_clipped() {
        let mut ed = Editor::new();
        // Create enough lines to exceed max_visible_lines
        for i in 0..20 {
            ed.lines.push(format!("line {}", i));
        }
        ed.max_visible_lines = 5;
        ed.cursor_line = 10; // somewhere in the middle
        let lines = ed.render(80);
        // Should have top and bottom indicators
        let joined = lines.join(" ");
        assert!(
            joined.contains('\u{2191}'),
            "should contain up arrow indicator"
        );
        assert!(
            joined.contains('\u{2193}'),
            "should contain down arrow indicator"
        );
    }

    #[test]
    fn test_render_zero_width_returns_single_line() {
        let ed = Editor::with_text("hello");
        let lines = ed.render(0);
        assert_eq!(lines.len(), 1);
    }

    // ------------------------------------------------------------------
    // Integration: editing followed by render
    // ------------------------------------------------------------------

    #[test]
    fn test_edit_then_render() {
        let mut ed = Editor::new();
        ed.handle_input("H");
        ed.handle_input("i");
        ed.handle_input("!");
        ed.handle_input("\r"); // Enter
        ed.handle_input("w");
        ed.handle_input("o");
        ed.handle_input("r");
        ed.handle_input("l");
        ed.handle_input("d");

        assert_eq!(ed.get_text(), "Hi!\nworld");
        assert_eq!(ed.line_count(), 2);

        let lines = ed.render(80);
        assert!(lines.len() >= 2);
    }

    // ------------------------------------------------------------------
    // Paste support
    // ------------------------------------------------------------------

    #[test]
    fn test_insert_text_single_line() {
        let mut ed = Editor::new();
        ed.insert_text("hello world");
        assert_eq!(ed.get_text(), "hello world");
        assert_eq!(ed.cursor_col, 11);
        assert_eq!(ed.line_count(), 1);
    }

    #[test]
    fn test_insert_text_multi_line() {
        let mut ed = Editor::new();
        ed.insert_text("hello\nworld");
        assert_eq!(ed.get_text(), "hello\nworld");
        assert_eq!(ed.line_count(), 2);
        assert_eq!(ed.cursor_line, 1);
        assert_eq!(ed.cursor_col, 5);
    }

    #[test]
    fn test_insert_text_mid_line_single() {
        let mut ed = Editor::with_text("hel world");
        ed.cursor_col = 3;
        ed.insert_text("lo");
        assert_eq!(ed.get_text(), "hello world");
        assert_eq!(ed.cursor_col, 5);
    }

    #[test]
    fn test_insert_text_mid_line_multi() {
        let mut ed = Editor::with_text("helworld\n!");
        ed.cursor_col = 3;
        ed.insert_text("lo\n");
        // "lo" inserted at col 3 → "helloworld", then newline splits at col 5
        assert_eq!(ed.get_text(), "hello\nworld\n!");
        assert_eq!(ed.cursor_line, 1);
        assert_eq!(ed.cursor_col, 0);
    }

    #[test]
    fn test_insert_text_newline_prefix() {
        let mut ed = Editor::with_text("hello");
        ed.cursor_col = 5;
        // Insert "\nworld": first part is empty, then newline + "world"
        ed.insert_text("\nworld");
        assert_eq!(ed.get_text(), "hello\nworld");
        assert_eq!(ed.cursor_line, 1);
        assert_eq!(ed.cursor_col, 5);
    }

    #[test]
    fn test_paste_marker_start_end() {
        let mut ed = Editor::new();
        // Simulate paste: start marker, content, end marker
        ed.handle_input("\x1b[200~");
        assert!(ed.pasting, "should be in paste mode");
        ed.handle_input("hello ");
        ed.handle_input("world");
        ed.handle_input("\x1b[201~");
        assert!(!ed.pasting, "paste mode should end");
        assert_eq!(ed.get_text(), "hello world");
        assert_eq!(ed.cursor_col, 11);
    }

    #[test]
    fn test_paste_marker_multi_line() {
        let mut ed = Editor::new();
        ed.handle_input("\x1b[200~");
        ed.handle_input("line1\nline2\nline3");
        ed.handle_input("\x1b[201~");
        assert_eq!(ed.get_text(), "line1\nline2\nline3");
        assert_eq!(ed.line_count(), 3);
        assert_eq!(ed.cursor_line, 2);
        assert_eq!(ed.cursor_col, 5);
    }
}
