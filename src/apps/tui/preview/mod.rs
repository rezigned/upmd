//! Markdown preview with live code output and width-dependent layout.
//!
//! Parsed nodes become width-independent [`LogicalLine`]s. [`LayoutLines`]
//! derives one or more [`LayoutLine`]s per logical line for wrapping and image
//! height. Painting combines content from the logical line with the
//! width-dependent slice described by the layout line:
//!
//! ```text
//! Node → LogicalLine ──┬─ render → rendered Line ─┬─→ terminal row
//!                      ├─ width  → LayoutLine(s) ─┤
//!                      └──── render metadata ─────┘
//! ```
//!
//! Content or output changes rebuild logical and layout lines. Width changes
//! rebuild only layout lines. Rendering and interaction use the full layout
//! index. Final row rendering covers the viewport and its overdraw margin.
//! Expensive content caches are prefetched one viewport ahead.

#[cfg(test)]
use ratatui::text::Line;
use ratatui::{
    layout::Rect,
    text::Text,
    widgets::{Borders, List, ListItem, ListState, Padding},
    Frame,
};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use crate::apps::config::{
    BORDER_HEIGHT, CODE_GUTTER_WIDTH, INLINE_MAX_LINES_DEFAULT, INLINE_MAX_LINES_FRACTION,
    INLINE_MAX_LINES_MIN, OVERDRAW_FRACTION, PREVIEW_CONTENT_TOP_OFFSET, PREVIEW_CONTENT_X_OFFSET,
    PREVIEW_FRAME_OVERHEAD,
};
use crate::apps::theme::Theme;
use crate::runner::CodeId;
use keymap::{DerivedConfig, KeyMap};
use upmd_parser::nodes::Node;
use upmd_parser::{Codes, Document};

use super::markdown::{
    highlight_line, LogicalLine, MarkdownHtml, MarkdownRenderer, RenderContext, RenderMode,
};
use super::selection::SelectionState;
use super::wrap::CopyLine;
use crate::apps::task::Task;
use crate::apps::tui::widgets::Spinner;

const INLINE_PTY_MIN_PERCENT: usize = 40;
const INLINE_PTY_MIN_ROWS: usize = 8;
const CODE_NAVIGATION_CONTEXT_ROWS: usize = 3;
use upmd_runtime::{
    runtimes::tui::{Input, Output},
    Component, Effect, NoOutcome,
};

mod images;
mod layout_lines;
mod search;
mod selection;

pub use images::image_base_dir;
use images::ImageCache;
pub(crate) use images::{decode_image, DecodedImage};
use layout_lines::{LayoutLine, LayoutLines};
use search::PreviewSearch;
use selection::PreviewSelection;

/// Identifies a layout line across layout rebuilds.
#[derive(Clone, Copy)]
enum LayoutLineIdentity {
    Code {
        id: CodeId,
        line_idx: usize,
        wrap_idx: usize,
    },
    Document {
        node_idx: Option<usize>,
        logical_idx: usize,
        wrap_idx: usize,
    },
}

/// Markdown preview state, rendering, and interaction.
pub struct Preview {
    source: String,
    nodes: Vec<Node>,
    /// Width-independent rendered content.
    logical_lines: Vec<LogicalLine>,
    /// One entry per terminal row.
    layout_lines: LayoutLines,
    state: RefCell<ListState>,
    theme: Theme,
    keymap: DerivedConfig<Action>,
    search: PreviewSearch,
    spinner: Spinner,
    inline_max_lines_cap: usize,
    inline_max_lines: Cell<usize>,
    last_area: Cell<Rect>,
    selection: PreviewSelection,
    /// If set, jump to this code block on the next layout rebuild.
    target_block: Cell<Option<CodeId>>,
    /// Transient: prefer this block's task status over active gutter.
    prefer_status_gutter: Cell<Option<CodeId>>,
    /// Code blocks in document order with dense one-based IDs.
    code_index: Codes,
    /// Transient result of the last clipboard copy attempt (None = no copy).
    copy_result: Cell<Option<bool>>,
    /// Prefix overhead in chars per code block (non-zero only for blockquote-nested blocks).
    code_prefix_overhead: HashMap<CodeId, usize>,
    /// Cache of images referenced by the document, keyed by resolved path.
    images: RefCell<ImageCache>,
    /// Directory that relative image paths resolve against (the open document's dir).
    image_base_dir: std::path::PathBuf,
    /// Current render mode (visual vs source-preserving markup).
    mode: Cell<RenderMode>,
}

#[derive(KeyMap, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action {
    /// Scrolls the preview content up by one line (mouse-triggered).
    ScrollUp,
    /// Scrolls the preview content down by one line (mouse-triggered).
    ScrollDown,
    /// Toggles table of contents mode.
    #[key("c")]
    ToggleToc,
    /// Toggles between visual and markup render modes.
    #[key("v")]
    ToggleMode,
    /// Jumps to a specific code block by ID.
    #[key("@digit")]
    Show(CodeId),
    /// Copies the current selection to the system clipboard.
    #[key("y")]
    Copy,
    /// Updates the text selection highlight (mouse-driven, no key binding).
    Select,
    /// Clicked on a code block line. Auto-select that code block.
    SelectCodeBlock(CodeId),
    /// Scrolls the viewport up by one page.
    #[key("pageup", "ctrl-b")]
    PageUp,
    /// Scrolls the viewport down by one page.
    #[key("pagedown", "ctrl-f")]
    PageDown,
}

impl Preview {
    /// Creates a preview from parsed AST nodes and code blocks.
    pub fn new(
        document: Document,
        theme: Theme,
        outputs: &HashMap<CodeId, Task>,
        inline_max_lines_cap: usize,
        keymap: DerivedConfig<Action>,
        image_base_dir: std::path::PathBuf,
    ) -> Self {
        let Document {
            source,
            nodes,
            codes,
            ..
        } = document;
        let mut preview = Self {
            source,
            nodes,
            logical_lines: vec![],
            layout_lines: LayoutLines::new(),
            state: RefCell::new(ListState::default()),
            theme: theme.clone(),
            keymap,
            search: PreviewSearch::new(),
            spinner: Spinner::default(),
            inline_max_lines_cap,
            inline_max_lines: Cell::new(INLINE_MAX_LINES_DEFAULT),
            last_area: Cell::new(Rect::default()),
            selection: PreviewSelection::new(),
            target_block: Cell::new(None),
            prefer_status_gutter: Cell::new(None),
            copy_result: Cell::new(None),
            code_index: codes,
            code_prefix_overhead: HashMap::new(),
            images: RefCell::new(ImageCache::new()),
            image_base_dir,
            mode: Cell::new(RenderMode::Visual),
        };
        preview.rebuild_view(outputs);
        if !preview.layout_lines.is_empty() {
            preview.state.borrow_mut().select(Some(0));
        }
        preview
    }

    pub fn prefer_status_gutter_for(&self, id: CodeId) {
        self.prefer_status_gutter.set(Some(id));
    }

    pub fn toggle_mode(&self) {
        self.mode.set(match self.mode.get() {
            RenderMode::Visual => RenderMode::Markup,
            RenderMode::Markup => RenderMode::Visual,
        });
    }

    /// Requests loading of images in the viewport and a small overdraw margin.
    fn request_visible_images(&self) {
        let viewport = self.layout_lines.last_height();
        let offset = self.state.borrow().offset();
        let overdraw = viewport / OVERDRAW_FRACTION;
        let start = offset.saturating_sub(overdraw);
        let end = offset
            .saturating_add(viewport)
            .saturating_add(overdraw)
            .min(self.layout_lines.len());
        let layout_lines = self.layout_lines.borrow();
        let mut images = self.images.borrow_mut();
        for src in layout_lines[start..end]
            .iter()
            .filter(|line| line.wrap_idx == 0)
            .filter_map(|line| self.logical_lines[line.logical_idx].image_src())
        {
            images.request(src, &self.image_base_dir);
        }
    }

    pub(crate) fn take_image_requests(&self) -> Vec<std::path::PathBuf> {
        self.images.borrow_mut().take_requests()
    }

    pub(crate) fn complete_image(&self, decoded: DecodedImage) {
        let width = self.image_width(self.layout_lines.last_width());
        if self.images.borrow_mut().complete(decoded, width) {
            self.rebuild_layout_lines(self.layout_lines.last_width());
        }
    }

