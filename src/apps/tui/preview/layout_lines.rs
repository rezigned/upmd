use std::cell::{Cell, RefCell};
use std::ops::Range;

use crate::apps::config::{PREVIEW_CODE_WRAP_OVERHEAD, PREVIEW_FRAME_OVERHEAD};
use crate::apps::theme::Theme;
use crate::runner::CodeId;

use crate::apps::tui::markdown::{apply_gutter, LogicalLine, RenderContext};
use crate::apps::tui::wrap::{slice_line, wrap_ranges};

/// Width-dependent slice of a logical line occupying one terminal row.
#[derive(Debug, Clone)]
pub struct LayoutLine {
    pub logical_idx: usize,
    /// Zero-based row within this logical line.
    pub wrap_idx: usize,
    /// Character range in the unwrapped rendered line.
    pub char_range: Range<usize>,
}

impl LayoutLine {
    fn new(logical_idx: usize, wrap_idx: usize, char_range: Range<usize>) -> Self {
        Self {
            logical_idx,
            wrap_idx,
            char_range,
        }
    }

    pub fn is_continuation(&self) -> bool {
        self.wrap_idx > 0
    }

    pub fn logical<'a>(&self, logical_lines: &'a [LogicalLine]) -> &'a LogicalLine {
        &logical_lines[self.logical_idx]
    }

    pub fn code_id(&self, logical_lines: &[LogicalLine]) -> Option<CodeId> {
        self.logical(logical_lines).code_id
    }

    pub fn render_plain(
        &self,
        logical_line: &LogicalLine,
        ctx: &RenderContext<'_>,
    ) -> ratatui::text::Line<'static> {
        if logical_line.is_image() && self.is_continuation() {
            return ratatui::text::Line::raw("");
        }
        let source = logical_line.render_plain(ctx);
        if logical_line.is_unwrappable() {
            source
        } else {
            let mut line = slice_line(&source, self.char_range.clone());
            self.prepend_wrap_prefix(logical_line, &source, &mut line);
            line
        }
    }

    /// Slices an already-rendered logical line for this terminal row and applies
    /// row-specific gutter styling.
    pub fn render(
        &self,
        logical_line: &LogicalLine,
        rendered_line: &ratatui::text::Line<'static>,
        ctx: &RenderContext<'_>,
    ) -> ratatui::text::Line<'static> {
        if logical_line.is_image() && self.is_continuation() {
            return ratatui::text::Line::raw("");
        }
        let mut line = if logical_line.is_unwrappable() {
            rendered_line.clone()
        } else {
            slice_line(rendered_line, self.char_range.clone())
        };
        if logical_line.has_code_gutter() && self.is_continuation() {
            apply_gutter(
                &mut line,
                logical_line.is_unwrappable(),
                ctx.active_code_id == logical_line.code_id,
                ctx.theme,
                logical_line.gutter_fg,
                ctx.prefer_status_gutter == logical_line.code_id,
                logical_line.gutter_fg == Some(ctx.theme.warning),
            );
        }
        self.prepend_wrap_prefix(logical_line, rendered_line, &mut line);
        line
    }

    fn prepend_wrap_prefix(
        &self,
        logical_line: &LogicalLine,
        rendered_line: &ratatui::text::Line<'static>,
        line: &mut ratatui::text::Line<'static>,
    ) {
        if !self.is_continuation() {
            return;
        }
        let width = logical_line.wrap_prefix_width();
        let prefix = logical_line
            .wrap_prefixes()
            .map(<[ratatui::text::Span<'static>]>::to_vec)
            .unwrap_or_else(|| slice_line(rendered_line, 0..width).spans);
        line.spans.splice(0..0, prefix);
    }
}

/// Terminal-row layout derived from logical lines.
pub struct LayoutLines {
    lines: RefCell<Vec<LayoutLine>>,
    last_width: Cell<usize>,
    last_height: Cell<usize>,
}

impl Default for LayoutLines {
    fn default() -> Self {
        Self::new()
    }
}

impl LayoutLines {
    pub fn new() -> Self {
        Self {
            lines: RefCell::new(vec![]),
            last_width: Cell::new(crate::apps::config::PTY_DEFAULT_COLS as usize),
            last_height: Cell::new(0),
        }
    }

    /// Rebuilds terminal rows for `width`.
    ///
    /// Wrappable lines use plain rendering for measurement. Images use
    /// `rows_for`; other unwrappable lines occupy one row. Returns the deferred
    /// target block's layout index, if any.
    pub fn rebuild(
        &self,
        logical_lines: &[LogicalLine],
        width: usize,
        theme: &Theme,
        target_block: &Cell<Option<CodeId>>,
        rows_for: impl Fn(&LogicalLine) -> usize,
    ) -> Option<usize> {
        if width == 0 {
            return None;
        }
        self.last_width.set(width);
        let ctx = RenderContext {
            theme,
            active_code_id: None,
            prefer_status_gutter: None,
            spinner_char: ' ',
            viewport_width: width,
        };

        let mut new_layout_lines = Vec::new();
        for (idx, logical_line) in logical_lines.iter().enumerate() {
            if logical_line.is_image() {
                let rows =
                    (0..rows_for(logical_line).max(1)).map(|row| LayoutLine::new(idx, row, 0..0));
                new_layout_lines.extend(rows);
                continue;
            }

            let line = logical_line.render_plain(&ctx);
            if logical_line.is_unwrappable() {
                let char_len = line.to_string().chars().count();
                new_layout_lines.push(LayoutLine::new(idx, 0, 0..char_len));
                continue;
            }

            let overhead = if logical_line.has_code_gutter() {
                PREVIEW_CODE_WRAP_OVERHEAD
            } else {
                PREVIEW_FRAME_OVERHEAD
            };
            let wrap_width = width
                .saturating_sub(overhead + logical_line.reserved_prefix_width())
                .max(1);
            let rows = wrap_ranges(&line, wrap_width)
                .into_iter()
                .enumerate()
                .map(|(row, range)| LayoutLine::new(idx, row, range));
            new_layout_lines.extend(rows);
        }
        *self.lines.borrow_mut() = new_layout_lines;

        // Apply deferred block jump.
        target_block
            .take()
            .and_then(|id| self.find_code_start(id, logical_lines))
    }

    pub fn len(&self) -> usize {
        self.lines.borrow().len()
    }

    pub fn is_empty(&self) -> bool {
        self.lines.borrow().is_empty()
    }

    pub fn get(&self, idx: usize) -> Option<LayoutLine> {
        self.lines.borrow().get(idx).cloned()
    }

    pub fn borrow(&self) -> std::cell::Ref<'_, Vec<LayoutLine>> {
        self.lines.borrow()
    }

    #[allow(dead_code)]
    pub fn iter(&self) -> impl Iterator<Item = LayoutLine> {
        self.lines.borrow().clone().into_iter()
    }

    pub fn layout_idx_of_logical(&self, logical_idx: usize) -> Option<usize> {
        self.lines
            .borrow()
            .iter()
            .position(|l| l.logical_idx == logical_idx)
    }

    pub fn find_code_start(&self, id: CodeId, logical_lines: &[LogicalLine]) -> Option<usize> {
        self.lines.borrow().iter().position(|line| {
            let logical = line.logical(logical_lines);
            logical.code_id == Some(id) && logical.is_code_start
        })
    }

    pub fn last_width(&self) -> usize {
        self.last_width.get()
    }

    #[allow(dead_code)]
    pub fn set_last_width(&self, width: usize) {
        self.last_width.set(width);
    }

    pub fn last_height(&self) -> usize {
        self.last_height.get()
    }

    pub fn set_last_height(&self, height: usize) {
        self.last_height.set(height);
    }
}
