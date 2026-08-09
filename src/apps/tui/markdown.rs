//! Produces styled [`LogicalLine`]s from parsed markdown [`Node`]s.
//!
//! [`MarkdownRenderer::render`] walks the AST and builds a vector of `LogicalLine`s,
//! one per semantic markdown element (headings, paragraphs, code body lines,
//! table rows, etc.). Tables expand into multiple `LogicalLine`s (one per row).
//!
//! [`LogicalLine::render`] lazily renders a `LogicalLine` into a ratatui
//! [`Line`] at draw time.  It applies syntax highlighting (with a per-line cache
//! in [`LazyText::cached`]), theme colours, active-code gutters, spinners,
//! and search highlights.  The resulting `Line` is then consumed by the preview
//! pane which may soft-wrap it across multiple terminal rows.

use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span, Text};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use unicode_width::UnicodeWidthChar;

use crate::apps::config::PREVIEW_FRAME_OVERHEAD;
use crate::apps::theme::Theme;
use crate::runner::CodeId;
use upmd_parser::nodes::{
    inline_text, Alignment, Code, DepsToken, InlineSpan, InlineStyle, ListKind, Table, TableCell,
    TaskStatus,
};
use upmd_parser::Codes;

use crate::apps::task::Task;
use crate::apps::tui::wrap::slice_line;

/// Render-time context passed to [`LogicalLine::render`].
pub struct RenderContext<'a> {
    pub theme: &'a Theme,
    pub active_code_id: Option<CodeId>,
    /// When set, this block's task status color overrides active gutter color.
    pub prefer_status_gutter: Option<CodeId>,
    pub spinner_char: char,
    pub viewport_width: usize,
}

/// Source text rendered lazily and cached independently of viewport paint.
#[derive(Debug, Clone)]
pub struct LazyText {
    pub(crate) text: String,
    pub(crate) language: String,
    /// Parsed inline Markdown formatting for this line.
    pub(crate) spans: Vec<InlineSpan>,
    /// Lazily populated render cache.
    pub(crate) cached: std::cell::RefCell<Option<Text<'static>>>,
}

impl LazyText {
    fn new(text: impl Into<String>, language: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            language: language.into(),
            spans: Vec::new(),
            cached: std::cell::RefCell::new(None),
        }
    }

    fn from_spans(spans: Vec<InlineSpan>) -> Self {
        let text = inline_text(&spans);
        Self {
            text,
            language: "markdown".into(),
            spans,
            cached: std::cell::RefCell::new(None),
        }
    }
}

impl Default for LazyText {
    fn default() -> Self {
        Self {
            text: String::new(),
            language: String::new(),
            spans: Vec::new(),
            cached: std::cell::RefCell::new(None),
        }
    }
}

#[derive(Debug)]
pub struct MarkdownHtml {
    source: String,
    lines: Vec<String>,
    cached: RefCell<Option<Text<'static>>>,
}

impl MarkdownHtml {
    fn new(source: String) -> Self {
        let lines = source.lines().map(str::to_owned).collect();
        Self {
            source,
            lines,
            cached: RefCell::new(None),
        }
    }

    pub(crate) fn source(&self) -> &str {
        &self.source
    }

    fn len(&self) -> usize {
        self.lines.len()
    }

    fn raw_line(&self, row_idx: usize) -> &str {
        self.lines.get(row_idx).map_or("", String::as_str)
    }

    fn line(&self, row_idx: usize, theme: &Theme) -> Line<'static> {
        let mut cached = self.cached.borrow_mut();
        if cached.is_none() {
            let mut rendered = theme.highlight(&self.source, "html");
            for line in &mut rendered.lines {
                *line = expand_tabs_in_line(std::mem::take(line));
            }
            *cached = Some(rendered);
        }
        cached
            .as_ref()
            .and_then(|text| text.lines.get(row_idx))
            .cloned()
            .unwrap_or_else(|| expand_tabs_in_line(Line::raw(self.raw_line(row_idx).to_owned())))
    }

    fn clear_cache(&self) {
        *self.cached.borrow_mut() = None;
    }
}

/// Code info bar: left spans + right text, right-aligned at render time.
#[derive(Debug, Clone)]
pub struct CodeInfoLine {
    left: Vec<(String, Style)>,
    right: String,
    style: Style,
}

#[derive(Debug)]
struct TableRenderCache {
    viewport_width: usize,
    lines: Vec<Line<'static>>,
}

/// Raw table data with rendered rows shared by every logical table line.
#[derive(Debug)]
pub struct MarkdownTable {
    source: Table,
    cache: std::cell::RefCell<Option<TableRenderCache>>,
}

impl MarkdownTable {
    fn new(source: Table, viewport_width: usize, lines: Vec<Line<'static>>) -> Self {
        Self {
            source,
            cache: std::cell::RefCell::new(Some(TableRenderCache {
                viewport_width,
                lines,
            })),
        }
    }

    fn line(&self, row_idx: usize, theme: &Theme, viewport_width: usize) -> Line<'static> {
        let mut cache = self.cache.borrow_mut();
        if cache
            .as_ref()
            .is_none_or(|cached| cached.viewport_width != viewport_width)
        {
            *cache = Some(TableRenderCache {
                viewport_width,
                lines: render_table(&self.source, theme, viewport_width),
            });
        }
        cache
            .as_ref()
            .and_then(|cached| cached.lines.get(row_idx))
            .cloned()
            .unwrap_or_else(|| Line::raw(""))
    }

    fn clear_cache(&self) {
        *self.cache.borrow_mut() = None;
    }
}

/// Element-specific content for one logical preview line.
///
/// Cross-cutting navigation and display metadata remains on [`LogicalLine`].
#[derive(Debug, Clone, Default)]
pub enum LogicalLineSource {
    Text(LazyText),
    ListItem(LazyText),
    Heading {
        level: u8,
        text: LazyText,
    },
    CodeInfo(CodeInfoLine),
    CodeBody(LazyText),
    Output(Text<'static>),
    /// One row of a raw HTML block highlighted and cached as a complete block.
    Html {
        block: Rc<MarkdownHtml>,
        row_idx: usize,
    },
    TableRow {
        /// Raw table data and responsive render cache shared by every row.
        table: Rc<MarkdownTable>,
        /// Which rendered row this `LogicalLine` represents.
        row_idx: usize,
        /// Initial content retained for search and copy before viewport painting.
        initial: Line<'static>,
    },
    /// A standalone image block rendered via ratatui_image.
    Image {
        alt: String,
        src: String,
    },
    ThematicBreak,
    #[default]
    Newline,
}

impl LogicalLineSource {
    fn lazy_text_mut(&mut self) -> Option<&mut LazyText> {
        match self {
            LogicalLineSource::Text(text)
            | LogicalLineSource::ListItem(text)
            | LogicalLineSource::CodeBody(text)
            | LogicalLineSource::Heading { text, .. } => Some(text),
            _ => None,
        }
    }

    fn lazy_text(&self) -> Option<&LazyText> {
        match self {
            LogicalLineSource::Text(text)
            | LogicalLineSource::ListItem(text)
            | LogicalLineSource::CodeBody(text)
            | LogicalLineSource::Heading { text, .. } => Some(text),
            _ => None,
        }
    }
}

/// A single *logical* line in the preview: one semantic markdown element.
///
/// `LogicalLine`s are produced once by [`MarkdownRenderer::render`] and cached in
/// [`Preview::logical_lines`](crate::apps::tui::preview::Preview::logical_lines).
/// They are **not** directly drawn; instead [`LogicalLine::render`]
/// lazily renders them into a [`Line`] each frame, and
/// [`Preview::rebuild_visual_lines`](crate::apps::tui::preview::Preview::rebuild_visual_lines)
/// optionally soft-wraps that `Line` into one or more
/// [`VisualLine`](crate::apps::tui::preview::VisualLine)s.
#[derive(Debug, Clone, Default)]
pub struct LogicalLine {
    pub source: LogicalLineSource,
    pub code_id: Option<CodeId>,
    pub is_block_start: bool,
    pub is_code_start: bool,
    /// Whether the associated code block is currently running (for CodeInfo lines).
    pub is_running: bool,
    /// Display prefixes that are prepended before the line content.
    ///
    /// Multiple prefixes can stack (for example, a blockquote marker followed by
    /// a list marker). Keeping them separate preserves each prefix's style.
    pub prefixes: Vec<Span<'static>>,
    /// Optional foreground color override for the gutter indicator (used by
    /// output lines to reflect task status).
    pub gutter_fg: Option<Color>,
}

impl LogicalLine {
    /// Creates an empty newline.
    pub fn newline(code_id: Option<CodeId>, is_block_start: bool) -> Self {
        Self {
            source: LogicalLineSource::Newline,
            code_id,
            is_block_start,
            ..Self::default()
        }
    }

    /// Creates a text line from inline markdown spans.
    pub fn text_lazy_spans(spans: Vec<InlineSpan>, is_block_start: bool) -> Self {
        Self {
            source: LogicalLineSource::Text(LazyText::from_spans(spans)),
            is_block_start,
            ..Self::default()
        }
    }

    /// Creates a list item from inline markdown spans with a styled prefix.
    pub fn list_item_spans(
        spans: Vec<InlineSpan>,
        prefix: Span<'static>,
        is_block_start: bool,
    ) -> Self {
        Self {
            source: LogicalLineSource::ListItem(LazyText::from_spans(spans)),
            is_block_start,
            prefixes: vec![prefix],
            ..Self::default()
        }
    }

