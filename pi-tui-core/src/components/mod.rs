//! TUI components — building blocks for terminal user interfaces.
//!
//! Each component implements the [`Component`](crate::component::Component) trait
//! and provides a `render(width)` and `handle_input(data)` method.
//!
//! For Phase B.4 these are intentionally kept simple — they are the primitives
//! that richer widgets (chat, scrollable views, settings panels) will be built
//! from in later phases.

pub mod autocomplete;
pub mod r#box;
pub mod image;
pub mod input;
pub mod loader;
pub mod select_list;
pub mod settings_list;
pub mod spacer;
pub mod terminal_image;
pub mod text;
pub mod truncated_text;

pub mod editor;
pub mod markdown;
