//! Terminal image protocol support (Kitty / iTerm2).
//!
//! Mirrors `packages/tui/src/terminal-image.ts`
//!
//! Supports:
//! - Capability detection via environment variables
//! - Kitty graphics protocol (base64 chunked transmit)
//! - iTerm2 inline image protocol
//! - Image dimension parsing (PNG/JPEG/GIF/WebP)
//! - Cell-size calculation
//! - Image deletion sequences

use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

// ============================================================================
// Types
// ============================================================================

/// Protocol identifier for image-capable terminals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageProtocol {
    Kitty,
    Iterm2,
}

/// Terminal capabilities relevant to image rendering.
#[derive(Debug, Clone)]
pub struct TerminalCapabilities {
    pub images: Option<ImageProtocol>,
    pub true_color: bool,
    pub hyperlinks: bool,
}

/// Cell dimensions in pixels.
#[derive(Debug, Clone, Copy)]
pub struct CellDimensions {
    pub width_px: f64,
    pub height_px: f64,
}

impl Default for CellDimensions {
    fn default() -> Self {
        Self { width_px: 9.0, height_px: 18.0 }
    }
}

/// Image dimensions in pixels.
#[derive(Debug, Clone, Copy)]
pub struct ImageDimensions {
    pub width_px: u32,
    pub height_px: u32,
}

/// Image cell size (columns × rows) in terminal cells.
#[derive(Debug, Clone, Copy)]
pub struct ImageCellSize {
    pub columns: usize,
    pub rows: usize,
}

/// Options for rendering an image.
#[derive(Debug, Clone)]
pub struct ImageRenderOptions {
    pub max_width_cells: Option<usize>,
    pub max_height_cells: Option<usize>,
    pub preserve_aspect_ratio: bool,
    pub image_id: Option<usize>,
    pub move_cursor: bool,
}

impl Default for ImageRenderOptions {
    fn default() -> Self {
        Self {
            max_width_cells: None,
            max_height_cells: None,
            preserve_aspect_ratio: true,
            image_id: None,
            move_cursor: true,
        }
    }
}

/// Encoded image data ready for terminal protocol transmission.
pub struct EncodedImage {
    pub data: String,
    pub mime_type: String,
}

/// Result from [`render_image`].
pub struct RenderedImage {
    pub sequence: String,
    pub rows: usize,
    pub image_id: Option<usize>,
}

// ============================================================================
// Global state
// ============================================================================

/// Cached terminal capabilities (set once on first access, resetable for tests).
static CACHED_CAPABILITIES: Mutex<Option<TerminalCapabilities>> = Mutex::new(None);

#[cfg(test)]
static TEST_TERMINAL_STATE_LOCK: Mutex<()> = Mutex::new(());

/// Global cell dimensions (default 9×18 px, updateable via `set_cell_dimensions`).
static CELL_DIMENSIONS: Mutex<CellDimensions> = Mutex::new(CellDimensions { width_px: 9.0, height_px: 18.0 });

/// Monotonically increasing image ID counter.
static NEXT_IMAGE_ID: AtomicU32 = AtomicU32::new(1);

// ============================================================================
// Capability detection
// ============================================================================

impl TerminalCapabilities {
    /// Detect capabilities from environment variables.
    pub fn detect() -> Self {
        let term_program = std::env::var("TERM_PROGRAM").unwrap_or_default().to_lowercase();
        let term = std::env::var("TERM").unwrap_or_default().to_lowercase();
        let color_term = std::env::var("COLORTERM").unwrap_or_default().to_lowercase();

        // tmux and screen swallow OSC 8 by default. Image protocols are also
        // unreliable under tmux/screen.
        let in_tmux_or_screen = std::env::var("TMUX").is_ok() || term.starts_with("tmux") || term.starts_with("screen");
        if in_tmux_or_screen {
            let true_color = color_term == "truecolor" || color_term == "24bit";
            return Self { images: None, true_color, hyperlinks: false };
        }

        if std::env::var("KITTY_WINDOW_ID").is_ok() || term_program == "kitty" {
            return Self { images: Some(ImageProtocol::Kitty), true_color: true, hyperlinks: true };
        }

        if term_program == "ghostty" || term.contains("ghostty") || std::env::var("GHOSTTY_RESOURCES_DIR").is_ok() {
            return Self { images: Some(ImageProtocol::Kitty), true_color: true, hyperlinks: true };
        }

        if std::env::var("WEZTERM_PANE").is_ok() || term_program == "wezterm" {
            return Self { images: Some(ImageProtocol::Kitty), true_color: true, hyperlinks: true };
        }

        if std::env::var("ITERM_SESSION_ID").is_ok() || term_program == "iterm.app" {
            return Self { images: Some(ImageProtocol::Iterm2), true_color: true, hyperlinks: true };
        }

        if term_program == "vscode" || term_program == "alacritty" {
            return Self { images: None, true_color: true, hyperlinks: true };
        }

        // Unknown terminal: be conservative.
        let true_color = color_term == "truecolor" || color_term == "24bit";
        Self { images: None, true_color, hyperlinks: false }
    }
}

