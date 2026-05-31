//! App-level selector and dialog components.
//!
//! Each component wraps TUI core widgets (SelectList, Input, SettingsList)
//! with application-specific data and styling.

pub mod assistant_message;
pub mod bash_execution;
pub mod branch_summary_message;
pub mod compaction_summary_message;
pub mod config_selector;
pub mod custom_message;
pub mod diff;
pub mod extension_selector;
pub mod footer;
pub mod login_dialog;
pub mod model_selector;
pub mod oauth_selector;
pub mod session_selector;
pub mod session_selector_search;
pub mod settings_selector;
pub mod skill_invocation_message;
pub mod theme_selector;
pub mod thinking_selector;
pub mod tool_execution;
pub mod user_message;

pub use config_selector::ConfigSelector;
pub use extension_selector::ExtensionSelector;
pub use login_dialog::LoginDialog;
pub use model_selector::ModelSelector;
pub use oauth_selector::OAuthSelector;
pub use session_selector::SessionSelector;
pub use session_selector_search::{
    MatchResult, NameFilter, ParsedSearchQuery, SortMode, filter_and_sort_sessions, match_session, parse_search_query,
};
pub use settings_selector::SettingsSelector;
pub use theme_selector::ThemeSelector;
pub use thinking_selector::ThinkingSelector;
