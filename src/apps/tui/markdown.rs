//! Produces styled [`ViewLine`]s from parsed markdown [`Node`]s.
//!
//! [`MarkdownRenderer::render`] walks the AST and builds a vector of `ViewLine`s,
//! one per semantic markdown element (headings, paragraphs, code body lines,
//! table rows, etc.).  Tables expand into multiple `ViewLine`s (one per row).
//!
//! [`ViewLine::render`] lazily renders a `ViewLine` into a ratatui
//! [`Line`] at draw time.  It applies syntax highlighting (with a per-line cache
//! in [`Content::Raw::cached`]), theme colours, active-code gutters, spinners,
//! and search highlights.  The resulting `Line` is then consumed by the preview
//! pane which may soft-wrap it across multiple terminal rows.

use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span, Text};
use std::collections::HashMap;
use unicode_width::UnicodeWidthChar;

use crate::apps::config::PREVIEW_FRAME_OVERHEAD;
use crate::apps::theme::Theme;
use crate::runner::CodeId;
use upmd_parser::nodes::{Alignment, Code, ListKind, Table as MarkdownTable, TaskStatus};

use crate::apps::task::Task;

/// Render-time context passed to [`ViewLine::render`].
pub struct RenderContext<'a> {
    pub theme: &'a Theme,
    pub active_code_id: Option<CodeId>,
    /// When set, this block's task status color overrides active gutter color.
    pub prefer_status_gutter: Option<CodeId>,
    pub spinner_char: char,
    pub viewport_width: usize,
}
/// The type of visual line in the preview.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LineKind {
    Text,
    ListItem,
    Heading(u8),
    CodeInfo,
    CodeBody,
    Output,
    Table,
    ThematicBreak,
    #[default]
    Newline,
}

/// Content storage - either raw text (needs highlighting) or already styled.
#[derive(Debug, Clone)]
pub enum Content {
    Raw {
        text: String,
        language: String,
        /// Lazily-populated cache: `None` until first highlight, then reused.
        cached: std::cell::RefCell<Option<Text<'static>>>,
    },
    Ready(Text<'static>),
    /// Code info bar: left spans + right text, right-aligned at render time.
    CodeInfo {
        left: Vec<(String, ratatui::style::Style)>,
        right: String,
        style: ratatui::style::Style,
    },
}

impl Default for Content {
    fn default() -> Self {
        Self::Ready(Text::raw(""))
    }
}

/// A single *logical* line in the preview: one semantic markdown element.
///
/// `ViewLine`s are produced once by [`MarkdownRenderer::render`] and cached in
/// [`Preview::logical_lines`](crate::apps::tui::preview::Preview::logical_lines).
/// They are **not** directly drawn; instead [`ViewLine::render`]
/// lazily renders them into a [`Line`] each frame, and
/// [`Preview::rebuild_visual_lines`](crate::apps::tui::preview::Preview::rebuild_visual_lines)
/// optionally soft-wraps that `Line` into one or more
/// [`VisualLine`](crate::apps::tui::preview::VisualLine)s.
#[derive(Debug, Clone, Default)]
pub struct ViewLine {
    pub kind: LineKind,
    pub content: Content,
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
    /// Stores the raw table data so it can be re-rendered when the viewport
    /// width changes.
    pub table: Option<MarkdownTable>,
    /// Which row of the table this `ViewLine` represents (only for [`LineKind::Table`]).
    pub table_row_idx: Option<usize>,
    /// Optional foreground color override for the gutter indicator (used by
    /// output lines to reflect task status).
    pub gutter_fg: Option<Color>,
}

impl ViewLine {
    /// Creates an empty newline.
    pub fn newline(code_id: Option<CodeId>, is_block_start: bool) -> Self {
        Self {
            kind: LineKind::Newline,
            content: Content::Ready(Text::raw("")),
            code_id,
            is_block_start,
            ..Self::default()
        }
    }

