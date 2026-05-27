//! Configuration selector for package resources (extensions, skills, prompts, themes).
//!
//! Shows a hierarchical view of resource groups and lets the user toggle
//! individual resources on/off.

use pi_tui_core::component::Component;
use pi_tui_core::components::input::Input;
use pi_tui_core::keys::{matches_key, parse_key};
use pi_tui_core::utils::truncate_to_width;
use crate::Theme;

/// Resource type being configured.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ResourceType {
    Extensions,
    Skills,
    Prompts,
    Themes,
}

impl ResourceType {
    pub fn label(&self) -> &'static str {
        match self {
            ResourceType::Extensions => "Extensions",
            ResourceType::Skills => "Skills",
            ResourceType::Prompts => "Prompts",
            ResourceType::Themes => "Themes",
        }
    }
}

/// A single resource entry.
#[derive(Debug, Clone)]
pub struct ResourceEntry {
    pub path: String,
    pub enabled: bool,
    pub resource_type: ResourceType,
    pub display_name: String,
    pub group_label: String,
    pub subgroup_label: String,
}

/// A hierarchical resource group.
#[derive(Debug, Clone)]
pub struct ResourceGroup {
    pub label: String,
    pub subgroups: Vec<ResourceSubgroup>,
}

/// A subgroup of resources of the same type.
#[derive(Debug, Clone)]
pub struct ResourceSubgroup {
    pub resource_type: ResourceType,
    pub label: String,
    pub items: Vec<ResourceEntry>,
}

/// Flattened item for display (group, subgroup, or resource item).
#[derive(Debug, Clone)]
enum FlatEntry {
    Group(String),
    Subgroup(String),
    Item(usize), // index into all_items
}

/// Resource configuration selector.
pub struct ConfigSelector {
    input: Input,
    groups: Vec<ResourceGroup>,
    all_items: Vec<ResourceEntry>,
    flat: Vec<FlatEntry>,
    filtered_flat: Vec<FlatEntry>,
    selected_index: usize,
    theme: Theme,
    /// Callback when a resource is toggled.
    pub on_toggle: Option<Box<dyn FnMut(usize, bool) + Send>>, // item index, new enabled state
    /// Callback when the user closes the config.
    pub on_cancel: Option<Box<dyn FnMut() + Send>>,
}

impl ConfigSelector {
    /// Create a new resource configuration selector.
    pub fn new(
        groups: Vec<ResourceGroup>,
        items: Vec<ResourceEntry>,
        theme: &Theme,
    ) -> Self {
        // Build flat structure
        let mut flat: Vec<FlatEntry> = Vec::new();
        let mut item_idx = 0;
        for group in &groups {
            flat.push(FlatEntry::Group(group.label.clone()));
            for subgroup in &group.subgroups {
                flat.push(FlatEntry::Subgroup(subgroup.label.clone()));
                for _ in &subgroup.items {
                    flat.push(FlatEntry::Item(item_idx));
                    item_idx += 1;
                }
            }
        }

        // Start selection on first item
        let first_item = flat.iter().position(|e| matches!(e, FlatEntry::Item(_))).unwrap_or(0);

        Self {
            input: Input::new(),
            groups,
            all_items: items,
            filtered_flat: flat.clone(),
            flat,
            selected_index: first_item,
            theme: theme.clone(),
            on_toggle: None,
            on_cancel: None,
        }
    }

    /// Toggle the item at the current selection.
    pub fn toggle_current(&mut self) {
        if let Some(FlatEntry::Item(idx)) = self.filtered_flat.get(self.selected_index).cloned() {
            if let Some(item) = self.all_items.get_mut(idx) {
                item.enabled = !item.enabled;
                let new_state = item.enabled;
                if let Some(ref mut cb) = self.on_toggle {
                    cb(idx, new_state);
                }
            }
        }
    }

