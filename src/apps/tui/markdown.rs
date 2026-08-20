//! Converts parsed markdown [`upmd_parser::nodes::Node`]s into renderable [`LogicalLine`]s.
//!
//! [`MarkdownRenderer::render`] expands the AST into semantic lines such as
//! paragraphs, headings, code rows, and table rows. [`LogicalLine::render`]
//! applies dynamic styling and caches expensive highlighting. The preview owns
//! width-dependent wrapping.

mod markup;
mod visual;

use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span, Text};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use unicode_width::UnicodeWidthChar;

use crate::apps::config::{GUTTER_GLYPH, PREVIEW_FRAME_OVERHEAD};
use crate::apps::theme::Theme;
use crate::runner::CodeId;
use upmd_parser::nodes::{
    inline_text, Alignment, Code, DepsToken, FrontmatterStyle, InlineSpan, InlineStyle, Table,
    TableCell,
};
use upmd_parser::Codes;

use crate::apps::task::Task;
use crate::apps::tui::wrap::slice_line;

/// CommonMark allows up to three leading spaces before a blockquote marker.
const MAX_BLOCKQUOTE_MARKER_INDENT: usize = 3;

/// How the preview renders the parsed markdown AST.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RenderMode {
    /// Semantic terminal UI: task glyphs, styled links, decoded images.
    #[default]
    Visual,
    /// Original Markdown source with interactive code blocks retained.
    Markup,
}

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

    fn markdown(text: impl Into<String>) -> Self {
        Self::new(text, "markdown")
    }

    fn from_spans(spans: Vec<InlineSpan>, source: &str) -> Self {
        let text = inline_text(&spans, source);
        let spans = spans
            .into_iter()
            .map(|span| span.into_owned(source))
            .collect();
        let mut text = Self::markdown(text);
        text.spans = spans;
        text
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

/// Lazily highlighted frontmatter.
#[derive(Debug)]
pub struct FrontmatterBlock {
    raw: String,
    delimiter: &'static str,
    language: &'static str,
    lines: Vec<String>,
    cached: RefCell<Option<Text<'static>>>,
}

impl FrontmatterBlock {
    fn new(style: FrontmatterStyle, raw: String) -> Self {
        let (delimiter, language) = match style {
            FrontmatterStyle::Yaml => ("---", "yaml"),
            FrontmatterStyle::Toml => ("+++", "toml"),
        };
        let mut lines = vec![delimiter.to_string()];
        lines.extend(raw.lines().map(String::from));
        lines.push(delimiter.to_string());

        Self {
            raw,
            delimiter,
            language,
            lines,
            cached: RefCell::new(None),
        }
    }

    fn len(&self) -> usize {
        self.lines.len()
    }

    fn raw_line(&self, row_idx: usize) -> &str {
        self.lines.get(row_idx).map_or("", String::as_str)
    }

    fn line(&self, row_idx: usize, theme: &Theme) -> Line<'static> {
        let mut cached = self.cached.borrow_mut();
        let text = cached.get_or_insert_with(|| {
            let rule = Line::from(Span::styled(self.delimiter, theme.rule_style()));
            let highlighted = theme.highlight(&self.raw, self.language);

            let lines: Vec<Line<'static>> = std::iter::once(rule.clone())
                .chain(highlighted.lines.into_iter().map(expand_tabs_in_line))
                .chain(std::iter::once(rule))
                .collect();

            Text::from(lines)
        });

        text.lines
            .get(row_idx)
            .cloned()
            .unwrap_or_else(|| Line::raw(self.raw_line(row_idx).to_owned()))
    }

    fn clear_cache(&self) {
        *self.cached.borrow_mut() = None;
    }
}

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
                lines: render_table(&self.source, "", theme, viewport_width),
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

/// Content specific to one logical line.
///
/// Shared navigation and display metadata lives on [`LogicalLine`].
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
    Frontmatter {
        block: Rc<FrontmatterBlock>,
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

/// Renderable content and metadata for one semantic preview line.
///
/// A logical line is width-independent; preview layout maps it to one or more
/// terminal rows.
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
    /// Number of leading rendered characters repeated on wrapped rows, such as
    /// `▎ ` in Visual mode or `> ` in Markup mode.
    wrap_prefix_width: usize,
    /// Styled prefix painted on continuation rows when it differs from the
    /// leading rendered text, such as spaces replacing a list marker.
    wrap_prefixes: Option<Vec<Span<'static>>>,
    /// Optional foreground color override for the gutter indicator (used by
    /// output lines to reflect task status).
    pub gutter_fg: Option<Color>,
    /// AST node index used to preserve the viewport across render modes.
    pub node_idx: Option<usize>,
}

impl LogicalLine {
    /// Creates an empty newline.
    pub fn newline() -> Self {
        Self {
            source: LogicalLineSource::Newline,
            ..Self::default()
        }
    }

