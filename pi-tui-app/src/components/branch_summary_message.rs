//! BranchSummaryMessage component — renders a branch summary.
//!
//! Shows a `[branch]` label with collapsible summary content.
//!
//! Mirrors `packages/coding-agent/src/modes/interactive/components/branch-summary-message.ts`

use pi_tui_core::components::spacer::Spacer;
use pi_tui_core::components::text::Text;
use pi_tui_core::{Component, Container};
use crate::Theme;

/// Renders a branch summary message with collapsed/expanded state.
///
/// Collapsed: shows `[branch]` with an expand hint.
/// Expanded: shows `[branch]` header followed by the summary content.
pub struct BranchSummaryMessage {
    inner: Container,
}

impl BranchSummaryMessage {
    /// Create a new branch summary component.
    ///
    /// * `summary` — the branch summary text.
    /// * `expanded` — when `true`, the summary body is visible.
    /// * `theme` — application theme for styling.
    pub fn new(summary: String, expanded: bool, theme: &Theme) -> Self {
        let mut inner = Container::new();

        if expanded {
            let label = theme.ansi(&theme.text, &theme.bold("[branch]"));
            inner.add(Text::with_padding(label, 1, 0));
            inner.add(Spacer::new(1));

            let trimmed = summary.trim();
            if !trimmed.is_empty() {
                inner.add(Text::with_padding(
                    theme.ansi(&theme.text, trimmed),
                    1,
                    0,
                ));
            }
        } else {
            let line = format!(
                "{} {}",
                theme.ansi(&theme.text, &theme.bold("[branch]")),
                theme.dim("(expand to view)"),
            );
            inner.add(Text::with_padding(line, 1, 0));
        }

        Self { inner }
    }
}

impl Component for BranchSummaryMessage {
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
    fn test_branch_summary_expanded() {
        let theme = Theme::dark();
        let msg = BranchSummaryMessage::new("Created new feature".into(), true, &theme);
        let lines = msg.render(80);
        assert!(lines.iter().any(|l| l.contains("[branch]")));
        assert!(lines.iter().any(|l| l.contains("Created new feature")));
    }

    #[test]
    fn test_branch_summary_collapsed() {
        let theme = Theme::dark();
        let msg = BranchSummaryMessage::new("Hidden".into(), false, &theme);
        let lines = msg.render(80);
        assert!(lines.iter().any(|l| l.contains("[branch]")));
        assert!(!lines.iter().any(|l| l.contains("Hidden")));
    }
}
