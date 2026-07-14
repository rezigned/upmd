use std::io::Read;
use std::path::Path;

use crate::apps::theme::Theme;
use crate::apps::tui::layout::centered_rect;
use crate::apps::tui::Shortcut;
use crate::markdown_files::MarkdownFile;
use keymap::DerivedConfig;
use ratatui::style::Style;
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::Paragraph;
use std::collections::HashMap;
use upmd_runtime::{
    runtimes::tui::{Input, Output},
    Cmd, Component,
};

const MIN_PREVIEW_WIDTH: u16 = 72;
const PREVIEW_OVERSCAN: usize = 3;
const PREVIEW_MAX_LINES: usize = 200;
const PREVIEW_MAX_BYTES: usize = 64 * 1024;

/// Cached highlighted preview for a file. Stores only a bounded prefix; when
/// `truncated` is true the file is longer than what `lines` covers.
struct PreviewCache {
    lines: Vec<Line<'static>>,
    line_count: usize,
    truncated: bool,
}

/// Intermediate result of reading a bounded file prefix before highlighting.
/// `content` holds the raw text of up to PREVIEW_MAX_LINES lines, capped at
/// PREVIEW_MAX_BYTES.  `line_count` is the number of complete lines in content;
/// `truncated` is set when the file has more content beyond what was read.
struct PreviewPrefix {
    content: String,
    line_count: usize,
    truncated: bool,
}

pub struct FilePicker {
    state: crate::apps::picker::PickerState,
    /// Cached highlighted preview prefixes keyed by file index.
    /// Repopulated when the selected file changes.
    previews: HashMap<usize, PreviewCache>,
    theme: Theme,
    keymap: DerivedConfig<Action>,
}

/// Reads a bounded prefix of a Markdown file for picker preview.
///
/// Two-stage truncation: first capped at PREVIEW_MAX_BYTES via `take()`, then
/// capped at PREVIEW_MAX_LINES via `split_inclusive('\n')`.  The result carries
/// `truncated=true` if either cap was exceeded, plus the actual line count so
/// the preview header can show `first N lines`.
///
/// Note on the byte boundary: `from_utf8_lossy` may insert replacement chars
/// for a multi-byte character split across the cap.  When that happens the
/// split-inclusive line counter treats the broken char's line as one complete
/// line, then `lines.next().is_some()` correctly detects further content beyond
/// the line cap.  So line_count is an upper bound for the visible content.
fn read_preview_prefix(path: &Path) -> std::io::Result<PreviewPrefix> {
    let file = std::fs::File::open(path)?;
    let total_bytes = file.metadata().ok().map(|metadata| metadata.len());
    let mut limited = file.take(PREVIEW_MAX_BYTES as u64);
    let mut bytes = Vec::new();
    limited.read_to_end(&mut bytes)?;

    let bytes_truncated = total_bytes.is_some_and(|len| len > bytes.len() as u64);
    let text = String::from_utf8_lossy(&bytes);
    let mut content = String::new();
    let mut line_count = 0usize;
    let mut lines = text.split_inclusive('\n');

    for line in lines.by_ref().take(PREVIEW_MAX_LINES) {
        content.push_str(line);
        line_count += 1;
    }

    // After the take() exhausted its iteration budget, did the original
    // iterator have any content left?  If yes, the line cap was exceeded.
    let line_truncated = lines.next().is_some();

    Ok(PreviewPrefix {
        content,
        line_count,
        truncated: bytes_truncated || line_truncated,
    })
}

pub use crate::apps::picker::PickerAction as Action;

impl FilePicker {
    pub fn new(files: Vec<MarkdownFile>, theme: Theme, keymap: DerivedConfig<Action>) -> Self {
        let mut picker = Self {
            state: crate::apps::picker::PickerState::new(files),
            theme,
            keymap,
            previews: HashMap::new(),
        };
        picker.refresh_preview();
        picker
    }

    fn selected_file_idx(&self) -> Option<usize> {
        self.state.selected_file_idx()
    }

    pub fn selected_path(&self) -> Option<&Path> {
        let idx = self.selected_file_idx()?;
        Some(self.state.files[idx].path.as_path())
    }

    /// Reads and highlights a bounded prefix of the selected file, caching by file index.
    fn refresh_preview(&mut self) {
        let Some(idx) = self.selected_file_idx() else {
            self.previews.clear();
            return;
        };
        if self.previews.contains_key(&idx) {
            return;
        }
        let path = &self.state.files[idx].path;
        match read_preview_prefix(path) {
            Ok(prefix) => {
                let highlighted = self.theme.highlight(&prefix.content, "md");
                let lines: Vec<Line<'static>> = highlighted.lines.into_iter().collect();
                self.previews.insert(
                    idx,
                    PreviewCache {
                        lines,
                        line_count: prefix.line_count,
                        truncated: prefix.truncated,
                    },
                );
            }
            Err(_) => {
                self.previews.insert(
                    idx,
                    PreviewCache {
                        lines: vec![Line::from("(error reading file)")],
                        line_count: 1,
                        truncated: false,
                    },
                );
            }
        }
    }

