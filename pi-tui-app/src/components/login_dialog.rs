//! Login dialog component.
//!
//! Replaces the main display during OAuth login flow.  Can show a URL,
//! prompt for text input, or display informational messages.

use crate::Theme;
use pi_tui_core::component::Component;
use pi_tui_core::components::input::Input;
use pi_tui_core::keys::{matches_key, parse_key};

/// Multi-purpose login dialog component.
pub struct LoginDialog {
    /// Inner text input for manual entry.
    input: Input,
    /// Provider name / title.
    title: String,
    /// Application theme for styling.
    theme: Theme,
    /// Lines currently displayed in the content area.
    content_lines: Vec<String>,
    /// Current input prompt text.
    prompt: String,
    /// Whether we are waiting for input.
    awaiting_input: bool,
    /// Callback when the user submits input.
    pub on_submit: Option<Box<dyn FnMut(String) + Send>>,
    /// Callback when the user cancels.
    pub on_cancel: Option<Box<dyn FnMut() + Send>>,
}

impl LoginDialog {
    /// Create a new login dialog.
    pub fn new(title: String, theme: &Theme) -> Self {
        Self {
            input: Input::new(),
            title,
            theme: theme.clone(),
            content_lines: Vec::new(),
            prompt: String::new(),
            awaiting_input: false,
            on_submit: None,
            on_cancel: None,
        }
    }

    /// Show a URL with click hint.
    pub fn show_auth_url(&mut self, url: &str, instructions: Option<&str>) {
        self.content_lines.clear();
        self.content_lines.push(url.to_string());
        self.content_lines.push("Ctrl+click to open".to_string());
        if let Some(instr) = instructions {
            self.content_lines.push(instr.to_string());
        }
        self.awaiting_input = false;
    }

    /// Show a prompt and wait for text input.
    pub fn show_prompt(&mut self, prompt: &str, placeholder: Option<&str>) {
        self.content_lines.push(prompt.to_string());
        if let Some(ph) = placeholder {
            self.content_lines.push(format!("e.g., {}", ph));
        }
        self.prompt = prompt.to_string();
        self.input.set_value(String::new());
        self.awaiting_input = true;
    }

    /// Show informational text without prompting.
    pub fn show_info(&mut self, lines: Vec<String>) {
        self.content_lines.clear();
        self.content_lines.extend(lines);
        self.awaiting_input = false;
    }

    /// Show a waiting message.
    pub fn show_waiting(&mut self, message: &str) {
        self.content_lines.push(message.to_string());
        self.awaiting_input = false;
    }

    /// Set a status/progress message.
    pub fn show_progress(&mut self, message: &str) {
        self.content_lines.push(message.to_string());
    }
}

impl Component for LoginDialog {
    fn render(&self, width: u16) -> Vec<String> {
        let w = width as usize;
        let mut lines: Vec<String> = Vec::new();

        // Top border
        lines.push(self.theme.ansi(&self.theme.border, &"\u{2500}".repeat(w.saturating_sub(1))));

        // Title
        lines.push(self.theme.ansi(&self.theme.primary, &self.theme.bold(&self.title)));
        lines.push(String::new());

        // Content
        for line in &self.content_lines {
            lines.push(line.clone());
        }

        // Input area if awaiting input
        if self.awaiting_input {
            lines.push(String::new());
            lines.extend(self.input.render(width));
        }

        // Bottom border
        lines.push(String::new());
        lines.push(self.theme.ansi(&self.theme.border, &"\u{2500}".repeat(w.saturating_sub(1))));

        lines
    }

    fn handle_input(&mut self, data: &str) {
        let event = parse_key(data);

        if matches_key(&event, "escape") {
            if let Some(ref mut cb) = self.on_cancel {
                cb();
            }
            return;
        }

        if self.awaiting_input {
            self.input.handle_input(data);
        }
    }

    fn invalidate(&mut self) {
        self.input.invalidate();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Theme;

    #[test]
    fn test_login_dialog_renders_title() {
        let theme = Theme::dark();
        let dialog = LoginDialog::new("Login to Anthropic".into(), &theme);
        let lines = dialog.render(80);
        let joined = lines.join(" ");
        assert!(joined.contains("Login"));
        assert!(joined.contains("Anthropic"));
    }

    #[test]
    fn test_login_dialog_shows_auth_url() {
        let theme = Theme::dark();
        let mut dialog = LoginDialog::new("Login".into(), &theme);
        dialog.show_auth_url("https://example.com/auth", Some("Open in browser"));
        let lines = dialog.render(80);
        let joined = lines.join(" ");
        assert!(joined.contains("example.com"));
    }

    #[test]
    fn test_login_dialog_shows_info() {
        let theme = Theme::dark();
        let mut dialog = LoginDialog::new("Login".into(), &theme);
        dialog.show_info(vec!["Logged in successfully!".into()]);
        let lines = dialog.render(80);
        let joined = lines.join(" ");
        assert!(joined.contains("successfully"));
    }
}
