//! SettingsList component — scrollable settings list with search.
//!
//! Mirrors `packages/tui/src/components/settings-list.ts`
//!
//! Supports:
//! - Items with label, value, description, and optional value cycling
//! - Selection and scrolling with cursor-centered viewport
//! - Description display for the selected item
//! - Fuzzy search filtering when search is enabled
//! - Submenu opening (stub, submenu component rendered in place)
//! - Value cycling through a predefined list

use crate::component::Component;
use crate::components::input::Input;
use crate::fuzzy::fuzzy_filter;
use crate::keys::{matches_key, parse_key};
use crate::utils::{truncate_to_width, visible_width, wrap_text_with_ansi};

/// A single setting item.
#[derive(Debug, Clone)]
pub struct SettingItem {
    /// Unique identifier for this setting.
    pub id: String,
    /// Display label (left side).
    pub label: String,
    /// Optional description shown when selected.
    pub description: Option<String>,
    /// Current value to display (right side).
    pub current_value: String,
    /// If provided, Enter/Space cycles through these values.
    pub values: Option<Vec<String>>,
}

/// Theme functions for styling a `SettingsList`.
pub struct SettingsListTheme {
    pub label: SettingsItemTextStyle,
    pub value: SettingsItemTextStyle,
    pub description: Box<dyn Fn(&str) -> String + Send>,
    pub cursor: String,
    pub hint: Box<dyn Fn(&str) -> String + Send>,
}

type SettingsItemTextStyle = Box<dyn Fn(&str, bool) -> String + Send>;
type SettingsChangeCallback = Box<dyn FnMut(&str, &str) + Send>;

impl std::fmt::Debug for SettingsListTheme {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SettingsListTheme").finish()
    }
}

/// A scrollable settings list with optional search.
pub struct SettingsList {
    items: Vec<SettingItem>,
    filtered_items: Vec<SettingItem>,
    theme: SettingsListTheme,
    selected_index: usize,
    max_visible: usize,
    search_enabled: bool,
    search_input: Option<Input>,
    /// Called when a setting value changes.
    pub on_change: Option<SettingsChangeCallback>,
    /// Called when the user presses Escape.
    pub on_cancel: Option<Box<dyn FnMut() + Send>>,
    // Submenu state
    submenu_component: Option<Box<dyn Component>>,
}

impl SettingsList {
    pub fn new(items: Vec<SettingItem>, max_visible: usize, theme: SettingsListTheme, search_enabled: bool) -> Self {
        Self {
            filtered_items: items.clone(),
            items,
            theme,
            selected_index: 0,
            max_visible: max_visible.max(1),
            search_enabled,
            search_input: if search_enabled { Some(Input::new()) } else { None },
            on_change: None,
            on_cancel: None,
            submenu_component: None,
        }
    }

    /// Update an item's `current_value` by its ID.
    pub fn update_value(&mut self, id: &str, new_value: &str) {
        if let Some(item) = self.items.iter_mut().find(|i| i.id == id) {
            item.current_value = new_value.to_string();
        }
        // Also update filtered_items if the item is visible
        if let Some(item) = self.filtered_items.iter_mut().find(|i| i.id == id) {
            item.current_value = new_value.to_string();
        }
    }

    /// Return the items this SettingsList manages (useful for serialization).
    pub fn items(&self) -> &[SettingItem] {
        &self.items
    }

    /// Return a mutable reference to the items.
    pub fn items_mut(&mut self) -> &mut [SettingItem] {
        &mut self.items
    }

    fn activate_item(&mut self) {
        let display_items = if self.search_enabled { &self.filtered_items } else { &self.items };

        let item = match display_items.get(self.selected_index) {
            Some(i) => i.clone(),
            None => return,
        };

        if let Some(values) = item.values {
            if !values.is_empty() {
                let current_idx = values.iter().position(|v| v == &item.current_value);
                let next_idx = match current_idx {
                    Some(idx) => (idx + 1) % values.len(),
                    None => 0,
                };
                let new_value = values[next_idx].clone();
                self.update_value(&item.id, &new_value);
                if let Some(ref mut cb) = self.on_change {
                    cb(&item.id, &new_value);
                }
            }
        }
    }

    fn apply_filter(&mut self, query: &str) {
        if query.is_empty() {
            self.filtered_items = self.items.clone();
        } else {
            let results = fuzzy_filter(query, &self.items.iter().map(|i| &i.label).collect::<Vec<_>>());
            self.filtered_items = results.iter().map(|(idx, _)| self.items[*idx].clone()).collect();
        }
        self.selected_index = 0;
    }

