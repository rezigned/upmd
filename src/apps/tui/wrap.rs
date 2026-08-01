use std::ops::Range;

use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthChar;

/// Describes a line of text for selection copying.
///
/// `is_continuation` is `true` when this visual line is a wrapped segment of
/// the same source line as the previous visual line.  Copying skips the newline
/// separator between a line and its continuation so that wrapped lines are
/// copied as a single line of text.
#[derive(Debug, Clone)]
pub struct CopyLine {
    pub text: String,
    pub is_continuation: bool,
    /// Number of display-only characters before `text` in the rendered line.
    pub display_prefix_len: usize,
}

/// Returns the character ranges produced by hard-wrapping `line` to
/// `max_width` display columns.
///
/// This stores no copied text or style data. Consumers that only need a
/// viewport index can slice the source line when a row becomes visible.
pub fn wrap_ranges(line: &Line<'_>, max_width: usize) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    let mut segment_start = 0;
    let mut segment_width = 0;
    let mut char_idx = 0;

    for ch in line.spans.iter().flat_map(|span| span.content.chars()) {
        let width = ch.width().unwrap_or(1);
        if segment_width + width > max_width && char_idx > segment_start {
            ranges.push(segment_start..char_idx);
            segment_start = char_idx;
            segment_width = 0;
        }
        segment_width += width;
        char_idx += 1;
    }

    if char_idx == 0 || segment_start < char_idx {
        ranges.push(segment_start..char_idx);
    }
    ranges
}

/// Extracts a character range while preserving span, line, and alignment styles.
pub fn slice_line(line: &Line<'static>, range: Range<usize>) -> Line<'static> {
    let mut cursor = 0;
    let spans = line
        .spans
        .iter()
        .filter_map(|span| {
            let span_len = span.content.chars().count();
            let span_start = cursor;
            let span_end = cursor + span_len;
            cursor = span_end;

            let start = range.start.max(span_start);
            let end = range.end.min(span_end);
            if start >= end {
                return None;
            }

            let text: String = span
                .content
                .chars()
                .skip(start - span_start)
                .take(end - start)
                .collect();
            Some(Span::styled(text, span.style))
        })
        .collect::<Vec<_>>();

    let mut sliced = Line::from(spans).style(line.style);
    if let Some(alignment) = line.alignment {
        sliced = sliced.alignment(alignment);
    }
    sliced
}

#[cfg(test)]
mod tests {
    use super::*;
    use insta::assert_snapshot;
    use ratatui::style::Style;

    fn wrap_line(line: Line<'static>, max_width: usize) -> Vec<(Line<'static>, usize, usize)> {
        let total_len = line.to_string().chars().count();
        wrap_ranges(&line, max_width)
            .into_iter()
            .map(|range| {
                let offset = range.start;
                (slice_line(&line, range), offset, total_len)
            })
            .collect()
    }

    #[test]
    fn test_wrap_line_plain() {
        let line = Line::from("This is a long line that needs wrapping.");
        let wrapped = wrap_line(line, 10);
        let summary: Vec<String> = wrapped
            .iter()
            .map(|(l, off, len)| format!("[off={off} len={len}] {l}"))
            .collect();
        assert_snapshot!("wrap_line_plain", summary.join("\n"));
    }

    #[test]
    fn test_wrap_line_styled() {
        let mut line = Line::from(vec![
            Span::styled("red ", Style::default().fg(ratatui::style::Color::Red)),
            Span::styled("blue ", Style::default().fg(ratatui::style::Color::Blue)),
            Span::styled("green", Style::default().fg(ratatui::style::Color::Green)),
        ]);
        line.style = Style::default().bg(ratatui::style::Color::Black);
        let wrapped = wrap_line(line, 8);
        let summary: Vec<String> = wrapped
            .iter()
            .map(|(l, off, len)| {
                let styles: Vec<String> = l
                    .spans
                    .iter()
                    .map(|s| format!("'{}' {:?}", s.content, s.style.fg))
                    .collect();
                format!("[off={off} len={len}] {}", styles.join(" | "))
            })
            .collect();
        assert_snapshot!("wrap_line_styled", summary.join("\n"));
    }

    #[test]
    fn test_slice_line_preserves_unicode_boundaries_and_styles() {
        let line = Line::from(vec![
            Span::styled("ab", Style::default().fg(ratatui::style::Color::Red)),
            Span::styled("çd", Style::default().fg(ratatui::style::Color::Blue)),
        ])
        .style(Style::default().bg(ratatui::style::Color::Black))
        .alignment(ratatui::layout::Alignment::Right);

        let sliced = slice_line(&line, 1..3);

        assert_eq!(sliced.to_string(), "bç");
        assert_eq!(sliced.spans.len(), 2);
        assert_eq!(sliced.spans[0].style.fg, Some(ratatui::style::Color::Red));
        assert_eq!(sliced.spans[1].style.fg, Some(ratatui::style::Color::Blue));
        assert_eq!(sliced.style.bg, Some(ratatui::style::Color::Black));
        assert_eq!(sliced.alignment, Some(ratatui::layout::Alignment::Right));
    }

    #[test]
    fn test_wrap_line_empty() {
        let line = Line::default();
        let wrapped = wrap_line(line, 10);
        assert_eq!(wrapped.len(), 1);
        assert_eq!(wrapped[0].1, 0); // offset
        assert_eq!(wrapped[0].2, 0); // source_len
    }
}
