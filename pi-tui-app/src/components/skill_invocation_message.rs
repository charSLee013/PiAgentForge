//! SkillInvocationMessage component — renders a skill invocation message.
//!
//! Shows a `[skill]` label with the skill name and collapsible content.
//!
//! Mirrors `packages/coding-agent/src/modes/interactive/components/skill-invocation-message.ts`

use pi_tui_core::components::spacer::Spacer;
use pi_tui_core::components::text::Text;
use pi_tui_core::{Component, Container};
use crate::Theme;

/// Renders a skill invocation message with collapsed/expanded state.
///
/// Collapsed: shows `[skill] name` with expand hint.
/// Expanded: shows `[skill]` header, skill name, and full content.
pub struct SkillInvocationMessage {
    inner: Container,
}

impl SkillInvocationMessage {
    /// Create a new skill invocation component.
    ///
    /// * `skill_name` — the name of the invoked skill.
    /// * `content` — the full skill block content.
    /// * `expanded` — when `true`, the content body is visible.
    /// * `theme` — application theme for styling.
    pub fn new(skill_name: String, content: String, expanded: bool, theme: &Theme) -> Self {
        let mut inner = Container::new();

        if expanded {
            let label = theme.ansi(&theme.text, &theme.bold("[skill]"));
            inner.add(Text::with_padding(label, 1, 0));

            let header = theme.ansi(&theme.text, &theme.bold(&skill_name));
            inner.add(Text::with_padding(header, 1, 0));
            inner.add(Spacer::new(1));

            let trimmed = content.trim();
            if !trimmed.is_empty() {
                inner.add(Text::with_padding(
                    theme.ansi(&theme.text, trimmed),
                    1,
                    0,
                ));
            }
        } else {
            let line = format!(
                "{} {} {}",
                theme.ansi(&theme.text, &theme.bold("[skill]")),
                theme.ansi(&theme.text, &skill_name),
                theme.dim("(expand to view)"),
            );
            inner.add(Text::with_padding(line, 1, 0));
        }

        Self { inner }
    }
}

impl Component for SkillInvocationMessage {
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
    fn test_skill_invocation_expanded() {
        let theme = Theme::dark();
        let msg = SkillInvocationMessage::new(
            "ctf-web".into(),
            "Run SQLMap on target".into(),
            true,
            &theme,
        );
        let lines = msg.render(80);
        assert!(lines.iter().any(|l| l.contains("[skill]")));
        assert!(lines.iter().any(|l| l.contains("ctf-web")));
        assert!(lines.iter().any(|l| l.contains("Run SQLMap on target")));
    }

    #[test]
    fn test_skill_invocation_collapsed() {
        let theme = Theme::dark();
        let msg = SkillInvocationMessage::new(
            "ctf-reverse".into(),
            "Analyze binary".into(),
            false,
            &theme,
        );
        let lines = msg.render(80);
        assert!(lines.iter().any(|l| l.contains("[skill]")));
        assert!(lines.iter().any(|l| l.contains("ctf-reverse")));
        assert!(!lines.iter().any(|l| l.contains("Analyze binary")));
    }

    #[test]
    fn test_skill_invocation_empty_content() {
        let theme = Theme::dark();
        let msg = SkillInvocationMessage::new("test".into(), String::new(), true, &theme);
        let lines = msg.render(80);
        assert!(lines.iter().any(|l| l.contains("[skill]")));
    }
}