    fn add_hint_line(&self, lines: &mut Vec<String>, width: usize) {
        lines.push(String::new());
        let hint = if self.search_enabled {
            "  Type to search · Enter/Space to change · Esc to cancel"
        } else {
            "  Enter/Space to change · Esc to cancel"
        };
        lines.push((self.theme.hint)(&truncate_to_width(hint, width)));
    }
}

impl Component for SettingsList {
    fn render(&self, width: u16) -> Vec<String> {
        let w = width as usize;

        // If submenu is active, delegate rendering to it
        if let Some(ref submenu) = self.submenu_component {
            return submenu.render(width);
        }

        let mut lines: Vec<String> = Vec::new();

        // Search bar
        if self.search_enabled {
            if let Some(ref input) = self.search_input {
                lines.extend(input.render(width));
                lines.push(String::new());
            }
        }

        if self.items.is_empty() {
            lines.push((self.theme.hint)("  No settings available"));
            if self.search_enabled {
                self.add_hint_line(&mut lines, w);
            }
            return lines;
        }

        let display_items = if self.search_enabled { &self.filtered_items } else { &self.items };

        if display_items.is_empty() {
            lines.push(truncate_to_width(&(self.theme.hint)("  No matching settings"), w));
            self.add_hint_line(&mut lines, w);
            return lines;
        }

        // Calculate visible range with scrolling
        let total = display_items.len();
        let half = self.max_visible / 2;
        let start_index = if self.selected_index < half {
            0
        } else if self.selected_index + half >= total {
            total.saturating_sub(self.max_visible)
        } else {
            self.selected_index.saturating_sub(half)
        };
        let end_index = (start_index + self.max_visible).min(total);

        // Calculate max label width for alignment
        let max_label_w = self.items.iter().map(|item| visible_width(&item.label)).max().unwrap_or(10).min(30);

        // Render visible items
        for i in start_index..end_index {
            if let Some(item) = display_items.get(i) {
                let is_selected = i == self.selected_index;
                let cursor_glyph = if is_selected { &self.theme.cursor } else { "  " };
                let prefix_w = visible_width(cursor_glyph);

                // Pad label to align values
                let label_padded =
                    format!("{}{}", item.label, " ".repeat(max_label_w.saturating_sub(visible_width(&item.label))));
                let label_styled = (self.theme.label)(&label_padded, is_selected);

                // Value display
                let separator = "  ";
                let used = prefix_w + max_label_w + visible_width(separator);
                let value_max = w.saturating_sub(used).saturating_sub(2);
                let value_styled = (self.theme.value)(&truncate_to_width(&item.current_value, value_max), is_selected);

                let full_line = format!("{}{}{}{}", cursor_glyph, label_styled, separator, value_styled);
                lines.push(truncate_to_width(&full_line, w));
            }
        }

        // Scroll indicator
        if start_index > 0 || end_index < total {
            let scroll_text = format!("  ({}/{})", self.selected_index + 1, total);
            lines.push((self.theme.hint)(&truncate_to_width(&scroll_text, w.saturating_sub(2))));
        }

        // Description for selected item
        if let Some(item) = display_items.get(self.selected_index) {
            if let Some(ref desc) = item.description {
                lines.push(String::new());
                let wrapped = wrap_text_with_ansi(desc, w.saturating_sub(4));
                for line in wrapped {
                    lines.push((self.theme.description)(&format!("  {}", line)));
                }
            }
        }

        // Hint line
        self.add_hint_line(&mut lines, w);

        lines
    }

    fn handle_input(&mut self, data: &str) {
        // If submenu is active, delegate input to it
        if let Some(ref mut submenu) = self.submenu_component {
            submenu.handle_input(data);
            return;
        }

        let event = parse_key(data);
        let display_items = if self.search_enabled { &self.filtered_items } else { &self.items };

        if matches_key(&event, "up") {
            if display_items.is_empty() {
                return;
            }
            self.selected_index =
                if self.selected_index == 0 { display_items.len() - 1 } else { self.selected_index - 1 };
        } else if matches_key(&event, "down") {
            if display_items.is_empty() {
                return;
            }
            self.selected_index =
                if self.selected_index == display_items.len() - 1 { 0 } else { self.selected_index + 1 };
        } else if matches_key(&event, "enter")
            || (event.code == crate::keys::KeyCode::Char(' ') && !event.modifiers.ctrl && !event.modifiers.alt)
        {
            self.activate_item();
        } else if matches_key(&event, "escape") {
            if let Some(ref mut cb) = self.on_cancel {
                cb();
            }
        } else if self.search_enabled {
            if let Some(ref mut input) = self.search_input {
                let sanitized: String = data.chars().filter(|c| !c.is_ascii_whitespace()).collect();
                if !sanitized.is_empty() {
                    input.handle_input(data);
                    let val = input.value().to_string();
                    self.apply_filter(&val);
                }
            }
        }
    }

