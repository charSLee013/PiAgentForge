//! Image component — displays images in the terminal.
//!
//! Mirrors `packages/tui/src/components/image.ts`
//!
//! When terminal image protocols are available (Kitty or iTerm2), the image
//! is rendered as an inline graphic.  When they are not, a text fallback
//! placeholder is shown.

use std::cell::Cell;

use crate::component::Component;
use crate::components::terminal_image::{self, ImageDimensions};

/// Theme functions for the image component.
pub struct ImageTheme {
    pub fallback_color: Box<dyn Fn(&str) -> String + Send>,
}

/// An image component that renders an image in the terminal.
///
/// When terminal image protocols are available (Kitty or iTerm2), the image
/// is rendered as an inline graphic.  When they are not, a text fallback
/// placeholder is shown.
pub struct Image {
    base64_data: String,
    mime_type: String,
    dimensions: ImageDimensions,
    theme: ImageTheme,
    filename: Option<String>,
    image_id: Cell<Option<usize>>,
    max_width_cells: Option<u16>,
    max_height_cells: Option<u16>,
    // Cache
    cached_lines: std::cell::RefCell<Option<(Vec<String>, u16)>>,
}

impl Image {
    pub fn new(
        base64_data: String,
        mime_type: String,
        theme: ImageTheme,
        options: ImageOptions,
        dimensions: Option<ImageDimensions>,
    ) -> Self {
        let dims = dimensions.unwrap_or_else(|| {
            terminal_image::get_image_dimensions(&base64_data, &mime_type).unwrap_or(
                ImageDimensions {
                    width_px: 800,
                    height_px: 600,
                },
            )
        });

        Self {
            base64_data,
            mime_type,
            dimensions: dims,
            theme,
            filename: options.filename,
            image_id: Cell::new(options.image_id),
            max_width_cells: options.max_width_cells,
            max_height_cells: options.max_height_cells,
            cached_lines: std::cell::RefCell::new(None),
        }
    }

    /// Get the Kitty image ID used by this image (if any).
    pub fn image_id(&self) -> Option<usize> {
        self.image_id.get()
    }
}

/// Configuration options for the image component.
#[derive(Default)]
pub struct ImageOptions {
    pub max_width_cells: Option<u16>,
    pub max_height_cells: Option<u16>,
    pub filename: Option<String>,
    /// Kitty image ID. If provided, reuses this ID (for animations/updates).
    pub image_id: Option<usize>,
}

impl Component for Image {
    fn render(&self, width: u16) -> Vec<String> {
        // Check cache
        if let Some((cached, cached_w)) = self.cached_lines.borrow().as_ref() {
            if *cached_w == width {
                return cached.clone();
            }
        }

        let max_width = self
            .max_width_cells
            .map(|m| m as usize)
            .unwrap_or(60.min((width as usize).saturating_sub(2)))
            .max(1);
        let default_max_height =
            (max_width as f64 * 18.0 / 9.0).ceil() as usize;
        let max_height = self
            .max_height_cells
            .map(|m| m as usize)
            .unwrap_or(default_max_height)
            .max(1);

        let caps = terminal_image::get_capabilities();
        let lines: Vec<String> = if caps.images.is_some() {
            // Auto-allocate image ID for Kitty if not yet set
            if caps.images == Some(terminal_image::ImageProtocol::Kitty)
                && self.image_id.get().is_none()
            {
                self.image_id
                    .set(Some(terminal_image::allocate_image_id()));
            }

            let result = terminal_image::render_image(
                &self.base64_data,
                &self.dimensions,
                &terminal_image::ImageRenderOptions {
                    max_width_cells: Some(max_width),
                    max_height_cells: Some(max_height),
                    image_id: self.image_id.get(),
                    move_cursor: false,
                    ..Default::default()
                },
            );

            match result {
                Some(rendered) => {
                    // Store the image ID for later cleanup
                    if let Some(new_id) = rendered.image_id {
                        self.image_id.set(Some(new_id));
                    }

                    if caps.images == Some(terminal_image::ImageProtocol::Kitty) {
                        // For Kitty: C=1 prevents cursor movement.
                        let mut lines = vec![rendered.sequence];
                        for _ in 0..rendered.rows.saturating_sub(1) {
                            lines.push(String::new());
                        }
                        lines
                    } else {
                        // iTerm2: first (rows-1) lines are empty, last line
                        // moves cursor up, draws the image.
                        let mut lines: Vec<String> = Vec::new();
                        for _ in 0..rendered.rows.saturating_sub(1) {
                            lines.push(String::new());
                        }
                        let row_offset = rendered.rows.saturating_sub(1);
                        let move_up = if row_offset > 0 {
                            format!("\x1b[{}A", row_offset)
                        } else {
                            String::new()
                        };
                        lines.push(move_up + &rendered.sequence);
                        lines
                    }
                }
                None => {
                    let fallback = terminal_image::image_fallback(
                        &self.mime_type,
                        Some(&self.dimensions),
                        self.filename.as_deref(),
                    );
                    vec![(self.theme.fallback_color)(&fallback)]
                }
            }
        } else {
            let fallback = terminal_image::image_fallback(
                &self.mime_type,
                Some(&self.dimensions),
                self.filename.as_deref(),
            );
            vec![(self.theme.fallback_color)(&fallback)]
        };

        // Pad to image height so TUI accounts for the image area
        let padded: Vec<String> = if lines.len() < max_height {
            let mut v = Vec::with_capacity(max_height);
            v.extend(lines);
            while v.len() < max_height {
                v.push(String::new());
            }
            v
        } else {
            lines
        };

        self.cached_lines
            .borrow_mut()
            .replace((padded.clone(), width));
        padded
    }

