//! Footer component — status bar with session information.
//!
//! Displays:
//! - Working directory (abbreviated)
//! - Git branch (optional)
//! - Token usage (input / output / cache)

//!
//! Mirrors `packages/coding-agent/src/modes/interactive/components/footer.ts`

use crate::Theme;
use pi_tui_core::Component;
use pi_tui_core::utils::{truncate_to_width, visible_width};

/// Format token counts into human-readable strings (e.g. `1.5k`, `3M`).
fn format_tokens(count: u64) -> String {
    if count < 1000 {
        return count.to_string();
    }
    if count < 10_000 {
        return format!("{:.1}k", count as f64 / 1000.0);
    }
    if count < 1_000_000 {
        return format!("{}k", (count as f64 / 1000.0).round() as u64);
    }
    if count < 10_000_000 {
        return format!("{:.1}M", count as f64 / 1_000_000.0);
    }
    format!("{}M", (count as f64 / 1_000_000.0).round() as u64)
}

/// Sanitize text for display in a single-line status (no newlines, collapse spaces).
fn sanitize(text: &str) -> String {
    text.replace(['\r', '\n', '\t'], " ").split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Footer component showing session metadata at the bottom of the screen.
///
/// Renders two lines:
/// 1. PWD (abbreviated) with optional git branch
/// 2. Token stats and context usage
pub struct Footer {
    /// Current working directory (may be abbreviated with `~`).
    cwd: String,
    /// Optional git branch name.
    git_branch: Option<String>,
    /// Total input tokens used.
    input_tokens: u64,
    /// Total output tokens used.
    output_tokens: u64,
    /// Total cache-read tokens.
    cache_read: u64,
    /// Total cache-write tokens.
    cache_write: u64,
    /// Model name string.
    model_name: String,
    /// Context usage percentage (0.0–100.0).
    context_percent: f64,
    /// Total context window capacity.
    context_window: u64,
    /// Whether auto-compact is enabled.
    auto_compact: bool,
    /// Application theme.
    theme: Theme,
}

impl Footer {
    /// Create a new footer component.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        cwd: String,
        git_branch: Option<String>,
        input_tokens: u64,
        output_tokens: u64,
        cache_read: u64,
        cache_write: u64,
        model_name: String,
        context_percent: f64,
        context_window: u64,
        auto_compact: bool,
        theme: &Theme,
    ) -> Self {
        Self {
            cwd,
            git_branch,
            input_tokens,
            output_tokens,
            cache_read,
            cache_write,
            model_name,
            context_percent,
            context_window,
            auto_compact,
            theme: theme.clone(),
        }
    }
}

impl Component for Footer {
    fn render(&self, width: u16) -> Vec<String> {
        let w = width as usize;
        if w == 0 {
            return vec![];
        }

        // ---- Line 1: PWD + git branch ----
        let mut pwd = sanitize(&self.cwd);
        if let Some(ref branch) = self.git_branch {
            pwd = format!("{} ({})", pwd, sanitize(branch));
        }
        let pwd_line = truncate_to_width(&self.theme.dim(&pwd), w);

        // ---- Line 2: Token stats + right-aligned model name ----
        let stats_parts = build_stats_parts(self);
        let stats = stats_parts.join(" ");

        // Context percentage with colour
        let auto_indicator = if self.auto_compact { " (auto)" } else { "" };
        let context_display = if self.context_percent.is_nan() {
            format!("?/{}{auto_indicator}", format_tokens(self.context_window))
        } else {
            format!("{:.1}%/{}{auto_indicator}", self.context_percent, format_tokens(self.context_window))
        };
        let context_colored = if self.context_percent > 90.0 {
            self.theme.ansi(&self.theme.error, &context_display)
        } else if self.context_percent > 70.0 {
            self.theme.ansi(&self.theme.warning, &context_display)
        } else {
            context_display
        };

        // Build left stats text with context at the end
        let left_stats = if stats.is_empty() { context_colored } else { format!("{stats} {context_colored}") };
        let dim_left = self.theme.dim(&left_stats);

        // Right side: model name
        let right_side = &self.model_name;
        let right_width = visible_width(right_side);

        // Layout: left stats + padding + right model
        let left_width = visible_width(&dim_left);
        let min_padding = 2;

        let stats_line = if left_width + min_padding + right_width <= w {
            let padding = " ".repeat(w.saturating_sub(left_width + right_width));
            format!("{dim_left}{padding}{right_side}")
        } else {
            dim_left
        };

        vec![pwd_line, stats_line]
    }

    fn invalidate(&mut self) {
        // No cached state
    }
}

/// Build the token-statistics string parts.
fn build_stats_parts(footer: &Footer) -> Vec<String> {
    let mut parts = Vec::new();
    if footer.input_tokens > 0 {
        parts.push(format!("\u{2191}{}", format_tokens(footer.input_tokens)));
    }
    if footer.output_tokens > 0 {
        parts.push(format!("\u{2193}{}", format_tokens(footer.output_tokens)));
    }
    if footer.cache_read > 0 {
        parts.push(format!("R{}", format_tokens(footer.cache_read)));
    }
    if footer.cache_write > 0 {
        parts.push(format!("W{}", format_tokens(footer.cache_write)));
    }
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_tokens() {
        assert_eq!(format_tokens(0), "0");
        assert_eq!(format_tokens(500), "500");
        assert_eq!(format_tokens(1500), "1.5k");
        assert_eq!(format_tokens(10000), "10k");
        assert_eq!(format_tokens(1_000_000), "1.0M");
        assert_eq!(format_tokens(1_500_000), "1.5M");
    }

    #[test]
    fn test_footer_renders_something() {
        let theme = Theme::dark();
        let footer = Footer::new(
            "/home/user/project".into(),
            Some("main".into()),
            1000,
            500,
            200,
            100,
            "claude-opus-4".into(),
            45.0,
            200000,
            true,
            &theme,
        );
        let lines = footer.render(80);
        assert_eq!(lines.len(), 2);
        // PWD line should contain project path
        assert!(lines[0].contains("project"));
        // Stats line should contain token indicators
        assert!(lines[1].contains("\u{2191}") || lines[1].contains("\u{2193}"));
    }

    #[test]
    fn test_footer_empty_stats() {
        let theme = Theme::dark();
        let footer = Footer::new("/tmp".into(), None, 0, 0, 0, 0, "test-model".into(), 0.0, 100000, false, &theme);
        let lines = footer.render(80);
        assert_eq!(lines.len(), 2);
    }

    #[test]
    fn test_footer_context_warning() {
        let theme = Theme::dark();
        let footer = Footer::new("/".into(), None, 100, 50, 0, 0, "model".into(), 85.0, 100000, false, &theme);
        let lines = footer.render(80);
        assert_eq!(lines.len(), 2);
    }

    #[test]
    fn test_footer_context_error() {
        let theme = Theme::dark();
        let footer = Footer::new("/".into(), None, 0, 0, 0, 0, "model".into(), 95.0, 100000, false, &theme);
        let lines = footer.render(80);
        assert_eq!(lines.len(), 2);
    }
}
