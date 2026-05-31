//! Markdown rendering component.
//!
//! Renders Markdown as ANSI-styled terminal output using `comrak` for parsing
//! and `syntect` for syntax highlighting.
//!
//! Mirrors `packages/tui/src/components/markdown.ts`

use std::cell::RefCell;
use std::sync::OnceLock;

use comrak::nodes::{AstNode, ListType, NodeCodeBlock, NodeHeading, NodeHtmlBlock, NodeList, NodeValue};
use comrak::{Arena, Options, parse_document};
use syntect::highlighting::ThemeSet;
use syntect::parsing::SyntaxSet;

use crate::component::Component;
use crate::utils::{visible_width, wrap_text_with_ansi};

// ---------------------------------------------------------------------------
// Comrak options (GFM-style)
// ---------------------------------------------------------------------------

/// Create comrak options with GFM extensions enabled.
fn comrak_options() -> Options {
    let mut opts = Options::default();
    opts.extension.table = true;
    opts.extension.strikethrough = true;
    opts.extension.autolink = true;
    opts.extension.tasklist = true;
    opts.extension.tagfilter = true;
    opts
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const RESET: &str = "\x1b[0m";

// ---------------------------------------------------------------------------
// Theme
// ---------------------------------------------------------------------------

/// Theme for markdown rendering.
///
/// Each field is an ANSI escape sequence prefix (e.g. `"\x1b[1m"` for bold)
/// that will be applied before the rendered text.  Use empty strings to omit
/// styling.
pub struct MarkdownTheme {
    /// Per-level heading styles (index 0 = level 1, index 5 = level 6).
    pub heading: Vec<&'static str>,
    pub bold: &'static str,
    pub italic: &'static str,
    pub code: &'static str,
    pub code_block: &'static str,
    pub code_block_border: &'static str,
    pub link: &'static str,
    pub link_url: &'static str,
    pub list_bullet: &'static str,
    pub quote: &'static str,
    pub quote_border: &'static str,
    pub hr: &'static str,
    pub strikethrough: &'static str,
    pub underline: &'static str,
}

impl Default for MarkdownTheme {
    fn default() -> Self {
        Self {
            heading: vec![
                "\x1b[1;4m", // level 1: bold + underline
                "\x1b[1m",   // level 2: bold
                "\x1b[1m",   // level 3: bold
                "\x1b[1m",   // level 4: bold
                "\x1b[1m",   // level 5: bold
                "\x1b[1m",   // level 6: bold
            ],
            bold: "\x1b[1m",
            italic: "\x1b[3m",
            code: "\x1b[33m",
            code_block: "\x1b[33m",
            code_block_border: "\x1b[2;90m",
            link: "\x1b[34;4m",
            link_url: "\x1b[2m",
            list_bullet: "\x1b[33m",
            quote: "\x1b[3m",
            quote_border: "\x1b[2;90m",
            hr: "\x1b[2;90m",
            strikethrough: "\x1b[9m",
            underline: "\x1b[4m",
        }
    }
}

// ---------------------------------------------------------------------------
// Markdown component
// ---------------------------------------------------------------------------

/// A component that renders Markdown as ANSI-styled terminal output.
///
/// The cached rendering is keyed on `(text, width)` and is invalidated when
/// either changes.  Interior mutability via `RefCell` lets the read-only
/// `Component::render(&self)` write to the cache.
pub struct Markdown {
    text: String,
    theme: MarkdownTheme,
    cached_lines: RefCell<Option<(String, u16, Vec<String>)>>,
}

impl Markdown {
    /// Create a new `Markdown` component.
    pub fn new(text: String, theme: MarkdownTheme) -> Self {
        Self { text, theme, cached_lines: RefCell::new(None) }
    }

    /// Return the current markdown source text.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Replace the source text and invalidate the cache.
    pub fn set_text(&mut self, text: String) {
        self.text = text;
        self.invalidate();
    }

    /// Normalise tabs (convert to 3 spaces).
    fn normalize_text(&self) -> String {
        self.text.replace('\t', "   ")
    }

    // -----------------------------------------------------------------------
    // Block-level rendering
    // -----------------------------------------------------------------------

    /// Render a single block-level node.
    fn render_block<'a>(&self, node: &'a AstNode<'a>, width: usize, add_spacing: bool, tight: bool) -> Vec<String> {
        let data = node.data.borrow();
        match &data.value {
            NodeValue::Heading(h) => self.render_heading(node, h, add_spacing, tight),
            NodeValue::Paragraph => self.render_paragraph(node, add_spacing, tight),
            NodeValue::CodeBlock(cb) => self.render_code_block(cb, add_spacing),
            NodeValue::List(list) => self.render_list(node, list, 0, width),
            NodeValue::BlockQuote => self.render_blockquote(node, width, add_spacing),
            NodeValue::Table(table) => self.render_table(node, table, width, add_spacing),
            NodeValue::ThematicBreak => self.render_thematic_break(width, add_spacing),
            NodeValue::HtmlBlock(html) => self.render_html_block(html, add_spacing),
            _ => Vec::new(),
        }
    }

    /// Return the per-level heading ANSI prefix, falling back to an empty
    /// string for out-of-range levels.
    fn heading_style(&self, level: usize) -> &'static str {
        self.theme.heading.get(level.saturating_sub(1)).copied().unwrap_or("")
    }

    // -- heading -----------------------------------------------------------

    fn render_heading<'a>(
        &self,
        node: &'a AstNode<'a>,
        heading: &NodeHeading,
        add_spacing: bool,
        tight: bool,
    ) -> Vec<String> {
        let level = heading.level as usize;
        let hstyle = self.heading_style(level);
        let content = self.render_inline_children(node);

        // Prefix "# ..." for level >= 3
        let prefix =
            if level >= 3 { format!("{}#{} \x1b[0m{}", hstyle, "#".repeat(level - 1), hstyle) } else { String::new() };

        let line = format!("{}{}{}{}", hstyle, prefix, content, RESET);
        let mut out = wrap_text_with_ansi(&line, 9999); // no width limit for heading itself

        if add_spacing && !tight {
            out.push(String::new());
        }
        out
    }

    // -- paragraph ---------------------------------------------------------

    fn render_paragraph<'a>(&self, node: &'a AstNode<'a>, add_spacing: bool, tight: bool) -> Vec<String> {
        let content = self.render_inline_children(node);
        let lines = wrap_text_with_ansi(&content, 9999); // width is applied at top level

        let mut out = lines;
        if add_spacing && !tight {
            out.push(String::new());
        }
        out
    }

    // -- code block --------------------------------------------------------

    fn render_code_block(&self, cb: &NodeCodeBlock, add_spacing: bool) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();

        // Opening fence
        let lang_info = if cb.info.is_empty() { String::from("```") } else { format!("```{}", cb.info) };
        out.push(format!("{}{}{}", self.theme.code_block_border, lang_info, RESET));

        // Strip trailing newline from literal to avoid empty last line
        let code_text = cb.literal.trim_end_matches('\n');

        // Highlighted or plain content, each line indented by 2 spaces
        let indent = "  ";
        if let Some(highlighted) = highlight_code(code_text, &cb.info) {
            for hl_line in highlighted {
                out.push(format!("{}{}", indent, hl_line));
            }
        } else {
            for code_line in code_text.split('\n') {
                out.push(format!("{}{}{}{}", indent, self.theme.code_block, code_line, RESET));
            }
        }

        // Closing fence
        out.push(format!("{}{}{}", self.theme.code_block_border, "```", RESET));

        if add_spacing {
            out.push(String::new());
        }
        out
    }

    // -- list --------------------------------------------------------------

    fn render_list<'a>(&self, node: &'a AstNode<'a>, list: &NodeList, depth: usize, _width: usize) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let indent = "    ".repeat(depth);
        let start_number = list.start;

        let children: Vec<_> = node.children().collect();
        let total_items = children.len();

        for (i, item_node) in children.iter().enumerate() {
            let item_data = item_node.data.borrow();
            if !matches!(item_data.value, NodeValue::Item(_)) {
                continue;
            }

            let bullet = match list.list_type {
                ListType::Ordered => format!("{}. ", start_number + i),
                ListType::Bullet => "- ".to_string(),
            };

            // Check for task marker
            let task_marker = get_task_marker(item_node);

            let marker = format!("{}{}", bullet, task_marker);
            let first_prefix = format!("{}{}{}{}", indent, self.theme.list_bullet, marker, RESET);
            let continuation_prefix = format!("{}{}", indent, " ".repeat(visible_width(&marker)));

            let mut rendered_any_line = false;

            for child in item_node.children() {
                let child_data = child.data.borrow();
                match &child_data.value {
                    NodeValue::List(nested_list) => {
                        let nested = self.render_list(child, nested_list, depth + 1, _width);
                        for line in nested {
                            out.push(line);
                        }
                        rendered_any_line = true;
                    }
                    _ => {
                        let tight = list.tight;
                        let item_lines = self.render_block(child, 9999, false, tight);
                        for line in item_lines {
                            let line_prefix =
                                if rendered_any_line { continuation_prefix.as_str() } else { first_prefix.as_str() };
                            out.push(format!("{}{}", line_prefix, line));
                            rendered_any_line = true;
                        }
                    }
                }
            }

            if !rendered_any_line {
                out.push(first_prefix);
            }

            // Add spacing between list items for loose lists
            if !list.tight && i < total_items - 1 {
                let has_paragraph = item_node.children().any(|c| matches!(c.data.borrow().value, NodeValue::Paragraph));
                if has_paragraph {
                    out.push(String::new());
                }
            }
        }

        out
    }

    // -- blockquote --------------------------------------------------------

    fn render_blockquote<'a>(&self, node: &'a AstNode<'a>, width: usize, add_spacing: bool) -> Vec<String> {
        let content_width = std::cmp::max(1, width.saturating_sub(2));

        let mut rendered: Vec<String> = Vec::new();

        for child in node.children() {
            let child_data = child.data.borrow();
            let tight = false;
            let block_lines = match &child_data.value {
                NodeValue::List(list) => self.render_list(child, list, 0, content_width),
                _ => self.render_block(child, content_width, false, tight),
            };

            for line in block_lines {
                let styled_line = format!("{}{}", self.theme.quote, line);
                let wrapped = wrap_text_with_ansi(&styled_line, content_width);
                for wl in wrapped {
                    rendered.push(format!("{}{}{}{}", self.theme.quote_border, "\u{2502} ", wl, RESET));
                }
            }
        }

        // Strip trailing empty lines inside the quote
        while rendered.last().is_some_and(|l| l.trim_end().is_empty()) {
            rendered.pop();
        }

        if add_spacing {
            rendered.push(String::new());
        }
        rendered
    }

    // -- table -------------------------------------------------------------

    fn render_table<'a>(
        &self,
        node: &'a AstNode<'a>,
        table: &comrak::nodes::NodeTable,
        width: usize,
        add_spacing: bool,
    ) -> Vec<String> {
        let num_cols = table.num_columns;
        if num_cols == 0 {
            return Vec::new();
        }

        // Collect header and body rows
        let mut header_cells: Vec<String> = Vec::new();
        let mut body_rows: Vec<Vec<String>> = Vec::new();

        for row in node.children() {
            let row_data = row.data.borrow();
            if let NodeValue::TableRow(is_header) = &row_data.value {
                let mut cells: Vec<String> = Vec::new();
                for cell in row.children() {
                    if matches!(cell.data.borrow().value, NodeValue::TableCell) {
                        let text = self.render_inline_children(cell);
                        cells.push(text);
                    }
                }
                if *is_header {
                    header_cells = cells;
                } else {
                    body_rows.push(cells);
                }
            }
        }

        if header_cells.is_empty() && !body_rows.is_empty() {
            header_cells = body_rows.remove(0);
        }

        let actual_cols = std::cmp::max(header_cells.len(), body_rows.iter().map(|r| r.len()).max().unwrap_or(0));

        if actual_cols == 0 {
            return Vec::new();
        }

        let border_overhead = 3 * actual_cols + 1;
        let available_for_cells = width.saturating_sub(border_overhead);
        if available_for_cells < actual_cols {
            let mut fallback: Vec<String> = Vec::new();
            if !header_cells.is_empty() {
                fallback.push(header_cells.join(" | "));
            }
            for row in &body_rows {
                fallback.push(row.join(" | "));
            }
            if add_spacing {
                fallback.push(String::new());
            }
            return fallback;
        }

        let max_unbroken = 30usize;
        let mut natural: Vec<usize> = vec![0; actual_cols];
        let mut min_word: Vec<usize> = vec![1; actual_cols];

        let mut update_widths = |cells: &[String]| {
            for (i, cell_text) in cells.iter().enumerate() {
                if i >= natural.len() {
                    break;
                }
                let w = visible_width(cell_text);
                natural[i] = std::cmp::max(natural[i], w);
                let longest = cell_text.split_whitespace().map(visible_width).max().unwrap_or(1);
                let clamped = std::cmp::min(longest, max_unbroken);
                min_word[i] = std::cmp::max(min_word[i], clamped);
            }
        };

        update_widths(&header_cells);

        let header_padded = {
            let mut h = header_cells.clone();
            while h.len() < actual_cols {
                h.push(String::new());
            }
            h
        };

        for row in &body_rows {
            let mut padded: Vec<String> = row.clone();
            while padded.len() < actual_cols {
                padded.push(String::new());
            }
            update_widths(&padded);
        }

        let total_natural: usize = natural.iter().sum();
        let mut col_widths: Vec<usize> = if total_natural <= available_for_cells {
            natural.iter().enumerate().map(|(i, &w)| std::cmp::max(w, min_word[i])).collect()
        } else {
            let total_min: usize = min_word.iter().sum();
            let extra = available_for_cells.saturating_sub(total_min);
            let mut widths = min_word.clone();

            if extra > 0 {
                let total_growable: usize =
                    natural.iter().zip(min_word.iter()).map(|(&n, &m)| n.saturating_sub(m)).sum();

                if total_growable > 0 {
                    let mut allocated = 0usize;
                    for (i, w) in widths.iter_mut().enumerate() {
                        let share = natural[i].saturating_sub(*w);
                        let add = share * extra / total_growable;
                        *w += add;
                        allocated += add;
                    }
                    let mut rem = extra.saturating_sub(allocated);
                    for i in 0..actual_cols {
                        if rem == 0 {
                            break;
                        }
                        if widths[i] < natural[i] {
                            widths[i] += 1;
                            rem -= 1;
                        }
                    }
                } else {
                    for width in widths.iter_mut().take(actual_cols) {
                        if extra > 0 {
                            *width += extra / actual_cols;
                        }
                    }
                    let rem = extra % actual_cols;
                    for width in widths.iter_mut().take(rem) {
                        *width += 1;
                    }
                }
            }
            widths
        };

        let total_alloc: usize = col_widths.iter().sum();
        if total_alloc < available_for_cells {
            let mut rem = available_for_cells - total_alloc;
            for w in col_widths.iter_mut() {
                if rem == 0 {
                    break;
                }
                *w += 1;
                rem -= 1;
            }
        }

        // Render borders and content
        let mut lines: Vec<String> = Vec::new();

        lines.push(format!("┌─{}─┐", col_widths.iter().map(|&w| "─".repeat(w)).collect::<Vec<_>>().join("─┬─")));

        self.render_table_row(&header_padded, &col_widths, true, &mut lines);

        lines.push(format!("├─{}─┤", col_widths.iter().map(|&w| "─".repeat(w)).collect::<Vec<_>>().join("─┼─")));

        for (ri, row) in body_rows.iter().enumerate() {
            let mut padded: Vec<String> = row.clone();
            while padded.len() < actual_cols {
                padded.push(String::new());
            }
            self.render_table_row(&padded, &col_widths, false, &mut lines);

            if ri < body_rows.len() - 1 {
                lines
                    .push(format!("├─{}─┤", col_widths.iter().map(|&w| "─".repeat(w)).collect::<Vec<_>>().join("─┼─")));
            }
        }

        lines.push(format!("└─{}─┘", col_widths.iter().map(|&w| "─".repeat(w)).collect::<Vec<_>>().join("─┴─")));

        if add_spacing {
            lines.push(String::new());
        }
        lines
    }

    /// Render a single table row (header or body).
    fn render_table_row(&self, cells: &[String], col_widths: &[usize], is_header: bool, out: &mut Vec<String>) {
        let cell_lines: Vec<Vec<String>> = cells
            .iter()
            .enumerate()
            .map(|(i, text)| {
                let cw = *col_widths.get(i).unwrap_or(&1);
                if text.is_empty() { vec![String::new()] } else { wrap_text_with_ansi(text, cw) }
            })
            .collect();

        let max_lines = cell_lines.iter().map(|c| c.len()).max().unwrap_or(1);

        for line_idx in 0..max_lines {
            let parts: Vec<String> = cell_lines
                .iter()
                .enumerate()
                .map(|(ci, cell)| {
                    let line_text = cell.get(line_idx).cloned().unwrap_or_default();
                    let cw = *col_widths.get(ci).unwrap_or(&1);
                    let visible = visible_width(&line_text);
                    let padding = " ".repeat(cw.saturating_sub(visible));
                    let padded = format!("{}{}", line_text, padding);
                    if is_header { format!("{}{}{}", self.theme.bold, padded, RESET) } else { padded }
                })
                .collect();

            out.push(format!("│ {} │", parts.join(" │ ")));
        }
    }

    // -- thematic break ----------------------------------------------------

    fn render_thematic_break(&self, width: usize, add_spacing: bool) -> Vec<String> {
        let count = std::cmp::min(width, 80);
        let line = format!("{}{}{}", self.theme.hr, "─".repeat(count), RESET);
        let mut out = vec![line];
        if add_spacing {
            out.push(String::new());
        }
        out
    }

    // -- html block --------------------------------------------------------

    fn render_html_block(&self, html: &NodeHtmlBlock, add_spacing: bool) -> Vec<String> {
        let trimmed = html.literal.trim();
        if trimmed.is_empty() {
            return Vec::new();
        }
        let mut out = vec![trimmed.to_string()];
        if add_spacing {
            out.push(String::new());
        }
        out
    }

    // -----------------------------------------------------------------------
    // Inline rendering
    // -----------------------------------------------------------------------

    /// Render all children of `node` as inline content and return a single
    /// string without any block-level wrapping.
    fn render_inline_children<'a>(&self, node: &'a AstNode<'a>) -> String {
        let mut result = String::new();
        for child in node.children() {
            result.push_str(&self.render_inline(child));
        }
        result
    }

    /// Render a single inline node to a styled string.
    fn render_inline<'a>(&self, node: &'a AstNode<'a>) -> String {
        let data = node.data.borrow();
        match &data.value {
            NodeValue::Text(text) => text.clone(),
            NodeValue::Strong => {
                let content = self.render_inline_children(node);
                format!("{}{}{}", self.theme.bold, content, RESET)
            }
            NodeValue::Emph => {
                let content = self.render_inline_children(node);
                format!("{}{}{}", self.theme.italic, content, RESET)
            }
            NodeValue::Strikethrough => {
                let content = self.render_inline_children(node);
                format!("{}{}{}", self.theme.strikethrough, content, RESET)
            }
            NodeValue::Code(code) => {
                format!("{}{}{}", self.theme.code, code.literal, RESET)
            }
            NodeValue::Link(link) => {
                let content = self.render_inline_children(node);
                let styled = format!("{}{}{}{}", self.theme.link, self.theme.underline, content, RESET);
                format!("\x1b]8;;{}\x1b\\{}\x1b]8;;\x1b\\", link.url, styled)
            }
            NodeValue::SoftBreak => "\n".to_string(),
            NodeValue::LineBreak => "\n".to_string(),
            NodeValue::HtmlInline(html) => html.clone(),
            NodeValue::Image(img) => {
                let alt = self.render_inline_children(node);
                if alt.is_empty() { format!("[{}]", img.url) } else { alt }
            }
            _ => String::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Component trait
// ---------------------------------------------------------------------------

impl Component for Markdown {
    fn render(&self, width: u16) -> Vec<String> {
        // Check cache
        if let Some((ref cached_text, cached_width, ref cached_lines)) = *self.cached_lines.borrow() {
            if cached_text == &self.text && cached_width == width {
                return cached_lines.clone();
            }
        }

        let content_width = std::cmp::max(1, width as usize);

        // Empty or whitespace-only text -> no output
        if self.text.trim().is_empty() {
            let result: Vec<String> = Vec::new();
            *self.cached_lines.borrow_mut() = Some((self.text.clone(), width, result.clone()));
            return result;
        }

        // Normalize and parse
        let normalized = self.normalize_text();
        let arena = Arena::new();
        let root = parse_document(&arena, &normalized, &comrak_options());

        // Render each top-level block
        let mut rendered_lines: Vec<String> = Vec::new();
        let children: Vec<_> = root.children().collect();

        for (i, child) in children.iter().enumerate() {
            let has_next = i + 1 < children.len();
            let block_lines = self.render_block(child, content_width, has_next, false);
            // Apply word wrapping to each line
            for line in &block_lines {
                for wrapped_line in wrap_text_with_ansi(line, content_width) {
                    rendered_lines.push(wrapped_line);
                }
            }
        }

        *self.cached_lines.borrow_mut() = Some((self.text.clone(), width, rendered_lines.clone()));
        rendered_lines
    }

    fn invalidate(&mut self) {
        *self.cached_lines.borrow_mut() = None;
    }
}

// ---------------------------------------------------------------------------
// Syntax highlighting (syntect)
// ---------------------------------------------------------------------------

/// Try to highlight `code` with the given language tag using syntect.
///
/// Returns `None` when the language is not recognised or highlighting fails,
/// allowing the caller to fall back to plain styling.
fn highlight_code(code: &str, lang: &str) -> Option<Vec<String>> {
    use syntect::easy::HighlightLines;
    use syntect::util::as_24_bit_terminal_escaped;

    let ss = syntax_set();
    let ts = theme_set();

    // The info string may be "rust" or "rust hidden" etc. Take the first token.
    let lang_token = lang.split_whitespace().next().unwrap_or("");
    let syntax = ss.find_syntax_by_token(lang_token)?;
    let theme = ts.themes.get("base16-ocean.dark")?;

    let mut h = HighlightLines::new(syntax, theme);
    let mut result: Vec<String> = Vec::new();

    for line in code.split('\n') {
        let ranges = h.highlight_line(line, ss).ok()?;
        let escaped = as_24_bit_terminal_escaped(&ranges, false);
        result.push(escaped);
    }

    Some(result)
}

/// Lazily-initialised global syntax set.
fn syntax_set() -> &'static SyntaxSet {
    static SS: OnceLock<SyntaxSet> = OnceLock::new();
    SS.get_or_init(SyntaxSet::load_defaults_newlines)
}

