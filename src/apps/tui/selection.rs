use std::cell::Cell;

use ratatui::{
    layout::Rect,
    style::Style,
    text::{Line, Span},
};
use unicode_width::UnicodeWidthChar;

use super::wrap::CopyLine;

/// Tracks an active text selection across lines with character-level precision.
///
/// Stores start and end positions as `(line_index, char_offset)` pairs so that
/// partial-line highlighting and copying work correctly.
#[derive(Debug, Clone)]
pub struct SelectionState {
    start: Cell<Option<(usize, usize)>>,
    end: Cell<Option<(usize, usize)>>,
    is_dragging: Cell<bool>,
}

impl Default for SelectionState {
    fn default() -> Self {
        Self {
            start: Cell::new(None),
            end: Cell::new(None),
            is_dragging: Cell::new(false),
        }
    }
}

impl SelectionState {
    /// Creates a new, empty selection state.
    pub fn new() -> Self {
        Self::default()
    }

    /// Clears the current selection and stops dragging.
    pub fn clear(&self) {
        self.start.set(None);
        self.end.set(None);
        self.is_dragging.set(false);
    }

    /// Clamps the selection to the given number of lines.
    ///
    /// If either endpoint references a line that no longer exists, the
    /// selection is cleared. This should be called after the underlying
    /// visual-line set changes (e.g. rebuilds, resizes, or reloads).
    pub fn clamp_or_clear(&self, line_count: usize) {
        if line_count == 0 {
            self.clear();
            return;
        }
        let max_idx = line_count - 1;
        let in_range = |p: Option<(usize, usize)>| p.is_none_or(|(line, _)| line <= max_idx);
        if !in_range(self.start.get()) || !in_range(self.end.get()) {
            self.clear();
        }
    }

    /// Starts a new selection at the given position.
    pub fn start(&self, line: usize, char_offset: usize) {
        self.start.set(Some((line, char_offset)));
        self.end.set(Some((line, char_offset)));
        self.is_dragging.set(true);
    }

    /// Extends the selection end to a new position while dragging.
    pub fn extend(&self, line: usize, char_offset: usize) {
        if self.is_dragging.get() {
            self.end.set(Some((line, char_offset)));
        }
    }

    /// Finalises the selection (stops dragging) and returns the selected text.
    ///
    /// `get_line` is called with a line index and should return the [`CopyLine`]
    /// for that visual line (or `None` if the index is out of bounds).
    pub fn finish<F>(&self, mut get_line: F) -> Option<String>
    where
        F: FnMut(usize) -> Option<CopyLine>,
    {
        if !self.is_dragging.get() {
            return None;
        }
        self.is_dragging.set(false);
        self.copy_text(&mut get_line)
    }

    /// Copies the current selection without finalising it.
    pub fn copy_text<F>(&self, mut get_line: F) -> Option<String>
    where
        F: FnMut(usize) -> Option<CopyLine>,
    {
        let (start_line, start_char) = self.start.get()?;
        let (end_line, end_char) = self.end.get()?;

        let (lo_line, lo_char, hi_line, hi_char) =
            if start_line < end_line || (start_line == end_line && start_char <= end_char) {
                (start_line, start_char, end_line, end_char)
            } else {
                (end_line, end_char, start_line, start_char)
            };

        let mut text = String::new();
        for line_idx in lo_line..=hi_line {
            let CopyLine {
                text: line_text,
                is_continuation,
                display_prefix_len,
            } = get_line(line_idx)?;
            if line_text.is_empty() {
                continue;
            }

            let line_len = line_text.chars().count();
            let sel_start = if line_idx == lo_line {
                lo_char.saturating_sub(display_prefix_len)
            } else {
                0
            };
            let sel_end = if line_idx == hi_line {
                hi_char.saturating_sub(display_prefix_len).min(line_len)
            } else {
                line_len
            };

            if sel_start >= sel_end {
                continue;
            }

            let selected: String = line_text
                .chars()
                .skip(sel_start)
                .take(sel_end - sel_start)
                .collect();
            if !selected.is_empty() {
                if !text.is_empty() && !is_continuation {
                    text.push('\n');
                }
                text.push_str(&selected);
            }
        }

        if text.is_empty() {
            None
        } else {
            Some(text)
        }
    }