    /// Builds the filter row with the match count right-aligned.
    fn filter_line(&self, width: u16) -> Text<'_> {
        let has_query = !self.state.query.is_empty();
        let query_display = if has_query {
            self.state.query.as_str()
        } else {
            "type to filter..."
        };
        let count = format!("({}/{})", self.state.matches.len(), self.state.files.len());
        let left_width = unicode_width::UnicodeWidthStr::width("File: ")
            + unicode_width::UnicodeWidthStr::width(query_display);
        let gap_width = (width as usize).saturating_sub(left_width + count.len() + 1);

        Text::from(Line::from(vec![
            Span::raw("File: "),
            Span::styled(
                query_display,
                Style::default().fg(if has_query {
                    self.theme.active
                } else {
                    self.theme.muted
                }),
            ),
            Span::raw(" ".repeat(gap_width)),
            Span::raw(format!(" {count}")),
        ]))
    }

    /// Builds a horizontally and vertically centered muted message.
    fn centered_message(&self, message: &'static str, height: u16) -> Paragraph<'static> {
        let pad = (height as usize).saturating_sub(1) / 2;
        let mut lines: Vec<Line<'static>> = (0..pad).map(|_| Line::raw("")).collect();
        lines.push(Line::from(Span::styled(
            message,
            Style::default().fg(self.theme.muted),
        )));

        Paragraph::new(Text::from(lines)).alignment(ratatui::layout::Alignment::Center)
    }

    /// Returns the visible match window for the current selection.
    fn visible_match_range(&self, height: u16) -> std::ops::Range<usize> {
        if height == 0 {
            return 0..0;
        }

        let total = self.state.matches.len();
        let capacity = (height as usize).saturating_sub(1);
        let start = if total > capacity && self.state.selected >= capacity {
            self.state.selected - capacity + 1
        } else {
            0
        };
        start..(start + height as usize).min(total)
    }

    /// Builds styled list rows for the visible match window.
    fn visible_match_items(&self, height: u16) -> Vec<ratatui::widgets::ListItem<'static>> {
        let range = self.visible_match_range(height);
        self.state.matches[range.clone()]
            .iter()
            .enumerate()
            .filter_map(|(offset, file_idx)| {
                let abs = range.start + offset;
                let is_selected = abs == self.state.selected;
                let file = self.state.files.get(*file_idx)?;
                let prefix = if is_selected { "▸ " } else { "  " };
                let fg = if is_selected {
                    self.theme.active
                } else {
                    self.theme.foreground
                };
                Some(ratatui::widgets::ListItem::new(Line::from(Span::styled(
                    format!("{prefix}{}", file.display),
                    Style::default().fg(fg),
                ))))
            })
            .collect()
    }

    /// Builds the selected file preview, including its header line.
    fn preview_lines(&self, height: u16) -> Vec<Line<'static>> {
        let Some(idx) = self.selected_file_idx() else {
            return Vec::new();
        };
        let Some(file) = self.state.files.get(idx) else {
            return Vec::new();
        };
        let Some(preview) = self.previews.get(&idx) else {
            return Vec::new();
        };

        let take = (height as usize + PREVIEW_OVERSCAN).min(preview.lines.len());
        let mut lines: Vec<Line<'static>> = preview.lines.iter().take(take).cloned().collect();
        if !lines.is_empty() {
            let count = if preview.truncated {
                format!("first {} lines", preview.line_count)
            } else {
                format!("{} lines", preview.line_count)
            };
            lines.insert(
                0,
                Line::from(Span::styled(
                    format!("{}  ({count})", file.display),
                    Style::default().fg(self.theme.muted),
                )),
            );
            if preview.truncated {
                lines.push(Line::from(Span::styled(
                    "... preview truncated",
                    Style::default().fg(self.theme.muted),
                )));
            }
        }
        lines
    }
}

impl Shortcut for FilePicker {
    fn footer_shortcuts(&self) -> Line<'static> {
        self.theme.shortcuts(&[
            ("↑↓".to_string(), "move".to_string()),
            ("↵".to_string(), "open".to_string()),
            ("esc".to_string(), "cancel".to_string()),
        ])
    }
}

impl Component for FilePicker {
    type Msg = Action;

    fn update(&mut self, msg: Self::Msg) -> Option<Cmd<Self::Msg>> {
        if self.state.handle_navigation(&msg) {
            self.refresh_preview();
            return None;
        }
        match msg {
            Action::Select | Action::Quit => {
                // These are terminal actions handled by the app.
                // Return a command so the app handler can pick them up.
                Some(Cmd::msg(msg))
            }
            _ => None,
        }
    }
}

