//! Input component — single-line text input with horizontal scrolling.
//!
//! Mirrors `packages/tui/src/components/input.ts`
//!
//! Supports basic Emacs-style editing:
//! - Printable characters: inserted at cursor
//! - Backspace / Delete: delete before / at cursor
//! - Left / Right: move cursor
//! - Home (Ctrl+A) / End (Ctrl+E): jump to start / end
//! - Enter: submit (calls `on_submit`)
//! - Escape: cancel (calls `on_escape`)

use crate::component::Component;
use crate::keys::{KeyCode, parse_key};
use crate::utils::visible_width;

/// Single-line text input component.
pub struct Input {
    /// Current text value.
    value: String,
    /// Cursor byte offset (always on a char boundary).
    cursor: usize,
    /// Whether this input is focused (shows cursor marker).
    pub focused: bool,
    /// Placeholder text shown when value is empty and not focused.
    pub placeholder: String,
    /// Called when Enter is pressed.
    pub on_submit: Option<Box<dyn FnMut(String) + Send>>,
    /// Called when Escape is pressed.
    pub on_cancel: Option<Box<dyn FnMut() + Send>>,
}

impl Input {
    pub fn new() -> Self {
        Self {
            value: String::new(),
            cursor: 0,
            focused: false,
            placeholder: String::new(),
            on_submit: None,
            on_cancel: None,
        }
    }

    pub fn with_value(value: String) -> Self {
        let cursor = value.len();
        Self { value, cursor, focused: false, placeholder: String::new(), on_submit: None, on_cancel: None }
    }

    /// Get the current input value.
    pub fn value(&self) -> &str {
        &self.value
    }

    /// Set the input value and place the cursor at the end.
    pub fn set_value(&mut self, value: String) {
        self.value = value;
        self.cursor = self.value.len();
    }

    /// Get the current cursor byte offset.
    pub fn cursor_pos(&self) -> usize {
        self.cursor
    }

    /// Set the cursor byte offset (will be clamped to value length
    /// and adjusted to a valid char boundary).
    pub fn set_cursor(&mut self, pos: usize) {
        let pos = pos.min(self.value.len());
        self.cursor = self.prev_char_boundary(pos);
    }

    /// Insert a character at the cursor.
    fn insert_char(&mut self, c: char) {
        self.value.insert(self.cursor, c);
        self.cursor += c.len_utf8();
    }

    /// Delete the character before the cursor.
    fn backspace(&mut self) {
        if self.cursor > 0 {
            let prev = self.prev_char_boundary(self.cursor - 1);
            self.value.drain(prev..self.cursor);
            self.cursor = prev;
        }
    }

    /// Delete the character at the cursor.
    fn delete(&mut self) {
        if self.cursor < self.value.len() {
            let next = self.next_char_boundary(self.cursor + 1);
            self.value.drain(self.cursor..next);
        }
    }

    /// Move cursor left by one char.
    fn move_left(&mut self) {
        if self.cursor > 0 {
            self.cursor = self.prev_char_boundary(self.cursor - 1);
        }
    }

    /// Move cursor right by one char.
    fn move_right(&mut self) {
        if self.cursor < self.value.len() {
            self.cursor = self.next_char_boundary(self.cursor + 1);
        }
    }

    /// Move cursor to the start of the previous word.
    fn move_word_left(&mut self) {
        if self.cursor == 0 {
            return;
        }
        // Skip trailing space
        let mut pos = self.prev_char_boundary(self.cursor - 1);
        while pos > 0 && self.value.as_bytes().get(pos).copied() == Some(b' ') {
            pos = self.prev_char_boundary(pos.saturating_sub(1));
        }
        // Skip word characters
        while pos > 0 {
            let prev = self.prev_char_boundary(pos.saturating_sub(1));
            let c = self.value[prev..pos].chars().next().unwrap_or(' ');
            if c.is_ascii_punctuation() || c == ' ' {
                break;
            }
            pos = prev;
        }
        self.cursor = pos;
    }