    /// Creates a heading line from inline markdown spans.
    pub fn heading_lazy_spans(spans: Vec<InlineSpan>, level: u8) -> Self {
        Self {
            source: LogicalLineSource::Heading {
                level,
                text: LazyText::from_spans(spans),
            },
            is_block_start: true,
            ..Self::default()
        }
    }

    /// Creates a code info line (header showing code ID, language, status).
    pub fn code_info(
        left: Vec<(String, Style)>,
        right: String,
        style: Style,
        code_id: CodeId,
        is_start: bool,
        is_running: bool,
    ) -> Self {
        Self {
            source: LogicalLineSource::CodeInfo(CodeInfoLine { left, right, style }),
            code_id: Some(code_id),
            is_block_start: is_start,
            is_code_start: is_start,
            is_running,
            ..Self::default()
        }
    }

    /// Creates a code body line with raw content that needs lazy highlighting.
    pub fn code_body(raw: impl Into<String>, language: impl Into<String>, code_id: CodeId) -> Self {
        Self {
            source: LogicalLineSource::CodeBody(LazyText::new(raw, language)),
            code_id: Some(code_id),
            ..Self::default()
        }
    }

    /// Creates an output line.
    pub fn output(content: impl Into<Text<'static>>, code_id: CodeId) -> Self {
        Self {
            source: LogicalLineSource::Output(content.into()),
            code_id: Some(code_id),
            ..Self::default()
        }
    }

    /// Creates one row of a raw HTML block.
    pub fn html(block: Rc<MarkdownHtml>, row_idx: usize, is_block_start: bool) -> Self {
        Self {
            source: LogicalLineSource::Html { block, row_idx },
            is_block_start,
            ..Self::default()
        }
    }

    /// Creates a table row or border line.
    pub fn table(table: Rc<MarkdownTable>, row_idx: usize, initial: Line<'static>) -> Self {
        Self {
            source: LogicalLineSource::TableRow {
                table,
                row_idx,
                initial,
            },
            ..Self::default()
        }
    }

    /// Creates a thematic break (horizontal rule).
    pub fn thematic_break() -> Self {
        Self {
            source: LogicalLineSource::ThematicBreak,
            is_block_start: true,
            ..Self::default()
        }
    }

    /// Creates the decorative bottom rule used by H1 and H2 headings.
    fn heading_rule() -> Self {
        Self {
            source: LogicalLineSource::ThematicBreak,
            ..Self::default()
        }
    }

    pub fn into_lazy_text(mut self) -> Option<LazyText> {
        self.source.lazy_text_mut().map(std::mem::take)
    }

    pub fn lazy_text_mut(&mut self) -> Option<&mut LazyText> {
        self.source.lazy_text_mut()
    }

    fn lazy_text(&self) -> Option<&LazyText> {
        self.source.lazy_text()
    }

    pub fn html_block(&self) -> Option<&Rc<MarkdownHtml>> {
        match &self.source {
            LogicalLineSource::Html { block, .. } => Some(block),
            _ => None,
        }
    }

    pub fn reuse_html_block(&mut self, cached: &Rc<MarkdownHtml>) {
        if let LogicalLineSource::Html { block, .. } = &mut self.source {
            if block.source() == cached.source() {
                *block = Rc::clone(cached);
            }
        }
    }

    /// Clears cached content so the next render uses the current theme.
    pub fn clear_cache(&self) {
        match &self.source {
            LogicalLineSource::TableRow { table, .. } => table.clear_cache(),
            LogicalLineSource::Html { block, .. } => block.clear_cache(),
            _ => {
                if let Some(text) = self.lazy_text() {
                    *text.cached.borrow_mut() = None;
                }
            }
        }
    }

    pub fn prefix_width(&self) -> usize {
        self.prefixes
            .iter()
            .map(|prefix| prefix.content.chars().count())
            .sum()
    }

    #[inline]
    pub fn heading_level(&self) -> Option<u8> {
        match self.source {
            LogicalLineSource::Heading { level, .. } => Some(level),
            _ => None,
        }
    }

    #[inline]
    pub fn is_code_info(&self) -> bool {
        matches!(self.source, LogicalLineSource::CodeInfo(_))
    }

    #[inline]
    pub fn is_code_body(&self) -> bool {
        matches!(self.source, LogicalLineSource::CodeBody(_))
    }

    #[inline]
    pub fn is_output(&self) -> bool {
        matches!(self.source, LogicalLineSource::Output(_))
    }

    #[inline]
    pub fn has_code_gutter(&self) -> bool {
        matches!(
            self.source,
            LogicalLineSource::CodeInfo(_)
                | LogicalLineSource::CodeBody(_)
                | LogicalLineSource::Output(_)
        )
    }

    #[inline]
    pub fn is_table(&self) -> bool {
        matches!(self.source, LogicalLineSource::TableRow { .. })
    }

    #[inline]
    pub fn is_image(&self) -> bool {
        matches!(self.source, LogicalLineSource::Image { .. })
    }

    /// Returns the image source path for image lines.
    pub fn image_src(&self) -> Option<&str> {
        match &self.source {
            LogicalLineSource::Image { src, .. } => Some(src),
            _ => None,
        }
    }

    /// Returns `true` for lines that must not be wrapped (already sized/formatted).
    #[inline]
    pub fn is_unwrappable(&self) -> bool {
        self.is_table() || self.is_output() || self.is_code_info() || self.is_image()
    }

    /// Returns text content, preferring raw text if available.
    pub fn text_content(&self) -> String {
        match &self.source {
            LogicalLineSource::Text(text)
            | LogicalLineSource::ListItem(text)
            | LogicalLineSource::CodeBody(text)
            | LogicalLineSource::Heading { text, .. } => text.text.clone(),
            LogicalLineSource::CodeInfo(info) => {
                let left: String = info.left.iter().map(|(text, _)| text.as_str()).collect();
                format!("{left} {}", info.right.trim_end())
            }
            LogicalLineSource::Output(text) => text.to_string(),
            LogicalLineSource::Html { block, row_idx } => block.raw_line(*row_idx).to_owned(),
            LogicalLineSource::TableRow { initial, .. } => initial.to_string(),
            LogicalLineSource::Image { alt, .. } => alt.clone(),
            LogicalLineSource::ThematicBreak | LogicalLineSource::Newline => String::new(),
        }
    }