    fn find_prev_item(&self, from: usize) -> usize {
        let mut i = from.saturating_sub(1);
        while i > 0 && !matches!(self.filtered_flat.get(i), Some(FlatEntry::Item(_))) {
            i -= 1;
        }
        if matches!(self.filtered_flat.get(i), Some(FlatEntry::Item(_))) {
            i
        } else {
            from
        }
    }

    fn find_next_item(&self, from: usize) -> usize {
        let mut i = from + 1;
        while i < self.filtered_flat.len() && !matches!(self.filtered_flat.get(i), Some(FlatEntry::Item(_))) {
            i += 1;
        }
        if i < self.filtered_flat.len() {
            i
        } else {
            from
        }
    }

    fn apply_filter(&mut self, query: &str) {
	if query.is_empty() {
	    self.filtered_flat = self.flat.clone();
	} else {
	    let lower = query.to_lowercase();
	    let matching_indices: std::collections::HashSet<usize> = self
		.all_items
		.iter()
		.enumerate()
		.filter(|(_, item)| {
		    item.display_name.to_lowercase().contains(&lower)
			|| item.path.to_lowercase().contains(&lower)
			|| item.resource_type.label().to_lowercase().contains(&lower)
		})
		.map(|(idx, _)| idx)
		.collect();

	    // Rebuild filtered flat with matching items + their groups/subgroups
	    self.filtered_flat.clear();
	    let mut global_idx = 0usize;
	    for group in &self.groups {
		let mut group_added = false;
		for subgroup in &group.subgroups {
		    let mut sg_added = false;
		    for _ in &subgroup.items {
			if matching_indices.contains(&global_idx) {
			    if !group_added {
				self.filtered_flat.push(FlatEntry::Group(group.label.clone()));
				group_added = true;
			    }
			    if !sg_added {
				let sg_key = format!("{}/{}", group.label, subgroup.label);
				self.filtered_flat.push(FlatEntry::Subgroup(sg_key));
				sg_added = true;
			    }
			    self.filtered_flat.push(FlatEntry::Item(global_idx));
			}
			global_idx += 1;
		    }
		}
	    }
	}

	// Reset selection to first item
	self.selected_index = self.filtered_flat.iter()
	    .position(|e| matches!(e, FlatEntry::Item(_)))
	    .unwrap_or(0);
    }
}

impl Component for ConfigSelector {
    fn render(&self, width: u16) -> Vec<String> {
        let w = width as usize;
        let mut lines: Vec<String> = Vec::new();

        // Header
        let title = self.theme.bold("Resource Configuration");
        let hint = self.theme.ansi(&self.theme.muted, "Space toggle  Esc close");
        let spacing = " ".repeat(w.saturating_sub(
            pi_tui_core::utils::visible_width(&title) + pi_tui_core::utils::visible_width(&hint) + 2
        ).max(1));
        lines.push(format!("{}{}{}", title, spacing, hint));
        lines.push(self.theme.ansi(&self.theme.muted, "Type to filter resources"));
        lines.push(String::new());

        // Search input
        lines.extend(self.input.render(width));
        lines.push(String::new());

        if self.filtered_flat.is_empty() {
            lines.push(self.theme.ansi(&self.theme.muted, "  No resources found"));
            return lines;
        }

        let max_visible = 15usize.min(self.filtered_flat.len());
        let half = max_visible / 2;
        let total = self.filtered_flat.len();
        let start = if self.selected_index < half {
            0
        } else if self.selected_index + half >= total {
            total.saturating_sub(max_visible)
        } else {
            self.selected_index.saturating_sub(half)
        };
        let end = (start + max_visible).min(total);

        for i in start..end {
            let is_selected = i == self.selected_index;
            match &self.filtered_flat[i] {
                FlatEntry::Group(label) => {
                    let styled = self.theme.ansi(&self.theme.primary, &self.theme.bold(label));
                    lines.push(format!("  {}", styled));
                }
                FlatEntry::Subgroup(label) => {
                    let styled = self.theme.ansi(&self.theme.muted, label);
                    lines.push(format!("    {}", styled));
                }
                FlatEntry::Item(idx) => {
                    if let Some(item) = self.all_items.get(*idx) {
                        let cursor = if is_selected { ">" } else { " " };
                        let checkbox = if item.enabled {
                            self.theme.ansi(&self.theme.success, "[x]")
                        } else {
                            self.theme.ansi(&self.theme.dim, "[ ]")
                        };
                        let name = if is_selected {
                            self.theme.bold(&item.display_name)
                        } else {
                            item.display_name.clone()
                        };
                        let line = truncate_to_width(&format!("{}  {}  {}", cursor, checkbox, name), w);
                        lines.push(line);
                    }
                }
            }
        }

        // Scroll indicator for items only
        if start > 0 || end < total {
            let item_count = self.filtered_flat.iter().filter(|e| matches!(e, FlatEntry::Item(_))).count();
            let items_before = self.filtered_flat[..self.selected_index]
                .iter()
                .filter(|e| matches!(e, FlatEntry::Item(_)))
                .count();
            let scroll = self.theme.ansi(&self.theme.muted,
                &format!("  ({}/{})", items_before + 1, item_count));
            lines.push(scroll);
        }

        lines
    }