    /// Creates a text line with raw content that needs lazy highlighting.
    pub fn text_lazy(raw: impl Into<String>, is_block_start: bool) -> Self {
        Self {
            kind: LineKind::Text,
            content: Content::Raw {
                text: raw.into(),
                language: "markdown".to_string(),
                cached: std::cell::RefCell::new(None),
            },
            is_block_start,
            ..Self::default()
        }
    }

    /// Creates a list item with raw content and styled prefix.
    pub fn list_item(raw: impl Into<String>, prefix: Span<'static>, is_block_start: bool) -> Self {
        Self {
            kind: LineKind::ListItem,
            content: Content::Raw {
                text: raw.into(),
                language: "markdown".to_string(),
                cached: std::cell::RefCell::new(None),
            },
            is_block_start,
            prefixes: vec![prefix],
            ..Self::default()
        }
    }

    /// Creates a heading line with raw content that needs lazy highlighting.
    pub fn heading_lazy(raw: impl Into<String>, level: u8) -> Self {
        Self {
            kind: LineKind::Heading(level),
            content: Content::Raw {
                text: raw.into(),
                language: "markdown".to_string(),
                cached: std::cell::RefCell::new(None),
            },
            is_block_start: true,
            ..Self::default()
        }
    }

    /// Creates a code info line (header showing code ID, language, status).
    pub fn code_info(
        content: impl Into<Text<'static>>,
        code_id: CodeId,
        is_start: bool,
        is_running: bool,
    ) -> Self {
        Self {
            kind: LineKind::CodeInfo,
            content: Content::Ready(content.into()),
            code_id: Some(code_id),
            is_block_start: is_start,
            is_code_start: is_start,
            is_running,
            prefixes: Vec::new(),
            table: None,
            table_row_idx: None,
            gutter_fg: None,
        }
    }

    /// Creates a code body line with raw content that needs lazy highlighting.
    pub fn code_body(raw: impl Into<String>, language: impl Into<String>, code_id: CodeId) -> Self {
        Self {
            kind: LineKind::CodeBody,
            content: Content::Raw {
                text: raw.into(),
                language: language.into(),
                cached: std::cell::RefCell::new(None),
            },
            code_id: Some(code_id),
            ..Self::default()
        }
    }

    /// Creates an output line.
    pub fn output(content: impl Into<Text<'static>>, code_id: CodeId) -> Self {
        Self {
            kind: LineKind::Output,
            content: Content::Ready(content.into()),
            code_id: Some(code_id),
            ..Self::default()
        }
    }

    /// Creates a table row or border line.
    pub fn table(table: MarkdownTable, row_idx: usize, line: Line<'static>) -> Self {
        Self {
            kind: LineKind::Table,
            content: Content::Ready(Text::from(line)),
            table: Some(table),
            table_row_idx: Some(row_idx),
            ..Self::default()
        }
    }

    /// Creates a thematic break (horizontal rule).
    pub fn thematic_break() -> Self {
        Self {
            kind: LineKind::ThematicBreak,
            content: Content::Ready(Text::raw("")),
            is_block_start: true,
            ..Self::default()
        }
    }

    /// Clears the lazy-highlight cache so the next render recomputes with the current theme.
    pub fn clear_cache(&self) {
        if let Content::Raw { cached, .. } = &self.content {
            *cached.borrow_mut() = None;
        }
    }

    pub fn prefix_width(&self) -> usize {
        self.prefixes
            .iter()
            .map(|prefix| prefix.content.chars().count())
            .sum()
    }

    #[inline]
    pub fn is_code_info(&self) -> bool {
        matches!(self.kind, LineKind::CodeInfo)
    }

    #[inline]
    pub fn is_code_body(&self) -> bool {
        matches!(self.kind, LineKind::CodeBody)
    }

    #[inline]
    pub fn is_output(&self) -> bool {
        matches!(self.kind, LineKind::Output)
    }

    #[inline]
    pub fn is_table(&self) -> bool {
        matches!(self.kind, LineKind::Table)
    }

    #[inline]
    /// Returns `true` for lines that must not be wrapped (already sized/formatted).
    pub fn is_unwrappable(&self) -> bool {
        self.is_table() || self.is_output() || self.is_code_info()
    }

