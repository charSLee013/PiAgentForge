//! TUI — Differential rendering engine.
//!
//! Mirrors `packages/tui/src/tui.ts` (the `TUI` class).
//!
//! ## Architecture
//!
//! - Owns a [`Terminal`] for raw I/O and a [`Container`] for the component tree.
//! - `render()` performs *differential rendering*: it compares the current output
//!   against the previously rendered state and emits only the ANSI escape
//!   sequences needed to update the terminal.
//! - Synchronized output (DEC private mode 2026) is used to avoid flicker.
//! - Overlays can be shown on top of the base content.
//! - Input events are forwarded to the focused component (or the topmost overlay).

use std::io;
use std::time::{Duration, Instant};

use crate::component::{Component, Container};
use crate::terminal::Terminal;
use crate::utils::visible_width;

/// Minimum interval between renders (≈60 fps).
const MIN_RENDER_INTERVAL: Duration = Duration::from_millis(16);

// ---------------------------------------------------------------------------
// Overlay types
// ---------------------------------------------------------------------------

/// Anchor position for an overlay within the terminal viewport.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayAnchor {
    Center,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

/// Configuration options for an overlay component.
#[derive(Debug, Clone)]
pub struct OverlayOptions {
    /// Desired width in columns. If `None`, the overlay occupies the full
    /// available width (clamped to terminal width).
    pub width: Option<u16>,
    /// Desired height in rows. If `None`, the overlay uses its natural height.
    pub height: Option<u16>,
    /// Anchor position (default: `Center`).
    pub anchor: OverlayAnchor,
    /// Whether the overlay is initially visible.
    pub visible: bool,
}

impl Default for OverlayOptions {
    fn default() -> Self {
        Self {
            width: None,
            height: None,
            anchor: OverlayAnchor::Center,
            visible: true,
        }
    }
}

/// Internal book-keeping for a single overlay.
struct OverlayEntry {
    id: usize,
    component: Box<dyn Component>,
    options: OverlayOptions,
    hidden: bool,
}

// ---------------------------------------------------------------------------
// TUI
// ---------------------------------------------------------------------------

/// The main TUI engine.
///
/// Drives the component tree, performs differential rendering, manages overlays,
/// and forwards keyboard input.
pub struct TUI {
    // Component tree
    container: Container,

    /// The underlying terminal abstraction.
    terminal: Terminal,

    // --- Previous render state (used for diffing) ---
    previous_lines: Vec<String>,
    previous_width: u16,
    previous_height: u16,

    // --- Render scheduling ---
    render_requested: bool,
    last_render_at: Instant,
    min_render_interval: Duration,

    // --- Overlay stack ---
    overlays: Vec<OverlayEntry>,
    next_overlay_id: usize,

    // --- Cursor tracking ---
    hardware_cursor_row: i32,
    show_hardware_cursor: bool,
}

impl TUI {
    /// Create a new TUI wrapping the given `Terminal`.
    ///
    /// The caller is responsible for calling `terminal.start()` before
    /// calling `render()`.
    pub fn new(terminal: Terminal) -> Self {
        Self {
            container: Container::new(),
            terminal,
            previous_lines: Vec::new(),
            previous_width: 0,
            previous_height: 0,
            render_requested: true,
            last_render_at: Instant::now(),
            min_render_interval: MIN_RENDER_INTERVAL,
            overlays: Vec::new(),
            next_overlay_id: 1,
            hardware_cursor_row: 0,
            show_hardware_cursor: false,
        }
    }

    // ------------------------------------------------------------------
    // Component tree management
    // ------------------------------------------------------------------

    /// Add a child component to the container.
    pub fn add(&mut self, component: impl Component + 'static) {
        self.container.add(component);
    }

    /// Invalidate all components (forces a full re-render on next frame).
    pub fn invalidate(&mut self) {
        self.container.invalidate();
        for overlay in &mut self.overlays {
            overlay.component.invalidate();
        }
    }

    // ------------------------------------------------------------------
    // Render scheduling
    // ------------------------------------------------------------------

