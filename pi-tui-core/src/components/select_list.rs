//! SelectList component — scrollable selection list.
//!
//! Mirrors `packages/tui/src/components/select-list.ts`
//!
//! Supports:
//! - Up / Down navigation with wrap-around
//! - Visible window that follows the selection
//! - Optional item descriptions (shown when width > 40)
//! - Scroll indicator when not all items are visible
//! - Fuzzy search filtering via `set_filter`
//! - Theme functions for styling (prefix, text, description, etc.)

use crate::component::Component;
use crate::keys::{matches_key, parse_key};
use crate::utils::{truncate_to_width, visible_width};

/// A selectable item in a `SelectList`.
#[derive(Debug, Clone)]
pub struct SelectItem {
    pub value: String,
    pub label: String,
    pub description: Option<String>,
}

/// Theme functions for styling a `SelectList`.
pub struct SelectListTheme {
    /// Style applied to the prefix arrow (`->`) of the selected item.
    pub selected_prefix: Box<dyn Fn(&str) -> String + Send>,
    /// Style applied to the text of the selected item.
    pub selected_text: Box<dyn Fn(&str) -> String + Send>,
    /// Style applied to the description column.
    pub description: Box<dyn Fn(&str) -> String + Send>,
    /// Style applied to the scroll info line.
    pub scroll_info: Box<dyn Fn(&str) -> String + Send>,
    /// Style applied to the "no matching items" message.
    pub no_match: Box<dyn Fn(&str) -> String + Send>,
}

/// A scrollable, selectable list component with optional descriptions and
/// fuzzy-search filtering.
pub struct SelectList {
    items: Vec<SelectItem>,
    filtered_items: Vec<SelectItem>,
    selected_index: usize,
    max_visible: usize,
    theme: SelectListTheme,
    pub on_select: Option<SelectItemCallback>,
    pub on_cancel: Option<Box<dyn FnMut() + Send>>,
    pub on_selection_change: Option<SelectItemCallback>,
}

type SelectItemCallback = Box<dyn FnMut(&SelectItem) + Send>;

impl SelectList {
    pub fn new(items: Vec<SelectItem>, max_visible: usize, theme: SelectListTheme) -> Self {
        Self {
            filtered_items: items.clone(),
            items,
            selected_index: 0,
            max_visible: max_visible.max(1),
            theme,
            on_select: None,
            on_cancel: None,
            on_selection_change: None,
        }
    }

    /// Apply a fuzzy-search filter.  Empty string shows all items.
    pub fn set_filter(&mut self, filter: &str) {
        if filter.is_empty() {
            self.filtered_items = self.items.clone();
        } else {
            let q = filter.to_lowercase();
            self.filtered_items =
                self.items.iter().filter(|item| item.label.to_lowercase().contains(&q)).cloned().collect();
        }
        self.selected_index = 0;
    }

    /// Directly set the selected index (clamped to valid range).
    pub fn set_selected_index(&mut self, index: usize) {
        if self.filtered_items.is_empty() {
            self.selected_index = 0;
            return;
        }
        self.selected_index = index.min(self.filtered_items.len() - 1);
    }

    /// Return the currently selected item, or `None` if the list is empty.
    pub fn selected_item(&self) -> Option<&SelectItem> {
        self.filtered_items.get(self.selected_index)
    }

    /// Return the number of visible items after filtering.
    pub fn filtered_count(&self) -> usize {
        self.filtered_items.len()
    }

    /// Notify the `on_selection_change` callback.
    fn notify_selection_change(&mut self) {
        if let Some(item) = self.filtered_items.get(self.selected_index).cloned() {
            if let Some(ref mut cb) = self.on_selection_change {
                cb(&item);
            }
        }
    }
}