impl Default for TerminalCapabilities {
    fn default() -> Self {
        Self::detect()
    }
}

// ============================================================================
// Capability cache
// ============================================================================

/// Get cached capabilities, detecting on first call.
pub fn get_capabilities() -> TerminalCapabilities {
    let mut cache = CACHED_CAPABILITIES.lock().unwrap();
    cache.get_or_insert_with(TerminalCapabilities::detect).clone()
}

/// Reset the capabilities cache (e.g. after terminal resize/reconfig).
pub fn reset_capabilities_cache() {
    *CACHED_CAPABILITIES.lock().unwrap() = None;
}

/// Override the cached capabilities. Useful in tests to exercise both code paths.
pub fn set_capabilities(caps: TerminalCapabilities) {
    *CACHED_CAPABILITIES.lock().unwrap() = Some(caps);
}

#[cfg(test)]
pub(crate) fn lock_test_terminal_state() -> std::sync::MutexGuard<'static, ()> {
    TEST_TERMINAL_STATE_LOCK.lock().unwrap()
}

// ============================================================================
// Cell dimensions
// ============================================================================

/// Get the global cell dimensions (default 9×18 px).
pub fn get_cell_dimensions() -> CellDimensions {
    *CELL_DIMENSIONS.lock().unwrap()
}

/// Set the global cell dimensions (e.g. from terminal query response).
pub fn set_cell_dimensions(dims: CellDimensions) {
    *CELL_DIMENSIONS.lock().unwrap() = dims;
}

// ============================================================================
// Image ID allocation
// ============================================================================

/// Allocate a unique image ID for the Kitty graphics protocol.
pub fn allocate_image_id() -> usize {
    NEXT_IMAGE_ID.fetch_add(1, Ordering::SeqCst) as usize
}

// ============================================================================
// Dimension parsing (image crate)
// ============================================================================

/// Parse image dimensions from raw bytes using the `image` crate.
/// Supports PNG, JPEG, GIF, WebP, and other formats recognized by `image`.
pub fn get_image_dimensions_from_bytes(bytes: &[u8]) -> Option<ImageDimensions> {
    let cursor = std::io::Cursor::new(bytes);
    let reader = image::ImageReader::new(cursor).with_guessed_format().ok()?;
    let (w, h) = reader.into_dimensions().ok()?;
    Some(ImageDimensions { width_px: w, height_px: h })
}

/// Parse image dimensions from base64-encoded data.
///
/// The `_mime_type` parameter is accepted for API compatibility with the TS
/// source; the `image` crate auto-detects the format from magic bytes.
pub fn get_image_dimensions(base64_data: &str, _mime_type: &str) -> Option<ImageDimensions> {
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::STANDARD.decode(base64_data.as_bytes()).ok()?;
    get_image_dimensions_from_bytes(&bytes)
}

// ============================================================================
// Cell size calculation
// ============================================================================

/// Calculate the number of terminal cells an image should occupy.
///
/// Mirrors `calculateImageCellSize` in the TS source.
pub fn calculate_image_cell_size(
    image_dimensions: &ImageDimensions,
    max_width_cells: usize,
    max_height_cells: Option<usize>,
    cell_dimensions: &CellDimensions,
) -> ImageCellSize {
    let max_width = (max_width_cells as f64).floor().max(1.0);
    let max_height = max_height_cells.map(|h| (h as f64).floor().max(1.0));

    let image_width = (image_dimensions.width_px as f64).max(1.0);
    let image_height = (image_dimensions.height_px as f64).max(1.0);

    let width_scale = (max_width * cell_dimensions.width_px) / image_width;
    let height_scale = match max_height {
        Some(mh) => (mh * cell_dimensions.height_px) / image_height,
        None => width_scale,
    };
    let scale = width_scale.min(height_scale);

    let scaled_width_px = image_width * scale;
    let scaled_height_px = image_height * scale;

    let columns = (scaled_width_px / cell_dimensions.width_px).ceil() as usize;
    let rows = (scaled_height_px / cell_dimensions.height_px).ceil() as usize;

    ImageCellSize {
        columns: columns.max(1).min(max_width as usize),
        rows: match max_height {
            Some(mh) => rows.max(1).min(mh as usize),
            None => rows.max(1),
        },
    }
}