    /// Move cursor to the start of the next word.
    #[cfg(test)]
    fn move_word_right(&mut self) {
        if self.cursor >= self.value.len() {
            return;
        }
        let mut pos = self.next_char_boundary(self.cursor + 1);
        // Skip leading space
        while pos < self.value.len() {
            let c = self.value[pos..].chars().next().unwrap_or(' ');
            if c != ' ' {
                break;
            }
            pos = self.next_char_boundary(pos + c.len_utf8());
        }
        // Skip word characters
        while pos < self.value.len() {
            let c = self.value[pos..].chars().next().unwrap_or(' ');
            if c.is_ascii_punctuation() || c == ' ' {
                break;
            }
            pos = self.next_char_boundary(pos + c.len_utf8());
        }
        self.cursor = pos;
    }

    /// Delete the word before the cursor.
    fn delete_word_backward(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let old_cursor = self.cursor;
        self.move_word_left();
        let del_start = self.cursor;
        self.cursor = old_cursor;
        self.value.drain(del_start..self.cursor);
        self.cursor = del_start;
    }

    /// Delete the word after the cursor.
    #[cfg(test)]
    fn delete_word_forward(&mut self) {
        if self.cursor >= self.value.len() {
            return;
        }
        let old_cursor = self.cursor;
        self.move_word_right();
        let del_end = self.cursor;
        self.cursor = old_cursor;
        self.value.drain(self.cursor..del_end);
    }

    /// Delete from cursor to line start.
    fn delete_to_line_start(&mut self) {
        if self.cursor == 0 {
            return;
        }
        self.value.drain(..self.cursor);
        self.cursor = 0;
    }

    /// Delete from cursor to line end.
    fn delete_to_line_end(&mut self) {
        if self.cursor >= self.value.len() {
            return;
        }
        self.value.truncate(self.cursor);
    }

    /// Find the previous char boundary at or before `pos`.
    fn prev_char_boundary(&self, pos: usize) -> usize {
        let mut p = pos.min(self.value.len());
        while p > 0 && !self.value.is_char_boundary(p) {
            p -= 1;
        }
        p
    }

    /// Find the next char boundary at or after `pos`.
    fn next_char_boundary(&self, pos: usize) -> usize {
        let mut p = pos.min(self.value.len());
        while p < self.value.len() && !self.value.is_char_boundary(p) {
            p += 1;
        }
        p
    }

    /// Build the visible portion of the input line.
    ///
    /// Returns `(display_text, cursor_col)` where `cursor_col` is the
    /// visible column of the cursor within `display_text`.
    fn compute_display(&self, available: usize) -> (String, usize) {
        if self.value.is_empty() {
            return (String::new(), 0);
        }

        let total_vis = visible_width(&self.value);
        let cursor_vis = visible_width(&self.value[..self.cursor]);

        if total_vis <= available {
            // Everything fits
            return (self.value.clone(), cursor_vis);
        }

        // Horizontal scrolling: position the viewport so the cursor is visible
        let scroll_width = available;
        let half = scroll_width / 2;

        let start_col = if cursor_vis < half {
            0
        } else if cursor_vis > total_vis - half {
            total_vis.saturating_sub(scroll_width)
        } else {
            cursor_vis.saturating_sub(half)
        };

        // Build visible substring starting at `start_col` columns
        let mut display = String::new();
        let mut col = 0usize;
        let mut cursor_col = 0usize;
        let mut cursor_col_set = false;

        for c in self.value.chars() {
            let cw = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
            let end_col = col + cw;

            if end_col <= start_col {
                col = end_col;
                continue;
            }

            if !cursor_col_set && col + cw > cursor_vis {
                // This character is at or past the cursor — figure out cursor_col
                // The cursor is between col and col+cw
                if col >= cursor_vis {
                    cursor_col = visible_width(&display);
                } else {
                    cursor_col = visible_width(&display) + (cursor_vis - col);
                }
                cursor_col_set = true;
            }

            if visible_width(&display) + cw > scroll_width {
                break;
            }

            display.push(c);
            col = end_col;
        }

        if !cursor_col_set {
            cursor_col = visible_width(&display);
        }

        (display, cursor_col)
    }
}