    /// Rebuilds logical content, then its width-dependent layout.
    ///
    /// Called when the markdown source changes (new document, code output update)
    /// or when the theme / inline cap changes.
    #[tracing::instrument(level = "info", skip_all, fields(lines))]
    pub fn rebuild_view(&mut self, outputs: &HashMap<CodeId, Task>) {
        self.rebuild_view_at_width(outputs, self.layout_lines.last_width());
    }

    fn rebuild_view_at_width(&mut self, outputs: &HashMap<CodeId, Task>, width: usize) {
        let renderer = MarkdownRenderer::new(
            &self.source,
            &self.theme,
            outputs,
            &self.code_index,
            self.inline_max_lines.get(),
            width,
        )
        .mode(self.mode.get());
        let mut old_lines = std::mem::take(&mut self.logical_lines);
        let rendered = renderer.render(&self.nodes);
        tracing::Span::current().record("lines", rendered.lines.len());
        self.logical_lines = rendered.lines;
        self.code_prefix_overhead = rendered.code_prefix_overhead;

        // Reuse caches from the previous render for unchanged lines. HTML
        // blocks are reused by whole-block source; other lines match on text.
        let old_html: HashMap<String, Rc<MarkdownHtml>> = old_lines
            .iter()
            .filter_map(LogicalLine::html_block)
            .map(|block| (block.source().to_owned(), Rc::clone(block)))
            .collect();
        let mut old_raw_iter = old_lines
            .iter_mut()
            .filter_map(LogicalLine::lazy_text_mut)
            .peekable();

        for line in &mut self.logical_lines {
            if let Some(block) = line.html_block() {
                if let Some(cached) = old_html.get(block.source()) {
                    line.reuse_html_block(cached);
                }
                continue;
            }
            let Some(text) = line.lazy_text_mut() else {
                continue;
            };
            while let Some(old) = old_raw_iter.peek() {
                if text.text == old.text && text.language == old.language && text.spans == old.spans
                {
                    if let Some(old) = old_raw_iter.next() {
                        std::mem::swap(&mut text.cached, &mut old.cached);
                    }
                    break;
                }
                old_raw_iter.next();
            }
        }

        // Cache lower-cased searchable text for each logical line so search
        // navigation does not re-allocate on every keystroke.
        self.search.rebuild_texts(&self.logical_lines);

        if width > 0 {
            self.rebuild_layout_lines_preserving(width, &old_lines);
        }
    }

    /// Updates layout-dependent lines while preserving stable logical lines
    /// when only the viewport width changes.
    pub fn resize(&mut self, outputs: &HashMap<CodeId, Task>, width: usize, height: usize) {
        let inline_max_changed = self.set_inline_max_lines(height);
        self.images.borrow_mut().set_width(self.image_width(width));
        if inline_max_changed {
            self.rebuild_view_at_width(outputs, width);
        } else {
            self.rebuild_layout_lines(width);
        }
    }

    /// Rebuilds layout lines from the current logical lines.
    #[tracing::instrument(level = "info", skip_all, fields(n_logical, n_layout, width))]
    fn rebuild_layout_lines(&self, width: usize) {
        self.rebuild_layout_lines_preserving(width, &self.logical_lines);
    }

    fn rebuild_layout_lines_preserving(
        &self,
        width: usize,
        previous_logical_lines: &[LogicalLine],
    ) {
        let previous_selection = self.selected_layout_line_identity(previous_logical_lines);
        let (previous_selected, previous_offset) = {
            let state = self.state.borrow();
            (state.selected(), state.offset())
        };
        let previous_code_rows = previous_selected
            .and_then(|idx| {
                self.layout_lines
                    .get(idx)
                    .and_then(|line| line.code_id(previous_logical_lines))
            })
            .and_then(|id| {
                self.layout_extent_for_code(id, previous_logical_lines)
                    .map(|(_, rows)| (id, rows))
            });

        self.rebuild_layout_lines_from(
            width,
            previous_selection,
            previous_selected,
            previous_offset,
            previous_code_rows,
        );
    }

    fn rebuild_layout_lines_from(
        &self,
        width: usize,
        previous_selection: Option<LayoutLineIdentity>,
        previous_selected: Option<usize>,
        previous_offset: usize,
        previous_code_rows: Option<(CodeId, usize)>,
    ) {
        if width == 0 {
            return;
        }
        let span = tracing::Span::current();
        let n_logical = self.logical_lines.len();
        let selected = self.layout_lines.rebuild(
            &self.logical_lines,
            width,
            &self.theme,
            &self.target_block,
            |line| {
                if let Some(src) = line.image_src() {
                    self.images.borrow().rows(src, &self.image_base_dir)
                } else {
                    1
                }
            },
        );
        if let Some(idx) = selected {
            // Explicit jump → scroll to target.
            let mut state = self.state.borrow_mut();
            state.select(Some(idx));
            *state.offset_mut() = idx;
        } else if let Some(idx) = previous_selection.and_then(|id| self.layout_idx_for_identity(id))
        {
            // Passive rebuild → preserve viewport row, tail-follow if
            // selected block's inline output grew past bottom.
            let mut offset = previous_selected.map_or(previous_offset, |previous_idx| {
                if idx >= previous_idx {
                    previous_offset.saturating_add(idx - previous_idx)
                } else {
                    previous_offset.saturating_sub(previous_idx - idx)
                }
            });
            if let Some((code_id, previous_rows)) = previous_code_rows {
                if self
                    .layout_lines
                    .get(idx)
                    .is_some_and(|line| line.code_id(&self.logical_lines) == Some(code_id))
                {
                    offset =
                        self.offset_following_grown_code_bottom(offset, code_id, previous_rows);
                }
            }
            let mut state = self.state.borrow_mut();
            state.select(Some(idx));
            *state.offset_mut() = offset;
        }
        span.record("n_logical", n_logical);
        span.record("n_layout", self.layout_lines.len());
        span.record("width", width);
        self.clamp_state_to_layout_lines();
    }

    /// Keeps list and selection state valid after layout changes.
    fn clamp_state_to_layout_lines(&self) {
        let len = self.layout_lines.len();
        let mut state = self.state.borrow_mut();

        if len == 0 {
            state.select(None);
            *state.offset_mut() = 0;
            self.selection.clear();
            return;
        }

        let max_idx = len - 1;
        if let Some(selected) = state.selected() {
            state.select(Some(selected.min(max_idx)));
        }
        let offset = state.offset();
        *state.offset_mut() = offset.min(max_idx);

        // Selection stores layout-line indices; clamp or clear if the range
        // changed underneath it.
        self.selection.clamp_or_clear(len);
    }

    /// Counts the number of heading lines at or before the given logical line index.
    pub fn heading_count_at_line(&self, logical_idx: usize) -> usize {
        self.logical_lines[..=logical_idx]
            .iter()
            .filter(|line| line.heading_level().is_some())
            .count()
            .saturating_sub(1)
    }

    pub fn selected_logical_line(&self) -> Option<usize> {
        let layout_idx = self.state.borrow().selected()?;
        self.layout_lines.get(layout_idx).map(|l| l.logical_idx)
    }

    fn selected_layout_line_identity(
        &self,
        logical_lines: &[LogicalLine],
    ) -> Option<LayoutLineIdentity> {
        let layout_idx = self.state.borrow().selected()?;
        let layout_lines = self.layout_lines.borrow();
        let line = layout_lines.get(layout_idx)?;

        match line.code_id(logical_lines) {
            Some(id) => {
                let first_logical_idx = layout_lines
                    .iter()
                    .find(|line| line.code_id(logical_lines) == Some(id))?
                    .logical_idx;
                Some(LayoutLineIdentity::Code {
                    id,
                    line_idx: line.logical_idx.saturating_sub(first_logical_idx),
                    wrap_idx: line.wrap_idx,
                })
            }
            None => Some(LayoutLineIdentity::Document {
                node_idx: logical_lines[line.logical_idx].node_idx,
                logical_idx: line.logical_idx,
                wrap_idx: line.wrap_idx,
            }),
        }
    }

    fn layout_idx_for_identity(&self, identity: LayoutLineIdentity) -> Option<usize> {
        match identity {
            LayoutLineIdentity::Code {
                id,
                line_idx,
                wrap_idx,
            } => self.layout_idx_for_code_identity(id, line_idx, wrap_idx),
            LayoutLineIdentity::Document {
                node_idx,
                logical_idx,
                wrap_idx,
            } => self.layout_idx_for_document_identity(node_idx, logical_idx, wrap_idx),
        }
    }

