//! Keybinding manager — maps key events to named actions.
//!
//! Provides default keybinding sets for editor and app modes,
//! and a `resolve` method that matches a `KeyEvent` against registered
//! bindings.
//!
//! Based on the TypeScript `keybindings.ts`.

use std::collections::HashMap;

use crate::keys::{matches_key, KeyEvent};

/// A named action triggered by a keybinding, e.g. `"submit"`, `"cancel"`.
pub type KeyAction = String;

/// Keybinding manager.
///
/// Stores a mapping from key description strings (like `"ctrl+c"`, `"enter"`)
/// to action names.
pub struct Keybindings {
    /// Map of key_id → action.
    bindings: HashMap<String, KeyAction>,
}

impl Keybindings {
    /// Create an empty keybinding set.
    pub fn new() -> Self {
        Self {
            bindings: HashMap::new(),
        }
    }

    /// Register a keybinding.
    ///
    /// `key_id` is a description like `"ctrl+c"`, `"enter"`, or `"alt+left"`.
    /// `action` is the action name to trigger (e.g. `"submit"`, `"cursorUp"`).
    pub fn bind(&mut self, key_id: &str, action: KeyAction) {
        self.bindings.insert(key_id.to_string(), action);
    }

    /// Remove a keybinding by its key ID.
    pub fn unbind(&mut self, key_id: &str) {
        self.bindings.remove(key_id);
    }

    /// Resolve a `KeyEvent` to an action name.
    ///
    /// Returns `Some(action)` if a matching keybinding is found.
    /// Returns `None` if no binding matches the event.
    pub fn resolve(&self, event: &KeyEvent) -> Option<&str> {
        for (key_id, action) in &self.bindings {
            if matches_key(event, key_id) {
                return Some(action);
            }
        }
        None
    }

    /// Check whether a specific key_id is bound.
    pub fn is_bound(&self, key_id: &str) -> bool {
        self.bindings.contains_key(key_id)
    }

    /// Get the action bound to a specific key_id, if any.
    pub fn action_for(&self, key_id: &str) -> Option<&str> {
        self.bindings.get(key_id).map(|s| s.as_str())
    }

    /// Number of registered bindings.
    pub fn len(&self) -> usize {
        self.bindings.len()
    }

    /// Whether this set is empty.
    pub fn is_empty(&self) -> bool {
        self.bindings.is_empty()
    }

    // ------------------------------------------------------------------
    // Default keybinding sets
    // ------------------------------------------------------------------

    /// Default keybindings for text editor mode.
    ///
    /// Includes cursor movement, word navigation, deletion operations,
    /// undo/yank, and page scrolling.
    pub fn default_editor() -> Self {
        let mut s = Self::new();
        s.bind("up", "cursorUp".into());
        s.bind("down", "cursorDown".into());
        s.bind("left", "cursorLeft".into());
        s.bind("ctrl+b", "cursorLeft".into());
        s.bind("right", "cursorRight".into());
        s.bind("ctrl+f", "cursorRight".into());
        s.bind("alt+left", "cursorWordLeft".into());
        s.bind("ctrl+left", "cursorWordLeft".into());
        s.bind("alt+b", "cursorWordLeft".into());
        s.bind("alt+right", "cursorWordRight".into());
        s.bind("ctrl+right", "cursorWordRight".into());
        s.bind("alt+f", "cursorWordRight".into());
        s.bind("home", "cursorLineStart".into());
        s.bind("ctrl+a", "cursorLineStart".into());
        s.bind("end", "cursorLineEnd".into());
        s.bind("ctrl+e", "cursorLineEnd".into());
        s.bind("pageup", "pageUp".into());
        s.bind("pagedown", "pageDown".into());
        s.bind("backspace", "deleteCharBackward".into());
        s.bind("delete", "deleteCharForward".into());
        s.bind("ctrl+d", "deleteCharForward".into());
        s.bind("ctrl+w", "deleteWordBackward".into());
        s.bind("alt+backspace", "deleteWordBackward".into());
        s.bind("alt+d", "deleteWordForward".into());
        s.bind("alt+delete", "deleteWordForward".into());
        s.bind("ctrl+u", "deleteToLineStart".into());
        s.bind("ctrl+k", "deleteToLineEnd".into());
        s.bind("ctrl+y", "yank".into());
        s.bind("alt+y", "yankPop".into());
        s.bind("ctrl+-", "undo".into());
        s.bind("ctrl+c", "copy".into());
        // Jump-to-character
        s.bind("ctrl+]", "jumpForward".into());
        s.bind("ctrl+alt+]", "jumpBackward".into());
        s
    }

    /// Default keybindings for input / app mode.
    ///
    /// Includes submit, newline, tab, copy, and directional navigation.
    /// Selection-specific bindings (confirm, cancel) are not included here
    /// since they may conflict with input bindings; they should be added by
    /// the caller when in a selection context.
    pub fn default_app() -> Self {
        let mut s = Self::new();
        s.bind("enter", "submit".into());
        s.bind("shift+enter", "newLine".into());
        s.bind("tab", "tab".into());
        s.bind("ctrl+c", "copy".into());
        s
    }
}