impl Default for Input {
    fn default() -> Self {
        Self::new()
    }
}

impl Component for Input {
    fn render(&self, width: u16) -> Vec<String> {
        let w = width as usize;
        let prompt = "> ";
        let prompt_w = visible_width(prompt);
        let available = w.saturating_sub(prompt_w);
        if available == 0 {
            return vec![prompt.to_string()];
        }

        // Determine display text
        let is_empty = self.value.is_empty();
        let (display_text, cursor_col) = if is_empty {
            if self.focused {
                (String::new(), 0usize)
            } else {
                // Show placeholder
                let ph = truncate_to_width_no_ellipsis(&self.placeholder, available);
                (ph, 0)
            }
        } else {
            self.compute_display(available)
        };

        // Build the line with cursor
        let line = if self.focused { insert_cursor(&display_text, cursor_col, available) } else { display_text };

        // Pad to full width
        let full_line = format!("{}{}", prompt, line);
        let vis = visible_width(&full_line);
        let mut result = full_line;
        if vis < w {
            result.push_str(&" ".repeat(w - vis));
        }

        vec![result]
    }

    fn handle_input(&mut self, data: &str) {
        let event = parse_key(data);

        match event.code {
            KeyCode::Char(c) if !event.modifiers.ctrl && !event.modifiers.alt => {
                // Regular printable character
                self.insert_char(c);
            }
            KeyCode::Enter => {
                let value = self.value.clone();
                if let Some(ref mut cb) = self.on_submit {
                    cb(value);
                }
            }
            KeyCode::Escape => {
                if let Some(ref mut cb) = self.on_cancel {
                    cb();
                }
            }
            KeyCode::Backspace => {
                self.backspace();
            }
            KeyCode::Delete => {
                self.delete();
            }
            KeyCode::Left => {
                self.move_left();
            }
            KeyCode::Right => {
                self.move_right();
            }
            KeyCode::Home => {
                self.cursor = 0;
            }
            KeyCode::End => {
                self.cursor = self.value.len();
            }
            _ => {
                // Handle control-based movement via char matching
                match event.code {
                    KeyCode::Char('a') if event.modifiers.ctrl => self.cursor = 0,
                    KeyCode::Char('e') if event.modifiers.ctrl => self.cursor = self.value.len(),
                    KeyCode::Char('b') if event.modifiers.ctrl => self.move_left(),
                    KeyCode::Char('f') if event.modifiers.ctrl => self.move_right(),
                    KeyCode::Char('d') if event.modifiers.ctrl => self.delete(),
                    KeyCode::Char('k') if event.modifiers.ctrl => self.delete_to_line_end(),
                    KeyCode::Char('u') if event.modifiers.ctrl => self.delete_to_line_start(),
                    KeyCode::Char('w') if event.modifiers.ctrl => self.delete_word_backward(),
                    KeyCode::Char('h') if event.modifiers.ctrl => self.backspace(),
                    KeyCode::Char('n') if event.modifiers.ctrl => {} // ignored (used for navigation)
                    KeyCode::Char('p') if event.modifiers.ctrl => {} // ignored (used for navigation)
                    _ => {}
                }
            }
        }
    }

    fn invalidate(&mut self) {
        // No cached state
    }
}

/// Truncate a string to fit `max_vis` visible columns, without adding ellipsis.
fn truncate_to_width_no_ellipsis(text: &str, max_vis: usize) -> String {
    if visible_width(text) <= max_vis {
        return text.to_string();
    }
    let mut result = String::new();
    let mut col = 0;
    for c in text.chars() {
        let cw = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        if col + cw > max_vis {
            break;
        }
        result.push(c);
        col += cw;
    }
    result
}

