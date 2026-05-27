//! Theme selector component.
//!
//! Shows a list of available themes for the user to pick from.
//! Wraps a `SelectList` with theme-aware styling.

use pi_tui_core::component::Component;
use pi_tui_core::components::select_list::{SelectItem, SelectList};
use crate::Theme;

/// A theme selector component.
pub struct ThemeSelector {
    select_list: SelectList,
}

impl ThemeSelector {
    /// Create a new theme selector.
    ///
    /// * `themes` — list of available theme names.
    /// * `current_theme` — the currently active theme name.
    /// * `theme` — application theme for styling.
    /// * `on_select` — called when the user selects a theme.
    /// * `on_cancel` — called when the user cancels.
    pub fn new<F1, F2>(
        themes: Vec<String>,
        current_theme: &str,
        theme: &Theme,
        on_select: F1,
        on_cancel: F2,
    ) -> Self
    where
        F1: FnMut(&SelectItem) + Send + 'static,
        F2: FnMut() + Send + 'static,
    {
        let items: Vec<SelectItem> = themes
            .iter()
            .map(|name| SelectItem {
                value: name.clone(),
                label: name.clone(),
                description: if name == current_theme {
                    Some("(current)".to_string())
                } else {
                    None
                },
            })
            .collect();

        let select_theme = theme.to_select_list_theme();
        let mut select_list = SelectList::new(items, 10, select_theme);

        // Preselect current theme
        if let Some(pos) = themes.iter().position(|t| t == current_theme) {
            select_list.set_selected_index(pos);
        }

        select_list.on_select = Some(Box::new(on_select));
        select_list.on_cancel = Some(Box::new(on_cancel));

        Self { select_list }
    }

    /// Access the inner select list.
    pub fn select_list(&self) -> &SelectList {
        &self.select_list
    }

    /// Access the inner select list mutably.
    pub fn select_list_mut(&mut self) -> &mut SelectList {
        &mut self.select_list
    }
}

impl Component for ThemeSelector {
    fn render(&self, width: u16) -> Vec<String> {
        self.select_list.render(width)
    }

    fn handle_input(&mut self, data: &str) {
        self.select_list.handle_input(data);
    }

    fn invalidate(&mut self) {
        self.select_list.invalidate();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Theme;

    #[test]
    fn test_theme_selector_renders_themes() {
        let theme = Theme::dark();
        let themes = vec!["dark".into(), "light".into(), "solarized".into()];
        let selector = ThemeSelector::new(themes, "dark", &theme, |_| {}, || {});
        let lines = selector.render(80);
        let joined = lines.join(" ");
        assert!(joined.contains("dark"));
        assert!(joined.contains("light"));
        assert!(joined.contains("solarized"));
    }

    #[test]
    fn test_theme_selector_current_theme_marked() {
        let theme = Theme::dark();
        let themes = vec!["dark".into(), "light".into()];
        let selector = ThemeSelector::new(themes, "dark", &theme, |_| {}, || {});
        let lines = selector.render(80);
        let joined = lines.join(" ");
        assert!(joined.contains("(current)"));
    }
}