    fn layout_idx_for_code_identity(
        &self,
        id: CodeId,
        line_idx: usize,
        wrap_idx: usize,
    ) -> Option<usize> {
        let layout_lines = self.layout_lines.borrow();
        let first_logical_idx = layout_lines
            .iter()
            .find(|line| line.code_id(&self.logical_lines) == Some(id))?
            .logical_idx;
        let logical_idx = first_logical_idx + line_idx;

        layout_lines
            .iter()
            .position(|line| line.logical_idx == logical_idx && line.wrap_idx == wrap_idx)
            .or_else(|| {
                layout_lines
                    .iter()
                    .position(|line| line.logical_idx == logical_idx)
            })
    }

    fn layout_idx_for_document_identity(
        &self,
        node_idx: Option<usize>,
        logical_idx: usize,
        wrap_idx: usize,
    ) -> Option<usize> {
        let layout_lines = self.layout_lines.borrow();

        // Prefer the same wrapped row within the AST node, then the first row
        // from that node before using the previous logical index.
        node_idx
            .and_then(|node_idx| {
                layout_lines
                    .iter()
                    .position(|line| {
                        self.logical_lines[line.logical_idx].node_idx == Some(node_idx)
                            && line.wrap_idx == wrap_idx
                    })
                    .or_else(|| {
                        layout_lines.iter().position(|line| {
                            self.logical_lines[line.logical_idx].node_idx == Some(node_idx)
                        })
                    })
            })
            .or_else(|| {
                layout_lines
                    .iter()
                    .position(|line| line.logical_idx == logical_idx && line.wrap_idx == wrap_idx)
                    .or_else(|| {
                        layout_lines
                            .iter()
                            .position(|line| line.logical_idx == logical_idx)
                    })
            })
    }

    fn layout_extent_for_code(
        &self,
        id: CodeId,
        logical_lines: &[LogicalLine],
    ) -> Option<(usize, usize)> {
        let layout_lines = self.layout_lines.borrow();
        let mut indices = layout_lines
            .iter()
            .enumerate()
            .filter_map(|(idx, line)| (line.code_id(logical_lines) == Some(id)).then_some(idx));
        let first = indices.next()?;
        let end = indices.next_back().unwrap_or(first);
        Some((end, end - first + 1))
    }

    fn offset_following_grown_code_bottom(
        &self,
        offset: usize,
        id: CodeId,
        previous_rows: usize,
    ) -> usize {
        let viewport = self.layout_lines.last_height();
        if viewport == 0 {
            return offset;
        }
        let Some((end, rows)) = self.layout_extent_for_code(id, &self.logical_lines) else {
            return offset;
        };
        if rows <= previous_rows || end < offset.saturating_add(viewport) {
            return offset;
        }
        end.saturating_add(1).saturating_sub(viewport)
    }

    /// Reserves rows below a code block for an alternate-screen PTY and
    /// minimally scrolls the preview when the remaining viewport is too small.
    pub fn fit_inline_pty_rows(&self, id: CodeId, viewport: usize) -> Option<usize> {
        let (start, end) = self.source_layout_extent(id)?;
        let source_rows = end.saturating_sub(start).saturating_add(1);
        let offset = self.state.borrow().offset();
        let (rows, new_offset) = inline_pty_rows(viewport, end, source_rows, offset);

        if new_offset != offset {
            *self.state.borrow_mut().offset_mut() = new_offset;
        }
        Some(rows)
    }

    fn source_layout_extent(&self, id: CodeId) -> Option<(usize, usize)> {
        let layout_lines = self.layout_lines.borrow();
        let mut indices = layout_lines.iter().enumerate().filter_map(|(idx, line)| {
            let logical_line = line.logical(&self.logical_lines);
            (logical_line.code_id == Some(id)
                && (logical_line.is_code_info() || logical_line.is_code_body()))
            .then_some(idx)
        });
        let first = indices.next()?;
        let end = indices.next_back().unwrap_or(first);
        Some((first, end))
    }

    pub fn inline_max_lines(&self) -> usize {
        self.inline_max_lines.get()
    }

    fn layout_idx_of_logical(&self, logical_idx: usize) -> Option<usize> {
        self.layout_lines.layout_idx_of_logical(logical_idx)
    }

    fn set_inline_max_lines(&self, height: usize) -> bool {
        let max_inline = (height / INLINE_MAX_LINES_FRACTION)
            .clamp(INLINE_MAX_LINES_MIN, self.inline_max_lines_cap);
        self.inline_max_lines.replace(max_inline) != max_inline
    }

    pub fn tick(&mut self) {
        self.spinner.tick();
    }

    /// Width available to images for a given preview width, in columns.
    fn image_width(&self, preview_width: usize) -> usize {
        preview_width.saturating_sub(PREVIEW_FRAME_OVERHEAD).max(1)
    }

    pub fn search(&mut self, term: &str) -> Vec<usize> {
        self.search.set_term(term);
        if term.is_empty() {
            return vec![];
        }
        self.search.matches(&self.layout_lines.borrow())
    }

    pub fn select_search_match(&mut self, idx: usize) -> Option<CodeId> {
        let max = self.layout_lines.len().saturating_sub(1);
        self.select_and_scroll_smooth(idx.min(max));
        self.selected_code_id()
    }

    pub fn set_theme(&mut self, theme: &Theme) {
        self.theme.clone_from(theme);
        self.logical_lines.iter().for_each(|l| l.clear_cache());
    }

    pub fn selected_code_id(&self) -> Option<CodeId> {
        let selected = self.state.borrow().selected()?;
        self.layout_lines
            .get(selected)?
            .code_id(&self.logical_lines)
    }

    /// Returns the 0-based row within the preview content area, or `None` if
    /// the mouse row is outside the content vertical bounds (e.g. on the border).
    fn mouse_content_rel_row(&self, mouse: &crossterm::event::MouseEvent) -> Option<usize> {
        let area = self.last_area.get();
        let content_y = area.y + PREVIEW_CONTENT_TOP_OFFSET;
        let content_bottom = area.y + area.height.saturating_sub(BORDER_HEIGHT as u16);
        if mouse.row < content_y || mouse.row >= content_bottom {
            return None;
        }
        Some(mouse.row.saturating_sub(content_y) as usize)
    }

    /// Returns the code block owning the layout row under a mouse event.
    ///
    /// Hit-tests by viewport row only. This treats code info, code body, and
    pub fn code_id_at_mouse(&self, mouse: &crossterm::event::MouseEvent) -> Option<CodeId> {
        if !crate::utils::mouse_in_area(mouse, self.last_area.get()) {
            return None;
        }
        let rel_row = self.mouse_content_rel_row(mouse)?;
        let layout_idx = self.state.borrow().offset() + rel_row;
        self.layout_lines
            .get(layout_idx)?
            .code_id(&self.logical_lines)
    }

    /// Looks up the raw `Code` node by ID from the flat Vec index.
    /// Returns all code blocks in document order.
    pub fn codes(&self) -> &Codes {
        &self.code_index
    }

    pub fn code_by_id(&self, id: CodeId) -> Option<&upmd_parser::nodes::Code> {
        self.code_index.by_id(id)
    }

    /// Converts a mouse click on the selected code block into PTY-relative
    /// SGR coordinates `(col, row)`, 1-based.
    /// Returns `None` when the click falls outside the code block's layout
    /// extent or when the block is not visible.
    pub fn mouse_to_pty_coords(
        &self,
        id: CodeId,
        mouse: &crossterm::event::MouseEvent,
        pty_cols: u16,
        pty_rows: u16,
    ) -> Option<(u16, u16)> {
        let area = self.last_area.get();
        let rel_row = self.mouse_content_rel_row(mouse)?;
        let state_offset = self.state.borrow().offset();
        let layout_idx = state_offset + rel_row;

        let block_first = self.layout_lines.find_code_start(id, &self.logical_lines)?;

        // The clicked layout line must belong to the selected code block.
        match self.layout_lines.get(layout_idx) {
            Some(line) if line.code_id(&self.logical_lines) == Some(id) => {}
            _ => return None,
        }

        let pty_row = layout_idx - block_first + 1;
        if pty_row > pty_rows as usize {
            return None;
        }

        let col = mouse
            .column
            .saturating_sub(
                area.x + PREVIEW_CONTENT_X_OFFSET + self.code_prefix_overhead(id) as u16,
            )
            .saturating_add(1)
            .min(pty_cols);

        Some((col, pty_row as u16))
    }

    /// Returns the prefix overhead in chars for a code block (e.g. 2 for "> " inside a blockquote).
    pub fn code_prefix_overhead(&self, id: CodeId) -> usize {
        self.code_prefix_overhead.get(&id).copied().unwrap_or(0)
    }