impl Component for SelectList {
    fn render(&self, width: u16) -> Vec<String> {
        let w = width as usize;
        let mut lines: Vec<String> = Vec::new();

        if self.filtered_items.is_empty() {
            lines.push((self.theme.no_match)("  No matching commands"));
            return lines;
        }

        // Calculate visible range with cursor-centered scrolling
        let total = self.filtered_items.len();
        let half = self.max_visible / 2;
        let start_index = if self.selected_index < half {
            0
        } else if self.selected_index + half >= total {
            total.saturating_sub(self.max_visible)
        } else {
            self.selected_index.saturating_sub(half)
        };
        let end_index = (start_index + self.max_visible).min(total);

        // Render visible items
        for i in start_index..end_index {
            if let Some(item) = self.filtered_items.get(i) {
                let is_selected = i == self.selected_index;
                lines.push(self.render_item(item, is_selected, w));
            }
        }

        // Scroll indicator
        if start_index > 0 || end_index < total {
            let scroll_text = format!("  ({}/{})", self.selected_index + 1, total);
            let truncated = truncate_to_width(&scroll_text, w.saturating_sub(2));
            lines.push((self.theme.scroll_info)(&truncated));
        }

        lines
    }

    fn handle_input(&mut self, data: &str) {
        let event = parse_key(data);

        if matches_key(&event, "up") {
            if self.filtered_items.is_empty() {
                return;
            }
            self.selected_index =
                if self.selected_index == 0 { self.filtered_items.len() - 1 } else { self.selected_index - 1 };
            self.notify_selection_change();
        } else if matches_key(&event, "down") {
            if self.filtered_items.is_empty() {
                return;
            }
            self.selected_index =
                if self.selected_index == self.filtered_items.len() - 1 { 0 } else { self.selected_index + 1 };
            self.notify_selection_change();
        } else if matches_key(&event, "enter") {
            if let Some(item) = self.filtered_items.get(self.selected_index).cloned() {
                if let Some(ref mut cb) = self.on_select {
                    cb(&item);
                }
            }
        } else if matches_key(&event, "escape") {
            if let Some(ref mut cb) = self.on_cancel {
                cb();
            }
        }
    }

    fn invalidate(&mut self) {
        // No cached state
    }
}

