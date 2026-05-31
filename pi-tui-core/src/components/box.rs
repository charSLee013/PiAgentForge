//! Box component — bordered container with padding.
//!
//! Draws a box around child content using Unicode box-drawing characters:
//! ```text
//! ┌────────────┐
//! │  content   │
//! │  goes here │
//! └────────────┘
//! ```

use crate::component::Component;
use crate::utils::visible_width;

/// A bordered container that renders children inside a box drawn with
/// Unicode box-drawing characters (│ ─ ┌ ┐ └ ┘).
pub struct Box {
    children: Vec<std::boxed::Box<dyn Component>>,
    pub padding_x: u16,
    pub padding_y: u16,
}

impl Box {
    pub fn new(padding_x: u16, padding_y: u16) -> Self {
        Self { children: Vec::new(), padding_x, padding_y }
    }

    pub fn add(&mut self, child: impl Component + 'static) {
        self.children.push(std::boxed::Box::new(child));
    }

    pub fn add_boxed(&mut self, child: std::boxed::Box<dyn Component>) {
        self.children.push(child);
    }

    pub fn clear(&mut self) {
        self.children.clear();
    }

    pub fn child_count(&self) -> usize {
        self.children.len()
    }
}

impl Component for Box {
    fn render(&self, width: u16) -> Vec<String> {
        let w = width as usize;

        // Compute content width: total - 2 borders - 2 * padding
        let content_width = w.saturating_sub(2 + 2 * self.padding_x as usize);
        if content_width == 0 || w < 3 {
            // Not enough room — render an empty box
            if w < 2 {
                return vec![];
            }
            return vec![
                format!("┌{}┐", "─".repeat(w.saturating_sub(2))),
                format!("└{}┘", "─".repeat(w.saturating_sub(2))),
            ];
        }

        // Render children at content width
        let mut child_lines: Vec<String> = Vec::new();
        for child in &self.children {
            let lines = Component::render(child.as_ref(), content_width as u16);
            child_lines.extend(lines);
        }

        let mut result: Vec<String> = Vec::new();

        // Top border
        result.push(format!("┌{}┐", "─".repeat(w - 2)));

        // Top padding
        let empty_content = " ".repeat(content_width);
        for _ in 0..self.padding_y {
            result.push(self.format_line(&empty_content, w));
        }

        // Content lines
        for line in &child_lines {
            let line_padded = self.pad_to(content_width, line);
            result.push(self.format_line(&line_padded, w));
        }

        // Bottom padding
        for _ in 0..self.padding_y {
            result.push(self.format_line(&empty_content, w));
        }

        // Bottom border
        result.push(format!("└{}┘", "─".repeat(w - 2)));

        result
    }

    fn invalidate(&mut self) {
        for child in &mut self.children {
            Component::invalidate(child.as_mut());
        }
    }
}

impl Box {
    /// Format a content line with borders and side padding.
    fn format_line(&self, content: &str, total_width: usize) -> String {
        let inner = total_width.saturating_sub(2); // space between borders
        let content_vis = visible_width(content);
        let pad_needed = inner.saturating_sub(content_vis);
        format!("│{}{}{}│", " ".repeat(self.padding_x as usize), content, " ".repeat(pad_needed))
    }

    /// Pad a string to exactly `target_width` visible columns.
    fn pad_to(&self, target_width: usize, s: &str) -> String {
        let vis = visible_width(s);
        if vis < target_width { format!("{}{}", s, " ".repeat(target_width - vis)) } else { s.to_string() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A simple component that returns fixed lines.
    struct TestComp {
        lines: Vec<String>,
    }

    impl Component for TestComp {
        fn render(&self, _width: u16) -> Vec<String> {
            self.lines.clone()
        }
        fn invalidate(&mut self) {}
    }

    #[test]
    fn test_box_draws_borders() {
        let mut b = Box::new(1, 0);
        b.add(TestComp { lines: vec!["hello".to_string()] });
        let lines = b.render(20);
        // Minimum: top border + content + bottom border = 3 lines
        assert!(lines.len() >= 3);
        // Top border starts with ┌
        assert!(lines[0].starts_with('┌'));
        // Top border ends with ┐
        assert!(lines[0].ends_with('┐'));
        // Content line starts with │
        assert!(lines[1].starts_with('│'));
        // Content line ends with │
        assert!(lines[1].ends_with('│'));
        // Bottom border starts with └
        assert!(lines.last().unwrap().starts_with('└'));
        // Bottom border ends with ┘
        assert!(lines.last().unwrap().ends_with('┘'));
    }

    #[test]
    fn test_box_width_respected() {
        let mut b = Box::new(0, 0);
        b.add(TestComp { lines: vec!["x".to_string()] });
        let w = 10u16;
        let lines = b.render(w);
        for line in &lines {
            assert_eq!(visible_width(line), w as usize, "line {line:?} has unexpected width");
        }
    }

    #[test]
    fn test_box_empty_no_children() {
        let b = Box::new(1, 0);
        let lines = b.render(20);
        // Even without children, borders are drawn
        assert_eq!(lines.len(), 2); // top + bottom border only
        assert_eq!(visible_width(&lines[0]), 20);
        assert_eq!(visible_width(&lines[1]), 20);
    }

    #[test]
    fn test_box_padding_y() {
        let mut b = Box::new(1, 2);
        b.add(TestComp { lines: vec!["hi".to_string()] });
        let lines = b.render(20);
        // top border + 2 padding + content + 2 padding + bottom border = 7
        assert_eq!(lines.len(), 7);
        // Padding lines should be │ followed by spaces then │
        assert!(lines[1].starts_with('│'));
        assert!(lines[1].ends_with('│'));
    }

    #[test]
    fn test_box_very_narrow() {
        let mut b = Box::new(0, 0);
        b.add(TestComp { lines: vec!["x".to_string()] });
        let lines = b.render(2);
        // At width 2, we have ┌┐ and └┘
        assert_eq!(lines.len(), 2);
    }

    #[test]
    fn test_box_clear() {
        let mut b = Box::new(1, 0);
        b.add(TestComp { lines: vec!["text".to_string()] });
        assert_eq!(b.child_count(), 1);
        b.clear();
        assert_eq!(b.child_count(), 0);
    }
}