    /// Returns the selected layout-line index, or `0`.
    fn selected_idx(&self) -> usize {
        self.state.borrow().selected().unwrap_or(0)
    }

    /// Builds a [`CopyLine`] from a layout line.
    fn copy_line_at(&self, line_idx: usize) -> Option<CopyLine> {
        let line = self.layout_lines.get(line_idx)?;
        let ctx = RenderContext {
            theme: &self.theme,
            active_code_id: None,
            prefer_status_gutter: None,
            spinner_char: ' ',
            viewport_width: self.layout_lines.last_width(),
        };
        let rendered = line.render_plain(&self.logical_lines[line.logical_idx], &ctx);
        let mut text: String = rendered
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        let has_code_gutter = self.logical_lines[line.logical_idx].has_code_gutter();
        let display_prefix_len = match (has_code_gutter, text.strip_prefix("▎ ")) {
            (true, Some(stripped)) => {
                text = stripped.to_string();
                CODE_GUTTER_WIDTH
            }
            (true, None) if line.wrap_idx > 0 => CODE_GUTTER_WIDTH,
            _ => 0,
        };
        Some(CopyLine {
            text,
            is_continuation: line.char_range.start > 0,
            display_prefix_len,
        })
    }

    pub fn page_down(&mut self) {
        let lh = self.layout_lines.last_height();
        let current = self.selected_idx();
        let next = (current + lh).min(self.layout_lines.len().saturating_sub(1));
        self.select_and_scroll_smooth(next);
    }

    pub fn page_up(&mut self) {
        let lh = self.layout_lines.last_height();
        let current = self.selected_idx();
        let next = current.saturating_sub(lh);
        self.select_and_scroll_smooth(next);
    }

    pub fn scroll_down(&mut self) {
        let len = self.layout_lines.len();
        if len == 0 {
            let mut state = self.state.borrow_mut();
            state.select(None);
            *state.offset_mut() = 0;
            return;
        }
        let mut state = self.state.borrow_mut();
        let next = (state.offset() + 1).min(len.saturating_sub(1));
        state.select(Some(next));
        *state.offset_mut() = next;
    }

    pub fn scroll_up(&mut self) {
        let mut state = self.state.borrow_mut();
        if self.layout_lines.is_empty() {
            state.select(None);
            *state.offset_mut() = 0;
            return;
        }
        let next = state.offset().saturating_sub(1);
        state.select(Some(next));
        *state.offset_mut() = next;
    }

    fn select_and_scroll_smooth(&mut self, idx: usize) {
        let mut state = self.state.borrow_mut();
        state.select(Some(idx));
        *state.offset_mut() = idx;
    }

    /// Selects a code block by ID, snapping it to the viewport top unless
    /// enough of the block is already visible to make the selection clear.
    pub fn select_code(&mut self, id: CodeId) {
        let Some(idx) = self.layout_lines.find_code_start(id, &self.logical_lines) else {
            self.target_block.set(Some(id));
            return;
        };

        if self.has_code_navigation_context(idx) {
            self.target_block.set(None);
            self.state.borrow_mut().select(Some(idx));
        } else {
            self.target_block.set(Some(id));
            self.select_and_scroll_smooth(idx);
        }
    }

    /// Selects an already-visible code block without moving the viewport.
    pub fn select_code_in_place(&mut self, id: CodeId) {
        self.target_block.set(None);
        if let Some(idx) = self.layout_lines.find_code_start(id, &self.logical_lines) {
            self.state.borrow_mut().select(Some(idx));
        }
    }

    /// Selects the Nth heading, snapping to it only when off-screen.
    pub fn select_heading(&mut self, heading_idx: usize) {
        let mut count = 0;
        for (logical_idx, line) in self.logical_lines.iter().enumerate() {
            if line.heading_level().is_some() {
                if count == heading_idx {
                    if let Some(layout_idx) = self.layout_idx_of_logical(logical_idx) {
                        let (offset, height) = {
                            let state = self.state.borrow();
                            (state.offset(), self.layout_lines.last_height())
                        };
                        if layout_idx >= offset && layout_idx < offset + height {
                            self.state.borrow_mut().select(Some(layout_idx));
                        } else {
                            self.select_and_scroll_smooth(layout_idx);
                        }
                    }
                    return;
                }
                count += 1;
            }
        }
    }

    fn has_code_navigation_context(&self, idx: usize) -> bool {
        let state = self.state.borrow();
        let offset = state.offset();
        let height = self.layout_lines.last_height();
        let required_rows = CODE_NAVIGATION_CONTEXT_ROWS.min(height);

        height > 0
            && idx >= offset
            && idx.saturating_add(required_rows) <= offset.saturating_add(height)
    }

    /// Takes the result of the most recent clipboard copy attempt.
    pub fn take_copy_result(&self) -> Option<bool> {
        self.copy_result.replace(None)
    }
}

impl Input for Preview {
    fn action(&self, event: crossterm::event::Event) -> Option<Self::Action> {
        match event {
            crossterm::event::Event::Key(key) => {
                if let Some(action) = self.keymap.get_bound(&key) {
                    return Some(action);
                }
            }
            crossterm::event::Event::Mouse(mouse) => {
                return self.handle_mouse_event(mouse);
            }
            _ => {}
        }
        None
    }
}

impl Preview {
    /// Handles mouse events: scroll, click-to-select code block, and text selection.
    fn handle_mouse_event(&self, mouse: crossterm::event::MouseEvent) -> Option<Action> {
        use crossterm::event::{MouseButton, MouseEventKind};

        match mouse.kind {
            MouseEventKind::ScrollUp => Some(Action::ScrollUp),
            MouseEventKind::ScrollDown => Some(Action::ScrollDown),
            MouseEventKind::Down(MouseButton::Left) => {
                let area = self.last_area.get();
                // Only process clicks inside the preview area.
                if !crate::utils::mouse_in_area(&mouse, area) {
                    return None;
                }
                let pos = self.mouse_selection_position(area, mouse.row, mouse.column);
                if let Some((layout_idx, char_idx)) = pos {
                    // Tracks the clicked layout line for selection/menu sync without
                    // moving the viewport. Mouse-wheel scroll uses the viewport
                    // offset, so scroll-after-click continues from the current view.
                    self.state.borrow_mut().select(Some(layout_idx));
                    let pending_code = self
                        .layout_lines
                        .get(layout_idx)
                        .and_then(|line| line.code_id(&self.logical_lines));
                    self.selection.set_pending_code_click(pending_code);
                    self.selection.start(layout_idx, char_idx);
                } else {
                    self.selection.set_pending_code_click(None);
                    self.selection.clear();
                }
                Some(Action::Select)
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if !self.selection.is_dragging() {
                    return None;
                }
                self.selection.set_pending_code_click(None);
                let area = self.last_area.get();
                let content_y = area.y + PREVIEW_CONTENT_TOP_OFFSET;
                let content_bottom = area.y + area.height.saturating_sub(BORDER_HEIGHT as u16);
                let clamped_row = mouse.row.clamp(content_y, content_bottom);
                let clamped_col = mouse.column.clamp(
                    area.x + PREVIEW_CONTENT_X_OFFSET,
                    area.x + area.width.saturating_sub(PREVIEW_CONTENT_X_OFFSET),
                );
                let pos = self.mouse_selection_position(area, clamped_row, clamped_col);
                if let Some((layout_idx, char_idx)) = pos {
                    self.selection.extend(layout_idx, char_idx);
                    Some(Action::Select)
                } else {
                    None
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                let area = self.last_area.get();
                // Only process releases inside the preview area.
                if !crate::utils::mouse_in_area(&mouse, area) {
                    return None;
                }
                let text = self
                    .selection
                    .finish(|line_idx| self.copy_line_at(line_idx));
                if let Some(text) = text {
                    self.copy_result
                        .set(Some(crate::utils::clipboard_copy(&text)));
                } else {
                    self.copy_result.set(None);
                }
                if let Some(code_id) = self.selection.take_pending_code_click() {
                    Some(Action::SelectCodeBlock(code_id))
                } else {
                    Some(Action::Select)
                }
            }
            _ => None,
        }
    }

    fn mouse_selection_position(
        &self,
        area: Rect,
        row: u16,
        column: u16,
    ) -> Option<(usize, usize)> {
        let layout_lines = self.layout_lines.borrow();
        let offset = self.state.borrow().offset();
        let ctx = RenderContext {
            theme: &self.theme,
            active_code_id: None,
            prefer_status_gutter: None,
            spinner_char: ' ',
            viewport_width: self.layout_lines.last_width(),
        };

        SelectionState::mouse_to_position(
            area,
            row,
            column,
            PREVIEW_CONTENT_X_OFFSET,
            |relative_row| {
                let layout_idx = offset + relative_row;
                let layout_line = layout_lines.get(layout_idx)?;
                let logical_line = layout_line.logical(&self.logical_lines);
                let rendered_line = logical_line.render_plain(&ctx);
                Some((
                    layout_idx,
                    layout_line.render(logical_line, &rendered_line, &ctx),
                ))
            },
        )
    }
}

/// Computes how many PTY rows fit below a block's source lines in the viewport,
/// returning `(rows, new_offset)`. Scrolls the viewport if fewer than 40% of
/// the rows (min 8) remain below the source.
fn inline_pty_rows(
    viewport: usize,
    source_end: usize,
    source_rows: usize,
    offset: usize,
) -> (usize, usize) {
    if viewport == 0 {
        return (1, offset);
    }

    let target = ((viewport * INLINE_PTY_MIN_PERCENT).div_ceil(100))
        .max(INLINE_PTY_MIN_ROWS)
        .min(viewport)
        .min(viewport.saturating_sub(source_rows).max(1));

    let available = if source_end < offset {
        viewport
    } else {
        viewport
            .saturating_sub(source_end.saturating_sub(offset).saturating_add(1))
            .max(1)
    };

    if available >= target {
        (available, offset)
    } else {
        let new_offset = source_end
            .saturating_add(1)
            .saturating_add(target)
            .saturating_sub(viewport);
        (target, new_offset)
    }
}

impl Component for Preview {
    type Action = Action;
    type Outcome = NoOutcome;

