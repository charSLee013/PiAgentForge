//! CompactionSummaryMessage component — renders a compaction summary.
//!
//! Shows a `[compaction]` label with token count and collapsible summary.
//!
//! Mirrors `packages/coding-agent/src/modes/interactive/components/compaction-summary-message.ts`

use crate::Theme;
use pi_tui_core::components::spacer::Spacer;
use pi_tui_core::components::text::Text;
use pi_tui_core::{Component, Container};

/// Renders a compaction summary message with collapsed/expanded state.
///
/// Collapsed: shows `[compaction]` with token count and expand hint.
/// Expanded: shows `[compaction]` header, token count, and summary.
pub struct CompactionSummaryMessage {
    inner: Container,
}

impl CompactionSummaryMessage {
    /// Create a new compaction summary component.
    ///
    /// * `summary` — the compaction summary text.
    /// * `tokens_before` — the token count before compaction.
    /// * `expanded` — when `true`, the summary body is visible.
    /// * `theme` — application theme for styling.
    pub fn new(summary: String, tokens_before: u64, expanded: bool, theme: &Theme) -> Self {
        let mut inner = Container::new();

        if expanded {
            let label = theme.ansi(&theme.text, &theme.bold("[compaction]"));
            inner.add(Text::with_padding(label, 1, 0));
            inner.add(Spacer::new(1));

            let header = format!("Compacted from {tokens_before} tokens");
            inner.add(Text::with_padding(theme.ansi(&theme.text, &header), 1, 0));
            inner.add(Spacer::new(1));

            let trimmed = summary.trim();
            if !trimmed.is_empty() {
                inner.add(Text::with_padding(theme.ansi(&theme.text, trimmed), 1, 0));
            }
        } else {
            let line = format!(
                "{} {} {}",
                theme.ansi(&theme.text, &theme.bold("[compaction]")),
                theme.dim(&format!("Compacted from {tokens_before} tokens")),
                theme.dim("(expand to view)"),
            );
            inner.add(Text::with_padding(line, 1, 0));
        }

        Self { inner }
    }
}

impl Component for CompactionSummaryMessage {
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
    fn test_compaction_summary_expanded() {
        let theme = Theme::dark();
        let msg = CompactionSummaryMessage::new("Removed old context".into(), 15000, true, &theme);
        let lines = msg.render(80);
        assert!(lines.iter().any(|l| l.contains("[compaction]")));
        assert!(lines.iter().any(|l| l.contains("15000")));
        assert!(lines.iter().any(|l| l.contains("Removed old context")));
    }

    #[test]
    fn test_compaction_summary_collapsed() {
        let theme = Theme::dark();
        let msg = CompactionSummaryMessage::new("Hidden body".into(), 50000, false, &theme);
        let lines = msg.render(80);
        assert!(lines.iter().any(|l| l.contains("[compaction]")));
        assert!(lines.iter().any(|l| l.contains("50000")));
        assert!(!lines.iter().any(|l| l.contains("Hidden body")));
    }
}
