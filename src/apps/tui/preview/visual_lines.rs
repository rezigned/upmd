use std::cell::{Cell, RefCell};
use std::ops::Range;

use crate::apps::config::{PREVIEW_CODE_WRAP_OVERHEAD, PREVIEW_FRAME_OVERHEAD};
use crate::apps::theme::Theme;
use crate::runner::CodeId;

use crate::apps::tui::markdown::{LogicalLine, RenderContext};
use crate::apps::tui::wrap::wrap_ranges;

/// A single viewport row mapped back to its logical source line.
///
/// `VisualLine`s contain layout metadata only. Text and styles remain on the
/// corresponding [`LogicalLine`](crate::apps::tui::markdown::LogicalLine) and are
/// sliced when the row is rendered, selected, or copied.
#[derive(Debug, Clone)]
pub struct VisualLine {
    pub code_id: Option<CodeId>,
    pub logical_idx: usize,
    /// Which wrapped segment of the original line this visual line represents.
    pub wrap_idx: usize,
    /// Character range within the original unwrapped display line.
    pub char_range: Range<usize>,
}

impl VisualLine {
    fn unwrapped(code_id: Option<CodeId>, logical_idx: usize, char_len: usize) -> Self {
        Self {
            code_id,
            logical_idx,
            wrap_idx: 0,
            char_range: 0..char_len,
        }
    }

    /// A segment produced by soft-wrapping a logical line.
    fn wrapped(
        code_id: Option<CodeId>,
        logical_idx: usize,
        wrap_idx: usize,
        char_range: Range<usize>,
    ) -> Self {
        Self {
            code_id,
            logical_idx,
            wrap_idx,
            char_range,
        }
    }
}

/// The viewport line cache.
///
/// Owns the [`VisualLine`]s produced from the logical [`LogicalLine`]s and tracks
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

    /// Each `LogicalLine` is rendered without syntax highlighting, then optionally
    /// measured by [`wrap_ranges`](crate::apps::tui::wrap::wrap_ranges). PTY
    /// output, tables, and dividers are indexed as one row.
    ///
    /// If `target_block` is set, it is consumed and the visual index of the
    /// requested code-start line is returned so the caller can update its
    /// selection state.
    pub fn rebuild(
        &self,
        logical_lines: &[LogicalLine],
        width: usize,
        theme: &Theme,
        target_block: &Cell<Option<CodeId>>,
        is_code_start_at: impl Fn(usize) -> bool,
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

        let mut new_visual_lines = Vec::new();
        for (idx, logical_line) in logical_lines.iter().enumerate() {
            if logical_line.is_image() {
                // Represent image height as repeated visual rows. Only the
                // first row carries alt text.
                for row in 0..rows_for(logical_line).max(1) {
                    new_visual_lines.push(VisualLine::wrapped(
                        logical_line.code_id,
                        idx,
                        row,
                        0..0,
                    ));
                }
                continue;
            }
            let line = logical_line.render_plain(&ctx);
            let prefix_width = logical_line.prefix_width();
            let wrap_width = if logical_line.has_code_gutter() {
                width
                    .saturating_sub(PREVIEW_CODE_WRAP_OVERHEAD + prefix_width)
                    .max(1)
            } else {
                width
                    .saturating_sub(PREVIEW_FRAME_OVERHEAD + prefix_width)
                    .max(1)
            };
            if logical_line.is_unwrappable() {
                let char_len = line.to_string().chars().count();
                new_visual_lines.push(VisualLine::unwrapped(logical_line.code_id, idx, char_len));
            } else {
                for (wrap_idx, char_range) in wrap_ranges(&line, wrap_width).into_iter().enumerate()
                {
                    new_visual_lines.push(VisualLine::wrapped(
                        logical_line.code_id,
                        idx,
                        wrap_idx,
                        char_range,
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
