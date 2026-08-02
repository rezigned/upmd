use ratatui::style::Style;
use ratatui::text::Line;

use crate::apps::theme::ansi_style;

const RESET: &str = "\x1b[0m";

/// Renders a [`Line`] as the ANSI escape sequence stream a terminal would
/// receive, merging adjacent same-style spans. Unstyled lines are plain text.
pub fn ansi_line(line: &Line<'static>) -> String {
    let mut out = String::new();
    let mut pending: Option<(Style, String)> = None;
    for span in &line.spans {
        if span.content.is_empty() {
            continue;
        }
        let style = line.style.patch(span.style);
        match &mut pending {
            Some((pending_style, text)) if *pending_style == style => text.push_str(&span.content),
            _ => {
                if let Some((style, text)) = pending.take() {
                    out.push_str(&ansi_style(style));
                    out.push_str(&text);
                }
                pending = Some((style, span.content.to_string()));
            }
        }
    }
    if let Some((style, text)) = pending {
        out.push_str(&ansi_style(style));
        out.push_str(&text);
    }
    if out.is_empty() {
        return String::new();
    }
    out.push_str(RESET);
    out
}

pub fn ansi_line_summary(lines: &[Line<'static>]) -> String {
    lines.iter().map(ansi_line).collect::<Vec<_>>().join("\n")
}
