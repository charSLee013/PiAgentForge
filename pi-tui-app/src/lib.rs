//! Pi TUI App -- Application-level theme system with 45+ named color tokens,
//! dark/light variants, ANSI code generation, hot-reload file watching,
//! builder functions for component themes, and the InteractiveMode
//! orchestrator for `pi --interactive`.

pub mod components;
pub mod interactive_mode;
pub mod theme;
pub use interactive_mode::InteractiveMode;
pub use theme::Theme;
