//! ToolExecution component — displays tool call execution.
//!
//! Shows the tool name header, arguments, and result content.
//!
//! Mirrors `packages/coding-agent/src/modes/interactive/components/tool-execution.ts`

use pi_tui_core::components::spacer::Spacer;
use pi_tui_core::components::text::Text;
use pi_tui_core::{Component, Container};
use crate::Theme;

/// Rendering state for tool execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolState {
    /// Tool call is pending (not yet executed).
    Pending,
    /// Tool is currently executing.
    Running,
    /// Tool completed successfully.
    Success,
    /// Tool completed with an error.
    Error,
}

/// Renders a tool call execution display with name, formatted arguments, and
/// optional result content.
pub struct ToolExecution {
    inner: Container,
}

impl ToolExecution {
    /// Create a new tool execution component.
    ///
    /// * `tool_name` — the name of the tool (e.g. `"read"`, `"bash"`).
    /// * `tool_call_id` — unique identifier for this tool call.
    /// * `args` — JSON-formatted tool arguments.
    /// * `result` — optional result text content.
    /// * `state` — current tool execution state.
    /// * `expanded` — when `true`, the full result is shown.
    /// * `theme` — application theme for styling.
    pub fn new(
        tool_name: String,
        tool_call_id: String,
        args: String,
        result: Option<String>,
        state: ToolState,
        expanded: bool,
        theme: &Theme,
    ) -> Self {
        let mut inner = Container::new();

        inner.add(Spacer::new(1));

        // Background color based on state
        let bg_color = match state {
            ToolState::Pending | ToolState::Running => &theme.tool_pending_bg,
            ToolState::Error => &theme.error,
            ToolState::Success => &theme.text,
        };

        // Tool name header
        let header = theme.ansi(bg_color, &theme.bold(&format!("[{tool_name}]")));
        inner.add(Text::with_padding(header, 1, 0));

        // Tool call ID
        if !tool_call_id.is_empty() {
            let id_text = theme.dim(&format!("  \u{2192} {tool_call_id}"));
            inner.add(Text::with_padding(id_text, 1, 0));
        }

        // Arguments
        let trimmed_args = args.trim();
        if !trimmed_args.is_empty() {
            inner.add(Text::with_padding(
                theme.ansi(bg_color, trimmed_args),
                2,
                0,
            ));
        }

        // Result content
        if let Some(result_text) = result {
            let trimmed = result_text.trim();
            if !trimmed.is_empty() {
                inner.add(Spacer::new(1));
                if expanded {
                    let styled = theme.ansi(bg_color, trimmed);
                    inner.add(Text::with_padding(styled, 1, 0));
                } else {
                    // Preview: show first/last few lines
                    let lines: Vec<&str> = trimmed.lines().collect();
                    let preview_lines: Vec<&str> = if lines.len() > 20 {
                        let mut preview = Vec::new();
                        preview.extend_from_slice(&lines[..5]);
                        preview.push("...");
                        preview.extend_from_slice(&lines[lines.len().saturating_sub(5)..]);
                        preview
                    } else {
                        lines.clone()
                    };
                    let preview = preview_lines.join("\n");
                    let styled = theme.ansi(bg_color, &preview);
                    inner.add(Text::with_padding(styled, 1, 0));
                }
            }
        }

        // State indicator
        let state_text = match state {
            ToolState::Pending => theme.dim("(pending)"),
            ToolState::Running => theme.dim("(running...)"),
            ToolState::Success => theme.dim("(completed)"),
            ToolState::Error => theme.ansi(&theme.error, "(error)"),
        };
        inner.add(Spacer::new(1));
        inner.add(Text::with_padding(state_text, 1, 0));

        Self { inner }
    }
}

impl Component for ToolExecution {
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
    fn test_tool_execution_success() {
        let theme = Theme::dark();
        let comp = ToolExecution::new(
            "read".into(),
            "call_123".into(),
            r#"{"file": "test.txt"}"#.into(),
            Some("file contents".into()),
            ToolState::Success,
            true,
            &theme,
        );
        let lines = comp.render(80);
        assert!(lines.iter().any(|l| l.contains("[read]")));
        assert!(lines.iter().any(|l| l.contains("call_123")));
        assert!(lines.iter().any(|l| l.contains("file contents")));
    }

    #[test]
    fn test_tool_execution_pending() {
        let theme = Theme::dark();
        let comp = ToolExecution::new(
            "bash".into(),
            "call_456".into(),
            r#"{"command": "ls"}"#.into(),
            None,
            ToolState::Pending,
            true,
            &theme,
        );
        let lines = comp.render(80);
        assert!(lines.iter().any(|l| l.contains("[bash]")));
        assert!(lines.iter().any(|l| l.contains("(pending)")));
    }

    #[test]
    fn test_tool_execution_error() {
        let theme = Theme::dark();
        let comp = ToolExecution::new(
            "write".into(),
            "call_err".into(),
            r#"{}"#.into(),
            Some("Permission denied".into()),
            ToolState::Error,
            true,
            &theme,
        );
        let lines = comp.render(80);
        assert!(lines.iter().any(|l| l.contains("[write]")));
        assert!(lines.iter().any(|l| l.contains("Permission denied")));
        assert!(lines.iter().any(|l| l.contains("(error)")));
    }

    #[test]
    fn test_tool_execution_no_result() {
        let theme = Theme::dark();
        let comp = ToolExecution::new(
            "read".into(),
            String::new(),
            String::new(),
            None,
            ToolState::Success,
            true,
            &theme,
        );
        let lines = comp.render(80);
        assert!(lines.iter().any(|l| l.contains("[read]")));
    }
}