    /// Returns the selection range `(start_char, end_char)` for a given line,
    /// or `None` if the line is outside the selection.
    pub fn range_for_line(&self, line_idx: usize, line_len: usize) -> Option<(usize, usize)> {
        let (start_line, start_char) = self.start.get()?;
        let (end_line, end_char) = self.end.get()?;

        let (lo_line, lo_char, hi_line, hi_char) =
            if start_line < end_line || (start_line == end_line && start_char <= end_char) {
                (start_line, start_char, end_line, end_char)
            } else {
                (end_line, end_char, start_line, start_char)
            };

        if line_idx < lo_line || line_idx > hi_line {
            return None;
        }

        let sel_start = if line_idx == lo_line { lo_char } else { 0 };
        let sel_end = if line_idx == hi_line {
            hi_char.min(line_len)
        } else {
            line_len
        };
        Some((sel_start, sel_end))
    }

    /// Returns `true` if a selection is currently active (either dragging or
    /// finished but not yet cleared).
    pub fn is_active(&self) -> bool {
        self.start.get().is_some()
    }

    /// Returns `true` while the user is actively dragging the mouse.
    pub fn is_dragging(&self) -> bool {
        self.is_dragging.get()
    }

    /// Maps a mouse column to a character offset within a single `Line`.
    pub fn char_offset_from_col(line: &Line<'_>, target_col: usize) -> usize {
        let mut col = 0;
        let mut char_offset = 0;
        for span in &line.spans {
            for ch in span.content.chars() {
                let width = ch.width().unwrap_or(1);
                if col + width > target_col {
                    return char_offset;
                }
                col += width;
                char_offset += 1;
            }
        }
        char_offset
    }

    /// Applies a selection style to a character range within a `Line`.
    ///
    /// Splits spans at the selection boundaries so that only the selected
    /// characters receive the highlight style while the rest keep their original
    /// styling.
    pub fn apply_range(
        line: Line<'_>,
        sel_start: usize,
        sel_end: usize,
        sel_style: Style,
    ) -> Line<'_> {
        let mut new_spans = Vec::new();
        let mut char_pos = 0;

        for span in line.spans {
            let span_text = span.content.to_string();
            let span_len = span_text.chars().count();
            let span_end = char_pos + span_len;

            if span_end <= sel_start || char_pos >= sel_end {
                new_spans.push(span);
            } else {
                let sel_start_in_span = sel_start.saturating_sub(char_pos);
                let sel_end_in_span = (sel_end - char_pos).min(span_len);

                if sel_start_in_span > 0 {
                    let before: String = span_text.chars().take(sel_start_in_span).collect();
                    new_spans.push(Span::styled(before, span.style));
                }

                let selected: String = span_text
                    .chars()
                    .skip(sel_start_in_span)
                    .take(sel_end_in_span.saturating_sub(sel_start_in_span))
                    .collect();
                if !selected.is_empty() {
                    new_spans.push(Span::styled(selected, span.style.patch(sel_style)));
                }

                if sel_end_in_span < span_len {
                    let after: String = span_text.chars().skip(sel_end_in_span).collect();
                    new_spans.push(Span::styled(after, span.style));
                }
            }

            char_pos += span_len;
        }

        Line::from(new_spans)
    }

    /// Converts global mouse coordinates to a `(line_index, char_offset)` within
    /// a text widget.
    ///
    /// `area` is the widget's `Rect`.  `content_offset` is the number of columns
    /// from the left edge of `area` to the start of the text (borders + padding).
    /// `line_at_row` is called with a visual row index and must return the
    /// corresponding global line index and the `Line` at that position.
    pub fn mouse_to_position<'a, F>(
        area: Rect,
        mouse_row: u16,
        mouse_col: u16,
        content_offset: u16,
        mut line_at_row: F,
    ) -> Option<(usize, usize)>
    where
        F: FnMut(usize) -> Option<(usize, &'a Line<'static>)>,
    {
        if area.width == 0 {
            return None;
        }

        let content_x = area.x + content_offset;
        let content_y = area.y + 1; // below top border
        let content_right = area.x + area.width.saturating_sub(1);
        let content_bottom = area.y + area.height.saturating_sub(1);

        if mouse_col < content_x
            || mouse_col >= content_right
            || mouse_row < content_y
            || mouse_row >= content_bottom
        {
            return None;
        }

        let rel_row = mouse_row.saturating_sub(content_y) as usize;
        let target_col = mouse_col.saturating_sub(content_x) as usize;

        let (global_line_idx, line) = line_at_row(rel_row)?;
        let char_offset = Self::char_offset_from_col(line, target_col);

        Some((global_line_idx, char_offset))
    }
}
