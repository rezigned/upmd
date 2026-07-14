//! Preview pane that renders markdown with soft-wrap, selection, and live code output.
//!
//! # Two-tier line model
//!
//! The preview separates *content* from *viewport layout* using two line types:
//!
//! 1. **Logical lines**: [`ViewLine`](crate::apps::tui::markdown::ViewLine)s stored in
//!    [`Preview::logical_lines`].  Each represents one semantic markdown element
//!    (heading, paragraph, code body line, table row, etc.).  They are produced once
//!    by [`MarkdownRenderer`](crate::apps::tui::markdown::MarkdownRenderer) and only
//!    rebuilt when the markdown source changes.
//!
//! 2. **Viewport lines**: [`VisualLine`]s stored in [`VisualLines`].
//!    Each represents **one terminal row** with plain layout text and the
//!    corresponding source character range.  A single `ViewLine` may expand into
//!    multiple `VisualLine`s when the text is wider than the pane and needs
//!    soft-wrapping (handled by [`wrap_line`](crate::apps::tui::wrap::wrap_line)).
//!    PTY output, tables, and dividers are never wrapped.
//!
//! # Render pipeline
//!
//! ```text
//! Markdown Nodes  →  MarkdownRenderer  →  [ViewLine]  →  VisualLines  →  [VisualLine]  →  Preview::render  →  [ListItem]
//!                       (parse)          (plain text)      rebuild       (plain layout    (syntax + slice)    (display)
//!                                                        (on resize)      every frame)
//! ```
//!
//! `render()` only paints the visible window plus a small overdraw margin.
//! Syntax caches are populated for the viewport and one viewport in each
//! direction.  Scrolling highlights new logical lines once; wrapping, search,
//! selection, and copying continue to use the complete plain layout index.

use ratatui::{
    layout::Rect,
    text::{Line, Text},
    widgets::{Borders, List, ListItem, ListState, Padding},
    Frame,
};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};

use crate::apps::config::{
    BORDER_HEIGHT, CODE_GUTTER_WIDTH, INLINE_MAX_LINES_DEFAULT, INLINE_MAX_LINES_FRACTION,
    INLINE_MAX_LINES_MIN, OVERDRAW_FRACTION, PREVIEW_CONTENT_TOP_OFFSET, PREVIEW_CONTENT_X_OFFSET,
};
use crate::apps::theme::Theme;
use crate::runner::CodeId;
use keymap::{DerivedConfig, KeyMap};
use upmd_parser::nodes::Node;

use super::markdown::{
    apply_gutter, highlight_line, Content, LineKind, MarkdownRenderer, RenderContext, ViewLine,
};
use super::selection::SelectionState;
use super::wrap::{slice_line, CopyLine};
use crate::apps::task::Task;
use crate::apps::tui::widgets::Spinner;

const INLINE_PTY_MIN_PERCENT: usize = 40;
const INLINE_PTY_MIN_ROWS: usize = 8;
use upmd_runtime::{
    runtimes::tui::{Input, Output},
    Cmd, Component,
};

mod search;
mod selection;
mod visual_lines;

use search::PreviewSearch;
use selection::PreviewSelection;
pub use visual_lines::{VisualLine, VisualLines};

