//! CustomMessage component — renders a custom message entry from extensions.
//!
//! Uses distinct styling to differentiate from user messages.
//!
//! Mirrors `packages/coding-agent/src/modes/interactive/components/custom-message.ts`

use crate::Theme;
use pi_tui_core::components::spacer::Spacer;
use pi_tui_core::components::text::Text;
use pi_tui_core::{Component, Container};

/// Renders a custom message with a label header and content body.
///
/// Supports an expanded/collapsed state (collapsed shows only the label).
pub struct CustomMessage {
    inner: Container,
}

impl CustomMessage {
    /// Create a new custom message component.
    ///
    /// * `custom_type` — the type label shown in brackets (e.g. `"info"`).
    /// * `content` — the message content text.
    /// * `expanded` — when `false`, only the label is visible.
    /// * `theme` — application theme for styling.
    pub fn new(custom_type: String, content: String, expanded: bool, theme: &Theme) -> Self {
        let mut inner = Container::new();

        inner.add(Spacer::new(1));

        if expanded {
            // Label: [type] in bold
            let label = theme.ansi(&theme.text, &theme.bold(&format!("[{}]", custom_type)));
            inner.add(Text::with_padding(label, 1, 0));
            inner.add(Spacer::new(1));

            // Content text
            if !content.trim().is_empty() {
                inner.add(Text::with_padding(theme.ansi(&theme.text, content.trim()), 1, 0));
            }
        } else {
            // Collapsed: just the label
            let label = theme.ansi(&theme.dim, &format!("[{}] (expand to view)", custom_type));
            inner.add(Text::with_padding(label, 1, 0));
        }

        Self { inner }
    }
}

impl Component for CustomMessage {
    fn render(&self, width: u16) -> Vec<String> {
        self.inner.render(width)
    }

    fn invalidate(&mut self) {
        self.inner.invalidate();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_custom_message_expanded() {
        let theme = Theme::dark();
        let msg = CustomMessage::new("info".into(), "Some content".into(), true, &theme);
        let lines = msg.render(80);
        assert!(!lines.is_empty());
        assert!(lines.iter().any(|l| l.contains("[info]")));
        assert!(lines.iter().any(|l| l.contains("Some content")));
    }

    #[test]
    fn test_custom_message_collapsed() {
        let theme = Theme::dark();
        let msg = CustomMessage::new("warning".into(), "Hidden content".into(), false, &theme);
        let lines = msg.render(80);
        assert!(!lines.is_empty());
        assert!(lines.iter().any(|l| l.contains("[warning]")));
        // Content should not appear when collapsed
        assert!(!lines.iter().any(|l| l.contains("Hidden content")));
    }

    #[test]
    fn test_custom_message_empty_content() {
        let theme = Theme::dark();
        let msg = CustomMessage::new("test".into(), String::new(), true, &theme);
        let lines = msg.render(80);
        assert!(!lines.is_empty());
        assert!(lines.iter().any(|l| l.contains("[test]")));
    }
}