    /// Request a render on the next call to [`render`].
    ///
    /// Multiple requests are coalesced into a single render.
    pub fn request_render(&mut self) {
        self.render_requested = true;
    }

    /// Force an immediate full redraw on the next call to [`render`].
    ///
    /// Resets all previous render state so the next render paints everything.
    pub fn force_render(&mut self) {
        self.previous_lines.clear();
        self.previous_width = 0;
        self.previous_height = 0;
        self.hardware_cursor_row = 0;
        self.render_requested = true;
    }

    // ------------------------------------------------------------------
    // Main render
    // ------------------------------------------------------------------

    /// Perform a differential render.
    ///
    /// Compares the current output against the previous frame and emits only
    /// the ANSI escape sequences necessary to update the terminal.  This
    /// includes synchronized-output wrapping (DECSET 2026) to avoid flicker.
    ///
    /// Returns an error when writing to the terminal fails.
    pub fn render(&mut self) -> io::Result<()> {
        // Throttle: skip if we rendered recently and no force was requested
        if !self.render_requested && !self.previous_lines.is_empty() {
            return Ok(());
        }
        let now = Instant::now();
        if now.duration_since(self.last_render_at) < self.min_render_interval
            && !self.previous_lines.is_empty()
        {
            return Ok(());
        }
        self.render_requested = false;
        self.last_render_at = now;

        let width = self.terminal.columns();
        let height = self.terminal.rows();

        let width_changed =
            self.previous_width != 0 && self.previous_width != width;
        let first_render = self.previous_lines.is_empty();

        // 1. Render all components
        let mut new_lines = self.container.render(width);

        // 2. Composite overlays
        if !self.overlays.is_empty() {
            new_lines = self.composite_overlays(new_lines, width, height);
        }

        // 3. Full redraw when needed
        if first_render || width_changed {
            return self.full_render(&new_lines, !first_render);
        }

        // 4. Diff against previous render
        let max_lines = new_lines.len().max(self.previous_lines.len());
        let mut first_changed = -1i32;
        let mut last_changed = -1i32;

        for i in 0..max_lines {
            let old = self
                .previous_lines
                .get(i)
                .map(|s| s.as_str())
                .unwrap_or("");
            let new = new_lines.get(i).map(|s| s.as_str()).unwrap_or("");
            if old != new {
                if first_changed < 0 {
                    first_changed = i as i32;
                }
                last_changed = i as i32;
            }
        }

        // Lines appended — treat the first appended line as the change start
        let appended_lines = new_lines.len() > self.previous_lines.len();
        if appended_lines && first_changed < 0 {
            first_changed = self.previous_lines.len() as i32;
            last_changed = (new_lines.len() - 1) as i32;
        }

        // 5. No changes — nothing to do
        if first_changed < 0 {
            return Ok(());
        }

        // 6. Differential update
        let mut buffer = String::new();
        buffer.push_str("\x1b[?2026h"); // begin synchronized output

        // Determine where to move the cursor
        let append_start =
            appended_lines && first_changed == self.previous_lines.len() as i32 && first_changed > 0;
        let move_target = if append_start {
            first_changed - 1
        } else {
            first_changed
        };

        // Vertical cursor movement
        let line_diff = move_target - self.hardware_cursor_row;
        if line_diff > 0 {
            buffer.push_str(&format!("\x1b[{}B", line_diff));
        } else if line_diff < 0 {
            buffer.push_str(&format!("\x1b[{}A", -line_diff));
        }

        // Carriage return (or newline when appending a completely new line)
        if append_start {
            buffer.push_str("\r\n");
        } else {
            buffer.push('\r');
        }

        // Render each changed line
        let render_end = last_changed.min((new_lines.len() - 1) as i32);
        for i in first_changed..=render_end {
            if i > first_changed {
                buffer.push_str("\r\n");
            }
            buffer.push_str("\x1b[2K"); // clear entire line
            if let Some(line) = new_lines.get(i as usize) {
                buffer.push_str(line);
            }
        }

        // Content shrunk — clear remaining old lines
        if self.previous_lines.len() > new_lines.len() {
            let extra = self.previous_lines.len() - new_lines.len();
            for _ in 0..extra {
                buffer.push_str("\r\n\x1b[2K");
            }
            buffer.push_str(&format!("\x1b[{}A", extra));
        }

        buffer.push_str("\x1b[?2026l"); // end synchronized output

        // If previous content entirely to the right of new content, use full render instead
        if first_changed >= new_lines.len() as i32 {
            return self.full_render(&new_lines, true);
        }

        // Write the buffer
        self.terminal.write(&buffer)?;

        // 7. Update state
        let final_cursor_row = render_end;
        self.hardware_cursor_row = final_cursor_row;
        self.previous_lines = new_lines;
        self.previous_width = width;
        self.previous_height = height;

        Ok(())
    }

