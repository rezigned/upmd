use std::cell::Cell;

use crate::runner::CodeId;

use crate::apps::tui::selection::SelectionState;
use crate::apps::tui::wrap::CopyLine;

/// Preview-specific selection state.
///
/// Wraps [`SelectionState`] with the additional bookkeeping needed for
/// click-to-select code blocks.
pub struct PreviewSelection {
    state: SelectionState,
    pending_code_click: Cell<Option<CodeId>>,
}

impl Default for PreviewSelection {
    fn default() -> Self {
        Self::new()
    }
}

impl PreviewSelection {
    pub fn new() -> Self {
        Self {
            state: SelectionState::new(),
            pending_code_click: Cell::new(None),
        }
    }

    pub fn start(&self, line: usize, char_offset: usize) {
        self.state.start(line, char_offset);
    }

    pub fn extend(&self, line: usize, char_offset: usize) {
        self.state.extend(line, char_offset);
    }

    pub fn finish<F>(&self, get_line: F) -> Option<String>
    where
        F: FnMut(usize) -> Option<CopyLine>,
    {
        self.state.finish(get_line)
    }

    pub fn clear(&self) {
        self.state.clear();
    }

    pub fn is_dragging(&self) -> bool {
        self.state.is_dragging()
    }

    pub fn range_for_line(&self, line_idx: usize, line_len: usize) -> Option<(usize, usize)> {
        self.state.range_for_line(line_idx, line_len)
    }

    pub fn set_pending_code_click(&self, id: Option<CodeId>) {
        self.pending_code_click.set(id);
    }

    pub fn take_pending_code_click(&self) -> Option<CodeId> {
        self.pending_code_click.take()
    }

    pub fn copy_text<F>(&self, get_line: F) -> Option<String>
    where
        F: FnMut(usize) -> Option<CopyLine>,
    {
        self.state.copy_text(get_line)
    }

    pub fn clamp_or_clear(&self, line_count: usize) {
        self.state.clamp_or_clear(line_count);
    }
}
