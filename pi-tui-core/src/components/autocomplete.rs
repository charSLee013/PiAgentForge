//! Autocomplete component — suggestions for text input.
//!
//! Mirrors `packages/tui/src/autocomplete.ts`
//!
//! For Phase B.4, basic item types and the suggestion/application interface
//! are defined.  Full file-system backed completion (fd, home-directory
//! expansion, slash-command arguments) will follow in a later phase.

use crate::component::Component;
use crate::components::select_list::SelectItem;
use crate::utils::{truncate_to_width, visible_width};

/// An autocomplete suggestion item.
pub type AutocompleteItem = SelectItem;

/// Additional metadata returned with a set of suggestions.
#[derive(Debug, Clone)]
pub struct AutocompleteSuggestions {
    /// The suggested items.
    pub items: Vec<AutocompleteItem>,
    /// The text prefix that was matched (e.g., "/star" or "src/").
    pub prefix: String,
}

// ---------------------------------------------------------------------------
// AutocompleteProvider trait
// ---------------------------------------------------------------------------

/// A provider that generates autocomplete suggestions for a given text
/// and cursor position.
pub trait AutocompleteProvider: Send {
    /// Get suggestions for the current input state.
    ///
    /// `lines` represents the full multi-line input buffer, `cursor_line`
    /// and `cursor_col` identify the cursor position within it.
    ///
    /// Returns `None` when no suggestions are available.
    fn get_suggestions(
        &self,
        lines: &[String],
        cursor_line: usize,
        cursor_col: usize,
    ) -> Option<AutocompleteSuggestions>;

    /// Apply a selected item to the input.
    ///
    /// Returns the updated lines and cursor position.
    fn apply_completion(
        &self,
        lines: &[String],
        cursor_line: usize,
        cursor_col: usize,
        item: &AutocompleteItem,
        prefix: &str,
    ) -> CompletionApplyResult;
}

/// Result of applying a completion.
#[derive(Debug, Clone)]
pub struct CompletionApplyResult {
    pub lines: Vec<String>,
    pub cursor_line: usize,
    pub cursor_col: usize,
}

// ---------------------------------------------------------------------------
// Simple autocomplete provider (prefix-based matching)
// ---------------------------------------------------------------------------

/// A simple autocomplete provider that matches items by case-insensitive
/// prefix.
pub struct SimpleAutocomplete {
    items: Vec<AutocompleteItem>,
}

impl SimpleAutocomplete {
    pub fn new(items: Vec<AutocompleteItem>) -> Self {
        Self { items }
    }

    pub fn with_items(items: Vec<AutocompleteItem>) -> Self {
        Self { items }
    }
}

impl AutocompleteProvider for SimpleAutocomplete {
    fn get_suggestions(
        &self,
        lines: &[String],
        _cursor_line: usize,
        cursor_col: usize,
    ) -> Option<AutocompleteSuggestions> {
        let current_line = lines.first()?;
        let text_before = &current_line[..cursor_col.min(current_line.len())];

        // Find the word prefix at the cursor
        let word_start = text_before.rfind([' ', '\t']).map(|i| i + 1).unwrap_or(0);
        let prefix = &text_before[word_start..];

        if prefix.is_empty() {
            return None;
        }

        let query = prefix.to_lowercase();
        let matched: Vec<AutocompleteItem> =
            self.items.iter().filter(|item| item.label.to_lowercase().starts_with(&query)).take(20).cloned().collect();

        if matched.is_empty() {
            None
        } else {
            Some(AutocompleteSuggestions { items: matched, prefix: prefix.to_string() })
        }
    }

    fn apply_completion(
        &self,
        lines: &[String],
        cursor_line: usize,
        cursor_col: usize,
        item: &AutocompleteItem,
        prefix: &str,
    ) -> CompletionApplyResult {
        let current_line = lines.get(cursor_line).cloned().unwrap_or_default();
        let before_prefix = if cursor_col >= prefix.len() { &current_line[..cursor_col - prefix.len()] } else { "" };
        let after_cursor = &current_line[cursor_col..];

        let new_line = format!("{}{} {}", before_prefix, item.value, after_cursor);
        let mut new_lines = lines.to_vec();
        new_lines[cursor_line] = new_line;

        CompletionApplyResult { lines: new_lines, cursor_line, cursor_col: before_prefix.len() + item.value.len() + 1 }
    }
}

// ---------------------------------------------------------------------------
// AutocompleteList — a Component that renders suggestions
// ---------------------------------------------------------------------------

/// A component that renders autocomplete suggestions as a selectable list.
///
/// Typically displayed as an overlay above the input area.
pub struct AutocompleteList {
    items: Vec<AutocompleteItem>,
    selected_index: usize,
    max_visible: usize,
}

impl AutocompleteList {
    pub fn new(max_visible: usize) -> Self {
        Self { items: Vec::new(), selected_index: 0, max_visible: max_visible.max(1) }
    }

    pub fn set_suggestions(&mut self, suggestions: Vec<AutocompleteItem>) {
        self.items = suggestions;
        self.selected_index = 0;
    }

    pub fn selected_item(&self) -> Option<&AutocompleteItem> {
        self.items.get(self.selected_index)
    }