    /// Full redraw: clear screen (if `clear` is true) and write all lines.
    fn full_render(&mut self, lines: &[String], clear: bool) -> io::Result<()> {
        let mut buffer = String::new();
        buffer.push_str("\x1b[?2026h"); // begin synchronized output
        if clear {
            buffer.push_str("\x1b[2J\x1b[H"); // clear screen + home
        }
        for (i, line) in lines.iter().enumerate() {
            if i > 0 {
                buffer.push_str("\r\n");
            }
            buffer.push_str(line);
        }
        buffer.push_str("\x1b[?2026l"); // end synchronized output
        self.terminal.write(&buffer)?;

        let cursor_row = if lines.is_empty() {
            0i32
        } else {
            (lines.len() - 1) as i32
        };
        self.hardware_cursor_row = cursor_row;
        self.previous_lines = lines.to_vec();
        self.previous_width = self.terminal.columns();
        self.previous_height = self.terminal.rows();

        Ok(())
    }

    // ------------------------------------------------------------------
    // Overlay management
    // ------------------------------------------------------------------

    /// Show an overlay component with the given options.
    ///
    /// Returns a unique overlay ID that can be passed to [`hide_overlay`].
    pub fn show_overlay(
        &mut self,
        component: Box<dyn Component>,
        options: OverlayOptions,
    ) -> usize {
        let id = self.next_overlay_id;
        self.next_overlay_id += 1;

        self.overlays.push(OverlayEntry {
            id,
            component,
            options,
            hidden: false,
        });

        self.request_render();
        id
    }

    /// Hide (remove) a previously shown overlay by ID.
    pub fn hide_overlay(&mut self, id: usize) {
        let before = self.overlays.len();
        self.overlays.retain(|o| o.id != id);
        if self.overlays.len() < before {
            self.request_render();
        }
    }

    /// Check whether any visible overlay exists.
    pub fn has_overlay(&self) -> bool {
        self.overlays.iter().any(|o| !o.hidden)
    }

    /// Composite overlays onto the base rendered `lines`.
    ///
    /// Each visible overlay is rendered at its calculated position and spliced
    /// into the result.
    fn composite_overlays(
        &self,
        mut lines: Vec<String>,
        term_width: u16,
        term_height: u16,
    ) -> Vec<String> {
        if self.overlays.is_empty() {
            return lines;
        }

        let tw = term_width as usize;
        let th = term_height as usize;

        // Ensure lines is tall enough for overlay placement
        let min_height = lines.len().max(th);
        while lines.len() < min_height {
            lines.push(String::new());
        }

        // Determine viewport start
        let viewport_start = lines.len().saturating_sub(th);

        for entry in &self.overlays {
            if entry.hidden || !entry.options.visible {
                continue;
            }

            let overlay_width = entry
                .options
                .width
                .map(|w| w as usize)
                .unwrap_or(tw.min(80));
            let overlay_width = overlay_width.min(tw);

            let overlay_lines = entry.component.render(overlay_width as u16);

            // Position the overlay
            let (row, col) = self.resolve_overlay_position(
                entry.options.anchor,
                overlay_lines.len(),
                overlay_width,
                tw,
                th,
            );

            for (i, overlay_line) in overlay_lines.iter().enumerate() {
                let idx = viewport_start + row + i;
                if idx >= lines.len() {
                    break;
                }
                // Truncate overlay line to the declared overlay width
                let truncated = if visible_width(overlay_line) > overlay_width {
                    crate::utils::truncate_to_width(overlay_line, overlay_width)
                } else {
                    overlay_line.clone()
                };
                lines[idx] = self.splice_line(&lines[idx], &truncated, col, overlay_width, tw);
            }
        }

        lines
    }

