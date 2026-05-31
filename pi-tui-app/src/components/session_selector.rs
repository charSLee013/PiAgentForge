//! Session selector component.
//!
//! Shows a scrollable list of sessions with filtering and sorting.
//! Simplified Rust version of the TS SessionSelectorComponent.

use super::session_selector_search::{NameFilter, SortMode, filter_and_sort_sessions};
use crate::Theme;
use pi_tui_core::component::Component;
use pi_tui_core::components::input::Input;
use pi_tui_core::keys::{matches_key, parse_key};
use pi_tui_core::utils::truncate_to_width;

type SessionSelectCallback = Box<dyn FnMut(&str) + Send>;

/// A session entry for display.
#[derive(Debug, Clone)]
pub struct SessionEntry {
    pub id: String,
    pub name: Option<String>,
    pub search_text: String,
    pub has_name: bool,
}

/// Session selector component with search and filtering.
pub struct SessionSelector {
    input: Input,
    sessions: Vec<SessionEntry>,
    /// Indices of filtered + sorted sessions (into `sessions`).
    filtered: Vec<usize>,
    selected_index: usize,
    theme: Theme,
    sort_mode: SortMode,
    name_filter: NameFilter,
    /// Callback when a session is selected.
    pub on_select: Option<SessionSelectCallback>, // session id
    /// Callback when cancelled.
    pub on_cancel: Option<Box<dyn FnMut() + Send>>,
}

impl SessionSelector {
    /// Create a new session selector.
    pub fn new(sessions: Vec<SessionEntry>, theme: &Theme) -> Self {
        let count = sessions.len();
        let filtered: Vec<usize> = (0..count).collect();
        Self {
            input: Input::new(),
            sessions,
            filtered,
            selected_index: 0,
            theme: theme.clone(),
            sort_mode: SortMode::Recent,
            name_filter: NameFilter::All,
            on_select: None,
            on_cancel: None,
        }
    }

    /// Toggle between All / Named name filter.
    pub fn toggle_name_filter(&mut self) {
        self.name_filter = match self.name_filter {
            NameFilter::All => NameFilter::Named,
            NameFilter::Named => NameFilter::All,
        };
        self.apply_filter();
    }

    /// Cycle sort mode.
    pub fn toggle_sort_mode(&mut self) {
        self.sort_mode = match self.sort_mode {
            SortMode::Recent => SortMode::Relevance,
            SortMode::Relevance => SortMode::Recent,
        };
        self.apply_filter();
    }

    /// Set sessions (called after loading completes).
    pub fn set_sessions(&mut self, sessions: Vec<SessionEntry>) {
        self.sessions = sessions;
        self.selected_index = 0;
        self.apply_filter();
    }

    fn apply_filter(&mut self) {
        let search_data: Vec<(&str, &str, bool)> =
            self.sessions.iter().map(|s| (s.id.as_str(), s.search_text.as_str(), s.has_name)).collect();

        let results = filter_and_sort_sessions(&search_data, self.input.value(), self.sort_mode, self.name_filter);

        self.filtered = results.iter().map(|(idx, _)| *idx).collect();
        self.selected_index = self.selected_index.min(self.filtered.len().saturating_sub(1));
    }

    fn visible_index_range(&self, max_visible: usize) -> (usize, usize) {
        let total = self.filtered.len();
        if total == 0 {
            return (0, 0);
        }
        let half = max_visible / 2;
        let start = if self.selected_index < half {
            0
        } else if self.selected_index + half >= total {
            total.saturating_sub(max_visible)
        } else {
            self.selected_index.saturating_sub(half)
        };
        let end = (start + max_visible).min(total);
        (start, end)
    }
}

