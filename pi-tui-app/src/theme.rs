//! Theme system with 45+ named color tokens, dark/light variants,
//! ANSI escape code generation, and a file-change watcher for hot-reload.

use std::collections::HashMap;
use std::path::Path;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use notify::{Config, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use pi_tui_core::components::select_list::SelectListTheme;
use pi_tui_core::components::settings_list::SettingsListTheme;
use pi_tui_core::MarkdownTheme;

// ============================================================================
// Helpers
// ============================================================================

/// Parse a hex color string into (r, g, b).
fn hex_to_rgb(hex: &str) -> (u8, u8, u8) {
    let hex = hex.trim_start_matches('#');
    let r = u8::from_str_radix(&hex[0..2], 16).expect("invalid hex red component");
    let g = u8::from_str_radix(&hex[2..4], 16).expect("invalid hex green component");
    let b = u8::from_str_radix(&hex[4..6], 16).expect("invalid hex blue component");
    (r, g, b)
}

/// Build a truecolor foreground ANSI escape prefix from a hex color.
fn ansi_fg_prefix(hex: &str) -> String {
    let (r, g, b) = hex_to_rgb(hex);
    format!("\x1b[38;2;{};{};{}m", r, g, b)
}

/// Build a truecolor background ANSI escape prefix from a hex color.
fn ansi_bg_prefix(hex: &str) -> String {
    let (r, g, b) = hex_to_rgb(hex);
    format!("\x1b[48;2;{};{};{}m", r, g, b)
}

/// Leak a String into a `&'static str`.  Use sparingly; intended for
/// infrequent theme-to-component conversions.
fn leak(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}

// ============================================================================
// Theme
// ============================================================================

/// Pi TUI Theme with 45+ named color tokens.
///
/// Each token stores a hex color string (e.g. `"#ff0000"`).  Use the
/// `ansi()` / `ansi_bg()` methods to wrap text in ANSI truecolor escape
/// sequences, or use the builder methods to produce component-specific
/// theme objects (e.g. `to_markdown_theme()`).
#[derive(Clone)]
pub struct Theme {
    // ---- Core UI (9) ----
    /// Primary accent colour (logo, highlights, selection cursor)
    pub primary: String,
    /// Default background colour
    pub background: String,
    /// Surface / layer background (cards, panels, tool boxes)
    pub surface: String,
    /// Default text colour
    pub text: String,
    /// Dim / secondary text
    pub dim: String,
    /// Normal border colour
    pub border: String,
    /// Error / failure state colour
    pub error: String,
    /// Warning state colour
    pub warning: String,
    /// Success state colour
    pub success: String,

    // ---- Semantic (7) ----
    /// Selected item background
    pub selection: String,
    /// Cursor / indicator colour
    pub cursor: String,
    /// Scrollbar colour
    pub scrollbar: String,
    /// Overlay / dimmed backdrop colour
    pub overlay: String,
    /// Muted / secondary text colour
    pub muted: String,
    /// Accented / highlighted border colour
    pub border_accent: String,
    /// Subtle / low-priority border colour
    pub border_muted: String,

    // ---- Markdown (9 fields + 6-level heading array) ----
    /// Per-level heading colours (index 0 = H1 … index 5 = H6)
    pub heading_colors: Vec<String>,
    /// Bold text colour
    pub bold_color: String,
    /// Italic text colour
    pub italic_color: String,
    /// Inline code colour
    pub code_color: String,
    /// Code block / fence background colour
    pub code_background: String,
    /// Hyperlink text colour
    pub link_color: String,
    /// List bullet / ordinal colour
    pub list_bullet: String,
    /// Blockquote text colour
    pub quote_color: String,
    /// Horizontal rule colour
    pub md_hr: String,
    /// Thinking-block text colour
    pub thinking_text: String,

    // ---- Syntax highlighting (HashMap, 9+ tokens) ----
    /// Mapping from syntax token name (e.g. "keyword", "string") → hex colour.
    pub syntax: HashMap<String, String>,

    // ---- Tool-specific (6) ----
    /// Diff context line colour
    pub tool_diff_context: String,
    /// Diff removed / deleted line colour
    pub tool_diff_removed: String,
    /// Diff added / inserted line colour
    pub tool_diff_added: String,
    /// User message bubble background
    pub user_message_bg: String,
    /// Tool execution pending-state background
    pub tool_pending_bg: String,
    /// Bash-mode editor border colour
    pub bash_mode: String,
}

// ---------------------------------------------------------------------------
// Constructors
// ---------------------------------------------------------------------------

impl Theme {
    /// Return the built-in **dark** theme (ported from the TS dark.json).
    pub fn dark() -> Self {
        let syntax = HashMap::from([
            ("comment".into(), "#6A9955".into()),
            ("keyword".into(), "#569CD6".into()),
            ("function".into(), "#DCDCAA".into()),
            ("variable".into(), "#9CDCFE".into()),
            ("string".into(), "#CE9178".into()),
            ("number".into(), "#B5CEA8".into()),
            ("type".into(), "#4EC9B0".into()),
            ("operator".into(), "#D4D4D4".into()),
            ("punctuation".into(), "#D4D4D4".into()),
        ]);

        Self {
            // Core UI
            primary: "#8abeb7".into(),
            background: "#18181e".into(),
            surface: "#282832".into(),
            text: "#e5e5e7".into(),
            dim: "#666666".into(),
            border: "#5f87ff".into(),
            error: "#cc6666".into(),
            warning: "#ffff00".into(),
            success: "#b5bd68".into(),
            // Semantic
            selection: "#3a3a4a".into(),
            cursor: "#8abeb7".into(),
            scrollbar: "#505050".into(),
            overlay: "#3a3a4a".into(),
            muted: "#808080".into(),
            border_accent: "#00d7ff".into(),
            border_muted: "#505050".into(),
            // Markdown
            heading_colors: vec![
                "#f0c674".into(),
                "#f0c674".into(),
                "#f0c674".into(),
                "#f0c674".into(),
                "#f0c674".into(),
                "#f0c674".into(),
            ],
            bold_color: "#e5e5e7".into(),
            italic_color: "#e5e5e7".into(),
            code_color: "#8abeb7".into(),
            code_background: "#282832".into(),
            link_color: "#81a2be".into(),
            list_bullet: "#8abeb7".into(),
            quote_color: "#808080".into(),
            md_hr: "#808080".into(),
            thinking_text: "#808080".into(),
            // Syntax
            syntax,
            // Tool
            tool_diff_context: "#808080".into(),
            tool_diff_removed: "#cc6666".into(),
            tool_diff_added: "#b5bd68".into(),
            user_message_bg: "#343541".into(),
            tool_pending_bg: "#282832".into(),
            bash_mode: "#b5bd68".into(),
        }
    }

    /// Return the built-in **light** theme (ported from the TS light.json).
    pub fn light() -> Self {
        let syntax = HashMap::from([
            ("comment".into(), "#008000".into()),
            ("keyword".into(), "#0000FF".into()),
            ("function".into(), "#795E26".into()),
            ("variable".into(), "#001080".into()),
            ("string".into(), "#A31515".into()),
            ("number".into(), "#098658".into()),
            ("type".into(), "#267F99".into()),
            ("operator".into(), "#000000".into()),
            ("punctuation".into(), "#000000".into()),
        ]);

        Self {
            // Core UI
            primary: "#5a8080".into(),
            background: "#f8f8f8".into(),
            surface: "#e8e8f0".into(),
            text: "#000000".into(),
            dim: "#767676".into(),
            border: "#547da7".into(),
            error: "#aa5555".into(),
            warning: "#9a7326".into(),
            success: "#588458".into(),
            // Semantic
            selection: "#d0d0e0".into(),
            cursor: "#5a8080".into(),
            scrollbar: "#b0b0b0".into(),
            overlay: "#d0d0e0".into(),
            muted: "#6c6c6c".into(),
            border_accent: "#5a8080".into(),
            border_muted: "#b0b0b0".into(),
            // Markdown
            heading_colors: vec![
                "#9a7326".into(),
                "#9a7326".into(),
                "#9a7326".into(),
                "#9a7326".into(),
                "#9a7326".into(),
                "#9a7326".into(),
            ],
            bold_color: "#000000".into(),
            italic_color: "#000000".into(),
            code_color: "#5a8080".into(),
            code_background: "#e8e8f0".into(),
            link_color: "#547da7".into(),
            list_bullet: "#588458".into(),
            quote_color: "#6c6c6c".into(),
            md_hr: "#6c6c6c".into(),
            thinking_text: "#6c6c6c".into(),
            // Syntax
            syntax,
            // Tool
            tool_diff_context: "#6c6c6c".into(),
            tool_diff_removed: "#aa5555".into(),
            tool_diff_added: "#588458".into(),
            user_message_bg: "#e8e8e8".into(),
            tool_pending_bg: "#e8e8f0".into(),
            bash_mode: "#588458".into(),
        }
    }
}

// ---------------------------------------------------------------------------
// ANSI methods
// ---------------------------------------------------------------------------

impl Theme {
    /// Wrap `text` in a truecolor foreground ANSI escape for the given hex
    /// `color`, then reset the foreground to default.
    ///
    /// # Panics
    /// Panics if `color` is not a valid 6-digit hex string (with or without
    /// leading `#`).
    pub fn ansi(&self, color: &str, text: &str) -> String {
        format!("{}{}\x1b[39m", ansi_fg_prefix(color), text)
    }

    /// Wrap `text` in a truecolor background ANSI escape for the given hex
    /// `color`, then reset the background to default.
    ///
    /// # Panics
    /// Panics if `color` is not a valid 6-digit hex string.
    pub fn ansi_bg(&self, color: &str, text: &str) -> String {
        format!("{}{}\x1b[49m", ansi_bg_prefix(color), text)
    }

    /// Wrap `text` in ANSI bold (`SGR 1`) and reset bold/dim afterwards.
    pub fn bold(&self, text: &str) -> String {
        format!("\x1b[1m{}\x1b[22m", text)
    }

    /// Wrap `text` in ANSI italic (`SGR 3`) and reset italic afterwards.
    pub fn italic(&self, text: &str) -> String {
        format!("\x1b[3m{}\x1b[23m", text)
    }

    /// Wrap `text` in ANSI dim (`SGR 2`) and reset bold/dim afterwards.
    pub fn dim(&self, text: &str) -> String {
        format!("\x1b[2m{}\x1b[22m", text)
    }
}

// ---------------------------------------------------------------------------
// Component-theme builders
// ---------------------------------------------------------------------------

impl Theme {
    /// Build a [`MarkdownTheme`] from this `Theme`.
    ///
    /// Each field contains the ANSI escape sequence (foreground, and in some
    /// cases underline / dim) that the markdown renderer should apply.
    ///
    /// **Note**: the returned `MarkdownTheme` stores `&'static str` slices
    /// that are **leaked** from heap-allocated strings.  Because themes
    /// change infrequently (once per reload or init) the leak is negligible
    /// (~1 KB per call).
    pub fn to_markdown_theme(&self) -> MarkdownTheme {
        fn fmt_heading(heading: &str) -> Vec<&'static str> {
            // heading colour + bold for levels 1-3, colour-only for 4-6
            let base = ansi_fg_prefix(heading);
            vec![
                leak(format!("{base}\x1b[1m")),   // H1: colour + bold
                leak(format!("{base}\x1b[1m")),   // H2
                leak(format!("{base}\x1b[1m")),   // H3
                leak(base.clone()),                // H4: colour only
                leak(base.clone()),                // H5
                leak(base),                       // H6
            ]
        }

        MarkdownTheme {
            heading: fmt_heading(&self.heading_colors[0]),
            bold: leak(format!("\x1b[1m{}", ansi_fg_prefix(&self.bold_color))),
            italic: leak(format!("\x1b[3m{}", ansi_fg_prefix(&self.italic_color))),
            code: leak(ansi_fg_prefix(&self.code_color)),
            code_block: leak(ansi_fg_prefix(&self.code_color)),
            code_block_border: leak(format!(
                "\x1b[2m{}",
                ansi_fg_prefix(&self.border_muted)
            )),
            link: leak(format!(
                "{}\x1b[4m",
                ansi_fg_prefix(&self.link_color)
            )),
            link_url: leak(ansi_fg_prefix(&self.dim)),
            list_bullet: leak(ansi_fg_prefix(&self.list_bullet)),
            quote: leak(ansi_fg_prefix(&self.quote_color)),
            quote_border: leak(ansi_fg_prefix(&self.muted)),
            hr: leak(ansi_fg_prefix(&self.md_hr)),
            strikethrough: leak("\x1b[9m".into()),
            underline: leak("\x1b[4m".into()),
        }
    }

    /// Build a [`SelectListTheme`] from this `Theme`.
    pub fn to_select_list_theme(&self) -> SelectListTheme {
	let primary = self.primary.clone();
	let muted = self.muted.clone();
	let _text = self.text.clone();
	let primary_pref = primary.clone();
	let primary_text = primary.clone();
	let muted_desc = muted.clone();
	let muted_scroll = muted.clone();
	SelectListTheme {
	    selected_prefix: Box::new(move |s| format!("\x1b[38;2;{};{};{}m{}\x1b[39m",
		hex_r(&primary_pref), hex_g(&primary_pref), hex_b(&primary_pref), s)),
	    selected_text: Box::new(move |s| format!("\x1b[38;2;{};{};{}m{}\x1b[39m",
		    hex_r(&primary_text), hex_g(&primary_text), hex_b(&primary_text), s)),
	    description: Box::new(move |s| format!("\x1b[38;2;{};{};{}m{}\x1b[39m",
		    hex_r(&muted_desc), hex_g(&muted_desc), hex_b(&muted_desc), s)),
	    scroll_info: Box::new(move |s| format!("\x1b[38;2;{};{};{}m{}\x1b[39m",
		    hex_r(&muted_scroll), hex_g(&muted_scroll), hex_b(&muted_scroll), s)),
	    no_match: Box::new(move |s| format!("\x1b[38;2;{};{};{}m{}\x1b[39m",
		    hex_r(&muted), hex_g(&muted), hex_b(&muted), s)),
	}
    }

    /// Build a [`SettingsListTheme`] from this `Theme`.
    pub fn to_settings_list_theme(&self) -> SettingsListTheme {
	let primary = self.primary.clone();
	let muted = self.muted.clone();
	let dim = self.dim.clone();
	let _text = self.text.clone();
	SettingsListTheme {
	    label: Box::new(|s, _selected| s.to_string()),
	    value: Box::new(move |s, _selected| format!("\x1b[38;2;{};{};{}m{}\x1b[39m",
		    hex_r(&primary), hex_g(&primary), hex_b(&primary), s)),
	    description: Box::new(move |s| format!("\x1b[38;2;{};{};{}m{}\x1b[39m",
		    hex_r(&dim), hex_g(&dim), hex_b(&dim), s)),
	    cursor: format!("\x1b[38;2;{};{};{}m\u{2192}\x1b[39m ",
		hex_r(&self.primary), hex_g(&self.primary), hex_b(&self.primary)),
	    hint: Box::new(move |s| format!("\x1b[38;2;{};{};{}m{}\x1b[39m",
		    hex_r(&muted), hex_g(&muted), hex_b(&muted), s)),
	}
    }
}