    /// Splice `overlay_line` into `base_line` at column `col`.
    fn splice_line(
        &self,
        base_line: &str,
        overlay_line: &str,
        col: usize,
        overlay_width: usize,
        total_width: usize,
    ) -> String {
        use crate::utils::extract_segments;

        let after_start = col + overlay_width;
        let segs = extract_segments(base_line, col, after_start, total_width.saturating_sub(after_start), true);

        let before_pad = col.saturating_sub(segs.before_width);
        let overlay_pad = overlay_width.saturating_sub(visible_width(overlay_line));
        let after_target = total_width.saturating_sub(col.max(segs.before_width)).saturating_sub(overlay_width);
        let after_pad = after_target.saturating_sub(segs.after_width);

        let reset = "\x1b[0m";
        let mut result = String::with_capacity(total_width + 32);
        result.push_str(&segs.before);
        for _ in 0..before_pad {
            result.push(' ');
        }
        result.push_str(reset);
        result.push_str(overlay_line);
        for _ in 0..overlay_pad {
            result.push(' ');
        }
        result.push_str(reset);
        result.push_str(&segs.after);
        for _ in 0..after_pad {
            result.push(' ');
        }

        // Final safety check: truncate if visible width exceeds total_width
        let rw = visible_width(&result);
        if rw > total_width {
            crate::utils::truncate_to_width(&result, total_width)
        } else {
            result
        }
    }

    /// Calculate the row and column for an overlay given its anchor and size.
    fn resolve_overlay_position(
        &self,
        anchor: OverlayAnchor,
        overlay_height: usize,
        overlay_width: usize,
        term_width: usize,
        term_height: usize,
    ) -> (usize, usize) {
        let row = match anchor {
            OverlayAnchor::Center => term_height.saturating_sub(overlay_height) / 2,
            OverlayAnchor::TopLeft | OverlayAnchor::TopRight => 0,
            OverlayAnchor::BottomLeft | OverlayAnchor::BottomRight => {
                term_height.saturating_sub(overlay_height)
            }
        };

        let col = match anchor {
            OverlayAnchor::Center | OverlayAnchor::TopLeft | OverlayAnchor::BottomLeft => 0,
            OverlayAnchor::TopRight | OverlayAnchor::BottomRight => {
                term_width.saturating_sub(overlay_width)
            }
        };

        (row, col)
    }

    // ------------------------------------------------------------------
    // Input handling
    // ------------------------------------------------------------------

    /// Handle a keyboard input event.
    ///
    /// Forwards the input to the topmost visible overlay (if any), or to all
    /// base-level children via the container.  Then requests a re-render.
    pub fn handle_input(&mut self, data: &str) {
        // Find the topmost visible overlay
        if let Some(top) = self
            .overlays
            .iter_mut()
            .filter(|o| !o.hidden && o.options.visible)
            .last()
        {
            top.component.handle_input(data);
        } else {
            // Forward to container children
            self.container.handle_input_all(data);
        }
        self.request_render();
    }

    // ------------------------------------------------------------------
    // Helpers
    // ------------------------------------------------------------------

    /// Whether the hardware cursor should be shown.
    pub fn show_hardware_cursor(&self) -> bool {
        self.show_hardware_cursor
    }