impl Component for SessionSelector {
    fn render(&self, width: u16) -> Vec<String> {
        let w = width as usize;
        let mut lines: Vec<String> = Vec::new();

        // Title with mode info
        let sort_label = match self.sort_mode {
            SortMode::Recent => "Recent",
            SortMode::Relevance => "Relevance",
        };
        let name_label = match self.name_filter {
            NameFilter::All => "All",
            NameFilter::Named => "Named",
        };

        let title = self.theme.bold("Resume Session");
        let info = format!(
            "{} Sort: {} | Name: {}",
            self.theme.ansi(&self.theme.muted, "|"),
            self.theme.ansi(&self.theme.primary, sort_label),
            self.theme.ansi(&self.theme.primary, name_label),
        );
        lines.push(format!("{}  {}", title, info));

        // Hints
        let hints = self
            .theme
            .ansi(&self.theme.dim, "re:pattern for regex  \"phrase\" for exact  Tab:scope  s:sort  n:named-filter");
        lines.push(hints);
        lines.push(String::new());

        // Search input
        lines.extend(self.input.render(width));
        lines.push(String::new());

        if self.filtered.is_empty() {
            lines.push(self.theme.ansi(
                &self.theme.muted,
                if self.sessions.is_empty() { "  No sessions" } else { "  No matching sessions" },
            ));
            return lines;
        }

        // Render visible slice
        let max_visible = 10usize;
        let (start, end) = self.visible_index_range(max_visible);

        for i in start..end {
            let idx = self.filtered[i];
            let session = &self.sessions[idx];
            let is_selected = i == self.selected_index;

            let display_name = match &session.name {
                Some(n) if !n.is_empty() => truncate_to_width(n, w.saturating_sub(30)),
                _ => truncate_to_width(&session.id, w.saturating_sub(30)),
            };
            let truncated = truncate_to_width(&display_name, w.saturating_sub(20));

            if is_selected {
                let prefix = self.theme.ansi(&self.theme.primary, "\u{2192} ");
                let text = self.theme.ansi(&self.theme.primary, &truncated);
                lines.push(format!("{}{}", prefix, text));
            } else {
                lines.push(format!("  {}", truncated));
            }
        }

        // Scroll indicator
        if start > 0 || end < self.filtered.len() {
            let scroll =
                self.theme.ansi(&self.theme.muted, &format!("  ({}/{})", self.selected_index + 1, self.filtered.len()));
            lines.push(scroll);
        }

        lines
    }

    fn handle_input(&mut self, data: &str) {
        let event = parse_key(data);

        if matches_key(&event, "up") {
            if self.filtered.is_empty() {
                return;
            }
            self.selected_index =
                if self.selected_index == 0 { self.filtered.len() - 1 } else { self.selected_index - 1 };
        } else if matches_key(&event, "down") {
            if self.filtered.is_empty() {
                return;
            }
            self.selected_index =
                if self.selected_index == self.filtered.len() - 1 { 0 } else { self.selected_index + 1 };
        } else if matches_key(&event, "enter") {
            if let Some(&idx) = self.filtered.get(self.selected_index) {
                let id = self.sessions[idx].id.clone();
                if let Some(ref mut cb) = self.on_select {
                    cb(&id);
                }
            }
        } else if matches_key(&event, "escape") {
            if let Some(ref mut cb) = self.on_cancel {
                cb();
            }
        } else if data == "s" || data == "S" {
            self.toggle_sort_mode();
        } else if data == "n" || data == "N" {
            self.toggle_name_filter();
        } else {
            self.input.handle_input(data);
            self.apply_filter();
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

    fn make_session(id: &str, name: Option<&str>) -> SessionEntry {
        let search = format!("{} {} some messages", id, name.unwrap_or(""));
        SessionEntry {
            id: id.to_string(),
            name: name.map(|s| s.to_string()),
            search_text: search,
            has_name: name.is_some_and(|s| !s.is_empty()),
        }
    }

    #[test]
    fn test_session_selector_renders_sessions() {
        let theme = Theme::dark();
        let sessions = vec![make_session("s1", Some("My Session")), make_session("s2", None)];
        let selector = SessionSelector::new(sessions, &theme);
        let lines = selector.render(80);
        let joined = lines.join(" ");
        assert!(joined.contains("My Session"));
        assert!(joined.contains("s2"));
        assert!(joined.contains("Resume"));
    }

    #[test]
    fn test_session_selector_empty() {
        let theme = Theme::dark();
        let selector = SessionSelector::new(vec![], &theme);
        let lines = selector.render(80);
        let joined = lines.join(" ");
        assert!(joined.contains("No sessions"));
    }

    #[test]
    fn test_session_selector_toggle_name_filter() {
        let theme = Theme::dark();
        let sessions = vec![make_session("s1", Some("Named Session")), make_session("s2", None)];
        let mut selector = SessionSelector::new(sessions, &theme);
        // Initially All, both visible
        assert_eq!(selector.filtered.len(), 2);
        // Toggle to Named
        selector.toggle_name_filter();
        assert_eq!(selector.name_filter, NameFilter::Named);
    }

    #[test]
    fn test_session_selector_navigation() {
        let theme = Theme::dark();
        let sessions = vec![make_session("s1", Some("A")), make_session("s2", Some("B"))];
        let mut selector = SessionSelector::new(sessions, &theme);
        assert_eq!(selector.selected_index, 0);
        selector.handle_input("\x1b[B"); // Down
        assert_eq!(selector.selected_index, 1);
    }
}