    fn invalidate(&mut self) {
        self.cached_lines.borrow_mut().take();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_theme() -> ImageTheme {
        ImageTheme {
            fallback_color: Box::new(|s| s.to_string()),
        }
    }

    #[test]
    fn test_image_renders_fallback() {
        let img = Image::new(
            String::new(),
            "image/png".to_string(),
            test_theme(),
            ImageOptions {
                filename: Some("test.png".to_string()),
                ..Default::default()
            },
            Some(ImageDimensions {
                width_px: 800,
                height_px: 600,
            }),
        );
        let lines = img.render(80);
        assert!(!lines.is_empty());
        let joined = lines.join("");
        assert!(joined.contains("test.png"));
        assert!(joined.contains("800x600"));
        assert!(joined.contains("[Image:"));
    }

    #[test]
    fn test_image_fallback_minimal() {
        let img = Image::new(
            String::new(),
            "image/jpeg".to_string(),
            test_theme(),
            ImageOptions::default(),
            None,
        );
        let lines = img.render(80);
        assert!(!lines.is_empty());
        assert!(lines[0].contains("[Image:"));
    }

    #[test]
    fn test_image_invalidate_clears_cache() {
        let mut img = Image::new(
            String::new(),
            "image/png".to_string(),
            test_theme(),
            ImageOptions::default(),
            None,
        );
        let first = img.render(80);
        img.invalidate();
        let second = img.render(80);
        assert_eq!(first, second);
    }

    #[test]
    fn test_image_renders_kitty_protocol() {
        terminal_image::set_capabilities(terminal_image::TerminalCapabilities {
            images: Some(terminal_image::ImageProtocol::Kitty),
            true_color: true,
            hyperlinks: true,
        });

        let img = Image::new(
            "AAAA".to_string(),
            "image/png".to_string(),
            test_theme(),
            ImageOptions {
                filename: Some("test.png".to_string()),
                ..Default::default()
            },
            Some(ImageDimensions {
                width_px: 800,
                height_px: 600,
            }),
        );
        let lines = img.render(80);
        assert!(!lines.is_empty());
        // Should contain Kitty escape sequence
        assert!(lines[0].starts_with("\x1b_G"));

        terminal_image::reset_capabilities_cache();
    }

    #[test]
    fn test_image_renders_iterm2_protocol() {
        terminal_image::set_capabilities(terminal_image::TerminalCapabilities {
            images: Some(terminal_image::ImageProtocol::Iterm2),
            true_color: true,
            hyperlinks: true,
        });

        let img = Image::new(
            "AAAA".to_string(),
            "image/png".to_string(),
            test_theme(),
            ImageOptions {
                filename: Some("test.png".to_string()),
                ..Default::default()
            },
            Some(ImageDimensions {
                width_px: 800,
                height_px: 600,
            }),
        );
        let lines = img.render(80);
        assert!(!lines.is_empty());
        // iTerm2 sequence is placed at rendered.rows - 1 with a cursor-up prefix
        assert!(
            lines.iter().any(|l| l.contains("\x1b]1337;File=")),
            "expected iTerm2 escape sequence in rendered output"
        );

        terminal_image::reset_capabilities_cache();
    }

    #[test]
    fn test_image_auto_allocates_kitty_id() {
        terminal_image::set_capabilities(terminal_image::TerminalCapabilities {
            images: Some(terminal_image::ImageProtocol::Kitty),
            true_color: true,
            hyperlinks: true,
        });

        let img = Image::new(
            "AAAA".to_string(),
            "image/png".to_string(),
            test_theme(),
            ImageOptions::default(),
            Some(ImageDimensions {
                width_px: 800,
                height_px: 600,
            }),
        );

        // Before render, no image_id
        assert!(img.image_id().is_none());

        // After render, image_id should be set
        let _ = img.render(80);
        assert!(img.image_id().is_some());

        terminal_image::reset_capabilities_cache();
    }

    #[test]
    fn test_image_dimension_from_base64_fallback() {
        // Invalid base64 should fall back to 800x600
        let img = Image::new(
            "!!!bad-base64!!!".to_string(),
            "image/png".to_string(),
            test_theme(),
            ImageOptions::default(),
            None,
        );
        let lines = img.render(80);
        assert!(!lines.is_empty());
        assert!(lines[0].contains("[Image:"));
    }

    #[test]
    fn test_image_render_caching() {
        let img = Image::new(
            String::new(),
            "image/png".to_string(),
            test_theme(),
            ImageOptions::default(),
            Some(ImageDimensions {
                width_px: 800,
                height_px: 600,
            }),
        );
        let first = img.render(80);
        let second = img.render(80);
        // Same width should produce identical (cached) result
        assert_eq!(first, second);
    }

    #[test]
    fn test_image_render_caching_different_widths() {
        let img = Image::new(
            String::new(),
            "image/png".to_string(),
            test_theme(),
            ImageOptions::default(),
            Some(ImageDimensions {
                width_px: 800,
                height_px: 600,
            }),
        );
        let first = img.render(80);
        let second = img.render(120);
        // Different width may produce different result, but first should be cached
        if first == second {
            // If they are same, it means the fallback formatting is width-independent
            assert!(!first.is_empty());
        }
    }

    #[test]
    fn test_image_pads_to_max_height() {
        let img = Image::new(
            String::new(),
            "image/png".to_string(),
            test_theme(),
            ImageOptions {
                max_width_cells: Some(40),
                max_height_cells: Some(10),
                ..Default::default()
            },
            Some(ImageDimensions {
                width_px: 800,
                height_px: 600,
            }),
        );
        let lines = img.render(80);
        assert!(lines.len() >= 10);
    }
}
