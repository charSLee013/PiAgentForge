//! Thinking level selector component.
//!
//! Shows available thinking (reasoning) levels for the user to choose from.

use pi_tui_core::component::Component;
use pi_tui_core::components::select_list::{SelectItem, SelectList};
use crate::Theme;

/// A thinking level item.
#[derive(Debug, Clone)]
pub struct ThinkingLevelItem {
    pub level: String,
    pub description: &'static str,
}

/// A thinking level selector component.
pub struct ThinkingSelector {
    select_list: SelectList,
}

impl ThinkingSelector {
    /// Create a new thinking selector.
    ///
    /// * `levels` — available thinking levels with descriptions.
    /// * `current_level` — the currently selected level name.
    /// * `theme` — application theme for styling.
    /// * `on_select` — called when the user selects a level.
    /// * `on_cancel` — called when the user cancels.
    pub fn new<F1, F2>(
        levels: Vec<ThinkingLevelItem>,
        current_level: &str,
        theme: &Theme,
        on_select: F1,
        on_cancel: F2,
    ) -> Self
    where
        F1: FnMut(&SelectItem) + Send + 'static,
        F2: FnMut() + Send + 'static,
    {
        let items: Vec<SelectItem> = levels
            .iter()
            .map(|l| SelectItem {
                value: l.level.clone(),
                label: l.level.clone(),
                description: Some(l.description.to_string()),
            })
            .collect();

        let select_theme = theme.to_select_list_theme();
        let mut select_list = SelectList::new(items, levels.len().max(1), select_theme);

        if let Some(pos) = levels.iter().position(|l| l.level == current_level) {
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
}

impl Component for ThinkingSelector {
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
    fn test_thinking_selector_renders_levels() {
        let theme = Theme::dark();
        let levels = vec![
            ThinkingLevelItem { level: "off".into(), description: "No reasoning" },
            ThinkingLevelItem { level: "low".into(), description: "Light reasoning" },
            ThinkingLevelItem { level: "high".into(), description: "Deep reasoning" },
        ];
        let selector = ThinkingSelector::new(levels, "off", &theme, |_| {}, || {});
        let lines = selector.render(80);
        let joined = lines.join(" ");
        assert!(joined.contains("off"));
        assert!(joined.contains("high"));
    }
}