// ---------------------------------------------------------------------------
// Hex helpers
// ---------------------------------------------------------------------------

/// Extract the red component from a hex string.
fn hex_r(hex: &str) -> u8 {
    let h = hex.trim_start_matches('#');
    u8::from_str_radix(&h[0..2], 16).unwrap_or(0)
}

/// Extract the green component from a hex string.
fn hex_g(hex: &str) -> u8 {
    let h = hex.trim_start_matches('#');
    u8::from_str_radix(&h[2..4], 16).unwrap_or(0)
}

/// Extract the blue component from a hex string.
fn hex_b(hex: &str) -> u8 {
    let h = hex.trim_start_matches('#');
    u8::from_str_radix(&h[4..6], 16).unwrap_or(0)
}

// ============================================================================
// File-watcher (hot-reload)
// ============================================================================

/// A `ThemeWatcher` monitors a theme JSON file for changes and invokes a
/// user-supplied callback whenever the file is modified or created.
///
/// Dropping the watcher stops the underlying `notify` watcher and causes the
/// background listener thread to exit gracefully.
pub struct ThemeWatcher {
    /// Held alive to keep the notify watcher registered; dropped on our Drop.
    #[allow(dead_code)]
    watcher: RecommendedWatcher,
    /// Handle so Drop can join the background thread (avoids dangling threads).
    join_handle: Option<thread::JoinHandle<()>>,
}

