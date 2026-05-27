//! OAuth/API-key provider selector component.
//!
//! Shows a searchable list of auth providers the user can log into or log out of.

use pi_tui_core::component::Component;
use pi_tui_core::components::input::Input;
use pi_tui_core::fuzzy::fuzzy_filter;
use pi_tui_core::keys::{matches_key, parse_key};
use pi_tui_core::utils::truncate_to_width;
use crate::Theme;

type OAuthSelectCallback = Box<dyn FnMut(&str) + Send>;

/// An auth provider entry.
#[derive(Debug, Clone)]
pub struct AuthProvider {
    pub id: String,
    pub name: String,
    pub auth_type: String, // "oauth" or "api_key"
    pub configured: bool,
    pub status_label: Option<String>,
}

/// An OAuth/API-key provider selector component.
pub struct OAuthSelector {
    input: Input,
    all_providers: Vec<AuthProvider>,
    filtered_providers: Vec<AuthProvider>,
    selected_index: usize,
    theme: Theme,
    mode_label: String,
    /// Callback when a provider is selected.
    pub on_select: Option<OAuthSelectCallback>,
    /// Callback when cancelled.
    pub on_cancel: Option<Box<dyn FnMut() + Send>>,
}

impl OAuthSelector {
    /// Create a new OAuth provider selector.
    ///
    /// * `mode_label` — "login" or "logout" for the title.
    /// * `providers` — list of available auth providers.
    /// * `theme` — application theme for styling.
    pub fn new(mode_label: String, providers: Vec<AuthProvider>, theme: &Theme) -> Self {
        Self {
            input: Input::new(),
            filtered_providers: providers.clone(),
            all_providers: providers,
            selected_index: 0,
            theme: theme.clone(),
            mode_label,
            on_select: None,
            on_cancel: None,
        }
    }

    fn filter_providers(&mut self, query: &str) {
        if query.is_empty() {
            self.filtered_providers = self.all_providers.clone();
        } else {
            let results = fuzzy_filter(query, &self.all_providers.iter().map(|p| &p.name).collect::<Vec<_>>());
            self.filtered_providers = results
                .iter()
                .map(|(idx, _)| self.all_providers[*idx].clone())
                .collect();
        }
        self.selected_index = self
            .selected_index
            .min(self.filtered_providers.len().saturating_sub(1));
    }
}

impl Component for OAuthSelector {
    fn render(&self, width: u16) -> Vec<String> {
        let w = width as usize;
        let mut lines: Vec<String> = Vec::new();

        // Title
        let title = format!("Select provider to {}:", self.mode_label);
        let title_styled = self.theme.ansi(&self.theme.primary, &self.theme.bold(&title));
        lines.push(title_styled);
        lines.push(String::new());

        // Search input
        lines.extend(self.input.render(width));
        lines.push(String::new());

        // Provider list
        if self.filtered_providers.is_empty() {
            let msg = if self.all_providers.is_empty() {
                "No providers available"
            } else {
                "No matching providers"
            };
            lines.push(self.theme.ansi(&self.theme.muted, &format!("  {}", msg)));
            return lines;
        }

        let max_visible = 8usize.min(self.filtered_providers.len());
        let half = max_visible / 2;
        let total = self.filtered_providers.len();
        let start = if self.selected_index < half {
            0
        } else if self.selected_index + half >= total {
            total.saturating_sub(max_visible)
        } else {
            self.selected_index.saturating_sub(half)
        };
        let end = (start + max_visible).min(total);

        for i in start..end {
            let provider = &self.filtered_providers[i];
            let is_selected = i == self.selected_index;
            let truncated = truncate_to_width(&provider.name, w.saturating_sub(20));

            let indicator = if provider.configured {
                self.theme.ansi(&self.theme.success, " \u{2713} configured")
            } else {
                self.theme.ansi(&self.theme.muted, " \u{2022} unconfigured")
            };

            if is_selected {
                let line = format!(
                    "\x1b[38;2;{};{};{}m\u{2192} \x1b[39m\x1b[38;2;{};{};{}m{}\x1b[39m{}",
                    hex_r(&self.theme.primary), hex_g(&self.theme.primary), hex_b(&self.theme.primary),
                    hex_r(&self.theme.primary), hex_g(&self.theme.primary), hex_b(&self.theme.primary),
                    truncated, indicator
                );
                lines.push(line);
            } else {
                lines.push(format!("  {}{}", truncated, indicator));
            }
        }

        // Scroll indicator
        if start > 0 || end < total {
            let scroll = self.theme.ansi(&self.theme.muted,
                &format!("  ({}/{})", self.selected_index + 1, total));
            lines.push(scroll);
        }

        lines
    }

    fn handle_input(&mut self, data: &str) {
        let event = parse_key(data);

        if matches_key(&event, "up") {
            if self.filtered_providers.is_empty() { return; }
            self.selected_index = if self.selected_index == 0 {
                self.filtered_providers.len() - 1
            } else {
                self.selected_index - 1
            };
        } else if matches_key(&event, "down") {
            if self.filtered_providers.is_empty() { return; }
            self.selected_index = if self.selected_index == self.filtered_providers.len() - 1 {
                0
            } else {
                self.selected_index + 1
            };
        } else if matches_key(&event, "enter") {
            if let Some(p) = self.filtered_providers.get(self.selected_index).cloned() {
                if let Some(ref mut cb) = self.on_select {
                    cb(&p.id);
                }
            }
        } else if matches_key(&event, "escape") {
            if let Some(ref mut cb) = self.on_cancel {
                cb();
            }
        } else {
            self.input.handle_input(data);
            let query = self.input.value().to_string();
            self.filter_providers(&query);
        }
    }

    fn invalidate(&mut self) {
        self.input.invalidate();
    }
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
    fn test_oauth_selector_renders_providers() {
        let theme = Theme::dark();
        let providers = vec![
            AuthProvider { id: "anthropic".into(), name: "Anthropic".into(), auth_type: "oauth".into(), configured: true, status_label: None },
            AuthProvider { id: "openai".into(), name: "OpenAI".into(), auth_type: "api_key".into(), configured: false, status_label: None },
        ];
        let selector = OAuthSelector::new("login".into(), providers, &theme);
        let lines = selector.render(80);
        let joined = lines.join(" ");
        assert!(joined.contains("Anthropic"));
        assert!(joined.contains("OpenAI"));
    }

    #[test]
    fn test_oauth_selector_empty() {
        let theme = Theme::dark();
        let selector = OAuthSelector::new("login".into(), vec![], &theme);
        let lines = selector.render(80);
        let joined = lines.join(" ");
        assert!(joined.contains("No providers"));
    }
}