    /// Creates a text line from inline markdown spans.
    pub fn text_lazy_spans(spans: Vec<InlineSpan>, source: &str, is_block_start: bool) -> Self {
        Self {
            source: LogicalLineSource::Text(LazyText::from_spans(spans, source)),
            is_block_start,
            ..Self::default()
        }
    }

    /// Creates a list item from inline markdown spans with a styled prefix.
    pub fn list_item_spans(
        spans: Vec<InlineSpan>,
        source: &str,
        prefix: Span<'static>,
        wrap_prefix: Span<'static>,
        is_block_start: bool,
    ) -> Self {
        Self {
            source: LogicalLineSource::ListItem(LazyText::from_spans(spans, source)),
            is_block_start,
            prefixes: vec![prefix],
            wrap_prefixes: Some(vec![wrap_prefix]),
            ..Self::default()
        }
    }

    /// Creates a heading line from inline markdown spans.
    pub fn heading_lazy_spans(spans: Vec<InlineSpan>, source: &str, level: u8) -> Self {
        Self {
            source: LogicalLineSource::Heading {
                level,
                text: LazyText::from_spans(spans, source),
            },
            is_block_start: true,
            ..Self::default()
        }
    }

    /// Creates a markup-mode text line, syntax-highlighted as markdown source.
    pub fn markup_text(text: impl Into<String>, is_block_start: bool) -> Self {
        let text = text.into();
        Self {
            wrap_prefix_width: source_quote_width(&text),
            source: LogicalLineSource::Text(LazyText::markdown(text)),
            is_block_start,
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

    pub fn frontmatter(block: Rc<FrontmatterBlock>, row_idx: usize, is_block_start: bool) -> Self {
        Self {
            source: LogicalLineSource::Frontmatter { block, row_idx },
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
            LogicalLineSource::Frontmatter { block, .. } => block.clear_cache(),
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

    pub fn wrap_prefix_width(&self) -> usize {
        self.wrap_prefix_width
    }

    pub fn wrap_prefixes(&self) -> Option<&[Span<'static>]> {
        self.wrap_prefixes.as_deref()
    }

    pub fn reserved_prefix_width(&self) -> usize {
        self.prefix_width().max(self.wrap_prefix_width)
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
    pub fn is_newline(&self) -> bool {
        matches!(self.source, LogicalLineSource::Newline)
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
        self.is_table()
            || self.is_output()
            || self.is_code_info()
            || self.is_image()
            || matches!(self.source, LogicalLineSource::ThematicBreak)
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
            LogicalLineSource::Frontmatter { block, row_idx } => {
                block.raw_line(*row_idx).to_owned()
            }
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
            LogicalLineSource::Frontmatter { block, row_idx } => {
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
                            "",
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
            LogicalLineSource::Frontmatter { block, row_idx } => block.line(*row_idx, ctx.theme),
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
    let gutter = Span::styled(GUTTER_GLYPH, gs);
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
fn render_table(
    table: &Table,
    source: &str,
    theme: &Theme,
    viewport_width: usize,
) -> Vec<Line<'static>> {
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
            let header_w = table.headers[i].char_len(source);
            let cell_w = table
                .rows
                .iter()
                .filter_map(|r| r.get(i))
                .map(|cell| cell.char_len(source))
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
                source,
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
    source: &str,
    width: usize,
    alignment: Alignment,
    base_style: Style,
    theme: &Theme,
) -> Vec<Span<'static>> {
    if width == 0 {
        return Vec::new();
    }

    let visible = cell.char_len(source);
    let mut line = render_inline_spans(&cell.spans, source, base_style, theme);
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
    /// Current blockquote nesting depth. Each level adds a display-only gutter.
    quote_depth: usize,
    /// Ordered display prefixes contributed by the active render mode.
    prefixes: Vec<Span<'static>>,
    /// Extra display width before code content, keyed by code block.
    code_prefix_overhead: HashMap<CodeId, usize>,
    /// Traversal index allocated to the next AST node.
    next_node_idx: usize,
    /// Index of the node currently being rendered.
    node_idx: usize,
}

impl RenderState {
    fn begin_node(&mut self) -> usize {
        let parent = self.node_idx;
        self.next_node_idx += 1;
        self.node_idx = self.next_node_idx;
        parent
    }

    fn end_node(&mut self, parent: usize) {
        self.node_idx = parent;
    }

    fn prefix_width(&self) -> usize {
        self.prefixes
            .iter()
            .map(|prefix| prefix.content.chars().count())
            .sum()
    }
}

pub struct RenderedMarkdown {
    pub lines: Vec<LogicalLine>,
    pub code_prefix_overhead: HashMap<CodeId, usize>,
}

/// From AST nodes to ratatui `Text` lines.
pub struct MarkdownRenderer<'a> {
    theme: &'a Theme,
    source: &'a str,
    outputs: &'a HashMap<u32, Task>,
    codes: &'a Codes,
    inline_max_lines: usize,
    viewport_width: usize,
    mode: RenderMode,
}

impl<'a> MarkdownRenderer<'a> {
    pub fn new(
        source: &'a str,
        theme: &'a Theme,
        outputs: &'a HashMap<u32, Task>,
        codes: &'a Codes,
        inline_max_lines: usize,
        viewport_width: usize,
    ) -> Self {
        Self {
            theme,
            source,
            outputs,
            codes,
            inline_max_lines,
            viewport_width,
            mode: RenderMode::Visual,
        }
    }

    pub fn mode(mut self, mode: RenderMode) -> Self {
        self.mode = mode;
        self
    }

    pub fn render(&self, nodes: &[upmd_parser::nodes::Node]) -> RenderedMarkdown {
        let mut lines = Vec::new();
        let mut state = RenderState::default();
        match self.mode {
            RenderMode::Visual => self.render_visual(nodes, &mut lines, &mut state),
            RenderMode::Markup => self.render_markup(nodes, &mut lines, &mut state),
        }
        RenderedMarkdown {
            lines,
            code_prefix_overhead: state.code_prefix_overhead,
        }
    }

    fn render_code(&self, code_id: CodeId, lines: &mut Vec<LogicalLine>, state: &mut RenderState) {
        let code = self
            .codes
            .by_id(code_id)
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
        let gutter_width = state.prefix_width();
        if gutter_width > 0 {
            state.code_prefix_overhead.insert(code.id, gutter_width);
        }
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
        let mut info = self.render_code_info(code, !is_start);
        info.gutter_fg = gutter_fg;
        self.push_line(lines, info, state);
        self.render_code_body(code, lines, state, gutter_fg, is_running);
        self.render_code_output(code, lines, state, gutter_fg, is_running);
    }

    fn push_line(&self, lines: &mut Vec<LogicalLine>, mut line: LogicalLine, state: &RenderState) {
        let mut wrap_prefixes = state.prefixes.clone();
        if let Some(line_prefixes) = line.wrap_prefixes.take() {
            wrap_prefixes.extend(line_prefixes);
        }
        line.wrap_prefix_width = wrap_prefixes
            .iter()
            .map(|prefix| prefix.content.chars().count())
            .sum();
        line.wrap_prefixes = (!wrap_prefixes.is_empty()).then_some(wrap_prefixes);
        line.prefixes.splice(0..0, state.prefixes.iter().cloned());
        self.push_unquoted_line(lines, line, state);
    }

    fn push_unquoted_line(
        &self,
        lines: &mut Vec<LogicalLine>,
        mut line: LogicalLine,
        state: &RenderState,
    ) {
        line.node_idx = Some(state.node_idx);
        lines.push(line);
    }

    fn render_code_info(&self, code: &Code, is_start: bool) -> LogicalLine {
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

    fn render_code_body(
        &self,
        code: &Code,
        lines: &mut Vec<LogicalLine>,
        state: &mut RenderState,
        gutter_fg: Option<Color>,
        is_running: bool,
    ) {
        for body_line in code.content.lines() {
            let mut line = LogicalLine::code_body(body_line.to_string(), &code.language, code.id);
            line.gutter_fg = gutter_fg;
            line.is_running = is_running;
            self.push_line(lines, line, state);
        }
    }

    fn render_code_output(
        &self,
        code: &Code,
        lines: &mut Vec<LogicalLine>,
        state: &mut RenderState,
        gutter_fg: Option<Color>,
        is_running: bool,
    ) {
        let Some(buffer) = self.outputs.get(&code.id) else {
            return;
        };

        let show_cursor = !buffer.done;
        let is_tui = buffer.parser.is_alternate_screen();
        let styled = buffer.parser.inline_contents(show_cursor);
        if styled.lines.is_empty() {
            return;
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

        let bg = self.theme.output_background;
        for mut line in styled.lines.into_iter().skip(start).take(end - start) {
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
            let mut line = LogicalLine::output(line, code.id);
            line.gutter_fg = gutter_fg;
            line.is_running = is_running;
            self.push_line(lines, line, state);
        }
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
pub(super) fn owned_table(table: &Table, source: &str) -> Table {
    let mut table = table.clone();
    for cell in table
        .headers
        .iter_mut()
        .chain(table.rows.iter_mut().flatten())
    {
        for span in &mut cell.spans {
            span.text = span.text.clone().into_owned(source);
        }
    }
    table
}

fn split_span_lines(spans: &[InlineSpan], source: &str) -> Vec<Vec<InlineSpan>> {
    let mut lines = Vec::new();
    let mut current = Vec::new();
    for span in spans {
        let mut text = span.text(source);
        loop {
            match text.find('\n') {
                Some(idx) => {
                    let (head, tail) = text.split_at(idx);
                    if !head.is_empty() {
                        current.push(InlineSpan {
                            text: head.into(),
                            style: span.style.clone(),
                        });
                    }
                    lines.push(std::mem::take(&mut current));
                    text = &tail[1..];
                }
                None => {
                    if !text.is_empty() {
                        current.push(InlineSpan {
                            text: text.into(),
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
fn render_inline_spans(
    spans: &[InlineSpan],
    source: &str,
    base_style: Style,
    theme: &Theme,
) -> Line<'static> {
    let rendered = spans
        .iter()
        .flat_map(|span| render_inline_span(span, source, base_style, theme))
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
            let base_style = if index == 0 && span.text("").starts_with('#') {
                theme.markdown_heading_marker_style()
            } else {
                heading_style
            };
            render_inline_span(span, "", base_style, theme)
        })
        .collect::<Vec<_>>();
    Line::from(rendered).style(heading_style)
}

fn render_inline_span(
    span: &InlineSpan,
    source: &str,
    base_style: Style,
    theme: &Theme,
) -> Vec<Span<'static>> {
    if span.style.iter().any(|s| matches!(s, InlineStyle::HtmlTag)) {
        return render_html_tag(span.text(source), base_style, theme);
    }
    let style = span.style.iter().fold(base_style, |style, inline| {
        style.patch(inline_style(theme, inline))
    });
    vec![Span::styled(span.text(source).to_owned(), style)]
}

/// Highlights an inline HTML tag with the HTML syntax.
fn render_html_tag(text: &str, base_style: Style, theme: &Theme) -> Vec<Span<'static>> {
    let rendered = theme.highlight(text, "html");
    let mut spans: Vec<Span<'static>> = rendered
        .lines
        .into_iter()
        .flat_map(|line| line.spans)
        .collect();
    if spans.is_empty() {
        spans.push(Span::styled(text.to_owned(), base_style));
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

/// Returns the byte width of the leading `> ` quote markers in source text.
fn source_quote_width(text: &str) -> usize {
    // Leading indentation is bounded by the CommonMark blockquote rule.
    let bytes = text.as_bytes();
    let mut pos = 0;
    let mut end = 0;

    loop {
        let spaces = bytes[pos..]
            .iter()
            .take(MAX_BLOCKQUOTE_MARKER_INDENT)
            .take_while(|&&byte| byte == b' ')
            .count();
        if bytes.get(pos + spaces) != Some(&b'>') {
            break;
        }
        pos += spaces + 1; // consume leading spaces + '>'
        if bytes.get(pos).is_some_and(|b| matches!(b, b' ' | b'\t')) {
            pos += 1; // consume one optional space/tab after '>'
        }
        end = pos;
    }
    end
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apps::task::Task;
    use insta::assert_snapshot;
    use ratatui::style::Color;
    use std::collections::HashMap;

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

    fn render_markdown_with_outputs(
        markdown: &str,
        outputs: &HashMap<CodeId, Task>,
    ) -> (Theme, RenderedMarkdown) {
        let doc = upmd_parser::new().parse(markdown);
        let nodes = &doc.nodes;
        let codes = &doc.codes;
        let theme = test_theme();
        let rendered = {
            let renderer = MarkdownRenderer::new(&doc.source, &theme, outputs, codes, 10, 80);
            renderer.render(nodes)
        };
        (theme, rendered)
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
            LogicalLineSource::Frontmatter { .. } => "Frontmatter".to_string(),
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
                if preview.is_empty() {
                    format!("{:2}: [{}]", i, source_label(l))
                } else {
                    format!("{:2}: [{}] {}", i, source_label(l), preview)
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
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
        let renderer = MarkdownRenderer::new(
            &doc.source,
            &theme,
            outputs,
            &doc.codes,
            inline_max_lines,
            80,
        );
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
            "",
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

    #[test]
    fn nested_children_do_not_change_following_list_item_identity() {
        let markdown = "- first\n\n    | A | B |\n    |---|---|\n    | x | y |\n\n- after\n";
        let doc = upmd_parser::new().parse(markdown);
        let theme = test_theme();
        let outputs = HashMap::new();
        let identity = |mode| {
            MarkdownRenderer::new(&doc.source, &theme, &outputs, &doc.codes, 10, 80)
                .mode(mode)
                .render(&doc.nodes)
                .lines
                .into_iter()
                .find(|line| line.text_content().trim_start_matches("- ") == "after")
                .expect("following list item")
                .node_idx
        };
        assert_eq!(identity(RenderMode::Visual), identity(RenderMode::Markup));
    }
}