/// Lazily-initialised global theme set.
fn theme_set() -> &'static ThemeSet {
    static TS: OnceLock<ThemeSet> = OnceLock::new();
    TS.get_or_init(ThemeSet::load_defaults)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Check whether `item_node` (a `NodeValue::Item`) has a `TaskItem` child and
/// return the checkbox marker string.
fn get_task_marker<'a>(item_node: &'a AstNode<'a>) -> String {
    for child in item_node.children() {
        if let NodeValue::TaskItem(c) = &child.data.borrow().value {
            let checked = match c {
                Some(ch) if *ch == 'x' || *ch == 'X' => 'x',
                _ => ' ',
            };
            return format!("[{}] ", checked);
        }
    }
    String::new()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: create a Markdown component with the default theme.
    fn md(text: &str) -> Markdown {
        Markdown::new(text.to_string(), MarkdownTheme::default())
    }

    // -- Plain text --------------------------------------------------------

    #[test]
    fn test_plain_paragraph() {
        let m = md("Hello, world!");
        let lines = m.render(80);
        assert!(
            lines.iter().any(|l| l.contains("Hello, world!")),
            "expected 'Hello, world!' in output, got {:?}",
            lines
        );
    }

    #[test]
    fn test_empty_text_returns_empty() {
        let m = md("");
        let lines = m.render(80);
        assert!(lines.is_empty(), "expected empty output, got {:?}", lines);
    }

    #[test]
    fn test_whitespace_text_returns_empty() {
        let m = md("   \n  \n  ");
        let lines = m.render(80);
        assert!(lines.is_empty());
    }

    // -- Heading -----------------------------------------------------------

    #[test]
    fn test_heading_prefix() {
        let m = md("## Section Title");
        let lines = m.render(80);
        let visible = lines.iter().map(|l| visible_width(l)).sum::<usize>();
        assert!(visible > 0, "heading produced no visible content");
        assert!(lines.iter().any(|l| l.contains("Section Title")), "heading should contain title text");
    }

    #[test]
    fn test_heading_level3_has_prefix() {
        let m = md("### Sub Section");
        let lines = m.render(80);
        let combined = lines.join(" ");
        assert!(combined.contains("###"), "H3 should show ### prefix, got: {:?}", lines);
    }

    // -- Code block --------------------------------------------------------

    #[test]
    fn test_code_block_fallback() {
        let m = md("```\nlet x = 1;\n```\n");
        let lines = m.render(80);
        let combined = lines.join("\n");
        assert!(combined.contains("let x = 1;"), "code block should contain the code text, got: {:?}", lines);
        assert!(combined.contains("```"), "code block should have fence markers");
    }

    #[test]
    fn test_code_block_with_lang() {
        let m = md("```rust\nfn main() {}\n```\n");
        let lines = m.render(80);
        let combined = lines.join("\n");
        assert!(combined.contains("```rust"), "opening fence should show language, got: {:?}", lines);
        // Note: syntax highlighting inserts ANSI codes between tokens,
        // so "fn main()" won't appear contiguously. Check for presence of
        // the individual words instead.
        assert!(combined.contains("fn"), "code block should contain 'fn', got: {:?}", lines);
        assert!(combined.contains("main"), "code block should contain 'main', got: {:?}", lines);
    }

    // -- List --------------------------------------------------------------

    #[test]
    fn test_bullet_list() {
        let m = md("- item one\n- item two\n- item three");
        let lines = m.render(80);
        assert!(lines.iter().any(|l| l.contains("item one")), "list should contain 'item one'");
        assert!(lines.iter().any(|l| l.contains("item two")), "list should contain 'item two'");
    }

    #[test]
    fn test_ordered_list() {
        let m = md("1. first\n2. second\n3. third");
        let lines = m.render(80);
        assert!(lines.iter().any(|l| l.contains("first")), "ordered list should contain 'first'");
        assert!(lines.iter().any(|l| l.contains("second")), "ordered list should contain 'second'");
    }

    #[test]
    fn test_nested_list() {
        let m = md("- outer\n  - inner\n- outer2");
        let lines = m.render(80);
        assert!(lines.iter().any(|l| l.contains("outer")), "nested list should contain 'outer'");
        assert!(lines.iter().any(|l| l.contains("inner")), "nested list should contain 'inner'");
    }

    // -- Inline bold / italic ----------------------------------------------

    #[test]
    fn test_inline_bold() {
        let m = md("This is **bold** text");
        let lines = m.render(80);
        let combined = lines.join(" ");
        assert!(combined.contains("\x1b[1m"), "bold text should contain bold ANSI code, got: {:?}", lines);
        assert!(combined.contains("bold"), "rendered text should contain 'bold'");
    }

    #[test]
    fn test_inline_italic() {
        let m = md("This is *italic* text");
        let lines = m.render(80);
        let combined = lines.join(" ");
        assert!(combined.contains("\x1b[3m"), "italic text should contain italic ANSI code");
    }

    #[test]
    fn test_inline_code() {
        let m = md("Use the `foo()` function");
        let lines = m.render(80);
        let combined = lines.join(" ");
        assert!(combined.contains("foo()"), "inline code should contain code text");
        assert!(combined.contains("\x1b[33m"), "inline code should contain code ANSI color");
    }

    // -- Cache invalidation ------------------------------------------------

    #[test]
    fn test_cache_invalidates_on_text_change() {
        let mut m = md("First paragraph");
        let lines1 = m.render(80);
        assert!(!lines1.is_empty());

        m.set_text("Second paragraph".to_string());
        let lines2 = m.render(80);
        assert!(lines2.iter().any(|l| l.contains("Second")));
    }

    #[test]
    fn test_cache_reuses_same_output() {
        let m = md("Cache test");
        let lines1 = m.render(80);
        let lines2 = m.render(80);
        assert_eq!(lines1, lines2, "same input should produce same output");
    }

    // -- Inline mixed formatting -------------------------------------------

    #[test]
    fn test_bold_and_italic_together() {
        let m = md("This is ***bold and italic*** text");
        let lines = m.render(80);
        let combined = lines.join(" ");
        assert!(combined.contains("\x1b[1m"), "should have bold ANSI");
        assert!(combined.contains("\x1b[3m"), "should have italic ANSI");
        assert!(combined.contains("bold and italic"), "should contain the text");
    }

    // -- Link --------------------------------------------------------------

    #[test]
    fn test_link_renders_osc8() {
        let m = md("Click [here](https://example.com) now");
        let lines = m.render(80);
        let combined = lines.join(" ");
        assert!(combined.contains("\x1b]8;;https://example.com"), "link should contain OSC 8 hyperlink open");
        assert!(combined.contains("\x1b]8;;\x1b\\"), "link should contain OSC 8 hyperlink close");
        assert!(combined.contains("here"), "link text should be present");
    }

    // -- Thematic break ----------------------------------------------------

    #[test]
    fn test_thematic_break() {
        let m = md("---\n");
        let lines = m.render(80);
        let combined = lines.join("\n");
        assert!(combined.contains("─"), "thematic break should contain ─ characters");
    }

    // -- Blockquote --------------------------------------------------------

    #[test]
    fn test_blockquote() {
        let m = md("> quoted text\n> more quote");
        let lines = m.render(80);
        let combined = lines.join("\n");
        assert!(combined.contains("quoted text"), "blockquote should contain quoted text");
        assert!(combined.contains("more quote"), "blockquote should contain continuation text");
        assert!(combined.contains("\u{2502}"), "blockquote should use │ border");
    }

    // -- Table -------------------------------------------------------------

    #[test]
    fn test_simple_table() {
        let m = md("| A | B |\n|---|---|\n| 1 | 2 |\n| 3 | 4 |\n");
        let lines = m.render(80);
        let combined = lines.join("\n");
        assert!(combined.contains("A"), "table should contain header 'A', got: {:?}", lines);
        assert!(combined.contains("1"), "table should contain cell '1', got: {:?}", lines);
        // Check for box-drawing characters (│ from table borders)
        assert!(combined.contains("\u{2502}"), "table should use column separators, got: {:?}", lines);
    }

    // -- Horizontal rule ---------------------------------------------------

    #[test]
    fn test_horizontal_rule() {
        let m = md("***\n");
        let lines = m.render(80);
        let combined = lines.join("\n");
        assert!(combined.contains("─"), "horizontal rule should contain ─");
    }

    // -- Word wrapping -----------------------------------------------------

    #[test]
    fn test_long_text_wraps() {
        let long =
            "This is a very long line that should be wrapped because it exceeds the available width of forty columns. ";

        let m = md(long);
        let lines = m.render(20);
        for line in &lines {
            let vw = visible_width(line);
            assert!(vw <= 20, "wrapped line has visible width {} > 20: {:?}", vw, line);
        }
        assert!(lines.len() > 1, "long text should produce multiple wrapped lines, got {}", lines.len());
    }

    // -- Escaped HTML ------------------------------------------------------

    #[test]
    fn test_html_block() {
        let m = md("<div>hello</div>\n\n<p>world</p>\n");
        let lines = m.render(80);
        let combined = lines.join(" ");
        assert!(combined.contains("hello"), "html block should contain 'hello'");
        assert!(combined.contains("world"), "html block should contain 'world'");
    }
}