    fn update(&mut self, action: Action) -> Option<Effect<Action, NoOutcome>> {
        match action {
            Action::ScrollUp => self.scroll_up(),
            Action::ScrollDown => self.scroll_down(),
            Action::PageUp => self.page_up(),
            Action::PageDown => self.page_down(),
            Action::ToggleToc => {}
            Action::ToggleMode => self.toggle_mode(),
            Action::Copy => {
                let ok = if let Some(text) = self
                    .selection
                    .copy_text(|line_idx| self.copy_line_at(line_idx))
                {
                    crate::utils::clipboard_copy(&text)
                } else {
                    false
                };
                self.copy_result.set(Some(ok));
            }
            Action::SelectCodeBlock(id) => self.select_code_in_place(id),
            Action::Show(id) => self.select_code(id),
            Action::Select => {}
        }
        None
    }
}

impl Output for Preview {
    /// Renders the preview into the given terminal `area`.
    ///
    /// Only draws the visible viewport window (plus a small overdraw margin) so
    /// large documents remain cheap to render.  Code blocks are re-evaluated each
    /// frame to pick up spinner changes and active-code styling, but unchanged
    /// text benefits from the [`LogicalLine`](crate::apps::tui::markdown::LogicalLine) cache.
    fn render(&self, frame: &mut Frame, area: Rect) {
        let height = area.height as usize;
        let width = area.width as usize;
        self.layout_lines
            .set_last_height(height.saturating_sub(BORDER_HEIGHT));
        self.last_area.set(area);

        self.request_visible_images();

        if self.layout_lines.last_width() != width && width > 0 {
            self.images.borrow_mut().set_width(self.image_width(width));
            self.rebuild_layout_lines(width);
        }

        let layout_lines = self.layout_lines.borrow();
        if layout_lines.is_empty() {
            return;
        }

        let viewport = height.saturating_sub(BORDER_HEIGHT);
        let overdraw = viewport / OVERDRAW_FRACTION;
        let state = self.state.borrow();
        let original_offset = state.offset();
        let original_selected = state.selected();
        drop(state);

        let win_start = original_offset.saturating_sub(overdraw);
        let win_end = (original_offset + viewport + overdraw).min(layout_lines.len());
        let window = &layout_lines[win_start..win_end];

        let selected_idx = original_selected.unwrap_or(0);
        let active_code_id = layout_lines
            .get(selected_idx)
            .and_then(|line| line.code_id(&self.logical_lines));
        // Status gutter persists while selected; consumed on navigate-away.
        let prefer_status_gutter = match self.prefer_status_gutter.get() {
            Some(id) if active_code_id == Some(id) => Some(id),
            Some(_) => {
                self.prefer_status_gutter.set(None);
                None
            }
            None => None,
        };

        let ctx = RenderContext {
            theme: &self.theme,
            active_code_id,
            prefer_status_gutter,
            spinner_char: self.spinner.render(),
            viewport_width: width,
        };

        self.prefetch_content(&layout_lines, original_offset, viewport, &ctx);

        // Wrapped rows share one rendered logical line per frame.
        let mut rendered_lines = HashMap::new();
        let mut items = Vec::with_capacity(window.len());
        let mut image_rows = Vec::new();
        for (win_i, layout_line) in window.iter().enumerate() {
            let layout_idx = win_start + win_i;
            let logical_idx = layout_line.logical_idx;
            let logical_line = &self.logical_lines[logical_idx];

            // Render each logical line and register each image once.
            let rendered_line = rendered_lines.entry(logical_idx).or_insert_with(|| {
                if logical_line.is_image() {
                    let first_global = layout_idx as i32 - layout_line.wrap_idx as i32;
                    image_rows.push((logical_idx, first_global));
                }
                logical_line.render(&ctx)
            });
            let mut line = layout_line.render(logical_line, rendered_line, &ctx);
            if let Some(term) = self.search.term() {
                line = highlight_line(line, term, self.theme.search_highlight_style());
            }

            if let Some((sel_start, sel_end)) = self
                .selection
                .range_for_line(layout_idx, line.to_string().chars().count())
            {
                line = SelectionState::apply_range(
                    line,
                    sel_start,
                    sel_end,
                    self.theme.selection_style(),
                );
            }

            items.push(ListItem::new(Text::from(line)));
        }

        drop(layout_lines);

        let mut render_state = *self.state.borrow();
        *render_state.offset_mut() = original_offset.saturating_sub(win_start);
        if let Some(sel) = original_selected {
            render_state.select(Some(sel.saturating_sub(win_start)));
        }

        let block = self
            .theme
            .block()
            .borders(Borders::ALL)
            .border_style(self.theme.inactive_style())
            .padding(Padding::horizontal(1));

        frame.render_stateful_widget(List::new(items).block(block), area, &mut render_state);

        self.render_images(frame, area, original_offset, &image_rows);
    }
}

impl Preview {
    fn prefetch_content(
        &self,
        layout_lines: &[LayoutLine],
        offset: usize,
        viewport: usize,
        ctx: &RenderContext<'_>,
    ) {
        let start = offset.saturating_sub(viewport);
        let end = offset
            .saturating_add(viewport.saturating_mul(2))
            .min(layout_lines.len());
        let mut prefetched = HashSet::new();
        let mut populated_caches = 0;
        for layout_line in &layout_lines[start..end] {
            if prefetched.insert(layout_line.logical_idx)
                && layout_line
                    .logical(&self.logical_lines)
                    .ensure_rendered(ctx)
            {
                populated_caches += 1;
            }
        }
        if populated_caches > 0 {
            tracing::debug!(
                prefetched_lines = prefetched.len(),
                populated_caches,
                "prefetched preview content caches"
            );
        }
    }