/// Simplified cell calculation (backward-compatible wrapper).
///
/// Uses the global cell dimensions from `get_cell_dimensions()`.
pub fn calculate_image_cells(
    dimensions: &ImageDimensions,
    max_width_cells: usize,
    max_height_cells: Option<usize>,
) -> (usize, usize) {
    let cell_dims = get_cell_dimensions();
    let size = calculate_image_cell_size(dimensions, max_width_cells, max_height_cells, &cell_dims);
    (size.columns, size.rows)
}

/// Calculate the number of rows an image occupies at a given column width.
///
/// Mirrors `calculateImageRows` in the TS source.
pub fn calculate_image_rows(
    image_dimensions: &ImageDimensions,
    target_width_cells: usize,
    cell_dimensions: Option<&CellDimensions>,
) -> usize {
    let cell_dims = cell_dimensions.copied().unwrap_or_default();
    let size = calculate_image_cell_size(image_dimensions, target_width_cells, None, &cell_dims);
    size.rows
}

// ============================================================================
// Image fallback
// ============================================================================

/// Generate a text fallback string for an image.
///
/// Produces output like `[Image: filename.png 800x600]` when a filename is
/// provided, or `[Image: image/png 800x600]` without one.
pub fn image_fallback(mime_type: &str, dimensions: Option<&ImageDimensions>, filename: Option<&str>) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(name) = filename {
        parts.push(name.to_string());
    }
    parts.push(format!("[{}]", mime_type));
    if let Some(dims) = dimensions {
        parts.push(format!("{}x{}", dims.width_px, dims.height_px));
    }
    format!("[Image: {}]", parts.join(" "))
}

// ============================================================================
// Kitty graphics protocol encoding
// ============================================================================

const KITTY_CHUNK_SIZE: usize = 4096;

/// Encode base64 image data using the Kitty graphics protocol.
///
/// Returns the raw escape sequence string.  The payload is split into 4096-byte
/// chunks when necessary.  Each chunk is wrapped in `\x1b_G ... \x1b\\`.
///
/// Mirrors `encodeKitty` in the TS source.
pub fn encode_kitty(
    base64_data: &str,
    columns: Option<usize>,
    rows: Option<usize>,
    image_id: Option<usize>,
    move_cursor: bool,
) -> String {
    let mut params: Vec<String> = vec!["a=T".to_string(), "f=100".to_string(), "q=2".to_string()];

    if !move_cursor {
        params.push("C=1".to_string());
    }
    if let Some(c) = columns {
        params.push(format!("c={}", c));
    }
    if let Some(r) = rows {
        params.push(format!("r={}", r));
    }
    if let Some(id) = image_id {
        params.push(format!("i={}", id));
    }

    let params_str = params.join(",");

    if base64_data.len() <= KITTY_CHUNK_SIZE {
        return format!("\x1b_G{};{}\x1b\\", params_str, base64_data);
    }

    let mut chunks: Vec<String> = Vec::new();
    let mut offset = 0;
    let mut is_first = true;

    while offset < base64_data.len() {
        let end = (offset + KITTY_CHUNK_SIZE).min(base64_data.len());
        let chunk = &base64_data[offset..end];
        let is_last = end >= base64_data.len();

        if is_first {
            chunks.push(format!("\x1b_G{},m=1;{}\x1b\\", params_str, chunk));
            is_first = false;
        } else if is_last {
            chunks.push(format!("\x1b_Gm=0;{}\x1b\\", chunk));
        } else {
            chunks.push(format!("\x1b_Gm=1;{}\x1b\\", chunk));
        }

        offset = end;
    }

    chunks.concat()
}

/// Generate a delete-image sequence for a single Kitty image.
///
/// Uses uppercase 'I' to also free the image data.
pub fn delete_kitty_image(image_id: usize) -> String {
    format!("\x1b_Ga=d,d=I,i={},q=2\x1b\\", image_id)
}

