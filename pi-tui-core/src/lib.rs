//! Pi TUI Core -- Terminal UI library with differential rendering.
//! Mirrors packages/tui/src/
//!
//! This crate provides the foundation for building terminal UIs:
//! - A [`Terminal`] abstraction over crossterm for raw I/O
//! - A [`Component`] trait for renderable UI elements
//! - A [`Container`] for composing multiple components

pub mod component;
pub mod components;
pub mod fuzzy;
pub mod keybindings;
pub mod keys;
pub mod stdin_buffer;
pub mod terminal;
pub mod tui;
pub mod utils;

pub use component::{Component, Container};
pub use components::markdown::{Markdown, MarkdownTheme};
pub use terminal::Terminal;
pub use tui::{OverlayAnchor, OverlayOptions, TUI};
pub use utils::visible_width;