    /// Enable/disable hardware cursor display.
    pub fn set_show_hardware_cursor(&mut self, enabled: bool) {
        if self.show_hardware_cursor == enabled {
            return;
        }
        self.show_hardware_cursor = enabled;
        if !enabled {
            let _ = self.terminal.hide_cursor();
        }
        self.request_render();
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// A simple component that returns fixed lines.
    struct TestComponent {
        lines: Vec<String>,
    }

    impl Component for TestComponent {
        fn render(&self, _width: u16) -> Vec<String> {
            self.lines.clone()
        }
        fn invalidate(&mut self) {}
    }

    #[test]
    fn test_render_with_empty_container() {
        let term = Terminal::new().expect("terminal size should be detectable");
        let mut tui = TUI::new(term);
        // First render with no children — should produce zero output lines
        let result = tui.render();
        assert!(result.is_ok());
        assert!(tui.previous_lines.is_empty());
    }

    #[test]
    fn test_render_with_text_component() {
        let term = Terminal::new().expect("terminal size should be detectable");
        let mut tui = TUI::new(term);
        tui.add(TestComponent {
            lines: vec!["hello".to_string(), "world".to_string()],
        });
        let result = tui.render();
        assert!(result.is_ok());
        assert_eq!(tui.previous_lines.len(), 2);
        assert_eq!(tui.previous_lines[0], "hello");
        assert_eq!(tui.previous_lines[1], "world");
    }

    #[test]
    fn test_incremental_render_no_change() {
        let term = Terminal::new().expect("terminal size should be detectable");
        let mut tui = TUI::new(term);
        tui.add(TestComponent {
            lines: vec!["hello".to_string()],
        });

        // First render populates previous_lines
        assert!(tui.render().is_ok());
        assert_eq!(tui.previous_lines.len(), 1);

        // Second render should detect no changes (render_requested is now false)
        // and skip the render
        tui.render_requested = false; // simulate that no request came in
        assert!(tui.render().is_ok());
        // previous_lines should still be ["hello"]
        assert_eq!(tui.previous_lines[0], "hello");
    }

    #[test]
    fn test_force_render_resets_state() {
        let term = Terminal::new().expect("terminal size should be detectable");
        let mut tui = TUI::new(term);
        tui.add(TestComponent {
            lines: vec!["hello".to_string()],
        });

        assert!(tui.render().is_ok());
        assert!(!tui.previous_lines.is_empty());

        tui.force_render();
        assert!(tui.previous_lines.is_empty());
        assert_eq!(tui.previous_width, 0);
        assert_eq!(tui.hardware_cursor_row, 0);
    }

    #[test]
    fn test_show_overlay_then_render() {
        let term = Terminal::new().expect("terminal size should be detectable");
        let mut tui = TUI::new(term);
        tui.add(TestComponent {
            lines: vec!["base".to_string()],
        });

        let id = tui.show_overlay(
            Box::new(TestComponent {
                lines: vec!["overlay".to_string()],
            }),
            OverlayOptions::default(),
        );

        assert!(tui.has_overlay());

        let result = tui.render();
        assert!(result.is_ok());

        // Hiding the overlay
        tui.hide_overlay(id);
        assert!(!tui.has_overlay());
    }

    #[test]
    fn test_overlay_position_resolution() {
        let term = Terminal::new().expect("terminal size should be detectable");
        let tui = TUI::new(term);

        let (row, col) = tui.resolve_overlay_position(OverlayAnchor::Center, 5, 40, 80, 24);
        // Center: row should be roughly (24-5)/2 ≈ 9-10
        assert!(row < 24);
        assert_eq!(col, 0);

        let (row, col) = tui.resolve_overlay_position(OverlayAnchor::TopRight, 5, 40, 80, 24);
        assert_eq!(row, 0);
        // TopRight: col should be 80-40 = 40
        assert_eq!(col, 40);
    }

    #[test]
    fn test_request_render_coalesces() {
        let mut tui = TUI::new(Terminal::new().unwrap());
        assert!(tui.render_requested); // initially true for first render

        tui.request_render();
        assert!(tui.render_requested);

        // After render, flag is cleared
        let _ = tui.render();
        assert!(!tui.render_requested);
    }
}
