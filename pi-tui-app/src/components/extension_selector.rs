//! Extension selector component.
//!
//! A generic list selector for extension options with title and keybinding hints.

use crate::Theme;
use pi_tui_core::component::Component;
use pi_tui_core::keys::{matches_key, parse_key};
use pi_tui_core::utils::truncate_to_width;

type ExtensionSelectCallback = Box<dyn FnMut(&str) + Send>;

/// A generic extension/option selector.
pub struct ExtensionSelector {
    /// List of option strings.
    options: Vec<String>,
    /// Index of the currently selected option.
    selected_index: usize,
    /// Title displayed at the top.
    title: String,
    /// Application theme for styling.
    theme: Theme,
    /// Callback when an option is selected.
    pub on_select: Option<ExtensionSelectCallback>,
    /// Callback when selection is cancelled.
    pub on_cancel: Option<Box<dyn FnMut() + Send>>,
}

impl ExtensionSelector {
    /// Create a new extension selector.
    pub fn new(title: String, options: Vec<String>, theme: &Theme) -> Self {
        Self { options, selected_index: 0, title, theme: theme.clone(), on_select: None, on_cancel: None }
    }

    /// Return the currently selected option text.
    pub fn selected(&self) -> Option<&str> {
        self.options.get(self.selected_index).map(|s| s.as_str())
    }

    /// Set the selected index (clamped).
    pub fn set_selected_index(&mut self, index: usize) {
        self.selected_index = index.min(self.options.len().saturating_sub(1));
    }
}

impl Component for ExtensionSelector {
    fn render(&self, width: u16) -> Vec<String> {
        let w = width as usize;
        let mut lines: Vec<String> = Vec::new();

        // Title
        let title_styled = self.theme.ansi(&self.theme.primary, &self.theme.bold(&self.title));
        lines.push(title_styled);

        // Empty line
        lines.push(String::new());

        if self.options.is_empty() {
            lines.push(format!("  {}", self.theme.ansi(&self.theme.dim, "No options available")));
            return lines;
        }

        // Render visible options (max 10)
        let max_visible = 10usize.min(self.options.len());
        let start = self.selected_index.saturating_sub(max_visible / 2);
        let start = start.min(self.options.len().saturating_sub(max_visible));
        let end = (start + max_visible).min(self.options.len());

        for i in start..end {
            let is_selected = i == self.selected_index;
            let option = &self.options[i];
            let truncated = truncate_to_width(option, w.saturating_sub(4));
            if is_selected {
                let line = format!(
                    "\x1b[38;2;{};{};{}m\u{2192} {}\x1b[39m",
                    hex_r(&self.theme.primary),
                    hex_g(&self.theme.primary),
                    hex_b(&self.theme.primary),
                    truncated
                );
                lines.push(line);
            } else {
                lines.push(format!("  {}", truncated));
            }
        }

        // Scroll indicator
        if max_visible < self.options.len() {
            let scroll =
                self.theme.ansi(&self.theme.muted, &format!("  ({}/{})", self.selected_index + 1, self.options.len()));
            lines.push(scroll);
        }

        // Hint line
        lines.push(String::new());
        let hint = self.theme.ansi(&self.theme.dim, "\u{2191}\u{2193} navigate  Enter select  Esc cancel");
        lines.push(truncate_to_width(&hint, w));

        lines
    }

    fn handle_input(&mut self, data: &str) {
        let event = parse_key(data);

        if matches_key(&event, "up") {
            if self.options.is_empty() {
                return;
            }
            self.selected_index =
                if self.selected_index == 0 { self.options.len() - 1 } else { self.selected_index - 1 };
        } else if matches_key(&event, "down") {
            if self.options.is_empty() {
                return;
            }
            self.selected_index =
                if self.selected_index == self.options.len() - 1 { 0 } else { self.selected_index + 1 };
        } else if matches_key(&event, "enter") {
            if let Some(opt) = self.options.get(self.selected_index).cloned() {
                if let Some(ref mut cb) = self.on_select {
                    cb(&opt);
                }
            }
        } else if matches_key(&event, "escape") {
            if let Some(ref mut cb) = self.on_cancel {
                cb();
            }
        }
    }

    fn invalidate(&mut self) {}
}

fn hex_r(hex: &str) -> u8 {
    let h = hex.trim_start_matches('#');
    u8::from_str_radix(&h[0..2], 16).unwrap_or(0)
}
fn hex_g(hex: &str) -> u8 {
    let h = hex.trim_start_matches('#');
    u8::from_str_radix(&h[2..4], 16).unwrap_or(0)
}
fn hex_b(hex: &str) -> u8 {
    let h = hex.trim_start_matches('#');
    u8::from_str_radix(&h[4..6], 16).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Theme;

    #[test]
    fn test_extension_selector_renders_options() {
        let theme = Theme::dark();
        let options = vec!["option1".into(), "option2".into(), "option3".into()];
        let selector = ExtensionSelector::new("Test".into(), options, &theme);
        let lines = selector.render(80);
        let joined = lines.join(" ");
        assert!(joined.contains("option1"));
        assert!(joined.contains("option2"));
        assert!(joined.contains("option3"));
    }

    #[test]
    fn test_extension_selector_navigation() {
        let theme = Theme::dark();
        let options = vec!["a".into(), "b".into(), "c".into()];
        let mut selector = ExtensionSelector::new("Test".into(), options, &theme);
        assert_eq!(selector.selected_index, 0);

        selector.handle_input("\x1b[B"); // Down
        assert_eq!(selector.selected_index, 1);

        selector.handle_input("\x1b[B"); // Down
        assert_eq!(selector.selected_index, 2);

        selector.handle_input("\x1b[B"); // Down (wraps)
        assert_eq!(selector.selected_index, 0);
    }

    #[test]
    fn test_extension_selector_empty() {
        let theme = Theme::dark();
        let selector = ExtensionSelector::new("Test".into(), vec![], &theme);
        let lines = selector.render(80);
        let joined = lines.join(" ");
        assert!(joined.contains("No options"));
    }
}