    /// Lazily renders this line into a styled ratatui [`Line`] for display.
    ///
    /// Synthetic lines (thematic break, table rows) bypass content styling and
    /// gutters, but still receive display prefixes such as blockquote markers.
    /// All other lines are produced in three passes:
    /// 1. `render_content`: syntax highlighting and raw text styling.
    /// 2. `apply_content_style`: state-driven content appearance (code
    ///    background, active-code colors).
    /// 3. `apply_chrome`: display prefixes and the code gutter.
    ///
    /// Called once per frame by the preview pane.
    pub fn render(&self, ctx: &RenderContext<'_>) -> Line<'static> {
        self.render_with(ctx, true)
    }

    /// Produces text-identical output without invoking syntax highlighting.
    ///
    /// Layout and painted output must contain the same characters in the same
    /// order. Styles may differ, but width and wrapping must not.
    pub fn render_plain(&self, ctx: &RenderContext<'_>) -> Line<'static> {
        self.render_with(ctx, false)
    }

    /// Populates the content cache without applying viewport-dependent paint.
    pub fn ensure_rendered(&self, ctx: &RenderContext<'_>) -> bool {
        if let Some(text) = self.lazy_text() {
            if text.cached.borrow().is_none() {
                drop(self.render_content(ctx));
                return true;
            }
        }
        false
    }

    fn render_with(&self, ctx: &RenderContext<'_>, highlight: bool) -> Line<'static> {
        if let Some(mut line) = self.render_synthetic(ctx) {
            self.apply_prefixes(&mut line);
            return line;
        }

        let is_active = ctx.active_code_id.is_some() && ctx.active_code_id == self.code_id;
        let mut line = if highlight {
            self.render_content(ctx)
        } else {
            self.render_plain_content(ctx)
        };
        self.apply_content_style(&mut line, is_active, ctx);
        self.apply_chrome(&mut line, is_active, ctx);
        line
    }

    /// State-driven styling for the content area: code background and
    /// active-code colors for info lines and dividers.
    fn apply_content_style(
        &self,
        line: &mut Line<'static>,
        is_active: bool,
        ctx: &RenderContext<'_>,
    ) {
        if self.is_code_body() {
            let bg = ctx.theme.code_style();
            line.style = line.style.patch(bg);
            for span in &mut line.spans {
                span.style = span.style.patch(bg);
            }
        }

        if is_active && self.is_code_info() {
            if let Some(first_span) = line.spans.first_mut() {
                first_span.style = first_span.style.patch(ctx.theme.active_fg_style());
            }
        }
    }

    /// Display-only second pass: applies prefixes and the code gutter.
    fn apply_prefixes(&self, line: &mut Line<'static>) {
        let mut spans = self.prefixes.to_vec();
        spans.append(&mut line.spans);
        line.spans = spans;
    }

    fn apply_chrome(&self, line: &mut Line<'static>, is_active: bool, ctx: &RenderContext<'_>) {
        self.add_gutter(line, is_active, ctx);
        self.apply_prefixes(line);
    }

    /// Handles synthetic line types (thematic break, table) that bypass content
    /// rendering; `render` adds display prefixes afterward.
    fn render_synthetic(&self, ctx: &RenderContext<'_>) -> Option<Line<'static>> {
        match &self.source {
            LogicalLineSource::ThematicBreak => {
                let width = ctx
                    .viewport_width
                    .saturating_sub(PREVIEW_FRAME_OVERHEAD + self.prefix_width());
                Some(Line::from(Span::styled(
                    "─".repeat(width),
                    ctx.theme.rule_style(),
                )))
            }
            LogicalLineSource::TableRow { table, row_idx, .. } => {
                let width = ctx
                    .viewport_width
                    .saturating_sub(PREVIEW_FRAME_OVERHEAD)
                    .saturating_sub(self.prefix_width())
                    .max(1);
                Some(table.line(*row_idx, ctx.theme, width))
            }
            _ => None,
        }
    }

    /// Renders text-identical content without syntax highlighting.
    fn render_plain_content(&self, ctx: &RenderContext<'_>) -> Line<'static> {
        match &self.source {
            LogicalLineSource::Html { block, row_idx } => {
                expand_tabs_in_line(Line::raw(block.raw_line(*row_idx).to_owned()))
            }
            _ if self.lazy_text().is_some() => {
                let text = self.lazy_text().expect("checked lazy text");
                expand_tabs_in_line(Line::raw(text.text.clone()))
            }
            _ => self.render_content(ctx),
        }
    }

    /// Renders the main line content, caching semantic Markdown or syntax-highlighted code.
    fn render_content(&self, ctx: &RenderContext<'_>) -> Line<'static> {
        match &self.source {
            LogicalLineSource::Text(text)
            | LogicalLineSource::ListItem(text)
            | LogicalLineSource::CodeBody(text)
            | LogicalLineSource::Heading { text, .. } => {
                let mut cache = text.cached.borrow_mut();
                if let Some(hit) = &*cache {
                    return hit
                        .lines
                        .first()
                        .cloned()
                        .unwrap_or_else(|| expand_tabs_in_line(Line::raw(text.text.clone())));
                }

                let mut rendered = if text.spans.is_empty() {
                    ctx.theme.highlight(&text.text, &text.language)
                } else {
                    let line = match self.source {
                        LogicalLineSource::Heading { .. } => {
                            render_heading_spans(&text.spans, ctx.theme)
                        }
                        _ => render_inline_spans(
                            &text.spans,
                            ctx.theme.markdown_text_style(),
                            ctx.theme,
                        ),
                    };
                    Text::from(line)
                };
                for line in &mut rendered.lines {
                    *line = expand_tabs_in_line(std::mem::take(line));
                }
                let line = rendered
                    .lines
                    .first()
                    .cloned()
                    .unwrap_or_else(|| expand_tabs_in_line(Line::raw(text.text.clone())));
                *cache = Some(rendered);
                line
            }
            LogicalLineSource::Html { block, row_idx } => block.line(*row_idx, ctx.theme),
            LogicalLineSource::CodeInfo(info) => {
                let prefix_width = self.prefix_width();
                let wrap_width = ctx
                    .viewport_width
                    .saturating_sub(crate::apps::config::PREVIEW_CODE_WRAP_OVERHEAD + prefix_width)
                    .max(1);
                let mut spans: Vec<Span<'static>> = info
                    .left
                    .iter()
                    .map(|(text, style)| Span::styled(text.clone(), *style))
                    .collect();
                if self.is_running {
                    spans.push(Span::styled(
                        format!(" {}", ctx.spinner_char),
                        ctx.theme.active_fg_style(),
                    ));
                }
                let left_chars: usize = spans.iter().map(|span| span.content.chars().count()).sum();
                let right_chars = info.right.chars().count();
                let gap = wrap_width.saturating_sub(left_chars + right_chars).max(1);
                spans.push(Span::styled(" ".repeat(gap), info.style));
                spans.push(Span::styled(info.right.clone(), info.style));
                Line::from(spans).style(info.style)
            }
            LogicalLineSource::Output(text) => {
                text.lines.first().cloned().unwrap_or_else(|| Line::raw(""))
            }
            LogicalLineSource::TableRow { initial, .. } => initial.clone(),
            LogicalLineSource::Image { alt, .. } => {
                Line::from(alt.clone()).style(ctx.theme.image_style())
            }
            LogicalLineSource::ThematicBreak | LogicalLineSource::Newline => Line::raw(""),
        }
    }

    /// Adds a gutter indicator for highlightable lines.
    fn add_gutter(&self, line: &mut Line<'static>, is_active: bool, ctx: &RenderContext<'_>) {
        if !self.has_code_gutter() {
            return;
        }
        apply_gutter(
            line,
            self.is_output(),
            is_active,
            ctx.theme,
            self.gutter_fg,
            ctx.prefer_status_gutter == self.code_id,
            self.is_running,
        );
    }
}

/// Prepends gutter "▎". Priority: running > active > status > inactive.
pub fn apply_gutter(
    line: &mut Line<'static>,
    is_unwrappable: bool,
    is_active: bool,
    theme: &Theme,
    gutter_fg: Option<Color>,
    prefer_status_gutter: bool,
    is_running: bool,
) {
    let gs = gutter_style(
        line.style.bg,
        is_unwrappable,
        is_active,
        theme,
        gutter_fg,
        prefer_status_gutter,
        is_running,
    );
    let gutter = Span::styled("\u{258E}", gs);
    let has_content = !line.spans.is_empty();
    line.spans.insert(0, gutter);
    if has_content {
        line.spans.insert(1, Span::from(" "));
    }
}

/// Computes gutter color for both live (code-info) and cached (code-body) paths.
/// Priority: running > active > success/error (when selected) > inactive.
pub fn gutter_style(
    bg: Option<Color>,
    is_unwrappable: bool,
    is_active: bool,
    theme: &Theme,
    gutter_fg: Option<Color>,
    prefer_status_gutter: bool,
    is_running: bool,
) -> Style {
    let mut style = if let Some(fg) = gutter_fg {
        if is_running || !is_active || prefer_status_gutter {
            Style::default().fg(fg)
        } else {
            theme.active_fg_style()
        }
    } else if is_active {
        theme.active_fg_style()
    } else {
        match is_unwrappable {
            true => Style::default(),
            false => theme.inactive_style(),
        }
    };
    if let Some(bg) = bg {
        style = style.bg(bg);
    }
    style
}

/// Renders a markdown table as box-drawing lines.
///
/// Column widths are capped so the total table width does not exceed
/// `viewport_width`.  Content is truncated with "…" when necessary.
fn render_table(table: &Table, theme: &Theme, viewport_width: usize) -> Vec<Line<'static>> {
    let content_fg = theme.foreground;
    let line_fg = theme.info_background;
    if table.headers.is_empty() {
        return vec![];
    }

    let n = table.headers.len();
    let min_col_width = 3usize; // enough for "…"
                                // Table frame overhead: left border + right border + n separators between columns.
    let frame_overhead = 3 * n + 1;

    let natural_widths: Vec<usize> = (0..n)
        .map(|i| {
            let header_w = table.headers[i].char_len();
            let cell_w = table
                .rows
                .iter()
                .filter_map(|r| r.get(i))
                .map(TableCell::char_len)
                .max()
                .unwrap_or(0);
            header_w.max(cell_w)
        })
        .collect();

    let natural_total = natural_widths.iter().sum::<usize>() + frame_overhead;

    // Cap column widths so the table fits within the viewport.
    let col_widths: Vec<usize> = if natural_total <= viewport_width {
        natural_widths
    } else {
        let available = viewport_width.saturating_sub(frame_overhead);
        let min_total = min_col_width * n;
        if available <= min_total {
            //_viewport is too narrow. Clamp everything to the minimum.
            vec![min_col_width; n]
        } else {
            let excess = natural_widths.iter().sum::<usize>() - available;
            let reducible: usize = natural_widths
                .iter()
                .map(|&w| w.saturating_sub(min_col_width))
                .sum();
            natural_widths
                .iter()
                .map(|&w| {
                    let reducible_here = w.saturating_sub(min_col_width);
                    if reducible == 0 {
                        min_col_width
                    } else if let Some(reduction) =
                        (reducible_here * excess + reducible / 2).checked_div(reducible)
                    {
                        w.saturating_sub(reduction).max(min_col_width)
                    } else {
                        min_col_width
                    }
                })
                .collect()
        }
    };

    let h_border = |left, mid, right| {
        format!(
            "{}{}{}",
            left,
            col_widths
                .iter()
                .map(|w| "─".repeat(w + 2))
                .collect::<Vec<_>>()
                .join(mid),
            right,
        )
    };

    let make_row = |cells: &[TableCell], base_style: Style| {
        let mut spans = vec![Span::raw("│").fg(line_fg)];
        for (i, cell) in cells.iter().enumerate() {
            let align = table.alignments.get(i).copied().unwrap_or(Alignment::Left);
            spans.push(Span::styled(" ", base_style));
            spans.extend(render_table_cell(
                cell,
                col_widths[i],
                align,
                base_style,
                theme,
            ));
            spans.push(Span::styled(" ", base_style));
            spans.push(Span::raw("│").fg(line_fg));
        }
        Line::from(spans)
    };

    let bold = Style::default().fg(content_fg).add_modifier(Modifier::BOLD);
    let normal = Style::default().fg(content_fg);
    let line = Style::default().fg(line_fg);

    let mut lines = vec![
        Line::raw(h_border("┌", "┬", "┐")).style(line),
        make_row(&table.headers, bold),
        Line::raw(h_border("├", "┼", "┤")).style(line),
    ];
    for row in &table.rows {
        lines.push(make_row(row, normal));
    }
    lines.push(Line::raw(h_border("└", "┴", "┘")).style(line));
    lines
}

fn render_table_cell(
    cell: &TableCell,
    width: usize,
    alignment: Alignment,
    base_style: Style,
    theme: &Theme,
) -> Vec<Span<'static>> {
    if width == 0 {
        return Vec::new();
    }

    let visible = cell.char_len();
    let mut line = render_inline_spans(&cell.spans, base_style, theme);
    let content_width = visible.min(width);
    if visible > width {
        if width == 1 {
            line = Line::from(Span::styled("…", base_style));
        } else {
            line = slice_line(&line, 0..width - 1);
            line.spans.push(Span::styled("…", base_style));
        }
    }

    let padding = width.saturating_sub(content_width);
    let (left, right) = match alignment {
        Alignment::Right => (padding, 0),
        Alignment::Center => (padding / 2, padding - padding / 2),
        Alignment::Left | Alignment::None => (0, padding),
    };

    let mut spans = Vec::with_capacity(line.spans.len() + 2);
    if left > 0 {
        spans.push(Span::styled(" ".repeat(left), base_style));
    }
    spans.extend(line.spans);
    if right > 0 {
        spans.push(Span::styled(" ".repeat(right), base_style));
    }
    spans
}

/// Render-time context for code-block snap-to-heading/paragraph.
#[derive(Default)]
struct SnapContext {
    /// Index of the heading LogicalLine that precedes the next code block.
    title_line: Option<usize>,
    /// Index of the first paragraph LogicalLine that precedes the next code block.
    description_line: Option<usize>,
}

impl SnapContext {
    /// Consumes the best snap target. Prefers title, falls back to description.
    /// Clears both after returning, so adjacent code blocks don't reuse context.
    fn take_target(&mut self) -> Option<usize> {
        let target = self.title_line.or(self.description_line);
        self.title_line = None;
        self.description_line = None;
        target
    }
}

#[derive(Default)]
struct RenderState {
    /// Snap targets are scoped separately from visual nesting.
    snap: SnapContext,
    /// Current blockquote nesting depth. Each level adds a display-only "> ".
    quote_depth: usize,
    /// Extra display width before code content, keyed by code block.
    code_prefix_overhead: HashMap<CodeId, usize>,
}

pub struct RenderedMarkdown {
    pub lines: Vec<LogicalLine>,
    pub code_prefix_overhead: HashMap<CodeId, usize>,
}

/// From AST nodes to ratatui `Text` lines.
pub struct MarkdownRenderer<'a> {
    theme: &'a Theme,
    outputs: &'a HashMap<u32, Task>,
    codes: &'a Codes,
    inline_max_lines: usize,
    viewport_width: usize,
}

impl<'a> MarkdownRenderer<'a> {
    pub fn new(
        theme: &'a Theme,
        outputs: &'a HashMap<u32, Task>,
        codes: &'a Codes,
        inline_max_lines: usize,
        viewport_width: usize,
    ) -> Self {
        Self {
            theme,
            outputs,
            codes,
            inline_max_lines,
            viewport_width,
        }
    }

    pub fn render(&self, nodes: &[upmd_parser::nodes::Node]) -> RenderedMarkdown {
        let mut lines = Vec::new();
        let mut state = RenderState::default();
        for node in nodes {
            self.push_node(node, &mut lines, &mut state);
        }
        RenderedMarkdown {
            lines,
            code_prefix_overhead: state.code_prefix_overhead,
        }
    }

    fn quote_prefix_span(&self) -> Span<'static> {
        // Quote markers sit outside code backgrounds. Pinning the background
        // avoids inheriting the highlighted/code block background that is added
        // later during LogicalLine::render.
        Span::styled(
            "> ",
            Style::default()
                .fg(self.theme.muted)
                .bg(self.theme.background),
        )
    }

    fn quote_prefix_width(depth: usize) -> usize {
        depth * 2
    }

    fn push_line(&self, lines: &mut Vec<LogicalLine>, mut line: LogicalLine, quote_depth: usize) {
        // Quote prefixes are outer chrome. Insert before any existing list/task
        // prefix so rendering preserves markdown order: "> • item", not
        // "• > item".
        for _ in 0..quote_depth {
            line.prefixes.insert(0, self.quote_prefix_span());
        }
        lines.push(line);
    }

    fn push_node(
        &self,
        node: &upmd_parser::nodes::Node,
        lines: &mut Vec<LogicalLine>,
        state: &mut RenderState,
    ) {
        use upmd_parser::nodes::Node;
        match node {
            Node::HtmlBlock(html) => {
                let block = Rc::new(MarkdownHtml::new(html.clone()));
                for row_idx in 0..block.len() {
                    self.push_line(
                        lines,
                        LogicalLine::html(Rc::clone(&block), row_idx, row_idx == 0),
                        state.quote_depth,
                    );
                }
                self.push_line(lines, LogicalLine::newline(None, false), state.quote_depth);
            }
            Node::Text(t) => {
                self.push_highlighted_lines(t, lines, true, state.quote_depth);
            }
            Node::Paragraph(t) => {
                if let Some(idx) = self.push_highlighted_lines(t, lines, true, state.quote_depth) {
                    state.snap.description_line = Some(idx);
                }
            }
            Node::BlockQuote(children) => {
                // Blockquotes are visual nesting, but title/description snap
                // context is scoped: a quoted paragraph should not become the
                // snap target for a following non-quoted code block, and an
                // outer paragraph should not snap to quoted code.
                let parent_snap = std::mem::take(&mut state.snap);
                state.quote_depth += 1;
                for child in children {
                    self.push_node(child, lines, state);
                }
                state.quote_depth = state.quote_depth.saturating_sub(1);
                state.snap = parent_snap;
            }
            Node::Heading { text: t, level } => {
                let line_idx = lines.len();
                let prefix = "#".repeat(*level as usize);
                let mut content = t.clone();
                if let Some(first) = content.first_mut() {
                    first.text = first.text.trim_start_matches('#').trim_start().to_string();
                }
                content.insert(
                    0,
                    InlineSpan {
                        text: format!("{prefix} "),
                        style: Vec::new(),
                    },
                );
                self.push_line(
                    lines,
                    LogicalLine::heading_lazy_spans(content, *level),
                    state.quote_depth,
                );
                if *level <= 2 {
                    self.push_line(lines, LogicalLine::heading_rule(), state.quote_depth);
                }
                state.snap.title_line = Some(line_idx);
            }
            Node::List(items) => self.push_list(items, lines, state),
            Node::Code(code_id) => {
                let code = self
                    .codes
                    .by_id(*code_id)
                    .expect("CodeId must resolve to a Code in Document.codes");
                let is_start = match state.snap.take_target() {
                    Some(idx) => {
                        if let Some(line) = lines.get_mut(idx) {
                            line.code_id = Some(code.id);
                            line.is_code_start = true;
                            line.is_block_start = true;
                            true
                        } else {
                            false
                        }
                    }
                    None => false,
                };
                let quote_overhead = Self::quote_prefix_width(state.quote_depth);
                if quote_overhead > 0 {
                    state.code_prefix_overhead.insert(code.id, quote_overhead);
                }
                // Compute block-wide gutter color from task status.
                let is_running = self.outputs.get(&code.id).is_some_and(|t| t.running());
                let gutter_fg = self.outputs.get(&code.id).and_then(|buffer| {
                    use crate::apps::task::TaskStatus;
                    match buffer.status() {
                        TaskStatus::Running => Some(self.theme.warning),
                        TaskStatus::Success => Some(self.theme.success),
                        TaskStatus::Error => Some(self.theme.error),
                        TaskStatus::Idle => None,
                    }
                });
                let mut info = self.push_code_info(code, !is_start);
                info.gutter_fg = gutter_fg;
                self.push_line(lines, info, state.quote_depth);
                for mut line in self.push_code_body(code) {
                    line.gutter_fg = gutter_fg;
                    line.is_running = is_running;
                    self.push_line(lines, line, state.quote_depth);
                }
                for mut line in self.push_code_output(code) {
                    line.gutter_fg = gutter_fg;
                    line.is_running = is_running;
                    self.push_line(lines, line, state.quote_depth);
                }
                self.push_line(
                    lines,
                    LogicalLine::newline(Some(code.id), false),
                    state.quote_depth,
                );
            }
            Node::Table(table) => {
                let table_width = self
                    .viewport_width
                    .saturating_sub(PREVIEW_FRAME_OVERHEAD)
                    .saturating_sub(Self::quote_prefix_width(state.quote_depth))
                    .max(1);
                let rendered = render_table(table, self.theme, table_width);
                let row_count = rendered.len();
                let table = Rc::new(MarkdownTable::new(table.clone(), table_width, rendered));
                for row_idx in 0..row_count {
                    let initial = table.line(row_idx, self.theme, table_width);
                    self.push_line(
                        lines,
                        LogicalLine::table(Rc::clone(&table), row_idx, initial),
                        state.quote_depth,
                    );
                }
                self.push_line(lines, LogicalLine::newline(None, false), state.quote_depth);
            }
            Node::ThematicBreak => {
                self.push_line(lines, LogicalLine::thematic_break(), state.quote_depth);
                self.push_line(lines, LogicalLine::newline(None, false), state.quote_depth);
            }
            Node::Image { alt, src } => {
                let line = LogicalLine {
                    source: LogicalLineSource::Image {
                        alt: alt.clone(),
                        src: src.clone(),
                    },
                    is_block_start: true,
                    ..LogicalLine::default()
                };
                self.push_line(lines, line, state.quote_depth);
                self.push_line(lines, LogicalLine::newline(None, false), state.quote_depth);
            }
        }
    }

    fn push_highlighted_lines(
        &self,
        spans: &[InlineSpan],
        lines: &mut Vec<LogicalLine>,
        block_start: bool,
        quote_depth: usize,
    ) -> Option<usize> {
        let start_idx = lines.len();
        let mut first = block_start;
        let mut emitted = false;
        for line in split_span_lines(spans) {
            self.push_line(
                lines,
                LogicalLine::text_lazy_spans(line, first),
                quote_depth,
            );
            first = false;
            emitted = true;
        }
        self.push_line(lines, LogicalLine::newline(None, false), quote_depth);
        if emitted {
            Some(start_idx)
        } else {
            None
        }
    }

    fn push_list(
        &self,
        items: &[upmd_parser::nodes::ListItem],
        lines: &mut Vec<LogicalLine>,
        state: &mut RenderState,
    ) {
        for (i, item) in items.iter().enumerate() {
            let indent = " ".repeat(item.depth.saturating_sub(1) * 4);

            let (marker, color) = match &item.kind {
                ListKind::Bullet => ("• ".to_string(), self.theme.foreground),
                ListKind::Ordered(n) => (format!("{}. ", n), self.theme.foreground),
                ListKind::Task(status) => match status {
                    TaskStatus::Checked => ("󰱒  ".to_string(), self.theme.success),
                    TaskStatus::InProgress => ("󰡖  ".to_string(), self.theme.muted),
                    TaskStatus::Unchecked => ("󰄱  ".to_string(), self.theme.muted),
                },
            };

            let continuation = format!("{}{}", indent, " ".repeat(marker.chars().count()));

            for (line_idx, line) in split_span_lines(&item.text).into_iter().enumerate() {
                let prefix = if line_idx == 0 {
                    Span::styled(format!("{}{}", indent, marker), Style::default().fg(color))
                } else {
                    Span::raw(continuation.clone())
                };
                self.push_line(
                    lines,
                    LogicalLine::list_item_spans(line, prefix, i == 0 && line_idx == 0),
                    state.quote_depth,
                );
            }
            // Render nested children (code blocks, sub-lists, etc.) in the same
            // quote scope so blockquote chrome applies consistently.
            for child in &item.children {
                self.push_node(child, lines, state);
            }
        }
        // Skip trailing newline for nested lists to avoid blank lines between siblings.
        if items.first().is_some_and(|i| i.depth == 1) {
            self.push_line(lines, LogicalLine::newline(None, false), state.quote_depth);
        }
    }

    fn push_code_info(&self, code: &Code, is_start: bool) -> LogicalLine {
        let buffer = self.outputs.get(&code.id);
        let is_executed =
            |done: bool| buffer.is_some_and(|b| b.execution.is_some() && b.done == done);
        let is_running = is_executed(false);
        let is_done = buffer.is_some_and(|b| b.done);
        let language = upmd_runner::find_language(&code.language);
        let info_style = self.theme.code_info_style();

        // Left: "{id}" or "{id} {name}" with optional deps bracket.
        let left_text = if code.name.is_empty() {
            format!("{}", code.id)
        } else {
            format!("{} {}", code.id, code.name)
        };
        let mut left = vec![(left_text, info_style)];

        if code.deps.is_err() {
            left.push((" [invalid]".to_string(), info_style.fg(self.theme.error)));
        } else {
            for token in code.deps.segments() {
                let style = match token {
                    DepsToken::Punct(_) => info_style.patch(self.theme.muted_style()),
                    DepsToken::Name(_) => info_style,
                };
                left.push((token.text().to_string(), style));
            }
        }

        // Status symbol with its own color, appended to left spans.
        if is_done {
            let (sym, style) = match buffer.and_then(|b| b.exit_code) {
                Some(0) => (
                    crate::apps::config::SUCCESS_SYMBOL,
                    self.theme.success_style(),
                ),
                Some(_) | None => (crate::apps::config::ERROR_SYMBOL, self.theme.error_style()),
            };
            left.push((format!(" {sym}"), info_style.patch(style)));
        }

        // The language label is right-aligned at render time.
        let right = format!("{} ", language.name);
        LogicalLine::code_info(left, right, info_style, code.id, is_start, is_running)
    }

    fn push_code_body(&self, code: &Code) -> Vec<LogicalLine> {
        code.content
            .lines()
            .map(|line| LogicalLine::code_body(line.to_string(), &code.language, code.id))
            .collect()
    }

    fn push_code_output(&self, code: &Code) -> Vec<LogicalLine> {
        let Some(buffer) = self.outputs.get(&code.id) else {
            return Vec::new();
        };

        let show_cursor = !buffer.done;
        let is_tui = buffer.parser.is_alternate_screen();
        let styled = buffer.parser.inline_contents(show_cursor);
        if styled.lines.is_empty() {
            return Vec::new();
        }

        let total = styled.lines.len();
        let (start, end) = if is_tui {
            // Full-screen interactive TUI applications (like nvim, btop, opencode) run in the
            // alternate screen buffer. We render their entire viewport height so that their
            // status lines, headers, and full interactive UI elements are displayed correctly.
            (0, total)
        } else {
            // The parser scrollback has been synced to `inline_scroll` in
            // `scroll_inline_up`. Compute the visible window within the current
            // `rows`-tall screen.
            let rows_usize = buffer.parser.screen().size().0 as usize;
            let scrollback = buffer.parser.screen().scrollback();
            // `inline_scroll` may exceed scrollback when we've reached the oldest page
            // and are scrolling further to show the very first lines.
            let offset_in_screen = buffer.inline_scroll.saturating_sub(scrollback);
            let end = rows_usize.saturating_sub(offset_in_screen).min(total);
            let start = end.saturating_sub(self.inline_max_lines);
            (start, end)
        };
        let visible = &styled.lines[start..end];

        let mut out = Vec::with_capacity(visible.len());
        let bg = self.theme.output_background;
        for mut line in visible.iter().cloned() {
            let needs_bg = line.style.bg.is_none() || line.style.bg == Some(Color::Reset);
            if needs_bg {
                line.style.bg = Some(bg);
            }
            for span in &mut line.spans {
                let needs_bg = span.style.bg.is_none() || span.style.bg == Some(Color::Reset);
                if needs_bg {
                    span.style.bg = Some(bg);
                }
            }
            out.push(LogicalLine::output(line, code.id));
        }
        out
    }
}

/// Highlights all occurrences of `term` in a single `Line`.
pub fn highlight_line(line: Line<'static>, term: &str, highlight_style: Style) -> Line<'static> {
    let ranges = highlight_ranges(&line.to_string(), term);

    if ranges.is_empty() {
        return line;
    }

    let mut new_spans = Vec::new();
    let mut offset = 0;

    for span in &line.spans {
        let span_len = span.content.chars().count();
        let span_start = offset;
        let mut cursor = 0;

        for r in &ranges {
            let rel_start = r.start.saturating_sub(span_start).min(span_len);
            let rel_end = r.end.saturating_sub(span_start).min(span_len);
            if rel_start >= rel_end || rel_end <= cursor {
                continue;
            }
            if rel_start > cursor {
                new_spans.push(Span::styled(
                    span.content
                        .chars()
                        .skip(cursor)
                        .take(rel_start - cursor)
                        .collect::<String>(),
                    span.style,
                ));
            }
            new_spans.push(Span::styled(
                span.content
                    .chars()
                    .skip(rel_start)
                    .take(rel_end - rel_start)
                    .collect::<String>(),
                highlight_style,
            ));
            cursor = rel_end;
        }
        if cursor < span_len {
            new_spans.push(Span::styled(
                span.content.chars().skip(cursor).collect::<String>(),
                span.style,
            ));
        }
        offset += span_len;
    }

    Line::from(new_spans)
        .style(line.style)
        .alignment(line.alignment.unwrap_or_default())
}

fn highlight_ranges(text: &str, term: &str) -> Vec<std::ops::Range<usize>> {
    if term.is_empty() {
        return vec![];
    }

    let mut folded = String::new();
    let mut folded_to_original = Vec::new();
    for (original_idx, ch) in text.chars().enumerate() {
        for lower in ch.to_lowercase() {
            folded.push(lower);
            folded_to_original.push(original_idx);
        }
    }

    let folded_term = term.to_lowercase();
    folded
        .match_indices(folded_term.as_str())
        .filter_map(|(byte_start, matched)| {
            let char_start = folded[..byte_start].chars().count();
            let char_end = char_start + matched.chars().count();
            let original_start = *folded_to_original.get(char_start)?;
            let original_end = *folded_to_original.get(char_end.saturating_sub(1))? + 1;
            Some(original_start..original_end)
        })
        .collect()
}

/// Expands raw tab characters for display while preserving the source text in
/// [`LazyText`].
///
/// Ratatui renders text as terminal cells; raw `\t` is a control character that
/// terminals interpret as cursor movement. Expanding tabs after syntax
/// highlighting keeps logical text/source copies unchanged while ensuring the
/// displayed spans contain only printable cells.
fn expand_tabs_in_line(mut line: Line<'static>) -> Line<'static> {
    const TAB_WIDTH: usize = 4;
    let mut col = 0usize;
    let mut spans = Vec::with_capacity(line.spans.len());

    for span in line.spans {
        let mut text = String::with_capacity(span.content.len());
        for ch in span.content.chars() {
            if ch == '\t' {
                let spaces = TAB_WIDTH - (col % TAB_WIDTH);
                text.extend(std::iter::repeat_n(' ', spaces));
                col += spaces;
            } else {
                text.push(ch);
                col += ch.width().unwrap_or(0);
            }
        }
        spans.push(Span::styled(text, span.style));
    }

    line.spans = spans;
    line
}

/// Splits inline spans on `\n` into one group of spans per line, mirroring
/// `str::lines()` (trailing empty segments are dropped).
fn split_span_lines(spans: &[InlineSpan]) -> Vec<Vec<InlineSpan>> {
    let mut lines = Vec::new();
    let mut current = Vec::new();
    for span in spans {
        let mut text = span.text.as_str();
        loop {
            match text.find('\n') {
                Some(idx) => {
                    let (head, tail) = text.split_at(idx);
                    if !head.is_empty() {
                        current.push(InlineSpan {
                            text: head.to_string(),
                            style: span.style.clone(),
                        });
                    }
                    lines.push(std::mem::take(&mut current));
                    text = &tail[1..];
                }
                None => {
                    if !text.is_empty() {
                        current.push(InlineSpan {
                            text: text.to_string(),
                            style: span.style.clone(),
                        });
                    }
                    break;
                }
            }
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

/// Renders parsed inline Markdown spans directly from their semantic styles.
fn render_inline_spans(spans: &[InlineSpan], base_style: Style, theme: &Theme) -> Line<'static> {
    let rendered = spans
        .iter()
        .flat_map(|span| render_inline_span(span, base_style, theme))
        .collect::<Vec<_>>();
    Line::from(rendered).style(base_style)
}

/// Preserves Syntect's distinct styles for the heading marker and heading text.
fn render_heading_spans(spans: &[InlineSpan], theme: &Theme) -> Line<'static> {
    let heading_style = theme.markdown_heading_style();
    let rendered = spans
        .iter()
        .enumerate()
        .flat_map(|(index, span)| {
            let base_style = if index == 0 && span.text.starts_with('#') {
                theme.markdown_heading_marker_style()
            } else {
                heading_style
            };
            render_inline_span(span, base_style, theme)
        })
        .collect::<Vec<_>>();
    Line::from(rendered).style(heading_style)
}

fn render_inline_span(span: &InlineSpan, base_style: Style, theme: &Theme) -> Vec<Span<'static>> {
    if span.style.iter().any(|s| matches!(s, InlineStyle::HtmlTag)) {
        return render_html_tag(span, base_style, theme);
    }
    let style = span.style.iter().fold(base_style, |style, inline| {
        style.patch(inline_style(theme, inline))
    });
    vec![Span::styled(span.text.clone(), style)]
}

/// Highlights an inline HTML tag with the HTML syntax.
fn render_html_tag(span: &InlineSpan, base_style: Style, theme: &Theme) -> Vec<Span<'static>> {
    let rendered = theme.highlight(&span.text, "html");
    let mut spans: Vec<Span<'static>> = rendered
        .lines
        .into_iter()
        .flat_map(|line| line.spans)
        .collect();
    if spans.is_empty() {
        spans.push(Span::styled(span.text.clone(), base_style));
    }
    spans
}

/// Maps an [`InlineStyle`] to the ratatui [`Style`] patched onto a semantic
/// Markdown span. Nested modifiers combine; semantic colors override the base.
fn inline_style(theme: &Theme, style: &InlineStyle) -> Style {
    match style {
        InlineStyle::Bold => Style::default().add_modifier(Modifier::BOLD),
        InlineStyle::Italic => Style::default().add_modifier(Modifier::ITALIC),
        InlineStyle::Strikethrough => Style::default().add_modifier(Modifier::CROSSED_OUT),
        InlineStyle::InlineCode => theme.inline_code_style(),
        InlineStyle::Link { .. } => theme.link_style(),
        InlineStyle::Image { .. } => theme.image_style(),
        InlineStyle::HtmlTag => Style::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apps::task::Task;
    use crate::apps::tui::testutil::ansi_line;
    use insta::assert_snapshot;
    use ratatui::style::Color;
    use std::collections::HashMap;
    use upmd_parser::Parser;

    fn test_theme() -> Theme {
        Theme::new("base16-ocean.dark", false)
    }

    fn test_ctx(theme: &Theme, width: usize) -> RenderContext<'_> {
        RenderContext {
            theme,
            active_code_id: None,
            prefer_status_gutter: None,
            spinner_char: ' ',
            viewport_width: width,
        }
    }

    fn render_markdown(markdown: &str) -> RenderedMarkdown {
        let doc = upmd_parser::new().parse(markdown);
        let nodes = &doc.nodes;
        let codes = &doc.codes;
        let theme = test_theme();
        let outputs = HashMap::new();
        let renderer = MarkdownRenderer::new(&theme, &outputs, codes, 10, 80);
        renderer.render(nodes)
    }

    fn render_markdown_with_outputs(
        markdown: &str,
        outputs: &HashMap<CodeId, Task>,
    ) -> (Theme, RenderedMarkdown) {
        let doc = upmd_parser::new().parse(markdown);
        let nodes = &doc.nodes;
        let codes = &doc.codes;
        let theme = test_theme();
        let rendered = {
            let renderer = MarkdownRenderer::new(&theme, outputs, codes, 10, 80);
            renderer.render(nodes)
        };
        (theme, rendered)
    }

    fn render_nodes(markdown: &str) -> Vec<LogicalLine> {
        render_markdown(markdown).lines
    }

    fn source_label(line: &LogicalLine) -> String {
        match &line.source {
            LogicalLineSource::Text(_) => "Text".to_string(),
            LogicalLineSource::ListItem(_) => "ListItem".to_string(),
            LogicalLineSource::Heading { level, .. } => format!("Heading({level})"),
            LogicalLineSource::CodeInfo(_) => "CodeInfo".to_string(),
            LogicalLineSource::CodeBody(_) => "CodeBody".to_string(),
            LogicalLineSource::Output(_) => "Output".to_string(),
            LogicalLineSource::Html { .. } => "Html".to_string(),
            LogicalLineSource::TableRow { .. } => "Table".to_string(),
            LogicalLineSource::Image { .. } => "Image".to_string(),
            LogicalLineSource::ThematicBreak => "ThematicBreak".to_string(),
            LogicalLineSource::Newline => "Newline".to_string(),
        }
    }

    fn logical_line_summary(lines: &[LogicalLine]) -> String {
        lines
            .iter()
            .enumerate()
            .map(|(i, l)| {
                let text = l.text_content();
                let char_count = text.chars().count();
                let preview = if char_count > 60 {
                    let truncated: String = text.chars().take(57).collect();
                    format!("{}...", truncated)
                } else {
                    text
                };
                format!("{:2}: [{}] {}", i, source_label(l), preview)
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn code_start_lines(lines: &[LogicalLine]) -> Vec<(usize, String, Option<CodeId>, String)> {
        lines
            .iter()
            .enumerate()
            .filter(|(_, line)| line.is_code_start)
            .map(|(idx, line)| (idx, source_label(line), line.code_id, line.text_content()))
            .collect()
    }

    #[test]
    fn test_snap_heading_to_code_start() {
        let lines = render_nodes("# Title\n\n```bash\necho hi\n```");
        let starts = code_start_lines(&lines);

        assert_eq!(starts.len(), 1);
        assert_eq!(starts[0].0, 0);
        assert_eq!(starts[0].1, "Heading(1)");
        assert_eq!(starts[0].3, "# Title");
    }

    #[test]
    fn test_snap_paragraph_fallback_to_code_start() {
        let lines = render_nodes("Intro paragraph.\n\n```bash\necho hi\n```");
        let starts = code_start_lines(&lines);

        assert_eq!(starts.len(), 1);
        assert_eq!(starts[0].0, 0);
        assert_eq!(starts[0].1, "Text");
        assert_eq!(starts[0].3, "Intro paragraph.");
    }

    #[test]
    fn test_adjacent_code_blocks_do_not_reuse_snap_context() {
        let lines = render_nodes("# Title\n\n```bash\necho one\n```\n\n```bash\necho two\n```");
        let starts = code_start_lines(&lines);

        assert_eq!(starts.len(), 2);
        assert_eq!(starts[0].1, "Heading(1)");
        assert_eq!(starts[0].3, "# Title");
        assert_eq!(starts[1].1, "CodeInfo");
        assert_ne!(starts[0].2, starts[1].2);
    }

    #[test]
    fn test_blockquote_snap_context_does_not_leak_outward() {
        let lines = render_nodes("> quoted note\n\n```bash\necho hi\n```");
        let starts = code_start_lines(&lines);

        assert_eq!(starts.len(), 1);
        assert_eq!(starts[0].1, "CodeInfo");
        assert_eq!(lines[0].text_content(), "quoted note");
        assert!(lines[0].code_id.is_none());
    }

    #[test]
    fn test_blockquote_snap_context_does_not_leak_inward() {
        let lines = render_nodes("Intro paragraph.\n\n> ```bash\n> echo hi\n> ```");
        let starts = code_start_lines(&lines);

        assert_eq!(starts.len(), 1);
        assert_eq!(starts[0].1, "CodeInfo");
        assert_eq!(lines[0].text_content(), "Intro paragraph.");
        assert!(lines[0].code_id.is_none());
    }

    #[test]
    fn test_code_prefix_overhead_tracks_blockquote_depth() {
        for (name, markdown, expected) in [
            ("flat", "```bash\necho hi\n```", 0),
            ("single blockquote", "> ```bash\n> echo hi\n> ```", 2),
            ("nested blockquote", "> > ```bash\n> > echo hi\n> > ```", 4),
        ] {
            let rendered = render_markdown(markdown);
            let code_id = rendered
                .lines
                .iter()
                .find(|line| line.is_code_body())
                .and_then(|line| line.code_id)
                .unwrap_or_else(|| panic!("expected code body for {name}"));

            assert_eq!(
                rendered
                    .code_prefix_overhead
                    .get(&code_id)
                    .copied()
                    .unwrap_or(0),
                expected,
                "{name} code prefix overhead should match its quote depth"
            );
        }
    }

    #[test]
    fn test_blockquote_list_item_renders_quote_and_list_prefixes() {
        let theme = test_theme();
        let ctx = test_ctx(&theme, 80);
        let lines = render_nodes("> - quoted item");
        let list_item = lines
            .iter()
            .find(|line| matches!(line.source, LogicalLineSource::ListItem(_)))
            .expect("expected a list item inside the blockquote");

        assert_eq!(list_item.prefix_width(), 4);
        assert_eq!(list_item.render(&ctx).to_string(), "> • quoted item");
    }

    #[test]
    fn test_apply_gutter_prefers_active_color_without_prefer_status_gutter() {
        let theme = test_theme();
        let mut line = Line::from("done");

        apply_gutter(
            &mut line,
            false,
            true,
            &theme,
            Some(theme.success),
            false,
            false,
        );

        assert_eq!(
            line.spans.first().and_then(|span| span.style.fg),
            Some(theme.active)
        );
    }

    #[test]
    fn test_apply_gutter_prefers_status_color_with_prefer_status_gutter() {
        let theme = test_theme();
        let mut line = Line::from("done");

        apply_gutter(
            &mut line,
            false,
            true,
            &theme,
            Some(theme.success),
            true,
            false,
        );

        assert_eq!(
            line.spans.first().and_then(|span| span.style.fg),
            Some(theme.success)
        );
    }

    #[test]
    fn test_failed_start_code_info_renders_error_status_and_gutter() {
        let mut outputs = HashMap::new();
        let mut output = Task::new(80, 24, 500);
        output.done = true;
        output.exit_code = None;
        outputs.insert(1, output);

        let (theme, rendered) = render_markdown_with_outputs("```bash\necho hello\n```", &outputs);
        let code_info = rendered
            .lines
            .iter()
            .find(|line| line.is_code_info())
            .expect("expected a code info line");

        assert_eq!(code_info.gutter_fg, Some(theme.error));
        assert!(code_info
            .text_content()
            .contains(crate::apps::config::ERROR_SYMBOL));
    }

    #[test]
    fn test_render_headings() {
        let lines = render_nodes("# Hello\n\n## World\n\n### Rust");
        assert_snapshot!("headings", logical_line_summary(&lines));
    }

    #[test]
    fn test_render_paragraph() {
        let lines = render_nodes("This is a paragraph.\n\nWith a blank line.");
        assert_snapshot!("paragraph", logical_line_summary(&lines));
    }

    #[test]
    fn test_render_fenced_code_block() {
        let lines = render_nodes("```bash\necho hello\n```");
        assert_snapshot!("fenced_code", logical_line_summary(&lines));
    }

    #[test]
    fn test_render_code_with_language_attr() {
        let lines = render_nodes("```python [os:linux]\nprint('hi')\n```");
        assert_snapshot!("code_with_attr", logical_line_summary(&lines));
    }

    #[test]
    fn test_render_bullet_list() {
        let lines = render_nodes("- item one\n- item two\n- item three");
        assert_snapshot!("bullet_list", logical_line_summary(&lines));
    }

    #[test]
    fn test_render_ordered_list() {
        let lines = render_nodes("1. first\n2. second\n3. third");
        assert_snapshot!("ordered_list", logical_line_summary(&lines));
    }

    #[test]
    fn test_render_task_list() {
        let lines = render_nodes("- [ ] unchecked\n- [x] checked\n- [-] in progress");
        assert_snapshot!("task_list", logical_line_summary(&lines));
    }

    #[test]
    fn test_render_thematic_break() {
        let lines = render_nodes("above\n\n-----\n\nbelow");
        assert_snapshot!("thematic_break", logical_line_summary(&lines));
    }

    #[test]
    fn test_render_github_heading_rules() {
        for level in 1..=6 {
            let lines = render_nodes(&format!("{} Heading", "#".repeat(level)));
            assert_eq!(
                lines
                    .iter()
                    .any(|line| matches!(line.source, LogicalLineSource::ThematicBreak)),
                level <= 2,
                "H{level}"
            );
        }
    }

    #[test]
    fn test_render_blockquote() {
        let lines = render_nodes(
            "> quoted paragraph\n\
             > \n\
             > ```bash\n\
             > echo hi\n\
             > ```\n\
             \n\
             > - quoted item\n\
             > - nested quote\n\
             >   - deeper",
        );
        assert_snapshot!("blockquote", logical_line_summary(&lines));
    }

    #[test]
    fn test_render_html_tabs_preserved_logically_expanded_visually() {
        let lines = render_nodes("<pre>\n\tindented\n</pre>\n");
        let html: Vec<&LogicalLine> = lines
            .iter()
            .filter(|l| matches!(l.source, LogicalLineSource::Html { .. }))
            .collect();
        assert_eq!(html.len(), 3);
        assert_eq!(html[1].text_content(), "\tindented");

        let theme = test_theme();
        let ctx = test_ctx(&theme, 80);
        let painted = html[1].render(&ctx).to_string();
        assert!(!painted.contains('\t'));
        assert!(painted.contains("    indented"));
        assert_eq!(html[1].render_plain(&ctx).to_string(), painted);
    }

    #[test]
    fn test_render_html_styles() {
        let theme = test_theme();
        let ctx = test_ctx(&theme, 80);

        for (name, line) in [
            (
                "block",
                render_nodes("<div class=\"card\">\n</div>\n")
                    .iter()
                    .find(|l| matches!(l.source, LogicalLineSource::Html { .. }))
                    .unwrap()
                    .render(&ctx),
            ),
            (
                "inline",
                render_nodes("x <img src=\"a.png\"> y")
                    .iter()
                    .find(|l| matches!(l.source, LogicalLineSource::Text(_)))
                    .unwrap()
                    .render(&ctx),
            ),
        ] {
            let mut distinct: Vec<Color> = Vec::new();
            for span in &line.spans {
                if let Some(fg) = span.style.fg {
                    if !distinct.contains(&fg) {
                        distinct.push(fg);
                    }
                }
            }
            assert!(
                distinct.len() >= 2,
                "{name} HTML should use multiple syntax colors, got {distinct:?}"
            );
        }
    }

    #[test]
    fn test_render_inline_heading_styles() {
        let theme = test_theme();
        let ctx = test_ctx(&theme, 80);
        let lines = render_nodes("# **Bold** title");
        let heading = lines
            .iter()
            .find(|l| l.heading_level().is_some())
            .expect("expected a heading");
        let rendered = heading.render(&ctx);

        assert_snapshot!("inline_heading_styles", ansi_line(&rendered));
    }

    #[test]
    fn test_render_inline_list_item_styles() {
        let theme = test_theme();
        let ctx = test_ctx(&theme, 80);
        let lines = render_nodes("- **bold item**");
        let item = lines
            .iter()
            .find(|l| matches!(l.source, LogicalLineSource::ListItem(_)))
            .expect("expected a list item");
        let rendered = item.render(&ctx);

        assert_snapshot!("inline_list_item_styles", ansi_line(&rendered));
    }

    #[test]
    fn test_split_span_lines() {
        let spans = vec![
            InlineSpan {
                text: "line one\n".into(),
                style: vec![InlineStyle::Bold],
            },
            InlineSpan {
                text: "line two".into(),
                style: vec![],
            },
        ];
        let lines = split_span_lines(&spans);
        assert_eq!(lines.len(), 2);
        assert_eq!(inline_text(&lines[0]), "line one");
        assert_eq!(lines[0][0].style, vec![InlineStyle::Bold]);
        assert_eq!(inline_text(&lines[1]), "line two");

        // Empty input yields no lines.
        assert!(split_span_lines(&[]).is_empty());
        // Trailing newline drops the empty tail.
        assert_eq!(
            split_span_lines(&[InlineSpan {
                text: "a\n".into(),
                style: vec![]
            }])
            .len(),
            1
        );
    }

    #[test]
    fn test_render_inline_styles_snapshot() {
        let lines = render_nodes(
            "**bold** *italic* ~~strike~~ `code` [link](https://x.dev) ![alt text](img.png)\n\
             \n\
             ## Heading with **bold**\n\
             \n\
             - **bold item**\n\
             - *italic item*",
        );
        assert_snapshot!("inline_styles", logical_line_summary(&lines));
    }

    #[test]
    fn test_render_table() {
        let lines =
            render_nodes("| Name  | Age |\n|-------|-----|\n| Alice | 30  |\n| Bob   | 25  |");
        assert_snapshot!("table", logical_line_summary(&lines));
    }

    #[test]
    fn test_render_table_inline_styles() {
        let theme = test_theme();
        let ctx = test_ctx(&theme, 80);
        let lines = render_nodes(
            "| Name | Reference |\n\
             |------|-----------|\n\
             | **bold** | [docs](https://x.dev) |",
        );
        let row = lines
            .iter()
            .filter(|line| matches!(line.source, LogicalLineSource::TableRow { .. }))
            .map(|line| line.render(&ctx))
            .find(|line| line.to_string().contains("bold"))
            .expect("expected styled table body row");

        assert_snapshot!("table_inline_styles", ansi_line(&row));
    }

    #[test]
    fn test_render_mixed_runbook() {
        let input = r#"# Setup

Install dependencies.

```bash
npm install
```

## Test

Run the test suite.

```python [os:linux]
pytest tests/
```
"#;
        let lines = render_nodes(input);
        assert_snapshot!("mixed_runbook", logical_line_summary(&lines));
    }

    #[test]
    fn test_render_empty_input() {
        let lines = render_nodes("");
        assert!(lines.is_empty());
    }

    #[test]
    fn test_render_code_ids_sequence() {
        let lines = render_nodes("```bash\necho a\n```\n\n```python\nprint(1)\n```");
        // Both code blocks should have distinct IDs
        let code_lines: Vec<_> = lines.iter().filter(|line| line.is_code_info()).collect();
        assert_eq!(code_lines.len(), 2);
        let id0 = code_lines[0].code_id;
        let id1 = code_lines[1].code_id;
        assert!(id0.is_some());
        assert!(id1.is_some());
        assert_ne!(id0, id1);
    }

    #[test]
    fn test_render_code_info_line_has_code_id() {
        let lines = render_nodes("```bash\necho test\n```");
        let code_info = lines.iter().find(|line| line.is_code_info());
        assert!(code_info.is_some());
        assert!(code_info.unwrap().code_id.is_some());
    }

    #[test]
    fn test_render_code_body_associated_with_code_id() {
        let lines = render_nodes("```bash\necho test\n```");
        let code_bodies: Vec<_> = lines.iter().filter(|line| line.is_code_body()).collect();
        assert!(!code_bodies.is_empty());
        for body in code_bodies {
            assert!(body.code_id.is_some(), "code body missing code_id");
        }
    }

    #[test]
    fn test_render_code_tabs_preserved_logically_expanded_visually() {
        let lines = render_nodes(
            "```go\npackage main\n\t\"fmt\"\n\tos.Setenv(\"FROM_GO\", \"set by go\")\n```",
        );
        let code_bodies: Vec<_> = lines.iter().filter(|line| line.is_code_body()).collect();
        assert_eq!(code_bodies.len(), 3);
        assert_eq!(code_bodies[0].text_content(), "package main");
        assert_eq!(code_bodies[1].text_content(), "\t\"fmt\"");
        assert_eq!(
            code_bodies[2].text_content(),
            "\tos.Setenv(\"FROM_GO\", \"set by go\")"
        );

        let theme = test_theme();
        let ctx = test_ctx(&theme, 80);
        let import_line = code_bodies[1].render(&ctx).to_string();
        let call_line = code_bodies[2].render(&ctx).to_string();

        assert!(!import_line.contains('\t'));
        assert!(!call_line.contains('\t'));
        assert!(import_line.contains("    \"fmt\""));
        assert!(call_line.contains("    os.Setenv"));
        assert!(!call_line.contains("os. Setenv"));
    }

    #[test]
    fn test_render_heading_source() {
        let lines = render_nodes("# Title\n\n## Subtitle");
        let headings: Vec<_> = lines
            .iter()
            .filter(|line| line.heading_level().is_some())
            .collect();
        assert_eq!(headings.len(), 2);
        assert_eq!(headings[0].heading_level(), Some(1));
        assert_eq!(headings[1].heading_level(), Some(2));
    }

    #[test]
    fn test_render_heading_level() {
        let lines = render_nodes("# H1\n## H2\n### H3\n#### H4");
        let levels: Vec<u8> = lines
            .iter()
            .filter_map(LogicalLine::heading_level)
            .collect();
        assert_eq!(levels, [1, 2, 3, 4]);
    }

    #[test]
    fn test_highlight_line_thai_match_middle() {
        let style = Style::default().fg(Color::Red);
        let line = highlight_line(Line::from("เปิดภาษาไทยได้"), "ภาษาไทย", style);

        assert_eq!(line.to_string(), "เปิดภาษาไทยได้");
        assert_eq!(line.spans[1].content, "ภาษาไทย");
        assert_eq!(line.spans[1].style.fg, Some(Color::Red));
    }

    #[test]
    fn test_highlight_line_thai_match_at_start() {
        let style = Style::default().fg(Color::Red);
        let line = highlight_line(Line::from("สวัสดีจาก upmd"), "สวัสดี", style);

        assert_eq!(line.to_string(), "สวัสดีจาก upmd");
        assert_eq!(line.spans[0].content, "สวัสดี");
        assert_eq!(line.spans[0].style.fg, Some(Color::Red));
    }

    #[test]
    fn test_render_table_narrow() {
        let doc = upmd_parser::new().parse("| Name | Age | City |\n|------|-----|------|\n| Alice | 30 | New York |\n| Bob | 25 | London |");
        let theme = test_theme();
        let ctx = test_ctx(&theme, 25);
        let outputs = HashMap::new();
        let renderer = MarkdownRenderer::new(&theme, &outputs, &doc.codes, 10, 25);
        let lines = renderer.render(&doc.nodes).lines;
        let summary: Vec<String> = lines
            .iter()
            .filter(|l| l.is_table())
            .map(|l| l.render(&ctx).to_string())
            .collect();
        assert_snapshot!("table_narrow", summary.join("\n"));
    }

    #[test]
    fn test_render_table_wide() {
        let doc = upmd_parser::new().parse("| Name | Age | City |\n|------|-----|------|\n| Alice | 30 | New York |\n| Bob | 25 | London |");
        let theme = test_theme();
        let ctx = test_ctx(&theme, 80);
        let outputs = HashMap::new();
        let renderer = MarkdownRenderer::new(&theme, &outputs, &doc.codes, 10, 80);
        let lines = renderer.render(&doc.nodes).lines;
        let summary: Vec<String> = lines
            .iter()
            .filter(|l| l.is_table())
            .map(|l| l.render(&ctx).to_string())
            .collect();
        assert_snapshot!("table_wide", summary.join("\n"));
    }

    /// Creates a [`Task`] pre-loaded with 50 lines of output for use in
    /// inline-scroll snapshot tests.
    fn output_task() -> Task {
        let mut buf = Task::new(40, 24, 500);
        for i in 0..50 {
            buf.parser
                .parse(&format!("This is output line number {i}\n"));
        }
        buf
    }

    /// Renders a markdown document with the given outputs, returning the text of
    /// all rendered logical lines.
    fn render_with_outputs(
        outputs: &HashMap<u32, Task>,
        markdown: &str,
        inline_max_lines: usize,
    ) -> String {
        let doc = upmd_parser::new().parse(markdown);
        let theme = test_theme();
        let renderer = MarkdownRenderer::new(&theme, outputs, &doc.codes, inline_max_lines, 80);
        let lines = renderer.render(&doc.nodes).lines;
        logical_line_summary(&lines)
    }

    #[test]
    fn test_inline_scroll_first_line_reachable() {
        let mut buf = output_task();
        // Scroll to max position. Should show the very first line of output.
        buf.inline_scroll = usize::MAX;
        buf.sync_inline_scrollback(10);

        let mut outputs = HashMap::new();
        outputs.insert(1, buf);
        let result = render_with_outputs(&outputs, "```bash\necho test\n```", 10);
        assert_snapshot!("inline_scroll_first_line", result);
    }

    #[test]
    fn test_inline_scroll_bottom() {
        let mut buf = output_task();
        // At the bottom. Should show the last 10 lines.
        buf.inline_scroll = 0;
        buf.sync_inline_scrollback(10);

        let mut outputs = HashMap::new();
        outputs.insert(1, buf);
        let result = render_with_outputs(&outputs, "```bash\necho test\n```", 10);
        assert_snapshot!("inline_scroll_bottom", result);
    }

    #[test]
    fn test_inline_scroll_middle() {
        let mut buf = output_task();
        // Scrolled partway up. Should show middle lines.
        buf.inline_scroll = 25;
        buf.sync_inline_scrollback(10);

        let mut outputs = HashMap::new();
        outputs.insert(1, buf);
        let result = render_with_outputs(&outputs, "```bash\necho test\n```", 10);
        assert_snapshot!("inline_scroll_middle", result);
    }

    #[test]
    fn test_inline_scroll_clamp_no_collapse() {
        let mut buf = output_task();
        // inline_scroll well past the end. Should not collapse to zero lines.
        buf.inline_scroll = 999;
        buf.sync_inline_scrollback(10);

        let mut outputs = HashMap::new();
        outputs.insert(1, buf);
        let result = render_with_outputs(&outputs, "```bash\necho test\n```", 10);
        assert_snapshot!("inline_scroll_clamp", result);
    }

    #[test]
    fn test_inline_scroll_short_output() {
        // Output shorter than inline_max_lines. All lines visible at any scroll.
        let mut buf = Task::new(40, 24, 500);
        for i in 0..5 {
            buf.parser.parse(&format!("short line {i}\n"));
        }
        buf.inline_scroll = 10; // scroll past end
        buf.sync_inline_scrollback(10);

        let mut outputs = HashMap::new();
        outputs.insert(1, buf);
        let result = render_with_outputs(&outputs, "```bash\necho test\n```", 10);
        assert_snapshot!("inline_scroll_short", result);
    }

    #[test]
    fn render_plain_does_not_populate_content_cache() {
        let theme = test_theme();
        let ctx = test_ctx(&theme, 80);
        let line = LogicalLine::text_lazy_spans(
            vec![InlineSpan {
                text: "let value = 1;".into(),
                style: Vec::new(),
            }],
            false,
        );

        let rendered = line.render_plain(&ctx);

        assert_eq!(rendered.to_string(), "let value = 1;");
        let cached = &line.lazy_text().expect("expected lazy text").cached;
        assert!(cached.borrow().is_none());

        assert!(line.ensure_rendered(&ctx));
        assert!(cached.borrow().is_some());
        assert!(!line.ensure_rendered(&ctx));
    }

    #[test]
    fn running_code_body_does_not_append_spinner() {
        let theme = test_theme();
        let ctx = RenderContext {
            theme: &theme,
            active_code_id: Some(1),
            prefer_status_gutter: None,
            spinner_char: '⠲',
            viewport_width: 80,
        };
        let mut line = LogicalLine::code_body("read -p \"Your name: \" ME", "bash", 1);
        line.is_running = true;

        let rendered = line.render(&ctx);

        assert!(!rendered.to_string().contains('⠲'));
        assert!(rendered.to_string().ends_with(" ME"));
    }
}
