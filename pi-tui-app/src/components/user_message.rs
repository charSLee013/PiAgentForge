//! UserMessage component — renders a user message with styled text.
//!
//! Mirrors `packages/coding-agent/src/modes/interactive/components/user-message.ts`

use pi_tui_core::components::spacer::Spacer;
use pi_tui_core::components::text::Text;
use pi_tui_core::{Component, Container};
use crate::Theme;

/// Renders a user message in a styled container with foreground coloring.
pub struct UserMessage {
    inner: Container,
}

impl UserMessage {
    /// Create a new user message component.
    ///
    /// * `text` — the message content (plain text, displayed as-is).
    /// * `theme` — application theme for styling.
    pub fn new(text: String, theme: &Theme) -> Self {
        let mut inner = Container::new();

        let trimmed = text.trim();
        if !trimmed.is_empty() {
            inner.add(Spacer::new(1));
            let styled = theme.ansi(&theme.text, trimmed);
            inner.add(Text::with_padding(styled, 1, 0));
        }

        Self { inner }
    }
}

impl Component for UserMessage {
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
    fn test_user_message_renders_text() {
        let theme = Theme::dark();
        let msg = UserMessage::new("Hello from user".into(), &theme);
        let lines = msg.render(80);
        assert!(!lines.is_empty());
        assert!(lines.iter().any(|l| l.contains("Hello from user")));
    }

    #[test]
    fn test_user_message_empty() {
        let theme = Theme::dark();
        let msg = UserMessage::new(String::new(), &theme);
        let lines = msg.render(80);
        assert!(lines.is_empty());
    }

    #[test]
    fn test_user_message_whitespace() {
        let theme = Theme::dark();
        let msg = UserMessage::new("   \n  ".into(), &theme);
        let lines = msg.render(80);
        assert!(lines.is_empty());
    }

    #[test]
    fn test_user_message_width() {
        let theme = Theme::dark();
        let msg = UserMessage::new("Hello".into(), &theme);
        let lines = msg.render(20);
        assert!(!lines.is_empty());
        assert!(lines.iter().any(|l| l.contains("Hello")));
    }
}