    fn invalidate(&mut self) {
        if let Some(ref mut sub) = self.submenu_component {
            sub.invalidate();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_theme() -> SettingsListTheme {
        SettingsListTheme {
            label: Box::new(|s, _selected| s.to_string()),
            value: Box::new(|s, _selected| s.to_string()),
            description: Box::new(|s| s.to_string()),
            cursor: "\u{2192} ".to_string(),
            hint: Box::new(|s| s.to_string()),
        }
    }

    #[test]
    fn test_settings_list_renders_items() {
        let items = vec![
            SettingItem {
                id: "theme".into(),
                label: "Theme".into(),
                description: Some("Color theme".into()),
                current_value: "dark".into(),
                values: Some(vec!["dark".into(), "light".into()]),
            },
            SettingItem {
                id: "font".into(),
                label: "Font Size".into(),
                description: None,
                current_value: "14".into(),
                values: None,
            },
        ];
        let list = SettingsList::new(items, 10, test_theme(), false);
        let lines = list.render(80);
        // Should show both items
        let joined = lines.join(" ");
        assert!(joined.contains("Theme"));
        assert!(joined.contains("dark"));
        assert!(joined.contains("Font Size"));
        assert!(joined.contains("14"));
    }

    #[test]
    fn test_settings_list_empty() {
        let list = SettingsList::new(vec![], 10, test_theme(), false);
        let lines = list.render(80);
        assert!(!lines.is_empty());
        assert!(lines[0].contains("No settings available"));
    }

    #[test]
    fn test_settings_list_highlights_selection() {
        let items = vec![
            SettingItem {
                id: "a".into(),
                label: "Alpha".into(),
                description: None,
                current_value: "1".into(),
                values: None,
            },
            SettingItem {
                id: "b".into(),
                label: "Beta".into(),
                description: None,
                current_value: "2".into(),
                values: None,
            },
        ];
        let list = SettingsList::new(items, 10, test_theme(), false);
        let lines = list.render(80);
        // First item should have cursor marker
        assert!(lines[0].contains('\u{2192}'));
    }

    #[test]
    fn test_settings_list_update_value() {
        let items = vec![SettingItem {
            id: "test".into(),
            label: "Test".into(),
            description: None,
            current_value: "old".into(),
            values: None,
        }];
        let mut list = SettingsList::new(items, 10, test_theme(), false);
        list.update_value("test", "new");
        assert_eq!(list.items()[0].current_value, "new");
    }

    #[test]
    fn test_settings_list_handle_navigation() {
        let items = vec![
            SettingItem {
                id: "a".into(),
                label: "Alpha".into(),
                description: None,
                current_value: "1".into(),
                values: None,
            },
            SettingItem {
                id: "b".into(),
                label: "Beta".into(),
                description: None,
                current_value: "2".into(),
                values: None,
            },
        ];
        let mut list = SettingsList::new(items, 10, test_theme(), false);
        assert_eq!(list.selected_index, 0);

        list.handle_input("\x1b[B"); // Down
        assert_eq!(list.selected_index, 1);

        list.handle_input("\x1b[A"); // Up
        assert_eq!(list.selected_index, 0);
    }

    #[test]
    fn test_settings_list_description_shown() {
        let items = vec![SettingItem {
            id: "test".into(),
            label: "Test".into(),
            description: Some("This is a description".into()),
            current_value: "val".into(),
            values: None,
        }];
        let list = SettingsList::new(items, 10, test_theme(), false);
        let lines = list.render(80);
        let joined = lines.join(" ");
        assert!(joined.contains("This is a description"));
    }

    #[test]
    fn test_settings_list_value_cycling() {
        let items = vec![SettingItem {
            id: "theme".into(),
            label: "Theme".into(),
            description: None,
            current_value: "dark".into(),
            values: Some(vec!["dark".into(), "light".into(), "auto".into()]),
        }];
        let mut list = SettingsList::new(items, 10, test_theme(), false);
        assert_eq!(list.items[0].current_value, "dark");

        list.activate_item();
        assert_eq!(list.items[0].current_value, "light");

        list.activate_item();
        assert_eq!(list.items[0].current_value, "auto");

        list.activate_item();
        assert_eq!(list.items[0].current_value, "dark");
    }
}