/// The markdown preview pane.
///
/// Owns the two-tier line model (see module-level docs) and handles all
/// preview-specific interactions: scrolling, block navigation, search
/// highlighting, text selection, and copy-to-clipboard.
pub struct Preview {
    nodes: Vec<Node>,
    /// Logical lines produced by [`MarkdownRenderer`].
    logical_lines: Vec<ViewLine>,
    /// Viewport lines produced by [`VisualLines::rebuild`].
    visual_lines: VisualLines,
    state: RefCell<ListState>,
    theme: Theme,
    keymap: DerivedConfig<Action>,
    search: PreviewSearch,
    spinner: Spinner,
    inline_max_lines_cap: usize,
    inline_max_lines: Cell<usize>,
    last_area: Cell<Rect>,
    selection: PreviewSelection,
    /// If set, jump to this code block on the next visual-lines rebuild.
    target_block: Cell<Option<CodeId>>,
    /// Transient: prefer this block's task status over active gutter.
    prefer_status_gutter: Cell<Option<CodeId>>,
    /// Flat Vec of Code nodes indexed by (CodeId - 1).
    code_index: Vec<upmd_parser::nodes::Code>,
    /// Transient result of the last clipboard copy attempt (None = no copy).
    copy_result: Cell<Option<bool>>,
    /// Prefix overhead in chars per code block (non-zero only for blockquote-nested blocks).
    code_prefix_overhead: HashMap<CodeId, usize>,
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
        nodes: Vec<Node>,
        codes: Vec<upmd_parser::nodes::Code>,
        theme: Theme,
        outputs: &HashMap<CodeId, Task>,
        inline_max_lines_cap: usize,
        keymap: DerivedConfig<Action>,
    ) -> Self {
        let preview = Self {
            nodes: vec![],
            logical_lines: vec![],
            visual_lines: VisualLines::new(),
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
        };
        let mut preview = preview;
        preview.set_nodes(nodes, preview.code_index.clone(), outputs);
        if !preview.visual_lines.is_empty() {
            preview.state.borrow_mut().select(Some(0));
        }
        preview
    }

    pub fn set_nodes(
        &mut self,
        nodes: Vec<Node>,
        codes: Vec<upmd_parser::nodes::Code>,
        outputs: &HashMap<CodeId, Task>,
    ) {
        self.code_index = codes;
        self.nodes = nodes;
        self.rebuild_view(outputs);
    }

    pub fn prefer_status_gutter_for(&self, id: CodeId) {
        self.prefer_status_gutter.set(Some(id));
    }

    /// Rebuilds the logical lines from markdown nodes and then the visual lines.
    ///
    /// Called when the markdown source changes (new document, code output update)
    /// or when the theme / inline cap changes.
    #[tracing::instrument(level = "info", skip_all, fields(lines))]
    pub fn rebuild_view(&mut self, outputs: &HashMap<CodeId, Task>) {
        let width = self.visual_lines.last_width();
        let renderer = MarkdownRenderer::new(
            &self.theme,
            outputs,
            &self.code_index,
            self.inline_max_lines.get(),
            width,
        );
        let old_lines = std::mem::take(&mut self.logical_lines);
        let rendered = renderer.render(&self.nodes);
        tracing::Span::current().record("lines", rendered.lines.len());
        self.logical_lines = rendered.lines;
        self.code_prefix_overhead = rendered.code_prefix_overhead;
        // Preserve lazy syntax highlighting cache for unchanged raw lines.
        let mut old_raw_iter = old_lines
            .into_iter()
            .filter_map(|line| match line.content {
                Content::Raw {
                    text,
                    language,
                    cached,
                } => Some((text, language, cached)),
                _ => None,
            })
            .peekable();

        // Match new raw lines against old ones sequentially to migrate their cached
        // syntax highlighting if the text and language remain unchanged.
        for line in &mut self.logical_lines {
            if let Content::Raw {
                text,
                language,
                cached,
            } = &mut line.content
            {
                while let Some((old_text, old_lang, _)) = old_raw_iter.peek() {
                    if text == old_text && language == old_lang {
                        if let Some((_, _, old_cached)) = old_raw_iter.next() {
                            *cached = old_cached;
                        }
                        break;
                    } else {
                        old_raw_iter.next();
                    }
                }
            }
        }

        // Cache lower-cased searchable text for each logical line so search
        // navigation does not re-allocate on every keystroke.
        self.search.rebuild_texts(&self.logical_lines);

        if width > 0 {
            self.rebuild_visual_lines(width);
        }
    }

    /// Rebuilds all viewport [`VisualLine`]s from the current logical lines.
    #[tracing::instrument(level = "info", skip_all, fields(n_logical, n_visual, width))]
    fn rebuild_visual_lines(&self, width: usize) {
        if width == 0 {
            return;
        }
        let span = tracing::Span::current();
        let n_logical = self.logical_lines.len();
        let previous_selection = self.selected_visual_line_identity();
        let (previous_selected, previous_offset) = {
            let state = self.state.borrow();
            (state.selected(), state.offset())
        };
        let previous_code_rows = previous_selected
            .and_then(|idx| self.visual_lines.get(idx).and_then(|line| line.code_id))
            .and_then(|id| self.visual_extent_for_code(id).map(|(_, rows)| (id, rows)));
        let selected = self.visual_lines.rebuild(
            &self.logical_lines,
            width,
            &self.theme,
            &self.target_block,
            |idx| self.is_code_start_at(idx),
        );
        if let Some(idx) = selected {
            // Explicit jump → scroll to target.
            let mut state = self.state.borrow_mut();
            state.select(Some(idx));
            *state.offset_mut() = idx;
        } else if let Some(idx) = previous_selection.and_then(|id| self.visual_idx_for_identity(id))
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
                    .visual_lines
                    .get(idx)
                    .is_some_and(|line| line.code_id == Some(code_id))
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
        span.record("n_visual", self.visual_lines.len());
        span.record("width", width);
        self.clamp_state_to_visual_lines();
    }

    /// Keeps ListState and text selection valid after visual lines are rebuilt
    /// or shrink.
    fn clamp_state_to_visual_lines(&self) {
        let len = self.visual_lines.len();
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

        // Selection stores visual-line indices; clamp or clear if the range
        // changed underneath it.
        self.selection.clamp_or_clear(len);
    }

    /// Counts the number of heading lines at or before the given logical line index.
    pub fn heading_count_at_line(&self, logical_idx: usize) -> usize {
        use crate::apps::tui::markdown::LineKind;
        self.logical_lines[..=logical_idx]
            .iter()
            .filter(|l| matches!(l.kind, LineKind::Heading(_)))
            .count()
            .saturating_sub(1)
    }

    pub fn selected_logical_line(&self) -> Option<usize> {
        let visual_idx = self.state.borrow().selected()?;
        self.visual_lines.get(visual_idx).map(|l| l.logical_idx)
    }

    fn selected_visual_line_identity(&self) -> Option<(usize, usize)> {
        let visual_idx = self.state.borrow().selected()?;
        self.visual_lines
            .get(visual_idx)
            .map(|line| (line.logical_idx, line.wrap_idx))
    }

    fn visual_idx_for_identity(&self, (logical_idx, wrap_idx): (usize, usize)) -> Option<usize> {
        let visual_lines = self.visual_lines.borrow();
        visual_lines
            .iter()
            .position(|line| line.logical_idx == logical_idx && line.wrap_idx == wrap_idx)
            .or_else(|| {
                visual_lines
                    .iter()
                    .position(|line| line.logical_idx == logical_idx)
            })
    }

    fn visual_extent_for_code(&self, id: CodeId) -> Option<(usize, usize)> {
        let visual_lines = self.visual_lines.borrow();
        let mut indices = visual_lines
            .iter()
            .enumerate()
            .filter_map(|(idx, line)| (line.code_id == Some(id)).then_some(idx));
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
        let viewport = self.visual_lines.last_height();
        if viewport == 0 {
            return offset;
        }
        let Some((end, rows)) = self.visual_extent_for_code(id) else {
            return offset;
        };
        if rows <= previous_rows || end < offset.saturating_add(viewport) {
            return offset;
        }
        end.saturating_add(1).saturating_sub(viewport)
    }

    /// Returns the first and last visual line index for non-output rows of
    /// code block `id` (CodeInfo + CodeBody only, excluding Output).
    pub fn source_visual_extent(&self, id: CodeId) -> Option<(usize, usize)> {
        let visual_lines = self.visual_lines.borrow();
        let mut indices = visual_lines.iter().enumerate().filter_map(|(idx, line)| {
            let kind = self.logical_lines.get(line.logical_idx)?.kind;
            (line.code_id == Some(id) && matches!(kind, LineKind::CodeInfo | LineKind::CodeBody))
                .then_some(idx)
        });
        let first = indices.next()?;
        let end = indices.next_back().unwrap_or(first);
        Some((first, end))
    }

    /// Returns the current visual offset (first visible row index).
    pub fn visual_offset(&self) -> usize {
        self.state.borrow().offset()
    }

    /// Sets the visual offset.
    pub fn set_visual_offset(&self, offset: usize) {
        *self.state.borrow_mut().offset_mut() = offset;
    }

    pub fn inline_max_lines(&self) -> usize {
        self.inline_max_lines.get()
    }

    fn visual_idx_of_logical(&self, logical_idx: usize) -> Option<usize> {
        self.visual_lines.visual_idx_of_logical(logical_idx)
    }

    pub fn set_inline_max_lines(&self, height: usize) {
        let max_inline = (height / INLINE_MAX_LINES_FRACTION)
            .clamp(INLINE_MAX_LINES_MIN, self.inline_max_lines_cap);
        self.inline_max_lines.set(max_inline);
    }

    /// Advances the spinner tick counter (driven by Msg::Tick).
    pub fn tick(&mut self) {
        self.spinner.tick();
    }

    pub fn matches(&self, term: &str) -> Vec<usize> {
        if term.is_empty() {
            return vec![];
        }
        self.search.matches(&self.visual_lines.borrow())
    }

    pub fn set_theme(&mut self, theme: Theme) {
        self.theme = theme;
        self.logical_lines.iter().for_each(|l| l.clear_cache());
    }

    pub fn set_search_term(&mut self, term: &str) {
        self.search.set_term(term);
    }

    pub fn select_line(&mut self, idx: usize) {
        let max = self.visual_lines.len().saturating_sub(1);
        self.select_and_scroll_smooth(idx.min(max));
    }

    pub fn selected_code(&self) -> Option<&ViewLine> {
        let sel = self.state.borrow().selected()?;
        let logical_idx = self.visual_lines.get(sel)?.logical_idx;
        self.logical_lines.get(logical_idx)
    }

    pub fn selected_code_id(&self) -> Option<CodeId> {
        let sel = self.state.borrow().selected()?;
        self.visual_lines.get(sel)?.code_id
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

    /// Returns the code block owning the visual row under a mouse event.
    ///
    /// Hit-tests by viewport row only. This treats code info, code body, and
    pub fn code_id_at_mouse(&self, mouse: &crossterm::event::MouseEvent) -> Option<CodeId> {
        if !crate::utils::mouse_in_area(mouse, self.last_area.get()) {
            return None;
        }
        let rel_row = self.mouse_content_rel_row(mouse)?;
        let visual_idx = self.state.borrow().offset() + rel_row;
        self.visual_lines.get(visual_idx)?.code_id
    }

    /// Looks up the raw `Code` node by ID from the flat Vec index.
    /// Returns all code blocks in document order.
    pub fn codes(&self) -> &[upmd_parser::nodes::Code] {
        &self.code_index
    }

    pub fn code_by_id(&self, id: CodeId) -> Option<&upmd_parser::nodes::Code> {
        self.code_index.get(id as usize - 1)
    }

    /// Converts a mouse click on the selected code block into PTY-relative
    /// SGR coordinates `(col, row)`, 1-based.
    ///
    /// Returns `None` when the click falls outside the code block's visual
    /// extent (before its first visual line or after its last) or when the
    /// block is not visible.
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
        let visual_idx = state_offset + rel_row;

        let block_first = self
            .visual_lines
            .find_code_start(id, |i| self.is_code_start_at(i))?;

        // The clicked visual line must belong to the selected code block.
        match self.visual_lines.get(visual_idx) {
            Some(vl) if vl.code_id == Some(id) => {}
            _ => return None,
        }

        let pty_row = visual_idx - block_first + 1;
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

    /// Returns the currently selected visual line index, or `0` if none.
    fn selected_idx(&self) -> usize {
        self.state.borrow().selected().unwrap_or(0)
    }

    /// Returns `true` when the logical line at `logical_idx` is a code-start.
    fn is_code_start_at(&self, logical_idx: usize) -> bool {
        self.logical_lines
            .get(logical_idx)
            .is_some_and(|ll| ll.is_code_start)
    }

    /// Builds a [`CopyLine`] from the visual line at `line_idx`.
    fn copy_line_at(&self, line_idx: usize) -> Option<CopyLine> {
        let vl = self.visual_lines.get(line_idx)?;
        let mut text: String = vl.line.spans.iter().map(|s| s.content.as_ref()).collect();
        let display_prefix_len = match (vl.code_id.is_some(), text.strip_prefix("▎ ")) {
            (true, Some(stripped)) => {
                text = stripped.to_string();
                CODE_GUTTER_WIDTH
            }
            (true, None) if vl.wrap_idx > 0 => CODE_GUTTER_WIDTH,
            _ => 0,
        };
        Some(CopyLine {
            text,
            is_continuation: vl.char_range.start > 0,
            display_prefix_len,
        })
    }

    fn render_visual_line_from(
        &self,
        visual_line: &VisualLine,
        source_line: &Line<'static>,
        ctx: &RenderContext<'_>,
    ) -> Line<'static> {
        let logical_line = &self.logical_lines[visual_line.logical_idx];
        let mut line = if logical_line.is_unwrappable() {
            source_line.clone()
        } else {
            slice_line(source_line, visual_line.char_range.clone())
        };

        if visual_line.code_id.is_some() && visual_line.wrap_idx > 0 {
            let is_active = ctx.active_code_id == visual_line.code_id;
            apply_gutter(
                &mut line,
                logical_line.is_unwrappable(),
                is_active,
                ctx.theme,
                visual_line.gutter_fg,
                ctx.prefer_status_gutter == visual_line.code_id,
                visual_line.gutter_fg == Some(ctx.theme.warning),
            );
        }
        line
    }

    pub fn page_down(&mut self) {
        let lh = self.visual_lines.last_height();
        let current = self.selected_idx();
        let next = (current + lh).min(self.visual_lines.len().saturating_sub(1));
        self.select_and_scroll_smooth(next);
    }

    pub fn page_up(&mut self) {
        let lh = self.visual_lines.last_height();
        let current = self.selected_idx();
        let next = current.saturating_sub(lh);
        self.select_and_scroll_smooth(next);
    }

    pub fn scroll_down(&mut self) {
        let len = self.visual_lines.len();
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
        if self.visual_lines.is_empty() {
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

    /// Selects a code block by ID without scrolling the viewport.
    ///
    /// Used for click-to-select where the block is already visible and the
    /// viewport should stay exactly where it is.
    /// Selects a code block by ID, snapping to it only when off-screen.
    pub fn select_code(&mut self, id: CodeId) {
        if self.is_code_visible(id) {
            self.target_block.set(None);
            if let Some(idx) = self
                .visual_lines
                .find_code_start(id, |idx| self.is_code_start_at(idx))
            {
                self.state.borrow_mut().select(Some(idx));
            }
        } else {
            self.target_block.set(Some(id));
            if let Some(idx) = self
                .visual_lines
                .find_code_start(id, |idx| self.is_code_start_at(idx))
            {
                self.select_and_scroll_smooth(idx);
            }
        }
    }

    /// Selects the Nth heading, snapping to it only when off-screen.
    pub fn select_heading(&mut self, heading_idx: usize) {
        use crate::apps::tui::markdown::LineKind;
        let mut count = 0;
        for (logical_idx, line) in self.logical_lines.iter().enumerate() {
            if matches!(line.kind, LineKind::Heading(_)) {
                if count == heading_idx {
                    if let Some(visual_idx) = self.visual_idx_of_logical(logical_idx) {
                        let (offset, height) = {
                            let state = self.state.borrow();
                            (state.offset(), self.visual_lines.last_height())
                        };
                        if visual_idx >= offset && visual_idx < offset + height {
                            self.state.borrow_mut().select(Some(visual_idx));
                        } else {
                            self.select_and_scroll_smooth(visual_idx);
                        }
                    }
                    return;
                }
                count += 1;
            }
        }
    }

    /// Returns true if the code block's start line is within the current viewport.
    pub fn is_code_visible(&self, id: CodeId) -> bool {
        let Some(idx) = self
            .visual_lines
            .find_code_start(id, |i| self.is_code_start_at(i))
        else {
            return false;
        };
        let state = self.state.borrow();
        let offset = state.offset();
        let height = self.visual_lines.last_height();
        idx >= offset && idx < offset + height
    }

    /// Takes the result of the most recent clipboard copy attempt.
    pub fn take_copy_result(&self) -> Option<bool> {
        self.copy_result.replace(None)
    }
}

impl Input for Preview {
    fn action(&self, event: crossterm::event::Event) -> Option<Self::Msg> {
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
                let visual_lines = self.visual_lines.borrow();
                let state_offset = self.state.borrow().offset();
                let pos = SelectionState::mouse_to_position(
                    area,
                    mouse.row,
                    mouse.column,
                    PREVIEW_CONTENT_X_OFFSET,
                    |rel_row| {
                        let vidx = state_offset + rel_row;
                        visual_lines.get(vidx).map(|vl| (vidx, &vl.line))
                    },
                );
                drop(visual_lines);
                if let Some((vidx, cidx)) = pos {
                    // Tracks the clicked visual line for selection/menu sync without
                    // moving the viewport. Mouse-wheel scroll uses the viewport
                    // offset, so scroll-after-click continues from the current view.
                    self.state.borrow_mut().select(Some(vidx));
                    let visual_lines = self.visual_lines.borrow();
                    self.selection
                        .set_pending_code_click(visual_lines.get(vidx).and_then(|vl| vl.code_id));
                    drop(visual_lines);
                    self.selection.start(vidx, cidx);
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
                let visual_lines = self.visual_lines.borrow();
                let state_offset = self.state.borrow().offset();
                let content_y = area.y + PREVIEW_CONTENT_TOP_OFFSET;
                let content_bottom = area.y + area.height.saturating_sub(BORDER_HEIGHT as u16);
                let clamped_row = mouse.row.clamp(content_y, content_bottom);
                let clamped_col = mouse.column.clamp(
                    area.x + PREVIEW_CONTENT_X_OFFSET,
                    area.x + area.width.saturating_sub(PREVIEW_CONTENT_X_OFFSET),
                );
                let pos = SelectionState::mouse_to_position(
                    area,
                    clamped_row,
                    clamped_col,
                    PREVIEW_CONTENT_X_OFFSET,
                    |rel_row| {
                        let vidx = state_offset + rel_row;
                        visual_lines.get(vidx).map(|vl| (vidx, &vl.line))
                    },
                );
                drop(visual_lines);
                if let Some((vidx, cidx)) = pos {
                    self.selection.extend(vidx, cidx);
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
}
/// Computes how many PTY rows fit below a block's source lines in the viewport,
/// returning `(rows, new_offset)`. Scrolls the viewport if fewer than 40% of
/// the rows (min 8) remain below the source.
pub(crate) fn inline_pty_rows(
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
    type Msg = Action;

    fn update(&mut self, action: Action) -> Option<Cmd<Action>> {
        match action {
            Action::ScrollUp => self.scroll_up(),
            Action::ScrollDown => self.scroll_down(),
            Action::PageUp => self.page_up(),
            Action::PageDown => self.page_down(),
            Action::ToggleToc => {}
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
            Action::SelectCodeBlock(_) => {}
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
    /// text benefits from the [`ViewLine`](crate::apps::tui::markdown::ViewLine) cache.
    fn render(&self, frame: &mut Frame, area: Rect) {
        let height = area.height as usize;
        let width = area.width as usize;
        self.visual_lines
            .set_last_height(height.saturating_sub(BORDER_HEIGHT));
        self.last_area.set(area);

        if self.visual_lines.last_width() != width && width > 0 {
            self.rebuild_visual_lines(width);
        }

        let visual_lines = self.visual_lines.borrow();
        if visual_lines.is_empty() {
            return;
        }

        let viewport = height.saturating_sub(BORDER_HEIGHT);
        let overdraw = viewport / OVERDRAW_FRACTION;
        let state = self.state.borrow();
        let original_offset = state.offset();
        let original_selected = state.selected();
        drop(state);

        let win_start = original_offset.saturating_sub(overdraw);
        let win_end = (original_offset + viewport + overdraw).min(visual_lines.len());
        let window = &visual_lines[win_start..win_end];

        let selected_idx = original_selected.unwrap_or(0);
        let active_code_id = visual_lines.get(selected_idx).and_then(|l| l.code_id);
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

        let highlight_start = original_offset.saturating_sub(viewport);
        let highlight_end = original_offset
            .saturating_add(viewport.saturating_mul(2))
            .min(visual_lines.len());
        let mut warmed = HashSet::new();
        let mut cache_misses = 0;
        for visual_line in &visual_lines[highlight_start..highlight_end] {
            if warmed.insert(visual_line.logical_idx)
                && self.logical_lines[visual_line.logical_idx].ensure_highlighted(&ctx)
            {
                cache_misses += 1;
            }
        }

        let mut rendered_logical = HashMap::new();
        for visual_line in window {
            rendered_logical
                .entry(visual_line.logical_idx)
                .or_insert_with(|| self.logical_lines[visual_line.logical_idx].render(&ctx));
        }

        let items: Vec<ListItem> = window
            .iter()
            .enumerate()
            .map(|(win_i, visual_line)| {
                let visual_idx = win_start + win_i;
                let source_line = rendered_logical
                    .get(&visual_line.logical_idx)
                    .expect("visible logical line was rendered");
                let mut line = self.render_visual_line_from(visual_line, source_line, &ctx);
                if let Some(term) = self.search.term() {
                    line = highlight_line(line, term, self.theme.search_highlight_style());
                }

                if let Some((sel_start, sel_end)) = self
                    .selection
                    .range_for_line(visual_idx, line.to_string().chars().count())
                {
                    line = SelectionState::apply_range(
                        line,
                        sel_start,
                        sel_end,
                        self.theme.selection_style(),
                    );
                }

                ListItem::new(Text::from(line))
            })
            .collect();
        if cache_misses > 0 {
            tracing::debug!(
                visual_rows = window.len(),
                logical_lines = warmed.len(),
                cache_misses,
                "populated viewport syntax cache"
            );
        }
        drop(visual_lines);

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
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use insta::assert_snapshot;
    use std::collections::HashMap;
    use upmd_parser::Parser;

    fn preview_from_markdown(markdown: &str) -> Preview {
        let doc = upmd_parser::new().parse(markdown);
        let theme = Theme::new("base16-ocean.dark", false);
        let outputs = HashMap::new();
        let keymap: DerivedConfig<Action> = toml::from_str("").unwrap();
        Preview::new(doc.nodes, doc.codes, theme, &outputs, 10, keymap)
    }

    fn render_visual_line(
        preview: &Preview,
        visual_line: &VisualLine,
        ctx: &RenderContext<'_>,
    ) -> Line<'static> {
        let source_line = preview.logical_lines[visual_line.logical_idx].render(ctx);
        preview.render_visual_line_from(visual_line, &source_line, ctx)
    }

    fn visual_summary(preview: &Preview) -> String {
        preview
            .visual_lines
            .borrow()
            .iter()
            .enumerate()
            .map(|(i, vl)| {
                let text = vl.line.to_string();
                let char_count = text.chars().count();
                let preview_text = if char_count > 70 {
                    let t: String = text.chars().take(67).collect();
                    format!("{}...", t)
                } else {
                    text
                };
                format!("{:2}: [{}] {}", i, vl.wrap_idx, preview_text)
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn test_wrap_long_paragraph() {
        let preview = preview_from_markdown("This is a very long paragraph that should definitely wrap into multiple visual lines when the pane width is narrow.");
        preview.rebuild_visual_lines(30);
        let summary = visual_summary(&preview);
        assert_snapshot!("wrap_long_paragraph", summary);
    }

    #[test]
    fn test_no_wrap_code_output() {
        let preview = preview_from_markdown("```bash\necho hello\n```");
        preview.rebuild_visual_lines(20);
        let summary = visual_summary(&preview);
        assert_snapshot!("no_wrap_code_output", summary);
    }

    #[test]
    fn test_code_body_wraps() {
        let preview = preview_from_markdown("```rust\nfn very_long_function_name_that_exceeds_narrow_width() -> VeryLongReturnType {\n}\n```");
        preview.rebuild_visual_lines(30);
        let summary = visual_summary(&preview);
        assert_snapshot!("code_body_wraps", summary);
    }

    #[test]
    fn test_rebuild_clamps_selection_when_lines_shrink() {
        let mut preview = preview_from_markdown("This is a very long paragraph that should wrap into several rows at narrow widths but fit in fewer rows at wider widths.");
        preview.rebuild_visual_lines(20);
        let last_idx = preview.visual_lines.borrow().len().saturating_sub(1);
        {
            let mut state = preview.state.borrow_mut();
            state.select(Some(last_idx));
            *state.offset_mut() = last_idx;
        }
        preview.select_code(2);
        preview.rebuild_visual_lines(120);

        let len = preview.visual_lines.borrow().len();
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
        preview.rebuild_visual_lines(80);

        let state = preview.state.borrow();
        assert!(state.offset() > 0);
        assert_eq!(
            preview
                .visual_lines
                .get(state.offset())
                .and_then(|line| line.code_id),
            Some(2)
        );
    }

    #[test]
    fn test_cached_code_line_updates_active_gutter() {
        let preview = preview_from_markdown("```bash\necho hello\n```");
        preview.rebuild_visual_lines(80);
        let visual_lines = preview.visual_lines.borrow();
        let code_body_line = visual_lines
            .iter()
            .find(|line| {
                line.wrap_idx == 0
                    && line.code_id == Some(1)
                    && preview.logical_lines[line.logical_idx].is_code_body()
            })
            .expect("expected a code body visual line")
            .clone();
        drop(visual_lines);

        let ctx = RenderContext {
            theme: &preview.theme,
            active_code_id: Some(1),
            prefer_status_gutter: None,
            spinner_char: ' ',
            viewport_width: 80,
        };
        let rendered = render_visual_line(&preview, &code_body_line, &ctx);

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
        let preview = Preview::new(doc.nodes, doc.codes, theme, &outputs, 10, keymap);
        preview.rebuild_visual_lines(80);
        let code_info = preview
            .logical_lines
            .iter()
            .find(|line| line.is_code_info())
            .expect("expected a code info logical line");
        let code_body = preview
            .visual_lines
            .borrow()
            .iter()
            .find(|line| {
                line.wrap_idx == 0
                    && line.code_id == Some(1)
                    && preview.logical_lines[line.logical_idx].is_code_body()
            })
            .expect("expected a code body visual line")
            .clone();

        let active_ctx = RenderContext {
            theme: &preview.theme,
            active_code_id: Some(1),
            prefer_status_gutter: None,
            spinner_char: ' ',
            viewport_width: 80,
        };
        let active_info = code_info.render(&active_ctx);
        let active_body = render_visual_line(&preview, &code_body, &active_ctx);

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
        let status_body = render_visual_line(&preview, &code_body, &status_ctx);

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
        preview.rebuild_visual_lines(80);
        let visual_lines = preview.visual_lines.borrow();
        let paragraph_line = visual_lines
            .iter()
            .find(|line| {
                line.code_id.is_none()
                    && preview.logical_lines[line.logical_idx]
                        .text_content()
                        .starts_with("Code review")
            })
            .expect("expected a paragraph visual line")
            .clone();
        drop(visual_lines);

        let ctx = RenderContext {
            theme: &preview.theme,
            active_code_id: None,
            prefer_status_gutter: None,
            spinner_char: ' ',
            viewport_width: 80,
        };
        let rendered = render_visual_line(&preview, &paragraph_line, &ctx);

        assert_ne!(
            rendered.spans.first().and_then(|span| span.style.fg),
            Some(preview.theme.active)
        );
    }

    #[test]
    fn test_copy_code_line_excludes_gutter() {
        let preview = preview_from_markdown("```bash\necho hello\n```");
        preview.rebuild_visual_lines(80);
        let code_body_idx = preview
            .visual_lines
            .borrow()
            .iter()
            .position(|line| {
                line.code_id == Some(1) && preview.logical_lines[line.logical_idx].is_code_body()
            })
            .expect("expected a code body visual line");
        let display_len = preview.visual_lines.borrow()[code_body_idx]
            .line
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
        preview.rebuild_visual_lines(80);
        preview.last_area.set(Rect::new(0, 0, 80, 20));

        // Collect visual indices belonging to code block 1.
        let block_vlines: Vec<usize> = preview
            .visual_lines
            .borrow()
            .iter()
            .enumerate()
            .filter(|(_, vl)| vl.code_id == Some(1))
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
        preview.rebuild_visual_lines(80);
        preview.last_area.set(Rect::new(0, 0, 80, 20));

        // Find a visual index whose code_id is NOT block 1.
        let outside_vl = preview
            .visual_lines
            .borrow()
            .iter()
            .enumerate()
            .find(|(_, vl)| vl.code_id != Some(1))
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

    fn code_body_visual_idx(preview: &Preview) -> usize {
        preview
            .visual_lines
            .borrow()
            .iter()
            .position(|line| {
                line.code_id == Some(1) && preview.logical_lines[line.logical_idx].is_code_body()
            })
            .expect("expected a code body visual line")
    }

    #[test]
    fn test_click_code_line_selects_code_block_on_release() {
        let preview = preview_from_markdown("```bash\necho hello\n```");
        preview.rebuild_visual_lines(80);
        preview.last_area.set(Rect::new(0, 0, 80, 10));
        let row = code_body_visual_idx(&preview) as u16 + PREVIEW_CONTENT_TOP_OFFSET;
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
        preview.rebuild_visual_lines(80);
        preview.last_area.set(Rect::new(0, 0, 80, 10));
        let row = code_body_visual_idx(&preview) as u16 + PREVIEW_CONTENT_TOP_OFFSET;
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
        let preview = Preview::new(doc.nodes, doc.codes, theme, &outputs, 10, keymap);

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