/// Generate a delete-all-images sequence for Kitty.
///
/// Uses uppercase 'A' to also free all image data.
pub fn delete_all_kitty_images() -> String {
    "\x1b_Ga=d,d=A,q=2\x1b\\".to_string()
}

// ============================================================================
// iTerm2 inline image protocol encoding
// ============================================================================

/// Encode base64 image data using the iTerm2 inline image protocol.
///
/// Returns the raw escape sequence string: `\x1b]1337;File=...:data\x07`.
///
/// Mirrors `encodeITerm2` in the TS source.
pub fn encode_iterm2(
    base64_data: &str,
    width: Option<usize>,
    height: Option<&str>,
    name: Option<&str>,
    preserve_aspect_ratio: bool,
    inline: bool,
) -> String {
    let mut params: Vec<String> = vec![format!("inline={}", if inline { 1 } else { 0 })];

    if let Some(w) = width {
        params.push(format!("width={}", w));
    }
    if let Some(h) = height {
        params.push(format!("height={}", h));
    }
    if let Some(n) = name {
        use base64::Engine as _;
        let encoded = base64::engine::general_purpose::STANDARD.encode(n);
        params.push(format!("name={}", encoded));
    }
    if !preserve_aspect_ratio {
        params.push("preserveAspectRatio=0".to_string());
    }

    let params_str = params.join(";");
    format!("\x1b]1337;File={}:{}\x07", params_str, base64_data)
}

// ============================================================================
// High-level render
// ============================================================================

/// Render an image using the detected terminal protocol.
///
/// Returns `None` if no image protocol is available, or the rendered escape
/// sequence along with the number of terminal rows it occupies.
///
/// Mirrors `renderImage` in the TS source.
pub fn render_image(
    base64_data: &str,
    image_dimensions: &ImageDimensions,
    options: &ImageRenderOptions,
) -> Option<RenderedImage> {
    let caps = get_capabilities();
    let images = caps.images?;

    let max_width = options.max_width_cells.unwrap_or(80).max(1);
    let cell_dims = get_cell_dimensions();
    let size = calculate_image_cell_size(image_dimensions, max_width, options.max_height_cells, &cell_dims);

    match images {
        ImageProtocol::Kitty => {
            let sequence =
                encode_kitty(base64_data, Some(size.columns), Some(size.rows), options.image_id, options.move_cursor);
            Some(RenderedImage { sequence, rows: size.rows, image_id: options.image_id })
        }
        ImageProtocol::Iterm2 => {
            let sequence =
                encode_iterm2(base64_data, Some(size.columns), Some("auto"), None, options.preserve_aspect_ratio, true);
            Some(RenderedImage { sequence, rows: size.rows, image_id: None })
        }
    }
}

// ============================================================================
// Detection helpers
// ============================================================================

