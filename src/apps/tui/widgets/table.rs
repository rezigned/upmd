use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use ratatui::Frame;

use crate::apps::theme::Theme;

/// Renders the popup block, 2-column header, and returns the row content
/// area, footer area, and the inner width (for column width computations).
///
/// Caller should fill the content area with [`render_table`] and the footer
/// area with their own footer widget.
pub fn render_popup_frame(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    theme: &Theme,
    header_left: &str,
    header_right: &str,
) -> (Rect, Rect, u16) {
    let block = theme.popup_block(title);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let vert = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(inner);

    let header_h = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(vert[0]);

    let header_style = theme.table_header_style();
    frame.render_widget(Paragraph::new(header_left).style(header_style), header_h[0]);
    frame.render_widget(
        Paragraph::new(header_right).style(header_style),
        header_h[1],
    );

    (vert[1], vert[2], inner.width)
}

/// Renders a scrollable list of rows in 2 columns with wrapping.
///
/// `render_row` returns `(left_line, right_line, style)` for each item.
/// Both columns are rendered as [`Paragraph`] with [`Wrap`] enabled.
pub fn render_table<T>(
    frame: &mut Frame,
    rows_area: Rect,
    items: &[T],
    selected: Option<usize>,
    row_height: &[u16],
    mut render_row: impl FnMut(&T, usize, bool) -> (Line<'static>, Line<'static>, Style),
) {
    if items.is_empty() || row_height.is_empty() {
        return;
    }
    let area_bot = rows_area.y.saturating_add(rows_area.height);
    let start = selected.map_or(0, |sel| {
        sel.saturating_sub((rows_area.height as usize).saturating_div(2))
    });

    let mut y = rows_area.y;
    for (i, item) in items.iter().enumerate().skip(start) {
        let h = row_height.get(i).copied().unwrap_or(1);
        if y.saturating_add(h) > area_bot {
            break;
        }
        let row_area = Rect {
            x: rows_area.x,
            y,
            width: rows_area.width,
            height: h,
        };
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(row_area);

        let (left, right, style) = render_row(item, i, selected == Some(i));
        frame.render_widget(Paragraph::new(left).style(style), cols[0]);
        frame.render_widget(
            Paragraph::new(right)
                .style(style)
                .wrap(Wrap { trim: false }),
            cols[1],
        );

        y = y.saturating_add(h);
    }
}

/// Returns a [`Line`] with search term matches highlighted using `highlight_style`.
/// Works at char level to avoid byte-offset mismatches from Unicode case folding.
pub fn highlight_text(text: &str, search_term: &str, highlight_style: Style) -> Line<'static> {
    if search_term.is_empty() {
        return Line::from(text.to_string());
    }

    let chars: Vec<char> = text.chars().collect();
    let lower_chars: Vec<char> = text.to_lowercase().chars().collect();
    let lower_term: Vec<char> = search_term.to_lowercase().chars().collect();

    if lower_term.is_empty() {
        return Line::from(text.to_string());
    }

    let mut spans = vec![];
    let mut last_end = 0usize;

    // Slide over lowercased chars to find matching windows. Char-level
    // slicing avoids the byte-offset mismatch from Unicode case folds
    // that change byte length between original and lowercased text.
    let search_len = lower_term.len();
    let limit = lower_chars.len().saturating_sub(search_len);
    for start in 0..=limit {
        if lower_chars[start..].starts_with(&lower_term) {
            if start > last_end {
                spans.push(Span::raw(chars[last_end..start].iter().collect::<String>()));
            }
            let matched: String = chars[start..start + search_len].iter().collect();
            spans.push(Span::styled(matched, highlight_style));
            last_end = start + search_len;
        }
    }

    if last_end < chars.len() {
        spans.push(Span::raw(chars[last_end..].iter().collect::<String>()));
    }

    if spans.is_empty() {
        Line::from(text.to_string())
    } else {
        Line::from(spans)
    }
}