impl SelectList {
    fn render_item(&self, item: &SelectItem, is_selected: bool, width: usize) -> String {
        let prefix = if is_selected { "\u{2192} " } else { "  " };
        let prefix_w = visible_width(prefix);

        // Determine content width
        let max_content_w = width.saturating_sub(prefix_w).saturating_sub(1);

        if let Some(ref desc) = item.description {
            if width > 40 {
                // Two-column layout: label | description
                let label_w = visible_width(&item.label).min(max_content_w / 2).max(10);
                let gap = 2;
                let remaining = max_content_w.saturating_sub(label_w + gap);
                let max_desc = remaining.min(40);

                let truncated_label = truncate_to_width(&item.label, label_w);
                let truncated_desc = truncate_to_width(desc, max_desc);
                let label_vis = visible_width(&truncated_label);
                let padding = if label_vis < label_w { " ".repeat(label_w - label_vis) } else { String::new() };
                let spacer = " ".repeat(gap);

                if is_selected {
                    return (self.theme.selected_text)(&format!(
                        "{}{}{}{}{}",
                        prefix, truncated_label, padding, spacer, truncated_desc
                    ));
                } else {
                    let desc_styled = (self.theme.description)(&format!("{}{}", spacer, truncated_desc));
                    return format!("{}{}{}{}", prefix, truncated_label, padding, desc_styled);
                }
            }
        }

        // Single-column layout
        let truncated = truncate_to_width(&item.label, max_content_w);
        if is_selected {
            (self.theme.selected_text)(&format!("{}{}", prefix, truncated))
        } else {
            format!("{}{}", prefix, truncated)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_theme() -> SelectListTheme {
        SelectListTheme {
            selected_prefix: Box::new(|s| s.to_string()),
            selected_text: Box::new(|s| format!("\x1b[7m{}\x1b[27m", s)),
            description: Box::new(|s| format!("\x1b[2m{}\x1b[22m", s)),
            scroll_info: Box::new(|s| format!("\x1b[90m{}\x1b[0m", s)),
            no_match: Box::new(|s| s.to_string()),
        }
    }

    #[test]
    fn test_select_list_renders_items() {
        let items = vec![
            SelectItem { value: "a".into(), label: "Apple".into(), description: None },
            SelectItem { value: "b".into(), label: "Banana".into(), description: None },
        ];
        let list = SelectList::new(items, 10, test_theme());
        let lines = list.render(80);
        assert!(!lines.is_empty());
        // Both items should appear
        let joined = lines.join(" ");
        assert!(joined.contains("Apple"));
        assert!(joined.contains("Banana"));
    }

    #[test]
    fn test_select_list_highlights_selection() {
        let items = vec![
            SelectItem { value: "a".into(), label: "Apple".into(), description: None },
            SelectItem { value: "b".into(), label: "Banana".into(), description: None },
        ];
        let list = SelectList::new(items, 10, test_theme());
        let lines = list.render(80);
        // First item (index 0) should be selected, with the arrow prefix
        assert!(lines[0].contains('\u{2192}'));
    }

    #[test]
    fn test_select_list_empty() {
        let list = SelectList::new(vec![], 5, test_theme());
        let lines = list.render(80);
        assert!(!lines.is_empty());
        // Should show a "no match" message
        assert!(lines[0].contains("No matching"));
    }

    #[test]
    fn test_select_list_set_filter() {
        let items = vec![
            SelectItem { value: "a".into(), label: "Apple".into(), description: None },
            SelectItem { value: "b".into(), label: "Banana".into(), description: None },
            SelectItem { value: "c".into(), label: "Cherry".into(), description: None },
        ];
        let mut list = SelectList::new(items, 10, test_theme());
        list.set_filter("ap");
        assert_eq!(list.filtered_count(), 1);
        let lines = list.render(80);
        assert!(lines[0].contains("Apple"));
    }

    #[test]
    fn test_select_list_filter_empty_shows_all() {
        let items = vec![
            SelectItem { value: "a".into(), label: "Apple".into(), description: None },
            SelectItem { value: "b".into(), label: "Banana".into(), description: None },
        ];
        let mut list = SelectList::new(items.clone(), 10, test_theme());
        list.set_filter("");
        assert_eq!(list.filtered_count(), 2);
    }

    #[test]
    fn test_select_list_filter_no_match() {
        let items = vec![SelectItem { value: "a".into(), label: "Apple".into(), description: None }];
        let mut list = SelectList::new(items, 10, test_theme());
        list.set_filter("zzz");
        assert_eq!(list.filtered_count(), 0);
        let lines = list.render(80);
        assert!(lines[0].contains("No matching"));
    }

    #[test]
    fn test_select_list_selected_item() {
        let items = vec![
            SelectItem { value: "a".into(), label: "Apple".into(), description: None },
            SelectItem { value: "b".into(), label: "Banana".into(), description: None },
        ];
        let list = SelectList::new(items, 10, test_theme());
        assert_eq!(list.selected_item().unwrap().value, "a");
    }

    #[test]
    fn test_select_list_scroll_indicator() {
        let items: Vec<SelectItem> = (0..20)
            .map(|i| SelectItem { value: format!("v{}", i), label: format!("Item {}", i), description: None })
            .collect();
        let list = SelectList::new(items, 5, test_theme());
        let lines = list.render(80);
        // With 20 items and max_visible=5, we should have a scroll indicator
        let has_scroll = lines.iter().any(|l| l.contains('/') && l.contains(')'));
        assert!(has_scroll);
    }

    #[test]
    fn test_select_list_handle_navigation() {
        let items = vec![
            SelectItem { value: "a".into(), label: "Apple".into(), description: None },
            SelectItem { value: "b".into(), label: "Banana".into(), description: None },
        ];
        let mut list = SelectList::new(items, 10, test_theme());
        assert_eq!(list.selected_index, 0);

        list.handle_input("\x1b[B"); // Down
        assert_eq!(list.selected_index, 1);

        list.handle_input("\x1b[A"); // Up
        assert_eq!(list.selected_index, 0);
    }
}
