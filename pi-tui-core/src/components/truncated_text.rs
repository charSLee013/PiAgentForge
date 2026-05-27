//! TruncatedText component — single-line text truncated to fit viewport width.
//!
//! Mirrors `packages/tui/src/components/truncated-text.ts`

use crate::component::Component;
use crate::utils::{truncate_to_width, visible_width};

/// A single-line text component that truncates with an ellipsis when the
/// content exceeds the viewport width.
pub struct TruncatedText {
    pub text: String,
    pub padding_x: u16,
    pub padding_y: u16,
}

impl TruncatedText {
    pub fn new(text: String) -> Self {
        Self {
            text,
            padding_x: 0,
            padding_y: 0,
        }
    }

    pub fn with_padding(text: String, padding_x: u16, padding_y: u16) -> Self {
        Self {
            text,
            padding_x,
            padding_y,
        }
    }

    pub fn set_text(&mut self, text: String) {
        self.text = text;
    }
}

impl Component for TruncatedText {
    fn render(&self, width: u16) -> Vec<String> {
        let w = width as usize;
        let mut result: Vec<String> = Vec::new();

        // Vertical padding above
        let empty_line = " ".repeat(w);
        for _ in 0..self.padding_y {
            result.push(empty_line.clone());
        }

        // Available width after horizontal padding
        let available = w.saturating_sub(self.padding_x as usize * 2);
        if available == 0 {
            // Still need to account for padding_y
            if result.is_empty() {
                return vec![" ".repeat(w)];
            }
            return result;
        }

        // Take only the first line (stop at newline)
        let single_line = match self.text.find('\n') {
            Some(pos) => &self.text[..pos],
            None => &self.text,
        };

        // Truncate to fit
        let display = truncate_to_width(single_line, available);

        // Recompute widths: truncate_to_width may trim further with ellipsis
        let display_vis = visible_width(&display);

        // Left padding + content + right padding to fill `w`
        let left_pad = " ".repeat(self.padding_x as usize);
        let right_pad = " ".repeat(w.saturating_sub(self.padding_x as usize + display_vis));
        result.push(format!("{}{}{}", left_pad, display, right_pad));

        // Vertical padding below
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
    fn test_truncated_text_short() {
        let t = TruncatedText::new("hello".to_string());
        let lines = t.render(80);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("hello"));
    }

    #[test]
    fn test_truncated_text_truncates() {
        let t = TruncatedText::new("hello world this is long".to_string());
        let lines = t.render(10);
        assert_eq!(lines.len(), 1);
        let vis = visible_width(&lines[0]);
        assert!(vis <= 10, "visible width {vis} > 10");
        // Should contain ellipsis when truncated
        assert!(lines[0].contains('\u{2026}') || vis == "hello world this is long".len());
    }

    #[test]
    fn test_truncated_text_fits_exactly() {
        let t = TruncatedText::new("hello".to_string());
        let lines = t.render(5);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("hello"));
    }

    #[test]
    fn test_truncated_text_only_first_line() {
        let t = TruncatedText::new("first line\nsecond line".to_string());
        let lines = t.render(80);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("first line"));
        assert!(!lines[0].contains("second line"));
    }

    #[test]
    fn test_truncated_text_padding() {
        let t = TruncatedText::with_padding("hi".to_string(), 1, 1);
        let lines = t.render(20);
        // Expect: 1 top padding, 1 content, 1 bottom padding = 3 lines
        assert_eq!(lines.len(), 3);
        // Content line should have left padding
        assert_eq!(&lines[1][..1], " ");
        assert!(lines[1].contains("hi"));
    }

    #[test]
    fn test_truncated_text_empty() {
        let t = TruncatedText::new(String::new());
        let lines = t.render(80);
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn test_set_text() {
        let mut t = TruncatedText::new("before".to_string());
        t.set_text("after".to_string());
        let lines = t.render(80);
        assert!(lines[0].contains("after"));
        assert!(!lines[0].contains("before"));
    }
}
