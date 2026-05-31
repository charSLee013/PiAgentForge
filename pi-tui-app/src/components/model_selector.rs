//! Model selector component.
//!
//! Shows a searchable list of available models with provider badges.
//! Supports scoped/all view toggle.

use crate::Theme;
use pi_tui_core::component::Component;
use pi_tui_core::components::input::Input;
use pi_tui_core::fuzzy::fuzzy_filter;
use pi_tui_core::keys::{matches_key, parse_key};
use pi_tui_core::utils::truncate_to_width;

type ModelSelectCallback = Box<dyn FnMut(&str, &str) + Send>;

/// A model entry in the selector.
#[derive(Debug, Clone)]
pub struct ModelEntry {
    pub provider: String,
    pub id: String,
    pub is_current: bool,
    pub name: String,
}

/// A searchable model selector component.
pub struct ModelSelector {
    input: Input,
    all_models: Vec<ModelEntry>,
    scoped_models: Vec<ModelEntry>,
    active_models: Vec<ModelEntry>,
    filtered_models: Vec<ModelEntry>,
    selected_index: usize,
    theme: Theme,
    scope: ModelScope,
    has_scoped: bool,
    /// Callback when a model is selected.
    pub on_select: Option<ModelSelectCallback>, // provider, id
    /// Callback when cancelled.
    pub on_cancel: Option<Box<dyn FnMut() + Send>>,
}

#[derive(Debug, Clone, PartialEq)]
enum ModelScope {
    All,
    Scoped,
}

impl ModelSelector {
    /// Create a new model selector.
    ///
    /// * `all_models` — all available models.
    /// * `scoped_models` — models scoped to the current context (empty = no scoping).
    /// * `theme` — application theme.
    pub fn new(all_models: Vec<ModelEntry>, scoped_models: Vec<ModelEntry>, theme: &Theme) -> Self {
        let has_scoped = !scoped_models.is_empty();
        let scope = if has_scoped { ModelScope::Scoped } else { ModelScope::All };
        let active = if has_scoped { scoped_models.clone() } else { all_models.clone() };

        Self {
            input: Input::new(),
            filtered_models: active.clone(),
            all_models,
            scoped_models,
            active_models: active,
            selected_index: 0,
            theme: theme.clone(),
            scope,
            has_scoped,
            on_select: None,
            on_cancel: None,
        }
    }

    /// Set the current model to preselect it in the list.
    pub fn set_current(&mut self, provider: &str, id: &str) {
        let pos = self.filtered_models.iter().position(|m| m.provider == provider && m.id == id);
        if let Some(idx) = pos {
            self.selected_index = idx;
        }
    }

    fn toggle_scope(&mut self) {
        if !self.has_scoped {
            return;
        }
        self.scope = match self.scope {
            ModelScope::All => ModelScope::Scoped,
            ModelScope::Scoped => ModelScope::All,
        };
        self.active_models = match self.scope {
            ModelScope::All => self.all_models.clone(),
            ModelScope::Scoped => self.scoped_models.clone(),
        };
        self.selected_index = 0;
        let query = self.input.value().to_string();
        self.filter_models(&query);
    }

    fn filter_models(&mut self, query: &str) {
        if query.is_empty() {
            self.filtered_models = self.active_models.clone();
        } else {
            let results = fuzzy_filter(query, &self.active_models.iter().map(|m| &m.id).collect::<Vec<_>>());
            self.filtered_models = results.iter().map(|(idx, _)| self.active_models[*idx].clone()).collect();
        }
        self.selected_index = self.selected_index.min(self.filtered_models.len().saturating_sub(1));
    }
}

