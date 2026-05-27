//! AssistantMessage component — renders assistant message content blocks.
//!
//! Renders text blocks as Markdown, thinking blocks as italic dim text
//! (with optional hide label), and tracks tool call blocks.
//!
//! Mirrors `packages/coding-agent/src/modes/interactive/components/assistant-message.ts`

use pi_tui_core::components::markdown::Markdown;
use pi_tui_core::components::spacer::Spacer;
use pi_tui_core::components::text::Text;
use pi_tui_core::{Component, Container};
use crate::Theme;

/// A content block inside an assistant message.
#[derive(Debug, Clone)]
pub enum AssistantContentBlock {
    /// Markdown-formatted text content.
    Text(String),
    /// Thinking / reasoning content (shown in italic dim by default).
    Thinking(String),
    /// A tool call reference (name + JSON args as a string).
    ToolCall {
        name: String,
        args: String,
    },
}

/// Renders a complete assistant message with text, thinking, and tool-call blocks.
///
/// Children are built once in the constructor — no dynamic updates.
pub struct AssistantMessage {
    inner: Container,
}

impl AssistantMessage {
    /// Create a new assistant message component.
    ///
    /// * `content` — ordered list of content blocks.
    /// * `hide_thinking` — when `true`, thinking blocks are replaced by a label.
    /// * `hidden_thinking_label` — label text shown when thinking is hidden.
    /// * `stop_reason` — optional stop reason (`"aborted"`, `"error"`, or other).
    /// * `error_message` — optional error message to display with the stop reason.
    /// * `theme` — application theme for styling.
    pub fn new(
        content: Vec<AssistantContentBlock>,
        hide_thinking: bool,
        hidden_thinking_label: String,
        stop_reason: Option<String>,
        error_message: Option<String>,
        theme: &Theme,
    ) -> Self {
        let mut inner = Container::new();

        let has_visible = content.iter().any(|b| match b {
            AssistantContentBlock::Text(t) | AssistantContentBlock::Thinking(t) => {
                !t.trim().is_empty()
            }
            _ => false,
        });

        if has_visible {
            inner.add(Spacer::new(1));
        }

        let has_tool_calls = content.iter().any(|b| matches!(b, AssistantContentBlock::ToolCall { .. }));

        for (i, block) in content.iter().enumerate() {
            match block {
                AssistantContentBlock::Text(text) if !text.trim().is_empty() => {
                    inner.add(Markdown::new(
                        text.trim().to_string(),
                        theme.to_markdown_theme(),
                    ));
                }
                AssistantContentBlock::Thinking(text) if !text.trim().is_empty() => {
                    let has_after = content[i + 1..].iter().any(|b| match b {
                        AssistantContentBlock::Text(t) | AssistantContentBlock::Thinking(t) => {
                            !t.trim().is_empty()
                        }
                        _ => false,
                    });

                    if hide_thinking {
                        let label = theme.dim(&theme.italic(&hidden_thinking_label));
                        inner.add(Text::with_padding(label, 1, 0));
                    } else {
                        let styled =
                            theme.ansi(&theme.thinking_text, &theme.italic(text.trim()));
                        inner.add(Text::with_padding(styled, 1, 0));
                    }

                    if has_after {
                        inner.add(Spacer::new(1));
                    }
                }
                _ => {}
            }
        }

        // Stop-reason display (only when there are no tool calls)
        if !has_tool_calls {
            if let Some(reason) = stop_reason {
                match reason.as_str() {
                    "aborted" => {
                        let msg = error_message
                            .as_deref()
                            .filter(|m| *m != "Request was aborted")
                            .unwrap_or("Operation aborted");
                        if has_visible {
                            inner.add(Spacer::new(1));
                        }
                        inner.add(Text::with_padding(
                            theme.ansi(&theme.error, msg),
                            1,
                            0,
                        ));
                    }
                    "error" => {
                        let msg = error_message.as_deref().unwrap_or("Unknown error");
                        inner.add(Spacer::new(1));
                        inner.add(Text::with_padding(
                            theme.ansi(&theme.error, &format!("Error: {msg}")),
                            1,
                            0,
                        ));
                    }
                    _ => {}
                }
            }
        }

        Self { inner }
    }
}

impl Component for AssistantMessage {
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
    fn test_empty_content() {
        let theme = Theme::dark();
        let msg = AssistantMessage::new(vec![], false, "Thinking...".into(), None, None, &theme);
        let lines = msg.render(80);
        assert!(lines.is_empty());
    }

    #[test]
    fn test_text_block() {
        let theme = Theme::dark();
        let msg = AssistantMessage::new(
            vec![AssistantContentBlock::Text("Hello, world!".into())],
            false,
            "Thinking...".into(),
            None,
            None,
            &theme,
        );
        let lines = msg.render(80);
        assert!(!lines.is_empty());
        assert!(lines.iter().any(|l| l.contains("Hello, world!")));
    }

    #[test]
    fn test_thinking_block_visible() {
        let theme = Theme::dark();
        let msg = AssistantMessage::new(
            vec![AssistantContentBlock::Thinking("reasoning text".into())],
            false,
            "Thinking...".into(),
            None,
            None,
            &theme,
        );
        let lines = msg.render(80);
        assert!(lines.iter().any(|l| l.contains("reasoning text")));
    }

    #[test]
    fn test_thinking_block_hidden() {
        let theme = Theme::dark();
        let msg = AssistantMessage::new(
            vec![AssistantContentBlock::Thinking("hidden reasoning".into())],
            true,
            "Thinking...".into(),
            None,
            None,
            &theme,
        );
        let lines = msg.render(80);
        // The hidden content must not appear
        assert!(!lines.iter().any(|l| l.contains("hidden reasoning")));
        // The label must appear
        assert!(lines.iter().any(|l| l.contains("Thinking...")));
    }

    #[test]
    fn test_stop_reason_aborted() {
        let theme = Theme::dark();
        let msg = AssistantMessage::new(
            vec![],
            false,
            "Thinking...".into(),
            Some("aborted".into()),
            Some("Custom abort".into()),
            &theme,
        );
        let lines = msg.render(80);
        assert!(lines.iter().any(|l| l.contains("Custom abort")));
    }

    #[test]
    fn test_stop_reason_error() {
        let theme = Theme::dark();
        let msg = AssistantMessage::new(
            vec![],
            false,
            "Thinking...".into(),
            Some("error".into()),
            Some("Something broke".into()),
            &theme,
        );
        let lines = msg.render(80);
        assert!(lines.iter().any(|l| l.contains("Error")));
        assert!(lines.iter().any(|l| l.contains("Something broke")));
    }

    #[test]
    fn test_text_and_thinking() {
        let theme = Theme::dark();
        let msg = AssistantMessage::new(
            vec![
                AssistantContentBlock::Text("First block".into()),
                AssistantContentBlock::Thinking("reasoning".into()),
                AssistantContentBlock::Text("Second block".into()),
            ],
            false,
            "Thinking...".into(),
            None,
            None,
            &theme,
        );
        let lines = msg.render(80);
        assert!(lines.iter().any(|l| l.contains("First block")));
        assert!(lines.iter().any(|l| l.contains("reasoning")));
        assert!(lines.iter().any(|l| l.contains("Second block")));
    }
}