impl Input for FilePicker {
    fn action(&self, event: crossterm::event::Event) -> Option<Self::Msg> {
        match event {
            crossterm::event::Event::Key(key) => self.keymap.get_bound(&key),
            crossterm::event::Event::Mouse(mouse) => match mouse.kind {
                crossterm::event::MouseEventKind::ScrollDown => Some(Action::Next),
                crossterm::event::MouseEventKind::ScrollUp => Some(Action::Prev),
                _ => None,
            },
            _ => None,
        }
    }
}

impl Output for FilePicker {
    fn render(&self, frame: &mut ratatui::Frame, area: ratatui::layout::Rect) {
        use ratatui::{
            layout::{Constraint, Direction, Layout},
            widgets::{Clear, List},
        };

        let block = self.theme.popup_block("Open Markdown file");
        let popup_area = centered_rect(70, 50, area);
        frame.render_widget(Clear, popup_area);
        frame.render_widget(block.clone(), popup_area);

        let inner = block.inner(popup_area);
        let vert = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Min(0),
                Constraint::Length(1),
            ])
            .split(inner);

        frame.render_widget(Paragraph::new(self.filter_line(vert[0].width)), vert[0]);
        frame.render_widget(self.theme.footer(self.footer_shortcuts()), vert[2]);

        let body_area = vert[1];
        if self.state.matches.is_empty() {
            frame.render_widget(
                self.centered_message("No matching files", body_area.height),
                body_area,
            );
            return;
        }

        let show_preview = body_area.width >= MIN_PREVIEW_WIDTH;
        let (list_area, preview_area) = if show_preview {
            let horiz = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
                .split(body_area);
            (horiz[0], horiz[1])
        } else {
            (body_area, ratatui::layout::Rect::default())
        };

        frame.render_widget(
            List::new(self.visible_match_items(list_area.height)),
            list_area,
        );

        if show_preview {
            frame.render_widget(
                Paragraph::new(Text::from(self.preview_lines(preview_area.height))),
                preview_area,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    fn picker() -> FilePicker {
        FilePicker::new(
            vec![
                MarkdownFile {
                    path: PathBuf::from("/repo/README.md"),
                    display: "README.md".to_string(),
                },
                MarkdownFile {
                    path: PathBuf::from("/repo/docs/install.md"),
                    display: "docs/install.md".to_string(),
                },
            ],
            Theme::default(),
            toml::from_str("").unwrap(),
        )
    }

    #[test]
    fn filters_by_display_path() {
        let mut picker = picker();
        picker.update(Action::Input('i'));
        picker.update(Action::Input('n'));
        assert_eq!(picker.state.matches.len(), 1);
        assert_eq!(
            picker.selected_path().unwrap(),
            Path::new("/repo/docs/install.md")
        );
    }

    #[test]
    fn preview_prefix_reads_only_top_lines() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let content = (0..(PREVIEW_MAX_LINES + 25))
            .map(|idx| format!("line {idx}\n"))
            .collect::<String>();
        std::fs::write(tmp.path(), content).unwrap();

        let prefix = read_preview_prefix(tmp.path()).unwrap();

        assert_eq!(prefix.line_count, PREVIEW_MAX_LINES);
        assert!(prefix.truncated);
        assert!(prefix.content.contains("line 0\n"));
        assert!(prefix
            .content
            .contains(&format!("line {}\n", PREVIEW_MAX_LINES - 1)));
        assert!(!prefix
            .content
            .contains(&format!("line {}\n", PREVIEW_MAX_LINES)));
    }

    #[test]
    fn preview_prefix_marks_byte_truncation() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "a".repeat(PREVIEW_MAX_BYTES + 1)).unwrap();

        let prefix = read_preview_prefix(tmp.path()).unwrap();

        assert_eq!(prefix.line_count, 1);
        assert_eq!(prefix.content.len(), PREVIEW_MAX_BYTES);
        assert!(prefix.truncated);
    }

    #[test]
    fn query_with_no_matches_clears_selection_and_recovers_after_delete() {
        let mut picker = picker();

        picker.update(Action::Input('z'));
        assert!(picker.state.matches.is_empty());
        assert_eq!(picker.selected_path(), None);

        picker.update(Action::Delete);
        assert_eq!(picker.state.matches.len(), 2);
        assert_eq!(
            picker.selected_path().unwrap(),
            Path::new("/repo/README.md")
        );
    }

    #[test]
    fn navigation_clamps_to_matches() {
        let mut picker = picker();
        picker.update(Action::Next);
        picker.update(Action::Next);
        assert_eq!(
            picker.selected_path().unwrap(),
            Path::new("/repo/docs/install.md")
        );
        picker.update(Action::Prev);
        assert_eq!(
            picker.selected_path().unwrap(),
            Path::new("/repo/README.md")
        );
    }
}
