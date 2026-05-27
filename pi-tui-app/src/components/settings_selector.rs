//! Settings selector component.
//!
//! Wraps a core `SettingsList` with application-level setting items
//! and theme-aware styling.

use pi_tui_core::component::Component;
use pi_tui_core::components::settings_list::{SettingItem, SettingsList};
use crate::Theme;

/// A settings selector component wrapping a `SettingsList`.
pub struct SettingsSelector {
    settings_list: SettingsList,
}

impl SettingsSelector {
    /// Create a new settings selector with predefined settings.
    ///
    /// * `theme` — application theme for styling.
    /// * `settings` — initial setting items.
    /// * `enable_search` — whether to show the search bar.
    pub fn new(
        theme: &Theme,
        settings: Vec<SettingItem>,
        enable_search: bool,
    ) -> Self {
        let list_theme = theme.to_settings_list_theme();
        let max_visible = 10;
        let settings_list = SettingsList::new(settings, max_visible, list_theme, enable_search);

        Self {
            settings_list,
        }
    }

    /// Access the inner settings list.
    pub fn settings_list(&self) -> &SettingsList {
        &self.settings_list
    }

    /// Access the inner settings list mutably.
    pub fn settings_list_mut(&mut self) -> &mut SettingsList {
        &mut self.settings_list
    }

    /// Update a setting value by ID.
    pub fn update_value(&mut self, id: &str, value: &str) {
        self.settings_list.update_value(id, value);
    }

    /// Set the on_change callback.
    pub fn set_on_change<F>(&mut self, cb: F)
    where
        F: FnMut(&str, &str) + Send + 'static,
    {
        self.settings_list.on_change = Some(Box::new(cb));
    }

    /// Set the on_cancel callback.
    pub fn set_on_cancel<F>(&mut self, cb: F)
    where
        F: FnMut() + Send + 'static,
    {
        self.settings_list.on_cancel = Some(Box::new(cb));
    }
}

impl Component for SettingsSelector {
    fn render(&self, width: u16) -> Vec<String> {
        self.settings_list.render(width)
    }

    fn handle_input(&mut self, data: &str) {
        self.settings_list.handle_input(data);
    }

    fn invalidate(&mut self) {
        self.settings_list.invalidate();
    }
}

/// Build a set of common setting items using the theme for styling.
pub fn default_settings() -> Vec<SettingItem> {
    vec![
        SettingItem {
            id: "autocompact".into(),
            label: "Auto-compact".into(),
            description: Some("Automatically compact context when it gets too large".into()),
            current_value: "true".into(),
            values: Some(vec!["true".into(), "false".into()]),
        },
        SettingItem {
            id: "transport".into(),
            label: "Transport".into(),
            description: Some("Preferred provider transport".into()),
            current_value: "auto".into(),
            values: Some(vec!["sse".into(), "websocket".into(), "auto".into()]),
        },
        SettingItem {
            id: "hide-thinking".into(),
            label: "Hide thinking".into(),
            description: Some("Hide thinking blocks in assistant responses".into()),
            current_value: "false".into(),
            values: Some(vec!["true".into(), "false".into()]),
        },
        SettingItem {
            id: "quiet-startup".into(),
            label: "Quiet startup".into(),
            description: Some("Disable verbose printing at startup".into()),
            current_value: "false".into(),
            values: Some(vec!["true".into(), "false".into()]),
        },
        SettingItem {
            id: "double-escape-action".into(),
            label: "Double-escape action".into(),
            description: Some("Action on double Escape with empty editor".into()),
            current_value: "tree".into(),
            values: Some(vec!["tree".into(), "fork".into(), "none".into()]),
        },
        SettingItem {
            id: "show-terminal-progress".into(),
            label: "Terminal progress".into(),
            description: Some("Show progress indicators in terminal tab bar".into()),
            current_value: "false".into(),
            values: Some(vec!["true".into(), "false".into()]),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Theme;

    #[test]
    fn test_settings_selector_renders_items() {
        let theme = Theme::dark();
        let settings = default_settings();
        let selector = SettingsSelector::new(&theme, settings, false);
        let lines = selector.render(80);
        let joined = lines.join(" ");
        assert!(joined.contains("Auto-compact"));
        assert!(joined.contains("Transport"));
        assert!(joined.contains("Hide thinking"));
    }

    #[test]
    fn test_settings_selector_update_value() {
        let theme = Theme::dark();
        let settings = default_settings();
        let mut selector = SettingsSelector::new(&theme, settings, false);
        selector.update_value("autocompact", "false");
        let items = selector.settings_list().items();
        let item = items.iter().find(|i| i.id == "autocompact").unwrap();
        assert_eq!(item.current_value, "false");
    }

    #[test]
    fn test_settings_selector_custom_items() {
        let theme = Theme::dark();
        let items = vec![
            SettingItem {
                id: "test-option".into(),
                label: "Test Option".into(),
                description: None,
                current_value: "default".into(),
                values: Some(vec!["default".into(), "custom".into()]),
            },
        ];
        let selector = SettingsSelector::new(&theme, items, false);
        let lines = selector.render(80);
        let joined = lines.join(" ");
        assert!(joined.contains("Test Option"));
        assert!(joined.contains("default"));
    }
}