impl Component for ModelSelector {
    fn render(&self, width: u16) -> Vec<String> {
        let w = width as usize;
        let mut lines: Vec<String> = Vec::new();

        // Scope toggle display
        if self.has_scoped {
            let all_s = if self.scope == ModelScope::All {
                self.theme.ansi(&self.theme.primary, "all")
            } else {
                self.theme.ansi(&self.theme.muted, "all")
            };
            let scoped_s = if self.scope == ModelScope::Scoped {
                self.theme.ansi(&self.theme.primary, "scoped")
            } else {
                self.theme.ansi(&self.theme.muted, "scoped")
            };
            lines.push(format!(
                "{} {} | {} {}",
                self.theme.ansi(&self.theme.muted, "Scope:"),
                all_s,
                self.theme.ansi(&self.theme.muted, "|"),
                scoped_s
            ));
            lines.push(self.theme.ansi(&self.theme.dim, "Tab to toggle scope"));
            lines.push(String::new());
        }

        // Search input
        lines.extend(self.input.render(width));
        lines.push(String::new());

        if self.filtered_models.is_empty() {
            lines.push(self.theme.ansi(&self.theme.muted, "  No matching models"));
            return lines;
        }

        // Visible range
        let max_visible = 10usize.min(self.filtered_models.len());
        let half = max_visible / 2;
        let total = self.filtered_models.len();
        let start = if self.selected_index < half {
            0
        } else if self.selected_index + half >= total {
            total.saturating_sub(max_visible)
        } else {
            self.selected_index.saturating_sub(half)
        };
        let end = (start + max_visible).min(total);

        for i in start..end {
            let model = &self.filtered_models[i];
            let is_selected = i == self.selected_index;

            let id_truncated = truncate_to_width(&model.id, w.saturating_sub(30));
            let badge = format!("[{}]", model.provider);
            let check =
                if model.is_current { self.theme.ansi(&self.theme.success, " \u{2713}") } else { String::new() };
            let badge_muted = self.theme.ansi(&self.theme.muted, &badge);

            if is_selected {
                let prefix = self.theme.ansi(&self.theme.primary, "\u{2192} ");
                let text = self.theme.ansi(&self.theme.primary, &id_truncated);
                lines.push(format!("{}{} {}{}", prefix, text, badge_muted, check));
            } else {
                lines.push(format!("  {}{} {}", id_truncated, badge_muted, check));
            }
        }

        // Scroll indicator
        if start > 0 || end < total {
            let scroll = self.theme.ansi(&self.theme.muted, &format!("  ({}/{})", self.selected_index + 1, total));
            lines.push(scroll);
        }

        lines
    }

    fn handle_input(&mut self, data: &str) {
        let event = parse_key(data);

        if matches_key(&event, "tab") {
            self.toggle_scope();
            return;
        }

        if matches_key(&event, "up") {
            if self.filtered_models.is_empty() {
                return;
            }
            self.selected_index =
                if self.selected_index == 0 { self.filtered_models.len() - 1 } else { self.selected_index - 1 };
        } else if matches_key(&event, "down") {
            if self.filtered_models.is_empty() {
                return;
            }
            self.selected_index =
                if self.selected_index == self.filtered_models.len() - 1 { 0 } else { self.selected_index + 1 };
        } else if matches_key(&event, "enter") {
            if let Some(m) = self.filtered_models.get(self.selected_index).cloned() {
                if let Some(ref mut cb) = self.on_select {
                    cb(&m.provider, &m.id);
                }
            }
        } else if matches_key(&event, "escape") {
            if let Some(ref mut cb) = self.on_cancel {
                cb();
            }
        } else {
            self.input.handle_input(data);
            let query = self.input.value().to_string();
            self.filter_models(&query);
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
    fn test_model_selector_renders_models() {
        let theme = Theme::dark();
        let models = vec![
            ModelEntry {
                provider: "anthropic".into(),
                id: "claude-3-opus".into(),
                is_current: true,
                name: "Claude 3 Opus".into(),
            },
            ModelEntry { provider: "openai".into(), id: "gpt-4".into(), is_current: false, name: "GPT-4".into() },
        ];
        let selector = ModelSelector::new(models, vec![], &theme);
        let lines = selector.render(80);
        let joined = lines.join(" ");
        assert!(joined.contains("claude"));
        assert!(joined.contains("gpt-4"));
    }

    #[test]
    fn test_model_selector_empty() {
        let theme = Theme::dark();
        let selector = ModelSelector::new(vec![], vec![], &theme);
        let lines = selector.render(80);
        let joined = lines.join(" ");
        assert!(joined.contains("No matching"));
    }
}