impl Default for Keybindings {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for Keybindings {
    fn clone(&self) -> Self {
        Self {
            bindings: self.bindings.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::{KeyCode, KeyModifiers};

    fn make_event(code: KeyCode, ctrl: bool, alt: bool, shift: bool) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers { ctrl, alt, shift },
        }
    }

    #[test]
    fn test_bind_and_resolve() {
        let mut kb = Keybindings::new();
        kb.bind("ctrl+c", "copy".into());
        kb.bind("enter", "submit".into());

        let event = make_event(KeyCode::Char('c'), true, false, false);
        assert_eq!(kb.resolve(&event), Some("copy"));

        let event = make_event(KeyCode::Enter, false, false, false);
        assert_eq!(kb.resolve(&event), Some("submit"));
    }

    #[test]
    fn test_resolve_no_match() {
        let kb = Keybindings::new();
        let event = make_event(KeyCode::Char('x'), false, false, false);
        assert_eq!(kb.resolve(&event), None);
    }

    #[test]
    fn test_resolve_alt_arrow() {
        let mut kb = Keybindings::new();
        kb.bind("alt+left", "cursorWordLeft".into());

        let event = make_event(KeyCode::Left, false, true, false);
        assert_eq!(kb.resolve(&event), Some("cursorWordLeft"));

        // Plain left should NOT resolve
        let plain = make_event(KeyCode::Left, false, false, false);
        assert_eq!(kb.resolve(&plain), None);
    }

    #[test]
    fn test_unbind() {
        let mut kb = Keybindings::new();
        kb.bind("ctrl+c", "copy".into());
        assert!(kb.is_bound("ctrl+c"));
        kb.unbind("ctrl+c");
        assert!(!kb.is_bound("ctrl+c"));
    }

    #[test]
    fn test_is_empty() {
        let kb = Keybindings::new();
        assert!(kb.is_empty());
    }

    #[test]
    fn test_action_for() {
        let mut kb = Keybindings::new();
        kb.bind("escape", "cancel".into());
        assert_eq!(kb.action_for("escape"), Some("cancel"));
        assert_eq!(kb.action_for("enter"), None);
    }

    #[test]
    fn test_default_editor_has_bindings() {
        let kb = Keybindings::default_editor();
        assert!(!kb.is_empty());
        // Check that common editor bindings exist
        assert!(kb.is_bound("ctrl+c")); // copy
        assert!(kb.is_bound("ctrl+a")); // cursorLineStart
        assert!(kb.is_bound("ctrl+e")); // cursorLineEnd
        assert!(kb.is_bound("ctrl+b")); // cursorLeft
        assert!(kb.is_bound("ctrl+f")); // cursorRight
        assert!(kb.is_bound("ctrl+d")); // deleteCharForward
        assert!(kb.is_bound("ctrl+w")); // deleteWordBackward
        assert!(kb.is_bound("ctrl+u")); // deleteToLineStart
        assert!(kb.is_bound("ctrl+k")); // deleteToLineEnd
        assert!(kb.is_bound("ctrl+y")); // yank
        assert!(kb.is_bound("alt+y")); // yankPop
        assert!(kb.is_bound("ctrl+-")); // undo
    }

    #[test]
    fn test_default_editor_resolve() {
        let kb = Keybindings::default_editor();

        // Alt+left → cursorWordLeft
        let event = make_event(KeyCode::Left, false, true, false);
        assert_eq!(kb.resolve(&event), Some("cursorWordLeft"));

        // Ctrl+A → cursorLineStart
        let event = make_event(KeyCode::Char('a'), true, false, false);
        assert_eq!(kb.resolve(&event), Some("cursorLineStart"));

        // Ctrl+hyphen → undo
        let event = make_event(KeyCode::Char('_'), true, false, false);
        assert_eq!(kb.resolve(&event), Some("undo"));
    }

    #[test]
    fn test_default_app_has_bindings() {
        let kb = Keybindings::default_app();
        assert!(!kb.is_empty());
        assert!(kb.is_bound("enter"));
        assert!(kb.is_bound("shift+enter"));
        assert!(kb.is_bound("tab"));
        assert!(kb.is_bound("ctrl+c"));
    }

    #[test]
    fn test_default_app_resolve() {
        let kb = Keybindings::default_app();

        // Enter → submit
        let event = make_event(KeyCode::Enter, false, false, false);
        assert_eq!(kb.resolve(&event), Some("submit"));

        // Ctrl+C → copy
        let event = make_event(KeyCode::Char('c'), true, false, false);
        assert_eq!(kb.resolve(&event), Some("copy"));
    }

    #[test]
    fn test_clone() {
        let mut kb = Keybindings::new();
        kb.bind("escape", "cancel".into());
        let cloned = kb.clone();
        assert!(cloned.is_bound("escape"));
    }

    #[test]
    fn test_len() {
        let mut kb = Keybindings::new();
        assert_eq!(kb.len(), 0);
        kb.bind("a", "action_a".into());
        assert_eq!(kb.len(), 1);
    }
}
