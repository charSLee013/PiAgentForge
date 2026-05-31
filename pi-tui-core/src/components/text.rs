//! Text component — displays multi-line text with word wrapping.
//!
//! Mirrors `packages/tui/src/components/text.ts`

use crate::component::Component;
use crate::utils::{visible_width, wrap_text_with_ansi};

/// A text component that displays wrapped, padded content.
///
/// Text is word-wrapped to fit the available content width (viewport width
/// minus horizontal padding).  Vertical padding adds blank lines above and
/// below the content.
pub struct Text {
    pub content: String,
    pub padding_x: u16,
    pub padding_y: u16,
}

impl Text {
    pub fn new(content: String) -> Self {
        Self { content, padding_x: 1, padding_y: 1 }
    }

    pub fn with_padding(content: String, padding_x: u16, padding_y: u16) -> Self {
        Self { content, padding_x, padding_y }
    }

    pub fn set_content(&mut self, content: String) {
        self.content = content;
    }

    pub fn set_padding_x(&mut self, padding_x: u16) {
        self.padding_x = padding_x;
    }

    pub fn set_padding_y(&mut self, padding_y: u16) {
        self.padding_y = padding_y;
    }
}

impl Component for Text {
    fn render(&self, width: u16) -> Vec<String> {
        let w = width as usize;

        // Nothing to render for empty/whitespace-only text
        if self.content.is_empty() || self.content.trim().is_empty() {
            return vec![];
        }

        // Compute available content width after horizontal padding
        let content_width = w.saturating_sub(self.padding_x as usize * 2);
        if content_width == 0 {
            return vec![];
        }

        // Normalize tabs and wrap
        let normalized = self.content.replace('\t', "   ");
        let wrapped = wrap_text_with_ansi(&normalized, content_width);

        let left_margin = self.padding_x as usize;
        let mut content_lines: Vec<String> = Vec::new();

        for line in &wrapped {
            let line_vis = visible_width(line);
            let right_pad = w.saturating_sub(left_margin + line_vis);
            content_lines.push(format!("{}{}{}", " ".repeat(left_margin), line, " ".repeat(right_pad)));
        }

        // Vertical padding (empty lines)
        let empty_line = " ".repeat(w);
        let mut result: Vec<String> = Vec::new();

        // Top padding
        for _ in 0..self.padding_y {
            result.push(empty_line.clone());
        }

        // Content
        result.extend(content_lines);

        // Bottom padding
        for _ in 0..self.padding_y {
            result.push(empty_line.clone());
        }

        result
    }

    fn invalidate(&mut self) {
        // No cached state
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_text_renders_content() {
        let t = Text::new("Hello".to_string());
        let lines = t.render(80);
        assert!(!lines.is_empty());
        // Content should appear (after padding + wrapping)
        let joined = lines.join("");
        assert!(joined.contains("Hello"));
    }

    #[test]
    fn test_text_respects_width() {
        let t = Text::with_padding("hello world".to_string(), 0, 0);
        let lines = t.render(5);
        // At 5 columns width with 0 padding, "hello" should wrap to 5 chars per line.
        // "hello" = 5 cols fits, " world" = 6 cols -> wraps, so we get 2+ lines.
        assert!(lines.len() >= 2);
        for line in &lines {
            assert!(visible_width(line) <= 5, "line {line:?} exceeds width 5");
        }
    }

    #[test]
    fn test_text_empty() {
        let t = Text::new(String::new());
        let lines = t.render(80);
        assert!(lines.is_empty());
    }

    #[test]
    fn test_text_whitespace_only() {
        let t = Text::new("   ".to_string());
        let lines = t.render(80);
        assert!(lines.is_empty());
    }

    #[test]
    fn test_text_with_padding() {
        let t = Text::with_padding("Hi".to_string(), 2, 1);
        let lines = t.render(20);
        // Expected: 1 top padding line + 1 content line + 1 bottom padding line = 3
        assert_eq!(lines.len(), 3);
        // Content line should have left padding of 2
        assert_eq!(&lines[1][..2], "  ");
        assert!(lines[1].contains("Hi"));
        // Padding lines should be full width
        assert_eq!(visible_width(&lines[0]), 20);
        assert_eq!(visible_width(&lines[2]), 20);
    }

    #[test]
    fn test_text_zero_width() {
        let t = Text::new("Hello".to_string());
        let lines = t.render(0);
        assert!(lines.is_empty());
    }

    #[test]
    fn test_text_set_content() {
        let mut t = Text::new("Hello".to_string());
        t.set_content("World".to_string());
        let lines = t.render(80);
        let joined = lines.join("");
        assert!(!joined.contains("Hello"));
        assert!(joined.contains("World"));
    }
}