    /// Returns text content, preferring raw text if available.
    pub fn text_content(&self) -> String {
        match &self.content {
            Content::Raw { text, .. } => text.clone(),
            Content::Ready(text) => text.to_string(),
            Content::CodeInfo { left, right, .. } => {
                let l: String = left.iter().map(|(s, _)| s.as_str()).collect();
                format!("{l} {}", right.trim_end())
            }
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

    /// Populates the syntax cache without applying viewport-dependent paint.
    pub fn ensure_highlighted(&self, ctx: &RenderContext<'_>) -> bool {
        if let Content::Raw { cached, .. } = &self.content {
            if cached.borrow().is_none() {
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

        if is_active {
            // Code info IDs track the active block even when the preview is not focused.
            if self.is_code_info() {
                if let Some(first_span) = line.spans.first_mut() {
                    first_span.style = first_span.style.patch(ctx.theme.active_fg_style());
                }
            }
        }
    }

    /// Display-only second pass: applies prefixes and the code gutter.
    fn apply_prefixes(&self, line: &mut Line<'static>) {
        // Prefixes are stored outer-to-inner (e.g. [quote, list]). Prepend them
        // to the content spans in that order so the rendered line reads left
        // to right as expected ("> • item").
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
        if matches!(self.kind, LineKind::ThematicBreak) {
            let width = ctx.viewport_width.saturating_sub(PREVIEW_FRAME_OVERHEAD);
            return Some(Line::from(Span::styled(
                "─".repeat(width),
                ctx.theme.rule_style(),
            )));
        }

        if matches!(self.kind, LineKind::Table) {
            if let (Some(ref table), Some(row_idx)) = (&self.table, self.table_row_idx) {
                let lines = render_table(table, ctx.theme, ctx.viewport_width);
                return Some(
                    lines
                        .into_iter()
                        .nth(row_idx)
                        .unwrap_or_else(|| Line::raw("")),
                );
            }
        }

        None
    }

    /// Renders text-identical content without syntax highlighting.
    fn render_plain_content(&self, ctx: &RenderContext<'_>) -> Line<'static> {
        match &self.content {
            Content::Raw { text, .. } => expand_tabs_in_line(Line::raw(text.clone())),
            _ => self.render_content(ctx),
        }
    }

    /// Renders the main line content with lazy syntax highlighting.
    fn render_content(&self, ctx: &RenderContext<'_>) -> Line<'static> {
        match &self.content {
            Content::Raw {
                text,
                language,
                cached,
            } => {
                let mut cache = cached.borrow_mut();
                if let Some(ref hit) = *cache {
                    hit.lines
                        .first()
                        .cloned()
                        .unwrap_or_else(|| expand_tabs_in_line(Line::raw(text.clone())))
                } else {
                    let mut highlighted = ctx.theme.highlight(text, language);
                    for line in &mut highlighted.lines {
                        *line = expand_tabs_in_line(std::mem::take(line));
                    }
                    let l = highlighted
                        .lines
                        .first()
                        .cloned()
                        .unwrap_or_else(|| expand_tabs_in_line(Line::raw(text.clone())));
                    *cache = Some(highlighted);
                    l
                }
            }
            Content::Ready(text) => text.lines.first().cloned().unwrap_or_else(|| Line::raw("")),
            Content::CodeInfo { left, right, style } => {
                // Compute right-aligned padding using the live viewport width.
                // Subtract prefix width (e.g. "> " from blockquote) so the
                // right-aligned label doesn't get clipped.
                let prefix_width = self.prefix_width();
                let wrap_width = ctx
                    .viewport_width
                    .saturating_sub(crate::apps::config::PREVIEW_CODE_WRAP_OVERHEAD + prefix_width)
                    .max(1);
                let mut spans: Vec<Span<'static>> = left
                    .iter()
                    .map(|(s, st)| Span::styled(s.clone(), *st))
                    .collect();
                // Spinner goes inline right after left content, before the gap.
                if self.is_running {
                    spans.push(Span::styled(
                        format!(" {}", ctx.spinner_char),
                        ctx.theme.active_fg_style(),
                    ));
                }
                let left_chars: usize = spans.iter().map(|s| s.content.chars().count()).sum();
                let right_chars = right.chars().count();
                let gap = wrap_width.saturating_sub(left_chars + right_chars).max(1);
                spans.push(Span::styled(" ".repeat(gap), *style));
                spans.push(Span::styled(right.clone(), *style));
                let mut line = Line::from(spans);
                line.style = *style;
                line
            }
        }
    }

    /// Adds a gutter indicator for highlightable lines.
    fn add_gutter(&self, line: &mut Line<'static>, is_active: bool, ctx: &RenderContext<'_>) {
        if !matches!(
            self.kind,
            LineKind::CodeInfo | LineKind::CodeBody | LineKind::Output
        ) {
            return;
        }
        let is_unwrappable = self.is_output();
        let prefer_status_gutter = ctx.prefer_status_gutter == self.code_id;
        apply_gutter(
            line,
            is_unwrappable,
            is_active,
            ctx.theme,
            self.gutter_fg,
            prefer_status_gutter,
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
fn render_table(table: &MarkdownTable, theme: &Theme, viewport_width: usize) -> Vec<Line<'static>> {
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
            let header_w = table.headers[i].chars().count();
            let cell_w = table
                .rows
                .iter()
                .filter_map(|r| r.get(i))
                .map(|c| c.chars().count())
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

    let make_row = |cells: &[String], style_fn: &dyn Fn(usize) -> Style| {
        let mut spans = vec![Span::raw("│").fg(line_fg)];
        for (i, cell) in cells.iter().enumerate() {
            let align = table.alignments.get(i).copied().unwrap_or(Alignment::Left);
            spans.push(Span::styled(
                format!(" {} ", align_cell(cell, col_widths[i], align)),
                style_fn(i),
            ));
            spans.push(Span::raw("│").fg(line_fg));
        }
        Line::from(spans)
    };

    let bold = Style::default().fg(content_fg).add_modifier(Modifier::BOLD);
    let normal = Style::default().fg(content_fg);
    let line = Style::default().fg(line_fg);

    let mut lines = vec![
        Line::raw(h_border("┌", "┬", "┐")).style(line),
        make_row(&table.headers, &|_| bold),
        Line::raw(h_border("├", "┼", "┤")).style(line),
    ];
    for row in &table.rows {
        lines.push(make_row(row, &|_| normal));
    }
    lines.push(Line::raw(h_border("└", "┴", "┘")).style(line));
    lines
}

fn align_cell(text: &str, width: usize, align: Alignment) -> String {
    let visible = text.chars().count();
    let text = if visible > width {
        if width <= 1 {
            "…".to_string()
        } else {
            let prefix: String = text.chars().take(width.saturating_sub(1)).collect();
            format!("{}…", prefix)
        }
    } else {
        text.to_string()
    };

    match align {
        Alignment::Right => format!("{:>width$}", text),
        Alignment::Center => {
            let pad = width.saturating_sub(text.chars().count());
            format!(
                "{}{}{}",
                " ".repeat(pad / 2),
                text,
                " ".repeat(pad - pad / 2)
            )
        }
        Alignment::Left | Alignment::None => format!("{text:width$}"),
    }
}

/// Render-time context for code-block snap-to-heading/paragraph.
#[derive(Default)]
struct SnapContext {
    /// Index of the heading ViewLine that precedes the next code block.
    title_line: Option<usize>,
    /// Index of the first paragraph ViewLine that precedes the next code block.
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
    pub lines: Vec<ViewLine>,
    pub code_prefix_overhead: HashMap<CodeId, usize>,
}

/// From AST nodes to ratatui `Text` lines.
pub struct MarkdownRenderer<'a> {
    theme: &'a Theme,
    outputs: &'a HashMap<u32, Task>,
    codes: &'a [upmd_parser::nodes::Code],
    inline_max_lines: usize,
    viewport_width: usize,
}

impl<'a> MarkdownRenderer<'a> {
    pub fn new(
        theme: &'a Theme,
        outputs: &'a HashMap<u32, Task>,
        codes: &'a [upmd_parser::nodes::Code],
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
        // later during ViewLine::render.
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

    fn push_line(&self, lines: &mut Vec<ViewLine>, mut line: ViewLine, quote_depth: usize) {
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
        lines: &mut Vec<ViewLine>,
        state: &mut RenderState,
    ) {
        use upmd_parser::nodes::Node;
        match node {
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
                let text = t.trim_start_matches('#').trim();
                let content = if prefix.is_empty() {
                    text.to_string()
                } else {
                    format!("{} {}", prefix, text)
                };
                self.push_line(
                    lines,
                    ViewLine::heading_lazy(content, *level),
                    state.quote_depth,
                );
                state.snap.title_line = Some(line_idx);
            }
            Node::List(items) => self.push_list(items, lines, state),
            Node::Code(code_id) => {
                let code = self
                    .codes
                    .iter()
                    .find(|c| c.id == *code_id)
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
                    ViewLine::newline(Some(code.id), false),
                    state.quote_depth,
                );
            }
            Node::Table(table) => {
                let rendered = render_table(table, self.theme, self.viewport_width);
                for (i, line) in rendered.into_iter().enumerate() {
                    self.push_line(
                        lines,
                        ViewLine::table(table.clone(), i, line),
                        state.quote_depth,
                    );
                }
                self.push_line(lines, ViewLine::newline(None, false), state.quote_depth);
            }
            Node::ThematicBreak => {
                self.push_line(lines, ViewLine::thematic_break(), state.quote_depth);
                self.push_line(lines, ViewLine::newline(None, false), state.quote_depth);
            }
        }
    }

    fn push_highlighted_lines(
        &self,
        text: &str,
        lines: &mut Vec<ViewLine>,
        block_start: bool,
        quote_depth: usize,
    ) -> Option<usize> {
        let start_idx = lines.len();
        let mut first = block_start;
        let mut emitted = false;
        for line in text.lines() {
            self.push_line(
                lines,
                ViewLine::text_lazy(line.to_string(), first),
                quote_depth,
            );
            first = false;
            emitted = true;
        }
        self.push_line(lines, ViewLine::newline(None, false), quote_depth);
        if emitted {
            Some(start_idx)
        } else {
            None
        }
    }

    fn push_list(
        &self,
        items: &[upmd_parser::nodes::ListItem],
        lines: &mut Vec<ViewLine>,
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

            for (line_idx, line) in item.text.lines().enumerate() {
                let prefix = if line_idx == 0 {
                    Span::styled(format!("{}{}", indent, marker), Style::default().fg(color))
                } else {
                    Span::raw(continuation.clone())
                };
                self.push_line(
                    lines,
                    ViewLine::list_item(line.to_string(), prefix, i == 0 && line_idx == 0),
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
            self.push_line(lines, ViewLine::newline(None, false), state.quote_depth);
        }
    }
    fn push_code_info(&self, code: &Code, is_start: bool) -> ViewLine {
        let buffer = self.outputs.get(&code.id);
        let is_executed =
            |done: bool| buffer.is_some_and(|b| b.execution.is_some() && b.done == done);
        let is_running = is_executed(false);
        let is_done = buffer.is_some_and(|b| b.done);
        let language = upmd_runner::find_language(&code.language);
        let info_style = self.theme.code_info_style();

        // Left: "{id}" or "{id} {name}"
        let left_text = if code.name.is_empty() {
            format!("{}", code.id)
        } else {
            format!("{} {}", code.id, code.name)
        };
        let mut left = vec![(left_text, info_style)];

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

        // Right: "{language} " is right-aligned at render time via Content::CodeInfo.
        let right = format!("{} ", language.name);

        let mut view_line = ViewLine::code_info(Line::raw(""), code.id, is_start, is_running);
        view_line.content = Content::CodeInfo {
            left,
            right,
            style: info_style,
        };
        view_line
    }

    fn push_code_body(&self, code: &Code) -> Vec<ViewLine> {
        code.content
            .lines()
            .map(|line| ViewLine::code_body(line.to_string(), &code.language, code.id))
            .collect()
    }

    fn push_code_output(&self, code: &Code) -> Vec<ViewLine> {
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
            out.push(ViewLine::output(line, code.id));
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

/// Expands raw tab characters for display while preserving the source text stored
/// in [`Content::Raw`].
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apps::task::Task;
    use insta::assert_snapshot;
    use ratatui::style::Color;
    use std::collections::HashMap;
    use upmd_parser::Parser;

    fn render_markdown(markdown: &str) -> RenderedMarkdown {
        let doc = upmd_parser::new().parse(markdown);
        let nodes = &doc.nodes;
        let codes = &doc.codes;
        let theme = Theme::new("base16-ocean.dark", false);
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
        let theme = Theme::new("base16-ocean.dark", false);
        let rendered = {
            let renderer = MarkdownRenderer::new(&theme, outputs, codes, 10, 80);
            renderer.render(nodes)
        };
        (theme, rendered)
    }

    fn render_nodes(markdown: &str) -> Vec<ViewLine> {
        render_markdown(markdown).lines
    }

    fn viewline_summary(lines: &[ViewLine]) -> String {
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
                format!("{:2}: [{:?}] {}", i, l.kind, preview)
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn code_start_lines(lines: &[ViewLine]) -> Vec<(usize, LineKind, Option<CodeId>, String)> {
        lines
            .iter()
            .enumerate()
            .filter(|(_, line)| line.is_code_start)
            .map(|(idx, line)| (idx, line.kind, line.code_id, line.text_content()))
            .collect()
    }

    #[test]
    fn test_snap_heading_to_code_start() {
        let lines = render_nodes("# Title\n\n```bash\necho hi\n```");
        let starts = code_start_lines(&lines);

        assert_eq!(starts.len(), 1);
        assert_eq!(starts[0].0, 0);
        assert_eq!(starts[0].1, LineKind::Heading(1));
        assert_eq!(starts[0].3, "# Title");
    }

    #[test]
    fn test_snap_paragraph_fallback_to_code_start() {
        let lines = render_nodes("Intro paragraph.\n\n```bash\necho hi\n```");
        let starts = code_start_lines(&lines);

        assert_eq!(starts.len(), 1);
        assert_eq!(starts[0].0, 0);
        assert_eq!(starts[0].1, LineKind::Text);
        assert_eq!(starts[0].3, "Intro paragraph.");
    }

    #[test]
    fn test_adjacent_code_blocks_do_not_reuse_snap_context() {
        let lines = render_nodes("# Title\n\n```bash\necho one\n```\n\n```bash\necho two\n```");
        let starts = code_start_lines(&lines);

        assert_eq!(starts.len(), 2);
        assert_eq!(starts[0].1, LineKind::Heading(1));
        assert_eq!(starts[0].3, "# Title");
        assert_eq!(starts[1].1, LineKind::CodeInfo);
        assert_ne!(starts[0].2, starts[1].2);
    }

    #[test]
    fn test_blockquote_snap_context_does_not_leak_outward() {
        let lines = render_nodes("> quoted note\n\n```bash\necho hi\n```");
        let starts = code_start_lines(&lines);

        assert_eq!(starts.len(), 1);
        assert_eq!(starts[0].1, LineKind::CodeInfo);
        assert_eq!(lines[0].text_content(), "quoted note");
        assert!(lines[0].code_id.is_none());
    }

    #[test]
    fn test_blockquote_snap_context_does_not_leak_inward() {
        let lines = render_nodes("Intro paragraph.\n\n> ```bash\n> echo hi\n> ```");
        let starts = code_start_lines(&lines);

        assert_eq!(starts.len(), 1);
        assert_eq!(starts[0].1, LineKind::CodeInfo);
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
        let theme = Theme::new("base16-ocean.dark", false);
        let ctx = RenderContext {
            theme: &theme,
            active_code_id: None,
            prefer_status_gutter: None,
            spinner_char: ' ',
            viewport_width: 80,
        };
        let lines = render_nodes("> - quoted item");
        let list_item = lines
            .iter()
            .find(|line| matches!(line.kind, LineKind::ListItem))
            .expect("expected a list item inside the blockquote");

        assert_eq!(list_item.prefix_width(), 4);
        assert_eq!(list_item.render(&ctx).to_string(), "> • quoted item");
    }

    #[test]
    fn test_apply_gutter_prefers_active_color_without_prefer_status_gutter() {
        let theme = Theme::new("base16-ocean.dark", false);
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
        let theme = Theme::new("base16-ocean.dark", false);
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
        assert_snapshot!("headings", viewline_summary(&lines));
    }

    #[test]
    fn test_render_paragraph() {
        let lines = render_nodes("This is a paragraph.\n\nWith a blank line.");
        assert_snapshot!("paragraph", viewline_summary(&lines));
    }

    #[test]
    fn test_render_fenced_code_block() {
        let lines = render_nodes("```bash\necho hello\n```");
        assert_snapshot!("fenced_code", viewline_summary(&lines));
    }

    #[test]
    fn test_render_code_with_language_attr() {
        let lines = render_nodes("```python [os:linux]\nprint('hi')\n```");
        assert_snapshot!("code_with_attr", viewline_summary(&lines));
    }

    #[test]
    fn test_render_bullet_list() {
        let lines = render_nodes("- item one\n- item two\n- item three");
        assert_snapshot!("bullet_list", viewline_summary(&lines));
    }

    #[test]
    fn test_render_ordered_list() {
        let lines = render_nodes("1. first\n2. second\n3. third");
        assert_snapshot!("ordered_list", viewline_summary(&lines));
    }

    #[test]
    fn test_render_task_list() {
        let lines = render_nodes("- [ ] unchecked\n- [x] checked\n- [-] in progress");
        assert_snapshot!("task_list", viewline_summary(&lines));
    }

    #[test]
    fn test_render_thematic_break() {
        let lines = render_nodes("above\n\n-----\n\nbelow");
        assert_snapshot!("thematic_break", viewline_summary(&lines));
    }

    #[test]
    fn test_render_table() {
        let lines =
            render_nodes("| Name  | Age |\n|-------|-----|\n| Alice | 30  |\n| Bob   | 25  |");
        assert_snapshot!("table", viewline_summary(&lines));
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
        assert_snapshot!("mixed_runbook", viewline_summary(&lines));
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
        let code_lines: Vec<_> = lines
            .iter()
            .filter(|l| l.kind == LineKind::CodeInfo)
            .collect();
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
        let code_info = lines.iter().find(|l| l.kind == LineKind::CodeInfo);
        assert!(code_info.is_some());
        assert!(code_info.unwrap().code_id.is_some());
    }

    #[test]
    fn test_render_code_body_associated_with_code_id() {
        let lines = render_nodes("```bash\necho test\n```");
        let code_bodies: Vec<_> = lines
            .iter()
            .filter(|l| l.kind == LineKind::CodeBody)
            .collect();
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
        let code_bodies: Vec<_> = lines
            .iter()
            .filter(|line| line.kind == LineKind::CodeBody)
            .collect();
        assert_eq!(code_bodies.len(), 3);
        assert_eq!(code_bodies[0].text_content(), "package main");
        assert_eq!(code_bodies[1].text_content(), "\t\"fmt\"");
        assert_eq!(
            code_bodies[2].text_content(),
            "\tos.Setenv(\"FROM_GO\", \"set by go\")"
        );

        let theme = Theme::new("base16-ocean.dark", false);
        let ctx = RenderContext {
            theme: &theme,
            active_code_id: None,
            prefer_status_gutter: None,
            spinner_char: ' ',
            viewport_width: 80,
        };
        let import_line = code_bodies[1].render(&ctx).to_string();
        let call_line = code_bodies[2].render(&ctx).to_string();

        assert!(!import_line.contains('\t'));
        assert!(!call_line.contains('\t'));
        assert!(import_line.contains("    \"fmt\""));
        assert!(call_line.contains("    os.Setenv"));
        assert!(!call_line.contains("os. Setenv"));
    }

    #[test]
    fn test_render_heading_line_kind() {
        let lines = render_nodes("# Title\n\n## Subtitle");
        let headings: Vec<_> = lines
            .iter()
            .filter(|l| matches!(l.kind, LineKind::Heading(_)))
            .collect();
        assert_eq!(headings.len(), 2);
        assert_eq!(headings[0].kind, LineKind::Heading(1));
        assert_eq!(headings[1].kind, LineKind::Heading(2));
    }

    #[test]
    fn test_render_heading_level() {
        let lines = render_nodes("# H1\n## H2\n### H3\n#### H4");
        let levels: Vec<u8> = lines
            .iter()
            .filter_map(|l| match l.kind {
                LineKind::Heading(n) => Some(n),
                _ => None,
            })
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
        let theme = Theme::new("base16-ocean.dark", false);
        let outputs = HashMap::new();
        let renderer = MarkdownRenderer::new(&theme, &outputs, &doc.codes, 10, 25);
        let lines = renderer.render(&doc.nodes).lines;
        let ctx = RenderContext {
            theme: &theme,
            active_code_id: None,
            prefer_status_gutter: None,
            spinner_char: ' ',
            viewport_width: 25,
        };
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
        let theme = Theme::new("base16-ocean.dark", false);
        let outputs = HashMap::new();
        let renderer = MarkdownRenderer::new(&theme, &outputs, &doc.codes, 10, 80);
        let lines = renderer.render(&doc.nodes).lines;
        let ctx = RenderContext {
            theme: &theme,
            active_code_id: None,
            prefer_status_gutter: None,
            spinner_char: ' ',
            viewport_width: 80,
        };
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
    /// all rendered ViewLines.
    fn render_with_outputs(
        outputs: &HashMap<u32, Task>,
        markdown: &str,
        inline_max_lines: usize,
    ) -> String {
        let doc = upmd_parser::new().parse(markdown);
        let theme = Theme::new("base16-ocean.dark", false);
        let renderer = MarkdownRenderer::new(&theme, outputs, &doc.codes, inline_max_lines, 80);
        let lines = renderer.render(&doc.nodes).lines;
        viewline_summary(&lines)
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
    fn render_plain_does_not_populate_syntax_cache() {
        let theme = Theme::new("base16-ocean.dark", false);
        let ctx = RenderContext {
            theme: &theme,
            active_code_id: None,
            prefer_status_gutter: None,
            spinner_char: ' ',
            viewport_width: 80,
        };
        let line = ViewLine::text_lazy("let value = 1;", false);

        let rendered = line.render_plain(&ctx);

        assert_eq!(rendered.to_string(), "let value = 1;");
        let Content::Raw { cached, .. } = &line.content else {
            panic!("expected raw content");
        };
        assert!(cached.borrow().is_none());

        assert!(line.ensure_highlighted(&ctx));
        assert!(cached.borrow().is_some());
        assert!(!line.ensure_highlighted(&ctx));
    }

    #[test]
    fn running_code_body_does_not_append_spinner() {
        let theme = Theme::new("base16-ocean.dark", false);
        let ctx = RenderContext {
            theme: &theme,
            active_code_id: Some(1),
            prefer_status_gutter: None,
            spinner_char: '⠲',
            viewport_width: 80,
        };
        let mut line = ViewLine::text_lazy("read -p \"Your name: \" ME", false);
        line.kind = LineKind::CodeBody;
        line.code_id = Some(1);
        line.is_running = true;

        let rendered = line.render(&ctx);

        assert!(!rendered.to_string().contains('⠲'));
        assert!(rendered.to_string().ends_with(" ME"));
    }
}