/// Check whether a line of terminal output contains an image protocol sequence.
pub fn is_image_line(line: &str) -> bool {
    line.starts_with("\x1b_G") || line.starts_with("\x1b]1337;File=")
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn test_guard() -> std::sync::MutexGuard<'static, ()> {
        lock_test_terminal_state()
    }

    // ------------------------------------------------------------------
    // image_fallback tests
    // ------------------------------------------------------------------

    #[test]
    fn test_image_fallback_with_all_info() {
        let dims = ImageDimensions { width_px: 800, height_px: 600 };
        let result = image_fallback("image/png", Some(&dims), Some("photo.png"));
        assert_eq!(result, "[Image: photo.png [image/png] 800x600]");
    }

    #[test]
    fn test_image_fallback_no_filename() {
        let dims = ImageDimensions { width_px: 1024, height_px: 768 };
        let result = image_fallback("image/jpeg", Some(&dims), None);
        assert_eq!(result, "[Image: [image/jpeg] 1024x768]");
    }

    #[test]
    fn test_image_fallback_no_dimensions() {
        let result = image_fallback("image/gif", None, Some("anim.gif"));
        assert_eq!(result, "[Image: anim.gif [image/gif]]");
    }

    #[test]
    fn test_image_fallback_minimal() {
        let result = image_fallback("image/png", None, None);
        assert_eq!(result, "[Image: [image/png]]");
    }

    // ------------------------------------------------------------------
    // calculate_image_cells tests
    // ------------------------------------------------------------------

    #[test]
    fn test_calculate_image_cells() {
        let dims = ImageDimensions { width_px: 800, height_px: 600 };
        let (cols, rows) = calculate_image_cells(&dims, 60, None);
        assert!(cols >= 1);
        assert!(cols <= 60);
        assert!(rows >= 1);
    }

    #[test]
    fn test_calculate_image_cell_size_exact() {
        let dims = ImageDimensions { width_px: 800, height_px: 600 };
        let cell = CellDimensions { width_px: 9.0, height_px: 18.0 };
        let size = calculate_image_cell_size(&dims, 60, None, &cell);
        // At 60 cols: width_scale = (60 * 9) / 800 = 0.675
        // scaled_w = 800 * 0.675 = 540 -> 540/9 = 60 cols
        // scaled_h = 600 * 0.675 = 405 -> 405/18 = 22.5 -> 23 rows
        assert_eq!(size.columns, 60);
        assert_eq!(size.rows, 23);
    }

    #[test]
    fn test_calculate_image_cell_size_with_max_height() {
        let dims = ImageDimensions { width_px: 800, height_px: 600 };
        let cell = CellDimensions { width_px: 9.0, height_px: 18.0 };
        let size = calculate_image_cell_size(&dims, 60, Some(10), &cell);
        // width_scale = 0.675, height_scale = (10*18)/600 = 0.3
        // scale = min(0.675, 0.3) = 0.3
        // scaled_w = 800*0.3 = 240 -> 240/9 = 26.67 -> 27 cols
        // scaled_h = 600*0.3 = 180 -> 180/18 = 10 rows
        assert_eq!(size.columns, 27);
        assert_eq!(size.rows, 10);
    }

    #[test]
    fn test_calculate_image_rows() {
        let dims = ImageDimensions { width_px: 800, height_px: 600 };
        let rows = calculate_image_rows(&dims, 40, None);
        assert!(rows >= 1);
        // At 40 cols: width_scale = (40*9)/800 = 0.45
        // scaled_h = 600*0.45 = 270 -> 270/18 = 15 rows
        assert_eq!(rows, 15);
    }

    // ------------------------------------------------------------------
    // Kitty encoding tests
    // ------------------------------------------------------------------

    #[test]
    fn test_encode_kitty_small() {
        // Small enough to fit in one chunk
        let result = encode_kitty("AAAA", None, None, None, true);
        assert!(result.starts_with("\x1b_G"));
        assert!(result.ends_with("\x1b\\"));
        assert!(result.contains("AAAA"));
    }

    #[test]
    fn test_encode_kitty_large() {
        // Large enough for multi-chunk
        let data = "A".repeat(5000);
        let result = encode_kitty(&data, None, None, None, true);
        assert!(result.starts_with("\x1b_G"));
        assert!(result.ends_with("\x1b\\"));
        // Should contain multiple chunks
        assert!(result.contains("m=1"));
        assert!(result.contains("m=0"));
    }

    #[test]
    fn test_encode_kitty_with_params() {
        let result = encode_kitty("data", Some(40), Some(10), Some(42), false);
        assert!(result.contains("c=40"));
        assert!(result.contains("r=10"));
        assert!(result.contains("i=42"));
        assert!(result.contains("C=1"));
    }

    #[test]
    fn test_encode_kitty_move_cursor_default() {
        let result = encode_kitty("data", None, None, None, true);
        // When move_cursor is true, C=1 should NOT be present
        assert!(!result.contains("C=1"));
    }

    // ------------------------------------------------------------------
    // iTerm2 encoding tests
    // ------------------------------------------------------------------

    #[test]
    fn test_encode_iterm2() {
        let result = encode_iterm2("AAAA", None, None, None, true, true);
        assert!(result.starts_with("\x1b]1337;File="));
        assert!(result.ends_with("\x07"));
        assert!(result.contains("inline=1"));
    }

    #[test]
    fn test_encode_iterm2_with_params() {
        let result = encode_iterm2("data", Some(40), Some("auto"), Some("img"), false, false);
        assert!(result.contains("inline=0"));
        assert!(result.contains("width=40"));
        assert!(result.contains("height=auto"));
        assert!(result.contains("preserveAspectRatio=0"));
        // Name should be base64-encoded now
        assert!(result.contains("name=aW1n"));
    }

    // ------------------------------------------------------------------
    // Image deletion tests
    // ------------------------------------------------------------------

    #[test]
    fn test_delete_kitty_image() {
        let result = delete_kitty_image(42);
        assert_eq!(result, "\x1b_Ga=d,d=I,i=42,q=2\x1b\\");
    }

    #[test]
    fn test_delete_all_kitty_images() {
        let result = delete_all_kitty_images();
        assert_eq!(result, "\x1b_Ga=d,d=A,q=2\x1b\\");
    }

    // ------------------------------------------------------------------
    // Capability detection tests
    // ------------------------------------------------------------------

    #[test]
    fn test_detect_capabilities_tmux_sets_hyperlink_false() {
        let _guard = test_guard();
        // Simulate tmux
        temp_env::with_vars([("TMUX", Some("/tmp/tmux-1/default")), ("TERM", Some("screen-256color"))], || {
            let caps = TerminalCapabilities::detect();
            assert_eq!(caps.images, None);
            assert!(!caps.hyperlinks);
        });
    }

    #[test]
    fn test_detect_capabilities_kitty() {
        let _guard = test_guard();
        temp_env::with_vars([("KITTY_WINDOW_ID", Some("1")), ("TERM_PROGRAM", Some("kitty"))], || {
            let caps = TerminalCapabilities::detect();
            assert_eq!(caps.images, Some(ImageProtocol::Kitty));
            assert!(caps.true_color);
        });
    }

    #[test]
    fn test_detect_capabilities_iterm2() {
        let _guard = test_guard();
        temp_env::with_vars([("ITERM_SESSION_ID", Some("abc123")), ("TERM_PROGRAM", Some("iTerm.app"))], || {
            let caps = TerminalCapabilities::detect();
            assert_eq!(caps.images, Some(ImageProtocol::Iterm2));
            assert!(caps.true_color);
        });
    }

    #[test]
    fn test_detect_capabilities_wezterm() {
        let _guard = test_guard();
        temp_env::with_vars([("WEZTERM_PANE", Some("0")), ("TERM_PROGRAM", Some("WezTerm"))], || {
            let caps = TerminalCapabilities::detect();
            assert_eq!(caps.images, Some(ImageProtocol::Kitty));
        });
    }

    #[test]
    fn test_detect_capabilities_ghostty() {
        let _guard = test_guard();
        temp_env::with_vars([("GHOSTTY_RESOURCES_DIR", Some("/tmp")), ("TERM_PROGRAM", Some("ghostty"))], || {
            let caps = TerminalCapabilities::detect();
            assert_eq!(caps.images, Some(ImageProtocol::Kitty));
        });
    }

    #[test]
    fn test_detect_capabilities_unknown() {
        let _guard = test_guard();
        temp_env::with_vars(
            [
                ("KITTY_WINDOW_ID", None::<&str>),
                ("ITERM_SESSION_ID", None::<&str>),
                ("WEZTERM_PANE", None::<&str>),
                ("GHOSTTY_RESOURCES_DIR", None::<&str>),
                ("TERM_PROGRAM", None::<&str>),
                ("TMUX", None::<&str>),
            ],
            || {
                let caps = TerminalCapabilities::detect();
                assert_eq!(caps.images, None);
                assert!(!caps.hyperlinks);
            },
        );
    }

    // ------------------------------------------------------------------
    // Capability cache tests
    // ------------------------------------------------------------------

    #[test]
    fn test_set_capabilities_overrides_cache() {
        let _guard = test_guard();
        set_capabilities(TerminalCapabilities {
            images: Some(ImageProtocol::Kitty),
            true_color: true,
            hyperlinks: true,
        });
        let caps = get_capabilities();
        assert_eq!(caps.images, Some(ImageProtocol::Kitty));

        // Reset for other tests
        reset_capabilities_cache();
    }

    #[test]
    fn test_reset_capabilities_cache() {
        let _guard = test_guard();
        set_capabilities(TerminalCapabilities { images: None, true_color: false, hyperlinks: false });
        reset_capabilities_cache();
        // After reset, re-detect. Depending on env, images may or may not be available.
        let _caps = get_capabilities();
    }

    // ------------------------------------------------------------------
    // Image ID allocation tests
    // ------------------------------------------------------------------

    #[test]
    fn test_allocate_image_id_increments() {
        let id1 = allocate_image_id();
        let id2 = allocate_image_id();
        assert_eq!(id2, id1 + 1);
    }

    // ------------------------------------------------------------------
    // is_image_line tests
    // ------------------------------------------------------------------

    #[test]
    fn test_is_image_line_kitty() {
        assert!(is_image_line("\x1b_Ga=T,f=100;AAAA\x1b\\"));
        assert!(!is_image_line("regular text"));
    }

    #[test]
    fn test_is_image_line_iterm2() {
        assert!(is_image_line("\x1b]1337;File=inline=1:AAAA\x07"));
    }

    // ------------------------------------------------------------------
    // Dimension parsing tests (need temp_env for base64-encoded test data)
    // ------------------------------------------------------------------

    #[test]
    fn test_get_image_dimensions_invalid_base64() {
        let result = get_image_dimensions("!!!invalid-base64!!!", "image/png");
        assert!(result.is_none());
    }

    #[test]
    fn test_get_image_dimensions_empty_data() {
        let result = get_image_dimensions("", "image/png");
        assert!(result.is_none());
    }

    #[test]
    fn test_get_image_dimensions_from_bytes_empty() {
        let result = get_image_dimensions_from_bytes(&[]);
        assert!(result.is_none());
    }

    // ------------------------------------------------------------------
    // render_image tests
    // ------------------------------------------------------------------

    #[test]
    fn test_render_image_no_capabilities_fallback() {
        let _guard = test_guard();
        set_capabilities(TerminalCapabilities { images: None, true_color: false, hyperlinks: false });
        let dims = ImageDimensions { width_px: 800, height_px: 600 };
        let result = render_image("AAAA", &dims, &ImageRenderOptions::default());
        assert!(result.is_none());
        reset_capabilities_cache();
    }

    #[test]
    fn test_render_image_with_kitty_capability() {
        let _guard = test_guard();
        set_capabilities(TerminalCapabilities {
            images: Some(ImageProtocol::Kitty),
            true_color: true,
            hyperlinks: true,
        });
        let dims = ImageDimensions { width_px: 800, height_px: 600 };
        let result = render_image("AAAA", &dims, &ImageRenderOptions::default());
        assert!(result.is_some());
        let rendered = result.unwrap();
        assert!(rendered.sequence.starts_with("\x1b_G"));
        assert!(rendered.rows >= 1);
        reset_capabilities_cache();
    }

    #[test]
    fn test_render_image_with_iterm2_capability() {
        let _guard = test_guard();
        set_capabilities(TerminalCapabilities {
            images: Some(ImageProtocol::Iterm2),
            true_color: true,
            hyperlinks: true,
        });
        let dims = ImageDimensions { width_px: 800, height_px: 600 };
        let result = render_image("AAAA", &dims, &ImageRenderOptions::default());
        assert!(result.is_some());
        let rendered = result.unwrap();
        assert!(rendered.sequence.starts_with("\x1b]1337;File="));
        assert!(rendered.rows >= 1);
        reset_capabilities_cache();
    }

    #[test]
    fn test_kitty_chunk_size_boundary() {
        // Test data exactly at chunk size boundary
        let data = "A".repeat(4096);
        let result = encode_kitty(&data, None, None, None, true);
        assert!(result.starts_with("\x1b_G"));
        assert!(result.ends_with("\x1b\\"));
        // Should be a single chunk (no m= markers)
        assert!(!result.contains("m=1"));
        assert!(!result.contains("m=0"));
    }

    #[test]
    fn test_kitty_chunk_size_just_over() {
        // Test data just over chunk size
        let data = "A".repeat(4097);
        let result = encode_kitty(&data, None, None, None, true);
        assert!(result.contains("m=1"));
        assert!(result.contains("m=0"));
        // First chunk should have m=1, last should have m=0
        // The first chunk appears first
        let first_m1 = result.find("m=1").unwrap();
        let m0 = result.find("m=0").unwrap();
        assert!(first_m1 < m0);
    }

    #[test]
    fn test_cell_dimensions_roundtrip() {
        let orig = get_cell_dimensions();
        set_cell_dimensions(CellDimensions { width_px: 10.0, height_px: 20.0 });
        let updated = get_cell_dimensions();
        assert_eq!(updated.width_px, 10.0);
        assert_eq!(updated.height_px, 20.0);
        // Restore
        set_cell_dimensions(orig);
    }

    #[test]
    fn test_calculate_image_cells_with_max_height() {
        let dims = ImageDimensions { width_px: 800, height_px: 600 };
        let (cols, rows) = calculate_image_cells(&dims, 60, Some(5));
        assert!(cols >= 1);
        assert!(cols <= 60);
        assert!(rows >= 1);
        assert!(rows <= 5);
    }
}