    /// Draws loaded images over their reserved viewport rows.
    fn render_images(
        &self,
        frame: &mut Frame,
        area: Rect,
        original_offset: usize,
        image_rows: &[(usize, i32)],
    ) {
        use ratatui_image::sliced::SignedPosition;

        let images = self.images.borrow();
        let base_dir = &self.image_base_dir;
        let content_area = Rect::new(
            area.x + PREVIEW_CONTENT_X_OFFSET,
            area.y + PREVIEW_CONTENT_TOP_OFFSET,
            area.width.saturating_sub(PREVIEW_FRAME_OVERHEAD as u16),
            area.height.saturating_sub(BORDER_HEIGHT as u16),
        );
        for (logical_idx, first_global) in image_rows {
            let Some(src) = self.logical_lines[*logical_idx].image_src() else {
                continue;
            };
            if images.protocol(src, base_dir).is_none() {
                continue;
            }
            let position = SignedPosition::from((
                0,
                i16::try_from(first_global - original_offset as i32).unwrap_or(i16::MIN),
            ));
            images.render(frame, src, base_dir, content_area, position);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apps::tui::testutil::ansi_line_summary;
    use insta::assert_snapshot;
    use std::collections::HashMap;

    fn preview_from_markdown(markdown: &str) -> Preview {
        let doc = upmd_parser::new().parse(markdown);
        let theme = Theme::new("base16-ocean.dark", false);
        let outputs = HashMap::new();
        let keymap: DerivedConfig<Action> = toml::from_str("").unwrap();
        Preview::new(
            doc,
            theme,
            &outputs,
            10,
            keymap,
            std::path::PathBuf::from("."),
        )
    }

    #[test]
    fn html_block_cache_rebuilds() {
        let comment = "<!--\ninside comment\n-->\n";
        let div = "<div>\ninside element\n</div>\n";
        let block = |p: &Preview| {
            Rc::clone(
                p.logical_lines
                    .iter()
                    .find_map(LogicalLine::html_block)
                    .expect("HTML block"),
            )
        };

        for (name, updated, change_theme, expect_reuse) in [
            ("unchanged", comment, false, true),
            ("theme changed", comment, true, true),
            ("source changed", div, false, false),
        ] {
            let mut preview = preview_from_markdown(comment);
            let before = block(&preview);

            if change_theme {
                preview.set_theme(&Theme::new("base16-ocean.dark", true));
            }
            if updated != comment {
                let doc = upmd_parser::new().parse(updated);
                preview.source = doc.source;
                preview.nodes = doc.nodes;
                preview.code_index = doc.codes;
            }
            preview.rebuild_view(&HashMap::new());

            assert_eq!(
                Rc::ptr_eq(&before, &block(&preview)),
                expect_reuse,
                "{name}"
            );
        }
    }

    #[test]
    fn mode_toggle_switches_to_markup_and_preserves_code_selection() {
        let mut preview = preview_from_markdown("# Title\n\n```bash\necho hello\n```");
        preview.rebuild_layout_lines(60);
        preview.select_code(1);

        preview.toggle_mode();
        preview.rebuild_view(&HashMap::new());

        assert_eq!(preview.mode.get(), RenderMode::Markup);
        assert_eq!(preview.selected_code_id(), Some(1));
        let text = full_preview_text(&preview);
        assert!(text.contains("# Title"), "got: {text}");
        assert!(text.contains("echo hello"), "got: {text}");
    }

    #[test]
    fn mode_toggle_preserves_paragraph_selection_across_headings() {
        let markdown = "# One\n\n## Two\n\nmiddle paragraph\n\n# Three\n\n## Four\n\nend\n";
        let mut preview = preview_from_markdown(markdown);
        preview.rebuild_layout_lines(60);

        let target = "middle paragraph";
        let logical_idx = preview
            .logical_lines
            .iter()
            .position(|l| l.text_content() == target)
            .expect("paragraph logical line");
        let layout_idx = preview
            .layout_lines
            .borrow()
            .iter()
            .position(|l| l.logical_idx == logical_idx)
            .expect("paragraph layout line");
        {
            let mut state = preview.state.borrow_mut();
            state.select(Some(layout_idx));
            *state.offset_mut() = layout_idx;
        }

        for _ in 0..2 {
            preview.toggle_mode();
            preview.rebuild_view(&HashMap::new());
            let selected = preview
                .selected_logical_line()
                .expect("selection preserved across mode toggle");
            assert_eq!(
                preview.logical_lines[selected].text_content(),
                target,
                "mode {:?}",
                preview.mode.get()
            );
        }
    }

    #[test]
    fn mode_toggle_maps_heading_rule_to_heading() {
        let mut preview = preview_from_markdown("# One\n\nbody\n");
        preview.rebuild_layout_lines(60);
        let heading_idx = preview
            .logical_lines
            .iter()
            .position(|line| line.text_content() == "# One")
            .expect("heading line");
        let rule_idx = heading_idx + 1;
        let layout_idx = preview
            .layout_lines
            .borrow()
            .iter()
            .position(|line| line.logical_idx == rule_idx)
            .expect("heading rule layout line");
        preview.state.borrow_mut().select(Some(layout_idx));

        preview.toggle_mode();
        preview.rebuild_view(&HashMap::new());

        let selected = preview
            .selected_logical_line()
            .expect("heading selection preserved");
        assert_eq!(preview.logical_lines[selected].text_content(), "# One");
    }

    #[test]
    fn markup_mode_does_not_reserve_image_rows() {
        let mut preview = preview_from_markdown("![alt](image.png)");
        preview.toggle_mode();
        preview.rebuild_view(&HashMap::new());

        assert!(
            preview.logical_lines.iter().all(|l| !l.is_image()),
            "markup mode should emit images as text"
        );
    }

    #[test]
    fn image_continuation_rows_have_no_copy_text() {
        let preview = preview_from_markdown("![alt](image.png)");
        let row = LayoutLine {
            logical_idx: 0,
            wrap_idx: 1,
            char_range: 0..0,
        };

        assert!(row
            .render_plain(
                &preview.logical_lines[0],
                &RenderContext {
                    theme: &preview.theme,
                    active_code_id: None,
                    prefer_status_gutter: None,
                    spinner_char: ' ',
                    viewport_width: 80,
                }
            )
            .to_string()
            .is_empty());
    }

    fn render_layout_line(
        preview: &Preview,
        layout_line: &LayoutLine,
        ctx: &RenderContext<'_>,
    ) -> Line<'static> {
        let logical_line = &preview.logical_lines[layout_line.logical_idx];
        let rendered_line = logical_line.render(ctx);
        layout_line.render(logical_line, &rendered_line, ctx)
    }

    /// Renders every layout row to its final text, joined with newlines.
    fn full_preview_text(preview: &Preview) -> String {
        let ctx = RenderContext {
            theme: &preview.theme,
            active_code_id: None,
            prefer_status_gutter: None,
            spinner_char: ' ',
            viewport_width: preview.layout_lines.last_width(),
        };
        preview
            .layout_lines
            .borrow()
            .iter()
            .map(|vl| render_layout_line(preview, vl, &ctx).to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn snapshot_full_heading() {
        let preview = preview_from_markdown("# Title\n\n## Subtitle");
        preview.rebuild_layout_lines(60);
        assert_snapshot!("full_heading", full_preview_text(&preview));
    }

    #[test]
    fn snapshot_full_paragraph_wrap() {
        let preview = preview_from_markdown("A short paragraph.\n\nA much longer paragraph that will wrap when the available width is narrower than its content.");
        preview.rebuild_layout_lines(30);
        assert_snapshot!("full_paragraph_wrap", full_preview_text(&preview));
    }

    #[test]
    fn snapshot_full_fenced_code() {
        let preview = preview_from_markdown("```bash\necho hello\n```");
        preview.rebuild_layout_lines(60);
        assert_snapshot!("full_fenced_code", full_preview_text(&preview));
    }

    #[test]
    fn snapshot_full_code_wrap() {
        let preview = preview_from_markdown("```rust\nfn very_long_function_name_that_exceeds_narrow_width() -> VeryLongReturnType {}\n```");
        preview.rebuild_layout_lines(30);
        assert_snapshot!("full_code_wrap", full_preview_text(&preview));
    }

    #[test]
    fn snapshot_full_bullet_list() {
        let preview = preview_from_markdown("- item one\n- item two\n- item three");
        preview.rebuild_layout_lines(60);
        assert_snapshot!("full_bullet_list", full_preview_text(&preview));
    }

    #[test]
    fn snapshot_full_task_list() {
        let preview = preview_from_markdown("- [ ] unchecked\n- [x] checked\n- [ ] pending");
        preview.rebuild_layout_lines(60);
        assert_snapshot!("full_task_list", full_preview_text(&preview));
    }

    #[test]
    fn snapshot_full_blockquote() {
        let preview = preview_from_markdown("> quoted paragraph\n> ```bash\n> echo hi\n> ```");
        preview.rebuild_layout_lines(60);
        assert_snapshot!("full_blockquote", full_preview_text(&preview));
    }

    #[test]
    fn snapshot_full_table() {
        let preview =
            preview_from_markdown("| Name | Age |\n|------|-----|\n| Alice | 30 |\n| Bob | 25 |");
        preview.rebuild_layout_lines(60);
        assert_snapshot!("full_table", full_preview_text(&preview));
    }

    #[test]
    fn snapshot_full_thematic_break() {
        let preview = preview_from_markdown("above\n\n-----\n\nbelow");
        preview.rebuild_layout_lines(60);
        assert_snapshot!("full_thematic_break", full_preview_text(&preview));
    }

    #[test]
    fn snapshot_full_yaml_frontmatter() {
        let preview =
            preview_from_markdown("---\ntitle: Hello\ntags: [a, b]\n---\n# Doc\n\nBody text.\n");
        preview.rebuild_layout_lines(60);
        assert_snapshot!("full_yaml_frontmatter", full_preview_text(&preview));
    }

    #[test]
    fn snapshot_full_toml_frontmatter() {
        let preview = preview_from_markdown("+++\ntitle = \"Hello\"\nversion = 1\n+++\n# Doc\n");
        preview.rebuild_layout_lines(60);
        assert_snapshot!("full_toml_frontmatter", full_preview_text(&preview));
    }

    #[test]
    fn snapshot_full_mixed_runbook() {
        let preview = preview_from_markdown(
            "# Setup\n\nInstall deps.\n\n```bash\nnpm install\n```\n\n> note\n\n- a\n- b",
        );
        preview.rebuild_layout_lines(40);
        assert_snapshot!("full_mixed_runbook", full_preview_text(&preview));
    }

    #[test]
    fn snapshot_full_inline_styles() {
        let preview = preview_from_markdown(
            "**bold** *italic* ~~strike~~ `code` [link](https://example.com)\n\
             \n\
             ## Heading with **bold**\n\
             \n\
             - **bold item**\n\
             - *italic item*",
        );
        preview.rebuild_layout_lines(60);
        assert_snapshot!("full_inline_styles", full_preview_text(&preview));
    }

    #[test]
    fn wrapped_inline_styles_preserve_nested_modifiers() {
        let preview = preview_from_markdown(
            "start **[abcdefghijklmnopqrstuvwxyz](https://example.com)** end",
        );
        preview.rebuild_layout_lines(12);
        let ctx = RenderContext {
            theme: &preview.theme,
            active_code_id: None,
            prefer_status_gutter: None,
            spinner_char: ' ',
            viewport_width: preview.layout_lines.last_width(),
        };
        let rows: Vec<_> = preview
            .layout_lines
            .borrow()
            .iter()
            .map(|layout_line| render_layout_line(&preview, layout_line, &ctx))
            .collect();
        assert_snapshot!(
            "wrapped_inline_styles_nested_modifiers",
            ansi_line_summary(&rows)
        );
    }

    #[test]
    fn test_rebuild_clamps_selection_when_lines_shrink() {
        let mut preview = preview_from_markdown("This is a very long paragraph that should wrap into several rows at narrow widths but fit in fewer rows at wider widths.");
        preview.rebuild_layout_lines(20);
        let last_idx = preview.layout_lines.borrow().len().saturating_sub(1);
        {
            let mut state = preview.state.borrow_mut();
            state.select(Some(last_idx));
            *state.offset_mut() = last_idx;
        }
        preview.select_code(2);
        preview.rebuild_layout_lines(120);

        let len = preview.layout_lines.borrow().len();
        let state = preview.state.borrow();
        assert!(state.selected().is_some_and(|idx| idx < len));
        assert!(state.offset() < len);
    }

    #[test]
    fn test_deferred_block_jump_applies_during_full_rebuild() {
        let mut preview = preview_from_markdown(
            "# Intro\n\n```sh [name:first]\necho first\n```\n\nSome filler\n\n```sh [name:setup]\necho setup\n```\n",
        );

        preview.select_code(2);
        preview.rebuild_layout_lines(80);

        let state = preview.state.borrow();
        assert!(state.offset() > 0);
        assert_eq!(
            preview
                .layout_lines
                .get(state.offset())
                .and_then(|line| line.code_id(&preview.logical_lines)),
            Some(2)
        );
    }

    #[test]
    fn test_cached_code_line_updates_active_gutter() {
        let preview = preview_from_markdown("```bash\necho hello\n```");
        preview.rebuild_layout_lines(80);
        let layout_lines = preview.layout_lines.borrow();
        let code_body_line = layout_lines
            .iter()
            .find(|line| {
                line.wrap_idx == 0
                    && line.code_id(&preview.logical_lines) == Some(1)
                    && line.logical(&preview.logical_lines).is_code_body()
            })
            .expect("expected a code body layout line")
            .clone();
        drop(layout_lines);

        let ctx = RenderContext {
            theme: &preview.theme,
            active_code_id: Some(1),
            prefer_status_gutter: None,
            spinner_char: ' ',
            viewport_width: 80,
        };
        let rendered = render_layout_line(&preview, &code_body_line, &ctx);

        assert_eq!(
            rendered.spans.first().and_then(|s| s.style.fg),
            Some(preview.theme.active)
        );
    }

    #[test]
    fn test_cached_code_body_gutter_matches_code_info_prefer_status_gutter() {
        let doc = upmd_parser::new().parse("```bash\necho hello\n```");
        let theme = Theme::new("base16-ocean.dark", false);
        let mut outputs = HashMap::new();
        let mut output = Task::new(80, 24, 500);
        output.done = true;
        output.exit_code = Some(0);
        outputs.insert(1, output);
        let keymap: DerivedConfig<Action> = toml::from_str("").unwrap();
        let preview = Preview::new(
            doc,
            theme,
            &outputs,
            10,
            keymap,
            std::path::PathBuf::from("."),
        );
        preview.rebuild_layout_lines(80);
        let code_info = preview
            .logical_lines
            .iter()
            .find(|line| line.is_code_info())
            .expect("expected a code info logical line");
        let code_body = preview
            .layout_lines
            .borrow()
            .iter()
            .find(|line| {
                line.wrap_idx == 0
                    && line.code_id(&preview.logical_lines) == Some(1)
                    && line.logical(&preview.logical_lines).is_code_body()
            })
            .expect("expected a code body layout line")
            .clone();

        let active_ctx = RenderContext {
            theme: &preview.theme,
            active_code_id: Some(1),
            prefer_status_gutter: None,
            spinner_char: ' ',
            viewport_width: 80,
        };
        let active_info = code_info.render(&active_ctx);
        let active_body = render_layout_line(&preview, &code_body, &active_ctx);

        assert_eq!(
            active_info.spans.first().and_then(|span| span.style.fg),
            Some(preview.theme.active)
        );
        assert_eq!(
            active_body.spans.first().and_then(|span| span.style.fg),
            Some(preview.theme.active)
        );

        let status_ctx = RenderContext {
            theme: &preview.theme,
            active_code_id: Some(1),
            prefer_status_gutter: Some(1),
            spinner_char: ' ',
            viewport_width: 80,
        };
        let status_info = code_info.render(&status_ctx);
        let status_body = render_layout_line(&preview, &code_body, &status_ctx);

        assert_eq!(
            status_info.spans.first().and_then(|span| span.style.fg),
            Some(preview.theme.success)
        );
        assert_eq!(
            status_body.spans.first().and_then(|span| span.style.fg),
            Some(preview.theme.success)
        );
    }

    #[test]
    fn test_active_code_info_id_uses_accent_when_preview_unfocused() {
        let preview = preview_from_markdown("```bash\necho hello\n```");
        let info_line = preview
            .logical_lines
            .iter()
            .find(|line| line.is_code_info())
            .expect("expected a code info logical line");
        let ctx = RenderContext {
            theme: &preview.theme,
            active_code_id: Some(1),
            prefer_status_gutter: None,
            spinner_char: ' ',
            viewport_width: 80,
        };

        let rendered = info_line.render(&ctx);
        let id_span = rendered
            .spans
            .get(2)
            .expect("expected gutter, gap, id spans");

        assert_eq!(id_span.content.as_ref(), "1");
        assert_eq!(id_span.style.fg, Some(preview.theme.active));
        assert_eq!(id_span.style.bg, Some(preview.theme.info_background));
    }

    #[test]
    fn test_plain_text_stays_inactive_when_no_code_selected() {
        let preview =
            preview_from_markdown("# Review Findings\n\nCode review of `upmd`, `upmd-parser`.\n");
        preview.rebuild_layout_lines(80);
        let layout_lines = preview.layout_lines.borrow();
        let paragraph_line = layout_lines
            .iter()
            .find(|line| {
                line.code_id(&preview.logical_lines).is_none()
                    && line
                        .logical(&preview.logical_lines)
                        .text_content()
                        .starts_with("Code review")
            })
            .expect("expected a paragraph layout line")
            .clone();
        drop(layout_lines);

        let ctx = RenderContext {
            theme: &preview.theme,
            active_code_id: None,
            prefer_status_gutter: None,
            spinner_char: ' ',
            viewport_width: 80,
        };
        let rendered = render_layout_line(&preview, &paragraph_line, &ctx);

        assert_ne!(
            rendered.spans.first().and_then(|span| span.style.fg),
            Some(preview.theme.active)
        );
    }

    #[test]
    fn test_copy_code_line_excludes_gutter() {
        let preview = preview_from_markdown("```bash\necho hello\n```");
        preview.rebuild_layout_lines(80);
        let code_body_idx = preview
            .layout_lines
            .borrow()
            .iter()
            .position(|line| {
                line.code_id(&preview.logical_lines) == Some(1)
                    && line.logical(&preview.logical_lines).is_code_body()
            })
            .expect("expected a code body layout line");
        let line = &preview.layout_lines.borrow()[code_body_idx];
        let display_len = line
            .render_plain(
                &preview.logical_lines[line.logical_idx],
                &RenderContext {
                    theme: &preview.theme,
                    active_code_id: None,
                    prefer_status_gutter: None,
                    spinner_char: ' ',
                    viewport_width: 80,
                },
            )
            .to_string()
            .chars()
            .count();

        preview.selection.start(code_body_idx, 0);
        preview.selection.extend(code_body_idx, display_len);

        assert_eq!(
            preview
                .selection
                .copy_text(|line_idx| preview.copy_line_at(line_idx)),
            Some("echo hello".to_string())
        );
    }

    #[test]
    fn test_mouse_to_pty_coords_handles_scroll_above_block_start() {
        let preview = preview_from_markdown("# Intro\n\n```bash\necho first\necho second\n```\n");
        preview.rebuild_layout_lines(80);
        preview.last_area.set(Rect::new(0, 0, 80, 20));

        // Collect visual indices belonging to code block 1.
        let block_vlines: Vec<usize> = preview
            .layout_lines
            .borrow()
            .iter()
            .enumerate()
            .filter(|(_, line)| line.code_id(&preview.logical_lines) == Some(1))
            .map(|(idx, _)| idx)
            .collect();
        let block_first = *block_vlines.first().expect("block 1 should exist");
        let click_vl = *block_vlines.last().expect("block 1 should have body lines");
        assert!(block_vlines.len() >= 3, "need at least info + 2 body lines");

        // Scroll so block start is one line above viewport.
        *preview.state.borrow_mut().offset_mut() = block_first + 1;
        assert!(click_vl > block_first, "click line must be visible");

        let rel_row = click_vl - (block_first + 1);
        let click_row = rel_row as u16 + PREVIEW_CONTENT_TOP_OFFSET;
        let click_col = PREVIEW_CONTENT_X_OFFSET + 1;

        let mouse = mouse_event(
            crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            click_row,
            click_col,
        );

        let (pty_col, pty_row) = preview
            .mouse_to_pty_coords(1, &mouse, 78, 24)
            .expect("should return PTY coords for visible block row");

        // Column: click_col - X_OFFSET + 1 → (3 - 2) + 1 = 2
        assert_eq!(pty_col, click_col - PREVIEW_CONTENT_X_OFFSET + 1);
        // Row: click_vl - block_first + 1 (1-based from block start)
        assert_eq!(pty_row as usize, click_vl - block_first + 1);
    }

    #[test]
    fn test_mouse_to_pty_coords_rejects_click_outside_block() {
        let preview = preview_from_markdown(
            "# Intro\n\n```bash\necho first\n```\n\n## Details\n\n```bash\necho second\n```\n",
        );
        preview.rebuild_layout_lines(80);
        preview.last_area.set(Rect::new(0, 0, 80, 20));

        // Find a visual index whose code_id is NOT block 1.
        let outside_vl = preview
            .layout_lines
            .borrow()
            .iter()
            .enumerate()
            .find(|(_, line)| line.code_id(&preview.logical_lines) != Some(1))
            .map(|(idx, _)| idx)
            .expect("should have a non-block-1 visual line");

        let offset = preview.state.borrow().offset();
        let rel_row = outside_vl - offset;
        let click_row = rel_row as u16 + PREVIEW_CONTENT_TOP_OFFSET;
        let click_col = PREVIEW_CONTENT_X_OFFSET + 1;

        let mouse = mouse_event(
            crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            click_row,
            click_col,
        );
        assert!(preview.mouse_to_pty_coords(1, &mouse, 78, 24).is_none());
    }

    fn mouse_event(
        kind: crossterm::event::MouseEventKind,
        row: u16,
        column: u16,
    ) -> crossterm::event::MouseEvent {
        crossterm::event::MouseEvent {
            kind,
            column,
            row,
            modifiers: crossterm::event::KeyModifiers::empty(),
        }
    }

    fn code_body_layout_idx(preview: &Preview) -> usize {
        preview
            .layout_lines
            .borrow()
            .iter()
            .position(|line| {
                line.code_id(&preview.logical_lines) == Some(1)
                    && line.logical(&preview.logical_lines).is_code_body()
            })
            .expect("expected a code body layout line")
    }

    #[test]
    fn test_click_code_line_selects_code_block_on_release() {
        let preview = preview_from_markdown("```bash\necho hello\n```");
        preview.rebuild_layout_lines(80);
        preview.last_area.set(Rect::new(0, 0, 80, 10));
        let row = code_body_layout_idx(&preview) as u16 + PREVIEW_CONTENT_TOP_OFFSET;
        let column = PREVIEW_CONTENT_X_OFFSET + 1;

        assert_eq!(
            preview.handle_mouse_event(mouse_event(
                crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
                row,
                column,
            )),
            Some(Action::Select)
        );
        assert_eq!(
            preview.handle_mouse_event(mouse_event(
                crossterm::event::MouseEventKind::Up(crossterm::event::MouseButton::Left),
                row,
                column,
            )),
            Some(Action::SelectCodeBlock(1))
        );
    }

    #[test]
    fn test_drag_code_line_keeps_text_selection() {
        let preview = preview_from_markdown("```bash\necho hello\n```");
        preview.rebuild_layout_lines(80);
        preview.last_area.set(Rect::new(0, 0, 80, 10));
        let row = code_body_layout_idx(&preview) as u16 + PREVIEW_CONTENT_TOP_OFFSET;
        let column = PREVIEW_CONTENT_X_OFFSET + 1;

        assert_eq!(
            preview.handle_mouse_event(mouse_event(
                crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
                row,
                column,
            )),
            Some(Action::Select)
        );
        assert_eq!(
            preview.handle_mouse_event(mouse_event(
                crossterm::event::MouseEventKind::Drag(crossterm::event::MouseButton::Left),
                row,
                column + 5,
            )),
            Some(Action::Select)
        );
        assert_eq!(
            preview.handle_mouse_event(mouse_event(
                crossterm::event::MouseEventKind::Up(crossterm::event::MouseButton::Left),
                row,
                column + 5,
            )),
            Some(Action::Select)
        );
    }

    #[test]
    fn test_code_by_id_finds_nested_in_blockquote() {
        let md =
            "# Welcome\n\n> Blockquote\n> ```sh\n> ls\n> ```\n\n## Other\n```sh\necho hi\n```\n";
        let doc = upmd_parser::new().parse(md);
        let theme = Theme::new("base16-ocean.dark", false);
        let outputs = std::collections::HashMap::new();
        let keymap: keymap::DerivedConfig<Action> = toml::from_str("").unwrap();
        let preview = Preview::new(
            doc,
            theme,
            &outputs,
            10,
            keymap,
            std::path::PathBuf::from("."),
        );

        // code block 1 is inside blockquote, code block 2 is flat
        assert!(
            preview.code_by_id(1).is_some(),
            "code block 1 should be findable"
        );
        assert!(
            preview.code_by_id(2).is_some(),
            "code block 2 should be findable"
        );
    }

    #[test]
    fn test_code_prefix_overhead_tracks_blockquote_depth() {
        for (name, markdown, expected) in [
            ("flat", "```bash\necho hi\n```", 0),
            ("single blockquote", "> ```bash\n> echo hi\n> ```", 2),
            ("nested blockquote", "> > ```bash\n> > echo hi\n> > ```", 4),
        ] {
            let preview = preview_from_markdown(markdown);

            assert_eq!(
                preview.code_prefix_overhead(1),
                expected,
                "{name} code prefix overhead should match its quote depth"
            );
        }
    }

    #[test]
    fn inline_pty_uses_all_rows_below_visible_source() {
        assert_eq!(inline_pty_rows(40, 4, 5, 0), (35, 0));
    }

    #[test]
    fn inline_pty_scrolls_minimally_to_reserve_proportional_height() {
        assert_eq!(inline_pty_rows(40, 38, 5, 0), (16, 15));
    }

    #[test]
    fn inline_pty_caps_target_when_source_nearly_fills_viewport() {
        assert_eq!(inline_pty_rows(40, 35, 36, 0), (4, 0));
    }
}
