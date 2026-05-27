//! Spacer component — renders empty lines.
//!
//! Mirrors `packages/tui/src/components/spacer.ts`

use crate::component::Component;

/// A spacer that renders a configurable number of empty lines.
pub struct Spacer {
    /// Number of empty lines to render.
    pub lines: u16,
}

impl Spacer {
    pub fn new(lines: u16) -> Self {
        Self { lines }
    }

    pub fn set_lines(&mut self, lines: u16) {
        self.lines = lines;
    }
}

impl Component for Spacer {
    fn render(&self, _width: u16) -> Vec<String> {
        vec![String::new(); self.lines as usize]
    }

    fn invalidate(&mut self) {
        // No cached state
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_spacer_default_lines() {
        let spacer = Spacer::new(3);
        let lines = spacer.render(80);
        assert_eq!(lines.len(), 3);
        for line in &lines {
            assert_eq!(line, "");
        }
    }

    #[test]
    fn test_spacer_zero_lines() {
        let spacer = Spacer::new(0);
        let lines = spacer.render(80);
        assert!(lines.is_empty());
    }

    #[test]
    fn test_spacer_one_line() {
        let spacer = Spacer::new(1);
        let lines = spacer.render(80);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0], "");
    }

    #[test]
    fn test_set_lines() {
        let mut spacer = Spacer::new(1);
        spacer.set_lines(5);
        assert_eq!(spacer.render(80).len(), 5);
    }
}
