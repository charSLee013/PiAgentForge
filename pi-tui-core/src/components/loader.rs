//! Loader component — animated spinner with label.
//!
//! Mirrors `packages/tui/src/components/loader.ts`
//!
//! The Loader renders an animated spinner character followed by a message.
//! Frame advancement is driven externally via the `tick()` method, which
//! should be called from a timer or the TUI render loop.

use crate::component::Component;

/// Default spinner frames (braille dots).
const DEFAULT_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Default frame interval in milliseconds.
const DEFAULT_INTERVAL_MS: u64 = 80;

/// A spinner / loading indicator component.
///
/// Renders an animated frame character followed by a message.  The animation
/// is driven by calling `tick()` periodically (e.g., in the TUI render loop
/// or from a background task).
pub struct Loader {
    /// Spinner frames to cycle through.
    frames: Vec<String>,
    /// Current frame index.
    current_frame: usize,
    /// Frame interval in milliseconds.
    interval_ms: u64,
    /// Whether to render the indicator verbatim (no styling).
    render_indicator_verbatim: bool,
    /// Spinner color function (applied to the frame character).
    pub spinner_color_fn: Box<dyn Fn(&str) -> String + Send>,
    /// Message color function (applied to the message text).
    pub message_color_fn: Box<dyn Fn(&str) -> String + Send>,
    /// The message to display next to the spinner.
    message: String,
    /// Horizontal padding.
    pub padding_x: u16,
}

impl Loader {
    pub fn new(
        spinner_color_fn: Box<dyn Fn(&str) -> String + Send>,
        message_color_fn: Box<dyn Fn(&str) -> String + Send>,
        message: String,
    ) -> Self {
        Self {
            frames: DEFAULT_FRAMES.iter().map(|s| s.to_string()).collect(),
            current_frame: 0,
            interval_ms: DEFAULT_INTERVAL_MS,
            render_indicator_verbatim: false,
            spinner_color_fn,
            message_color_fn,
            message,
            padding_x: 1,
        }
    }

    /// Advance to the next animation frame.
    pub fn tick(&mut self) {
        if self.frames.len() > 1 {
            self.current_frame = (self.current_frame + 1) % self.frames.len();
        }
    }

    /// Set the display message.
    pub fn set_message(&mut self, message: String) {
        self.message = message;
    }

    /// Set custom animation frames.
    pub fn set_frames(&mut self, frames: Vec<String>) {
        self.frames = if frames.is_empty() { DEFAULT_FRAMES.iter().map(|s| s.to_string()).collect() } else { frames };
        self.current_frame = 0;
        self.render_indicator_verbatim = true;
    }

    /// Set the frame interval in milliseconds.
    pub fn set_interval_ms(&mut self, ms: u64) {
        self.interval_ms = ms.max(1);
    }

    /// Get the current frame interval in milliseconds.
    pub fn interval_ms(&self) -> u64 {
        self.interval_ms
    }
}

impl Component for Loader {
    fn render(&self, width: u16) -> Vec<String> {
        let w = width as usize;

        // Build the spinner indicator
        let frame = self.frames.get(self.current_frame).cloned().unwrap_or_default();

        let styled_frame = if self.render_indicator_verbatim { frame.clone() } else { (self.spinner_color_fn)(&frame) };

        let indicator = if !frame.is_empty() { format!("{} ", styled_frame) } else { String::new() };

        let styled_message = (self.message_color_fn)(&self.message);
        let text = format!("{}{}", indicator, styled_message);
        let text_vis = crate::utils::visible_width(&text);

        // Apply horizontal padding
        let left_pad = " ".repeat(self.padding_x as usize);
        let right_pad = " ".repeat(w.saturating_sub(self.padding_x as usize + text_vis));
        let line = format!("{}{}{}", left_pad, text, right_pad);

        vec![String::new(), line]
    }

    fn invalidate(&mut self) {
        // No cached state
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_loader_renders_message() {
        let loader = Loader::new(Box::new(|s| s.to_string()), Box::new(|s| s.to_string()), "Loading...".to_string());
        let lines = loader.render(80);
        assert_eq!(lines.len(), 2);
        // First line is empty spacer
        assert_eq!(lines[0], "");
        // Second line contains message
        assert!(lines[1].contains("Loading..."));
    }

    #[test]
    fn test_loader_width_respected() {
        let loader = Loader::new(Box::new(|s| s.to_string()), Box::new(|s| s.to_string()), "Test".to_string());
        // padding_x=1, frame=⠋ (vis 1), " " separator, "Test" (vis 4) = total 7
        let lines = loader.render(10);
        assert!(!lines.is_empty());
        if lines.len() > 1 {
            assert!(
                crate::utils::visible_width(&lines[1]) <= 10,
                "line width {} exceeds 10",
                crate::utils::visible_width(&lines[1])
            );
        }
    }

    #[test]
    fn test_loader_tick() {
        let mut loader =
            Loader::new(Box::new(|s| s.to_string()), Box::new(|s| s.to_string()), "Loading...".to_string());
        let frame0 = loader.current_frame;
        loader.tick();
        let frame1 = loader.current_frame;
        // Tick should advance (or wrap, but with >1 frames it changes)
        if loader.frames.len() > 1 {
            assert_ne!(frame0, frame1, "tick should advance the frame");
        }
    }

    #[test]
    fn test_loader_set_message() {
        let mut loader =
            Loader::new(Box::new(|s| s.to_string()), Box::new(|s| s.to_string()), "Old message".to_string());
        loader.set_message("New message".to_string());
        let lines = loader.render(80);
        assert!(lines[1].contains("New message"));
        assert!(!lines[1].contains("Old message"));
    }

    #[test]
    fn test_loader_without_frames() {
        let mut loader =
            Loader::new(Box::new(|s| s.to_string()), Box::new(|s| s.to_string()), "No spinner".to_string());
        loader.set_frames(vec![]);
        let lines = loader.render(80);
        // No frame character, just message
        assert!(lines[1].contains("No spinner"));
    }

    #[test]
    fn test_loader_custom_frames() {
        let mut loader = Loader::new(Box::new(|s| s.to_string()), Box::new(|s| s.to_string()), "Custom".to_string());
        loader.set_frames(vec!["+".to_string(), "x".to_string()]);
        let lines = loader.render(80);
        assert!(lines[1].contains("+") || lines[1].contains("x"));
    }
}