/// Insert a reverse-video cursor marker into `text` at `cursor_col`.
///
/// The character at the cursor position is wrapped in \x1b[7m...\x1b[27m
/// (reverse video).  If the cursor is beyond the end of the text, a space
/// is shown in reverse video instead.
fn insert_cursor(text: &str, cursor_col: usize, _available: usize) -> String {
    let mut before = String::new();
    let mut at_cursor = String::from(" ");
    let mut after = String::new();
    let mut col = 0usize;
    let mut cursor_placed = false;

    for c in text.chars() {
        let cw = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        if col >= cursor_col && !cursor_placed {
            at_cursor = c.to_string();
            cursor_placed = true;
        } else if cursor_placed {
            after.push(c);
        } else {
            before.push(c);
        }
        col += cw;
    }

    // If cursor is past all text, at_cursor stays as " "
    format!("{}\x1b[7m{}\x1b[27m{}", before, at_cursor, after)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_input_new_is_empty() {
        let input = Input::new();
        assert_eq!(input.value(), "");
        assert_eq!(input.cursor_pos(), 0);
    }

    #[test]
    fn test_input_with_value() {
        let input = Input::with_value("hello".to_string());
        assert_eq!(input.value(), "hello");
        assert_eq!(input.cursor_pos(), 5);
    }

    #[test]
    fn test_input_insert_chars() {
        let mut input = Input::new();
        input.insert_char('a');
        assert_eq!(input.value(), "a");
        assert_eq!(input.cursor_pos(), 1);
        input.insert_char('b');
        assert_eq!(input.value(), "ab");
        assert_eq!(input.cursor_pos(), 2);
    }

    #[test]
    fn test_input_insert_at_cursor() {
        let mut input = Input::with_value("ac".to_string());
        input.cursor = 1;
        input.insert_char('b');
        assert_eq!(input.value(), "abc");
        assert_eq!(input.cursor_pos(), 2);
    }

    #[test]
    fn test_input_backspace() {
        let mut input = Input::with_value("hello".to_string());
        input.cursor = 5;
        input.backspace();
        assert_eq!(input.value(), "hell");
        assert_eq!(input.cursor_pos(), 4);
    }

    #[test]
    fn test_input_backspace_at_start() {
        let mut input = Input::new();
        input.backspace(); // Should not panic
        assert_eq!(input.value(), "");
    }

    #[test]
    fn test_input_delete() {
        let mut input = Input::with_value("hello".to_string());
        input.cursor = 2;
        input.delete();
        assert_eq!(input.value(), "helo"); // removed 'l' at position 2
    }

    #[test]
    fn test_input_delete_at_end() {
        let mut input = Input::with_value("hello".to_string());
        input.cursor = 5;
        input.delete(); // Should not panic
        assert_eq!(input.value(), "hello");
    }

    #[test]
    fn test_input_move_left() {
        let mut input = Input::with_value("hello".to_string());
        input.cursor = 5;
        input.move_left();
        assert_eq!(input.cursor_pos(), 4);
    }

    #[test]
    fn test_input_move_right() {
        let mut input = Input::with_value("hello".to_string());
        input.cursor = 0;
        input.move_right();
        assert_eq!(input.cursor_pos(), 1);
    }

    #[test]
    fn test_input_home_end_via_handle() {
        let mut input = Input::with_value("hello".to_string());
        input.cursor = 3;
        input.handle_input("\x1b[H"); // Home
        assert_eq!(input.cursor_pos(), 0);
        input.handle_input("\x1b[F"); // End
        assert_eq!(input.cursor_pos(), 5);
    }

    #[test]
    fn test_input_render_contains_value() {
        let input = Input::with_value("hello".to_string());
        let lines = input.render(80);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("hello"));
    }

    #[test]
    fn test_input_render_placeholder() {
        let mut input = Input::new();
        input.placeholder = "type here".to_string();
        input.focused = false;
        let lines = input.render(80);
        assert!(lines[0].contains("type here"));
    }

    #[test]
    fn test_input_render_no_placeholder_when_focused() {
        let mut input = Input::new();
        input.placeholder = "type here".to_string();
        input.focused = true;
        let lines = input.render(80);
        // When focused, don't show placeholder even if empty
        assert!(!lines[0].contains("type here"));
    }

    #[test]
    fn test_input_render_width_respected() {
        let input = Input::with_value("hello world this is a test".to_string());
        let lines = input.render(10);
        assert!(visible_width(&lines[0]) <= 10);
    }

    #[test]
    fn test_input_set_value() {
        let mut input = Input::new();
        input.set_value("new text".to_string());
        assert_eq!(input.value(), "new text");
        assert_eq!(input.cursor_pos(), 8);
    }

    #[test]
    fn test_input_delete_to_line_start() {
        let mut input = Input::with_value("hello world".to_string());
        input.cursor = 5;
        input.delete_to_line_start();
        assert_eq!(input.value(), " world");
        assert_eq!(input.cursor_pos(), 0);
    }

    #[test]
    fn test_input_delete_to_line_end() {
        let mut input = Input::with_value("hello world".to_string());
        input.cursor = 5;
        input.delete_to_line_end();
        assert_eq!(input.value(), "hello");
        assert_eq!(input.cursor_pos(), 5);
    }

    #[test]
    fn test_input_delete_word_backward() {
        let mut input = Input::with_value("hello world".to_string());
        input.cursor = 11;
        input.delete_word_backward();
        assert_eq!(input.value(), "hello ");
    }

    #[test]
    fn test_input_delete_word_forward() {
        let mut input = Input::with_value("hello world".to_string());
        input.cursor = 0;
        input.delete_word_forward();
        assert_eq!(input.value(), " world");
    }

    #[test]
    fn test_input_handle_printable() {
        let mut input = Input::new();
        input.handle_input("a");
        assert_eq!(input.value(), "a");
        input.handle_input("b");
        assert_eq!(input.value(), "ab");
    }

    #[test]
    fn test_input_handle_backspace() {
        let mut input = Input::with_value("hello".to_string());
        input.cursor = 5;
        input.handle_input("\x08"); // Backspace
        assert_eq!(input.value(), "hell");
    }

    #[test]
    fn test_input_handle_delete() {
        let mut input = Input::with_value("hello".to_string());
        input.cursor = 0;
        input.handle_input("\x1b[3~"); // Delete key
        assert_eq!(input.value(), "ello");
    }

    #[test]
    fn test_input_ctrl_a_and_e() {
        let mut input = Input::with_value("hello".to_string());
        input.cursor = 3;
        input.handle_input("\x01"); // Ctrl+A
        assert_eq!(input.cursor_pos(), 0);
        input.handle_input("\x05"); // Ctrl+E
        assert_eq!(input.cursor_pos(), 5);
    }

    #[test]
    fn test_truncate_no_ellipsis() {
        assert_eq!(truncate_to_width_no_ellipsis("hello", 10), "hello");
        assert_eq!(truncate_to_width_no_ellipsis("hello world", 5), "hello");
        assert_eq!(truncate_to_width_no_ellipsis("", 5), "");
    }

    #[test]
    fn test_insert_cursor() {
        // Cursor at start
        let result = insert_cursor("abc", 0, 10);
        assert!(result.starts_with("\x1b[7ma\x1b[27m"));

        // Cursor in middle
        let result = insert_cursor("abc", 1, 10);
        assert!(result.contains("a"));
        assert!(result.contains("\x1b[7mb\x1b[27m"));

        // Cursor past end
        let result = insert_cursor("abc", 5, 10);
        assert!(result.ends_with("\x1b[7m \x1b[27m"));
    }
}