    pub fn selected_index(&self) -> usize {
        self.selected_index
    }

    pub fn move_selection_up(&mut self) {
        if !self.items.is_empty() {
            self.selected_index = if self.selected_index == 0 { self.items.len() - 1 } else { self.selected_index - 1 };
        }
    }

    pub fn move_selection_down(&mut self) {
        if !self.items.is_empty() {
            self.selected_index = if self.selected_index == self.items.len() - 1 { 0 } else { self.selected_index + 1 };
        }
    }
}

impl Component for AutocompleteList {
    fn render(&self, width: u16) -> Vec<String> {
        if self.items.is_empty() {
            return vec![];
        }

        let w = width as usize;
        let mut lines: Vec<String> = Vec::new();

        let visible_count = self.max_visible.min(self.items.len());
        for i in 0..visible_count {
            if let Some(item) = self.items.get(i) {
                let is_selected = i == self.selected_index;
                let prefix = if is_selected { "\u{2192} " } else { "  " };
                let display = truncate_to_width(&item.label, w.saturating_sub(visible_width(prefix)).saturating_sub(1));

                if is_selected {
                    // Highlight the selected item
                    lines.push(format!("\x1b[7m{}{}\x1b[27m", prefix, display));
                } else {
                    lines.push(format!("{}{}", prefix, display));
                }
            }
        }

        lines
    }

    fn handle_input(&mut self, data: &str) {
        let event = crate::keys::parse_key(data);
        if crate::keys::matches_key(&event, "up") {
            self.move_selection_up();
        } else if crate::keys::matches_key(&event, "down") {
            self.move_selection_down();
        }
    }

    fn invalidate(&mut self) {
        // No cached state
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_autocomplete_matches_prefix() {
        let provider = SimpleAutocomplete::new(vec![
            AutocompleteItem { value: "help".into(), label: "help".into(), description: Some("Show help".into()) },
            AutocompleteItem { value: "history".into(), label: "history".into(), description: None },
            AutocompleteItem { value: "run".into(), label: "run".into(), description: None },
        ]);

        let lines = vec!["h".to_string()];
        let result = provider.get_suggestions(&lines, 0, 1);
        assert!(result.is_some());
        let suggestions = result.unwrap();
        assert_eq!(suggestions.items.len(), 2);
        assert!(suggestions.items.iter().any(|i| i.value == "help"));
        assert!(suggestions.items.iter().any(|i| i.value == "history"));
    }

    #[test]
    fn test_simple_autocomplete_no_match() {
        let provider = SimpleAutocomplete::new(vec![AutocompleteItem {
            value: "help".into(),
            label: "help".into(),
            description: None,
        }]);

        let lines = vec!["xyz".to_string()];
        let result = provider.get_suggestions(&lines, 0, 3);
        assert!(result.is_none());
    }

    #[test]
    fn test_simple_autocomplete_empty_line() {
        let provider = SimpleAutocomplete::new(vec![AutocompleteItem {
            value: "help".into(),
            label: "help".into(),
            description: None,
        }]);

        let lines = vec![String::new()];
        let result = provider.get_suggestions(&lines, 0, 0);
        assert!(result.is_none());
    }

    #[test]
    fn test_autocomplete_list_renders() {
        let mut list = AutocompleteList::new(10);
        list.set_suggestions(vec![
            AutocompleteItem { value: "help".into(), label: "help".into(), description: None },
            AutocompleteItem { value: "history".into(), label: "history".into(), description: None },
        ]);
        let lines = list.render(80);
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("help"));
        assert!(lines[1].contains("history"));
    }

    #[test]
    fn test_autocomplete_list_empty() {
        let list = AutocompleteList::new(10);
        let lines = list.render(80);
        assert!(lines.is_empty());
    }

    #[test]
    fn test_autocomplete_list_navigation() {
        let mut list = AutocompleteList::new(10);
        list.set_suggestions(vec![
            AutocompleteItem { value: "a".into(), label: "Apple".into(), description: None },
            AutocompleteItem { value: "b".into(), label: "Banana".into(), description: None },
        ]);
        assert_eq!(list.selected_index(), 0);

        list.move_selection_down();
        assert_eq!(list.selected_index(), 1);

        list.move_selection_up();
        assert_eq!(list.selected_index(), 0);
    }

    #[test]
    fn test_autocomplete_list_highlights_selection() {
        let mut list = AutocompleteList::new(10);
        list.set_suggestions(vec![
            AutocompleteItem { value: "a".into(), label: "Apple".into(), description: None },
            AutocompleteItem { value: "b".into(), label: "Banana".into(), description: None },
        ]);
        let lines = list.render(80);
        // First line should have arrow and be highlighted
        assert!(lines[0].contains('\u{2192}'));
        assert!(lines[0].contains("\x1b[7m"));
    }

    #[test]
    fn test_apply_completion() {
        let provider = SimpleAutocomplete::new(vec![]);
        let item = AutocompleteItem { value: "help".into(), label: "help".into(), description: None };

        let result = provider.apply_completion(&["he".to_string()], 0, 2, &item, "he");
        assert_eq!(result.lines[0], "help ");
        assert_eq!(result.cursor_col, 5); // "help".len() + 1 (space)
    }
}
