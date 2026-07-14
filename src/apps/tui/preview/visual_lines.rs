use std::cell::{Cell, RefCell};
use std::ops::Range;

use ratatui::style::Color;
use ratatui::text::Line;

use crate::apps::config::{PREVIEW_CODE_WRAP_OVERHEAD, PREVIEW_FRAME_OVERHEAD};
use crate::apps::theme::Theme;
use crate::runner::CodeId;

use crate::apps::tui::markdown::{RenderContext, ViewLine};
use crate::apps::tui::wrap::wrap_line;

/// A single viewport line with plain layout text and paint source metadata.
///
/// `VisualLine`s are produced by [`VisualLines::rebuild`] and stored in
/// [`VisualLines`]. Each corresponds to exactly one row in the terminal, even
/// when the original [`ViewLine`](crate::apps::tui::markdown::ViewLine) was
/// wrapped across multiple rows. The `wrap_idx` and `char_range` fields identify
/// the segment so paint can slice highlighted spans while selection and copying
/// use the syntax-free text.
#[derive(Debug, Clone)]
pub struct VisualLine {
    pub line: Line<'static>,
    pub code_id: Option<CodeId>,
    pub logical_idx: usize,
    pub gutter_fg: Option<Color>,
    /// Which wrapped segment of the original line this visual line represents.
    pub wrap_idx: usize,
    /// Character range within the original unwrapped display line.
    pub char_range: Range<usize>,
}

impl VisualLine {
    /// A line that is never soft-wrapped (tables, PTY output, dividers).
    fn unwrapped(
        line: Line<'static>,
        code_id: Option<CodeId>,
        logical_idx: usize,
        gutter_fg: Option<Color>,
    ) -> Self {
        let char_len = line.to_string().chars().count();
        Self {
            line,
            code_id,
            logical_idx,
            gutter_fg,
            wrap_idx: 0,
            char_range: 0..char_len,
        }
    }

    /// A segment produced by soft-wrapping a logical line.
    fn wrapped(
        line: Line<'static>,
        code_id: Option<CodeId>,
        logical_idx: usize,
        wrap_idx: usize,
        char_offset: usize,
        gutter_fg: Option<Color>,
    ) -> Self {
        let char_len = line.to_string().chars().count();
        Self {
            line,
            code_id,
            logical_idx,
            gutter_fg,
            wrap_idx,
            char_range: char_offset..char_offset + char_len,
        }
    }
}

/// The viewport line cache.
///
/// Owns the [`VisualLine`]s produced from the logical [`ViewLine`]s and tracks
/// the last known terminal dimensions so the cache can be invalidated on resize.
pub struct VisualLines {
    lines: RefCell<Vec<VisualLine>>,
    last_width: Cell<usize>,
    last_height: Cell<usize>,
}

impl Default for VisualLines {
    fn default() -> Self {
        Self::new()
    }
}

impl VisualLines {
    pub fn new() -> Self {
        Self {
            lines: RefCell::new(vec![]),
            last_width: Cell::new(crate::apps::config::PTY_DEFAULT_COLS as usize),
            last_height: Cell::new(0),
        }
    }

    /// Converts logical [`ViewLine`]s into viewport [`VisualLine`]s.
    ///
    /// Each `ViewLine` is rendered without syntax highlighting, then optionally
    /// soft-wrapped by [`wrap_line`](crate::apps::tui::wrap::wrap_line). PTY
    /// output, tables, and dividers are passed through unchanged.
    ///
    /// If `target_block` is set, it is consumed and the visual index of the
    /// requested code-start line is returned so the caller can update its
    /// selection state.
    pub fn rebuild(
        &self,
        logical_lines: &[ViewLine],
        width: usize,
        theme: &Theme,
        target_block: &Cell<Option<CodeId>>,
        is_code_start_at: impl Fn(usize) -> bool,
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

        let mut new_visual_lines = Vec::new();
        for (idx, logical_line) in logical_lines.iter().enumerate() {
            let line = logical_line.render_plain(&ctx);
            let prefix_width = logical_line.prefix_width();
            let wrap_width = if logical_line.code_id.is_some() {
                width
                    .saturating_sub(PREVIEW_CODE_WRAP_OVERHEAD + prefix_width)
                    .max(1)
            } else {
                width
                    .saturating_sub(PREVIEW_FRAME_OVERHEAD + prefix_width)
                    .max(1)
            };
            if logical_line.is_unwrappable() {
                new_visual_lines.push(VisualLine::unwrapped(
                    line,
                    logical_line.code_id,
                    idx,
                    logical_line.gutter_fg,
                ));
            } else {
                for (wrap_idx, (wrapped_line, char_offset, _source_len)) in
                    wrap_line(line, wrap_width).into_iter().enumerate()
                {
                    new_visual_lines.push(VisualLine::wrapped(
                        wrapped_line,
                        logical_line.code_id,
                        idx,
                        wrap_idx,
                        char_offset,
                        logical_line.gutter_fg,
                    ));
                }
            }
        }
        *self.lines.borrow_mut() = new_visual_lines;

        // Apply deferred block jump.
        target_block
            .take()
            .and_then(|id| self.find_code_start(id, is_code_start_at))
    }

    pub fn len(&self) -> usize {
        self.lines.borrow().len()
    }

    pub fn is_empty(&self) -> bool {
        self.lines.borrow().is_empty()
    }

    pub fn get(&self, idx: usize) -> Option<VisualLine> {
        self.lines.borrow().get(idx).cloned()
    }

    pub fn borrow(&self) -> std::cell::Ref<'_, Vec<VisualLine>> {
        self.lines.borrow()
    }

    #[allow(dead_code)]
    pub fn iter(&self) -> impl Iterator<Item = VisualLine> {
        self.lines.borrow().clone().into_iter()
    }

    pub fn visual_idx_of_logical(&self, logical_idx: usize) -> Option<usize> {
        self.lines
            .borrow()
            .iter()
            .position(|l| l.logical_idx == logical_idx)
    }

    pub fn find_code_start(
        &self,
        id: CodeId,
        is_code_start_at: impl Fn(usize) -> bool,
    ) -> Option<usize> {
        self.lines
            .borrow()
            .iter()
            .position(|l| l.code_id == Some(id) && is_code_start_at(l.logical_idx))
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