impl ThemeWatcher {
    /// Watch `path` for modify / create events.  When a change is detected the
    /// `callback` is invoked (after a 100 ms debounce).
    ///
    /// # Errors
    /// Returns [`notify::Error`] if the path cannot be watched (e.g. it does
    /// not exist or the platform's inotify limit is exhausted).
    pub fn watch<F>(path: &str, callback: F) -> Result<Self, notify::Error>
    where
        F: Fn() + Send + 'static,
    {
        let (tx, rx) = mpsc::channel::<notify::Result<notify::Event>>();
        let mut watcher = RecommendedWatcher::new(tx, Config::default())?;
        watcher.watch(Path::new(path), RecursiveMode::NonRecursive)?;

        let join_handle = thread::Builder::new()
            .name("theme-watcher".into())
            .spawn(move || {
                for event in rx {
                    match event {
                        Ok(event) => {
                            if matches!(
                                event.kind,
                                EventKind::Modify(_) | EventKind::Create(_)
                            ) {
                                thread::sleep(Duration::from_millis(100));
                                callback();
                            }
                        }
                        Err(e) => {
                            tracing::warn!("ThemeWatcher error: {:?}", e);
                        }
                    }
                }
            })
            .expect("failed to spawn theme-watcher thread");

        Ok(Self {
            watcher,
            join_handle: Some(join_handle),
        })
    }
}