    fn handle_input(&mut self, data: &str) {
        let event = parse_key(data);

        if matches_key(&event, "up") {
            self.selected_index = self.find_prev_item(self.selected_index);
        } else if matches_key(&event, "down") {
            self.selected_index = self.find_next_item(self.selected_index);
        } else if matches_key(&event, "enter") || data == " " {
            self.toggle_current();
        } else if matches_key(&event, "escape") {
            if let Some(ref mut cb) = self.on_cancel {
                cb();
            }
        } else {
            self.input.handle_input(data);
            let query = self.input.value().to_string();
            self.apply_filter(&query);
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
    fn test_config_selector_renders() {
        let theme = Theme::dark();
        let items = vec![
            ResourceEntry {
                path: "/home/user/.pi/extensions/my-ext".into(),
                enabled: true,
                resource_type: ResourceType::Extensions,
                display_name: "my-ext".into(),
                group_label: "User".into(),
                subgroup_label: "Extensions".into(),
            },
        ];
        let groups = vec![
            ResourceGroup {
                label: "User".into(),
                subgroups: vec![
                    ResourceSubgroup {
                        resource_type: ResourceType::Extensions,
                        label: "Extensions".into(),
                        items: items.clone(),
                    },
                ],
            },
        ];

        let selector = ConfigSelector::new(groups, items, &theme);
        let lines = selector.render(80);
        let joined = lines.join(" ");
        assert!(joined.contains("my-ext"));
        assert!(joined.contains("Resource Configuration"));
    }

    #[test]
    fn test_config_selector_empty() {
        let theme = Theme::dark();
        let selector = ConfigSelector::new(vec![], vec![], &theme);
        let lines = selector.render(80);
        let joined = lines.join(" ");
        assert!(joined.contains("No resources"));
    }

    #[test]
    fn test_config_selector_toggle() {
        let theme = Theme::dark();
        let items = vec![
            ResourceEntry {
                path: "/test/ext".into(),
                enabled: false,
                resource_type: ResourceType::Extensions,
                display_name: "test-ext".into(),
                group_label: "Test".into(),
                subgroup_label: "Extensions".into(),
            },
        ];
        let groups = vec![
            ResourceGroup {
                label: "Test".into(),
                subgroups: vec![
                    ResourceSubgroup {
                        resource_type: ResourceType::Extensions,
                        label: "Extensions".into(),
                        items: items.clone(),
                    },
                ],
            },
        ];

        let mut selector = ConfigSelector::new(groups, items, &theme);
        // First item should be selected
        assert!(!selector.all_items[0].enabled);
        selector.toggle_current();
        assert!(selector.all_items[0].enabled);
    }
}
