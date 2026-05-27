//! App-level selector and dialog components.
//!
//! Each component wraps TUI core widgets (SelectList, Input, SettingsList)
//! with application-specific data and styling.

pub mod theme_selector;
pub mod thinking_selector;
pub mod extension_selector;
pub mod oauth_selector;
pub mod login_dialog;
pub mod model_selector;
pub mod session_selector;
pub mod session_selector_search;
pub mod settings_selector;
pub mod config_selector;
pub mod assistant_message;
pub mod bash_execution;
pub mod branch_summary_message;
pub mod compaction_summary_message;
pub mod custom_message;
pub mod diff;
pub mod footer;
pub mod skill_invocation_message;
pub mod tool_execution;
pub mod user_message;

pub use theme_selector::ThemeSelector;
pub use thinking_selector::ThinkingSelector;
pub use extension_selector::ExtensionSelector;
pub use oauth_selector::OAuthSelector;
pub use login_dialog::LoginDialog;
pub use model_selector::ModelSelector;
pub use session_selector::SessionSelector;
pub use session_selector_search::{parse_search_query, match_session, filter_and_sort_sessions, ParsedSearchQuery, MatchResult, SortMode, NameFilter};
pub use settings_selector::SettingsSelector;
pub use config_selector::ConfigSelector;
