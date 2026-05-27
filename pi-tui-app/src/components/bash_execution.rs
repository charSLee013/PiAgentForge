//! BashExecution component — displays bash command execution with streaming output.
//!
//! Shows the command header, stdout/stderr output, and a status line with the
//! exit code.
//!
//! Mirrors `packages/coding-agent/src/modes/interactive/components/bash-execution.ts`

use pi_tui_core::components::spacer::Spacer;
use pi_tui_core::components::text::Text;
use pi_tui_core::{Component, Container};
use crate::Theme;

/// Execution status for a bash command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionStatus {
    /// Command is still running.
    Running,
    /// Command completed successfully.
    Complete,
    /// Command was cancelled.
    Cancelled,
    /// Command finished with a non-zero exit code.
    Error,
}

/// Renders a bash command execution display with header, output, and status.
pub struct BashExecution {
    inner: Container,
}

impl BashExecution {
    /// Create a new bash execution component.
    ///
    /// * `command` — the shell command that was / is being executed.
    /// * `output` — the combined stdout/stderr output.
    /// * `status` — current execution status.
    /// * `exit_code` — the exit code (available when status is Complete or Error).
    /// * `expanded` — when `true`, the full output is shown; otherwise a preview.
    /// * `exclude_from_context` — when `true`, uses dimmer styling.
    /// * `theme` — application theme for styling.
    pub fn new(
        command: String,
        output: String,
        status: ExecutionStatus,
        exit_code: Option<i32>,
        expanded: bool,
        exclude_from_context: bool,
        theme: &Theme,
    ) -> Self {
        let mut inner = Container::new();
        let color_key = if exclude_from_context { theme.dim.as_str() } else { theme.bash_mode.as_str() };

        inner.add(Spacer::new(1));

        // Top border line
        let border = theme.ansi(color_key, "\u{2500}".repeat(60).as_str());
        inner.add(Text::with_padding(border, 1, 0));

        // Command header
        let header = theme.ansi(color_key, &theme.bold(&format!("$ {command}")));
        inner.add(Text::with_padding(header, 1, 0));

        // Output
        let trimmed_output = output.trim();
        if !trimmed_output.is_empty() {
            let output_lines: Vec<&str> = trimmed_output.lines().collect();
            let show_lines = if expanded {
                &output_lines[..]
            } else {
                // Show last 20 lines as preview
                let start = output_lines.len().saturating_sub(20);
                &output_lines[start..]
            };

            let hidden_count = output_lines.len().saturating_sub(show_lines.len());
            let output_text = show_lines.join("\n");
            let styled_output = theme.ansi(color_key, &output_text);
            inner.add(Text::with_padding(format!("\n{styled_output}"), 1, 0));

            if hidden_count > 0 && !expanded {
                let hint = theme.dim(&format!("... {hidden_count} more lines"));
                inner.add(Text::with_padding(hint, 1, 0));
            }
        }

        // Status line
        match status {
            ExecutionStatus::Running => {
                let status_text = theme.ansi(color_key, "Running...");
                inner.add(Text::with_padding(status_text, 1, 0));
            }
            ExecutionStatus::Complete => {
                let status_text = theme.dim("(completed)");
                inner.add(Text::with_padding(status_text, 1, 0));
            }
            ExecutionStatus::Cancelled => {
                let status_text = theme.ansi(&theme.warning, "(cancelled)");
                inner.add(Text::with_padding(status_text, 1, 0));
            }
            ExecutionStatus::Error => {
                let code = exit_code.unwrap_or(-1);
                let status_text = theme.ansi(&theme.error, &format!("(exit {code})"));
                inner.add(Text::with_padding(status_text, 1, 0));
            }
        }

        // Bottom border
        let border = theme.ansi(color_key, "\u{2500}".repeat(60).as_str());
        inner.add(Text::with_padding(border, 1, 0));

        Self { inner }
    }
}

impl Component for BashExecution {
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
    fn test_bash_execution_complete() {
        let theme = Theme::dark();
        let comp = BashExecution::new(
            "ls -la".into(),
            "file1\nfile2\nfile3".into(),
            ExecutionStatus::Complete,
            Some(0),
            true,
            false,
            &theme,
        );
        let lines = comp.render(80);
        assert!(!lines.is_empty());
        assert!(lines.iter().any(|l| l.contains("ls -la")));
        assert!(lines.iter().any(|l| l.contains("file1")));
        assert!(lines.iter().any(|l| l.contains("file2")));
        assert!(lines.iter().any(|l| l.contains("file3")));
    }

    #[test]
    fn test_bash_execution_error() {
        let theme = Theme::dark();
        let comp = BashExecution::new(
            "false".into(),
            String::new(),
            ExecutionStatus::Error,
            Some(1),
            true,
            false,
            &theme,
        );
        let lines = comp.render(80);
        assert!(lines.iter().any(|l| l.contains("(exit 1)")));
    }

    #[test]
    fn test_bash_execution_cancelled() {
        let theme = Theme::dark();
        let comp = BashExecution::new(
            "sleep 10".into(),
            String::new(),
            ExecutionStatus::Cancelled,
            None,
            true,
            false,
            &theme,
        );
        let lines = comp.render(80);
        assert!(lines.iter().any(|l| l.contains("(cancelled)")));
    }

    #[test]
    fn test_bash_execution_excluded() {
        let theme = Theme::dark();
        let comp = BashExecution::new(
            "secret".into(),
            String::new(),
            ExecutionStatus::Complete,
            Some(0),
            true,
            true,
            &theme,
        );
        let lines = comp.render(80);
        assert!(!lines.is_empty());
        // Should still show command
        assert!(lines.iter().any(|l| l.contains("secret")));
    }
}