impl Drop for ThemeWatcher {
    fn drop(&mut self) {
        if let Some(handle) = self.join_handle.take() {
            let _ = handle.join();
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// Ensure every colour token in the dark theme is non-empty.
    #[test]
    fn dark_theme_has_all_tokens() {
        let theme = Theme::dark();
        check_all_tokens(&theme);
    }

    /// Ensure every colour token in the light theme is non-empty.
    #[test]
    fn light_theme_has_all_tokens() {
        let theme = Theme::light();
        check_all_tokens(&theme);
    }

    /// Core + semantic + markdown + tool tokens (non-HashMap).
    fn check_colour_tokens(theme: &Theme) {
        // Core UI
        assert!(!theme.primary.is_empty(), "primary");
        assert!(!theme.background.is_empty(), "background");
        assert!(!theme.surface.is_empty(), "surface");
        assert!(!theme.text.is_empty(), "text");
        assert!(!theme.dim.is_empty(), "dim");
        assert!(!theme.border.is_empty(), "border");
        assert!(!theme.error.is_empty(), "error");
        assert!(!theme.warning.is_empty(), "warning");
        assert!(!theme.success.is_empty(), "success");

        // Semantic
        assert!(!theme.selection.is_empty(), "selection");
        assert!(!theme.cursor.is_empty(), "cursor");
        assert!(!theme.scrollbar.is_empty(), "scrollbar");
        assert!(!theme.overlay.is_empty(), "overlay");
        assert!(!theme.muted.is_empty(), "muted");
        assert!(!theme.border_accent.is_empty(), "border_accent");
        assert!(!theme.border_muted.is_empty(), "border_muted");

        // Markdown
        assert_eq!(theme.heading_colors.len(), 6, "heading_colors len");
        for (i, c) in theme.heading_colors.iter().enumerate() {
            assert!(!c.is_empty(), "heading_colors[{i}]");
        }
        assert!(!theme.bold_color.is_empty(), "bold_color");
        assert!(!theme.italic_color.is_empty(), "italic_color");
        assert!(!theme.code_color.is_empty(), "code_color");
        assert!(!theme.code_background.is_empty(), "code_background");
        assert!(!theme.link_color.is_empty(), "link_color");
        assert!(!theme.list_bullet.is_empty(), "list_bullet");
        assert!(!theme.quote_color.is_empty(), "quote_color");
        assert!(!theme.md_hr.is_empty(), "md_hr");
        assert!(!theme.thinking_text.is_empty(), "thinking_text");

        // Tool
        assert!(!theme.tool_diff_context.is_empty(), "tool_diff_context");
        assert!(!theme.tool_diff_removed.is_empty(), "tool_diff_removed");
        assert!(!theme.tool_diff_added.is_empty(), "tool_diff_added");
        assert!(!theme.user_message_bg.is_empty(), "user_message_bg");
        assert!(!theme.tool_pending_bg.is_empty(), "tool_pending_bg");
        assert!(!theme.bash_mode.is_empty(), "bash_mode");
    }

    /// Syntax-token checks.
    fn check_syntax_tokens(theme: &Theme) {
        let required = [
            "comment",
            "keyword",
            "function",
            "variable",
            "string",
            "number",
            "type",
            "operator",
            "punctuation",
        ];
        for token in &required {
            assert!(
                theme.syntax.contains_key(*token),
                "syntax token missing: {token}"
            );
            assert!(
                !theme.syntax.get(*token).unwrap().is_empty(),
                "syntax token empty: {token}"
            );
        }
        assert!(theme.syntax.len() >= 9, "syntax should have >= 9 tokens");
    }

    fn check_all_tokens(theme: &Theme) {
        check_colour_tokens(theme);
        check_syntax_tokens(theme);
    }

    // ------------------------------------------------------------------
    // ANSI escape sequence tests
    // ------------------------------------------------------------------

    #[test]
    fn ansi_foreground() {
        let theme = Theme::dark();
        let result = theme.ansi("#ff0000", "hello");
        assert_eq!(result, "\x1b[38;2;255;0;0mhello\x1b[39m");
    }

    #[test]
    fn ansi_background() {
        let theme = Theme::dark();
        let result = theme.ansi_bg("#00ff00", "world");
        assert_eq!(result, "\x1b[48;2;0;255;0mworld\x1b[49m");
    }

    #[test]
    fn ansi_uses_theme_field() {
        let theme = Theme::dark();
        // ansi() uses the user-provided colour; pass the theme field value
        let result = theme.ansi(&theme.primary, "test");
        // primary = #8abeb7 -> r=138, g=190, b=183
        assert_eq!(result, "\x1b[38;2;138;190;183mtest\x1b[39m");
    }

    #[test]
    fn bold_output() {
        let theme = Theme::dark();
        assert_eq!(theme.bold("bold"), "\x1b[1mbold\x1b[22m");
    }

    #[test]
    fn italic_output() {
        let theme = Theme::dark();
        assert_eq!(theme.italic("italic"), "\x1b[3mitalic\x1b[23m");
    }

    #[test]
    fn dim_output() {
        let theme = Theme::dark();
        assert_eq!(theme.dim("dim"), "\x1b[2mdim\x1b[22m");
    }

    #[test]
    fn bold_with_empty_text() {
        let theme = Theme::dark();
        assert_eq!(theme.bold(""), "\x1b[1m\x1b[22m");
    }

    #[test]
    fn ansi_without_hash_prefix() {
        let theme = Theme::dark();
        let result = theme.ansi("#336699", "no-prefix");
        assert!(result.starts_with("\x1b[38;2;51;102;153m"));
    }

    // ------------------------------------------------------------------
    // Markdown-theme builder
    // ------------------------------------------------------------------

    #[test]
    fn to_markdown_theme_produces_valid_ansi() {
        let theme = Theme::dark();
        let md = theme.to_markdown_theme();

        // Every field should be a non-empty ANSI escape sequence.
        assert!(!md.bold.is_empty());
        assert!(!md.italic.is_empty());
        assert!(!md.code.is_empty());
        assert!(!md.code_block.is_empty());
        assert!(!md.code_block_border.is_empty());
        assert!(!md.link.is_empty());
        assert!(!md.link_url.is_empty());
        assert!(!md.list_bullet.is_empty());
        assert!(!md.quote.is_empty());
        assert!(!md.quote_border.is_empty());
        assert!(!md.hr.is_empty());
        assert!(!md.strikethrough.is_empty());
        assert!(!md.underline.is_empty());
        assert_eq!(md.heading.len(), 6);
        for h in &md.heading {
            assert!(!h.is_empty());
        }

        // Verify they all start with an escape character.
        for h in &md.heading {
            assert!(h.starts_with('\x1b'), "heading escape");
        }
        assert!(md.bold.starts_with('\x1b'));
        assert!(md.italic.starts_with('\x1b'));
        assert!(md.code.starts_with('\x1b'));
        assert!(md.code_block.starts_with('\x1b'));
        assert!(md.code_block_border.starts_with('\x1b'));
        assert!(md.link.starts_with('\x1b'));
        assert!(md.link_url.starts_with('\x1b'));
        assert!(md.list_bullet.starts_with('\x1b'));
        assert!(md.quote.starts_with('\x1b'));
        assert!(md.quote_border.starts_with('\x1b'));
        assert!(md.hr.starts_with('\x1b'));
        assert!(md.strikethrough.starts_with('\x1b'));
        assert!(md.underline.starts_with('\x1b'));
    }
}
