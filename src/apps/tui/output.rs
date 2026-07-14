use crate::apps::task::Task;
use keymap::{DerivedConfig, KeyMap};
use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::Style,
    text::Line,
    widgets::{Block, Paragraph, Wrap},
    Frame,
};
use upmd_runtime::{runtimes::tui::Input, Cmd, Component};

#[derive(KeyMap, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action {
    /// Go back to home view
    #[key("ctrl-o", help = "back")]
    Back,
    /// Go back to home view when the buffer is done (esc passthrough when running)
    #[key("esc", help = "back (done)")]
    BackIfDone,
    /// Copy output to clipboard
    #[key("y", "Y", help = "copy")]
    Copy,
    /// Paste clipboard content into the active PTY
    #[key("ctrl-v", help = "paste")]
    Paste,
}

/// PTY terminal output with selection support.
pub struct Output {
    keymap: DerivedConfig<Action>,
    selection: crate::apps::tui::selection::SelectionState,
    last_area: std::cell::Cell<Rect>,
    spinner: crate::apps::tui::widgets::Spinner,
    done: bool,
}

impl Output {
    pub fn new(keymap: DerivedConfig<Action>) -> Self {
        Self {
            keymap,
            selection: crate::apps::tui::selection::SelectionState::new(),
            last_area: std::cell::Cell::new(Rect::default()),
            spinner: crate::apps::tui::widgets::Spinner::default(),
            done: false,
        }
    }

    /// Advances the spinner tick counter (driven by Msg::Tick).
    pub fn tick(&mut self) {
        self.spinner.tick();
    }

    pub fn update_state(&mut self, buf: &Task) {
        self.done = buf.done();
    }

    pub fn handle_mouse_event(
        &mut self,
        mouse: crossterm::event::MouseEvent,
        buf: &mut Task,
    ) -> bool {
        // Ensure parser scrollback matches buf.scroll (may have been changed
        // by inline scroll operations).
        buf.parser.scroll(buf.scroll.into());
        buf.scroll = buf.parser.screen().scrollback() as u16;

        use crossterm::event::{MouseButton, MouseEventKind};
        match mouse.kind {
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                if mouse.kind == MouseEventKind::ScrollUp {
                    buf.scroll = buf.scroll.saturating_add(1);
                } else {
                    buf.scroll = buf.scroll.saturating_sub(1);
                }
                buf.parser.scroll(buf.scroll.into());
                buf.scroll = buf.parser.screen().scrollback() as u16;
                false
            }
            MouseEventKind::Down(MouseButton::Left) => {
                let inner = self.inner_area();
                if mouse.column < inner.x
                    || mouse.column >= inner.x + inner.width
                    || mouse.row < inner.y
                    || mouse.row >= inner.y + inner.height
                {
                    self.selection.clear();
                    return false;
                }
                if let Some((global_line, char_offset)) =
                    self.position_at(mouse.row, mouse.column, buf)
                {
                    self.selection.start(global_line, char_offset);
                }
                false
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if !self.selection.is_dragging() {
                    return false;
                }
                let inner = self.inner_area();
                let clamped_row = mouse
                    .row
                    .clamp(inner.y, inner.y + inner.height.saturating_sub(1));
                let clamped_col = mouse
                    .column
                    .clamp(inner.x, inner.x + inner.width.saturating_sub(1));
                if let Some((global_line, char_offset)) =
                    self.position_at(clamped_row, clamped_col, buf)
                {
                    self.selection.extend(global_line, char_offset);
                }
                false
            }
            MouseEventKind::Up(MouseButton::Left) => {
                let result = self.selection.finish(|line_idx| {
                    let text = buf.parser.contents();
                    text.lines
                        .get(line_idx.saturating_sub(buf.scroll as usize))
                        .map(|l| crate::apps::tui::wrap::CopyLine {
                            text: l.to_string(),
                            is_continuation: false,
                            display_prefix_len: 0,
                        })
                });
                if let Some(text) = result {
                    crate::utils::clipboard_copy(&text)
                } else {
                    false
                }
            }
            _ => false,
        }
    }

    pub fn render(
        &self,
        frame: &mut Frame,
        area: Rect,
        buf: &Task,
        theme: &crate::apps::theme::Theme,
    ) {
        // Sync parser scrollback to match buf.scroll (inline scroll ops may have
        // moved it independently).
        buf.parser.scroll(buf.scroll.into());

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(0), Constraint::Length(1)])
            .split(area);

        let output_area = chunks[0];
        let footer_area = chunks[1];

        self.last_area.set(output_area);

        let mut text = buf.parser.contents();
        let block = Block::default().style(
            ratatui::style::Style::default()
                .fg(ratatui::style::Color::Reset)
                .bg(ratatui::style::Color::Reset),
        );
        let inner = block.inner(output_area);

        // Apply selection highlight to output lines.
        if self.selection.is_active() {
            let scroll = buf.scroll as usize;
            let sel_style = theme.selection_style();
            for (row, line) in text.lines.iter_mut().enumerate() {
                let global_line = row + scroll;
                let line_len = line.to_string().chars().count();
                if let Some((sel_start, sel_end)) =
                    self.selection.range_for_line(global_line, line_len)
                {
                    *line = crate::apps::tui::selection::SelectionState::apply_range(
                        line.clone(),
                        sel_start,
                        sel_end,
                        sel_style,
                    );
                }
            }
        }

        let content = Paragraph::new(text).block(block).wrap(Wrap { trim: false });

        frame.render_widget(content, output_area);

        if !buf.parser.screen().hide_cursor() {
            let (y, x) = buf.parser.screen().cursor_position();
            let cx = inner.x + x;
            let cy = y.saturating_add(inner.y).saturating_sub(buf.scroll);
            if cx >= inner.x
                && cx < inner.x + inner.width
                && cy >= inner.y
                && cy < inner.y + inner.height
            {
                frame.set_cursor_position((cx, cy));
            }
        }

        self.render_footer(frame, footer_area, theme, buf);
    }

    fn render_footer(
        &self,
        frame: &mut Frame,
        area: Rect,
        theme: &crate::apps::theme::Theme,
        buf: &Task,
    ) {
        use ratatui::text::Span;

        let done = buf.done();
        let left = theme.keymap_shortcuts(&self.keymap.items, move |action| match action {
            Action::Back => true,
            Action::BackIfDone => done,
            Action::Copy => done,
            Action::Paste => !done,
        });
        let right = if buf.running() {
            let ch = self.spinner.render();
            Line::from(vec![
                Span::styled(format!(" {ch}"), theme.active_fg_style()),
                Span::styled(" Running", theme.muted_style()),
            ])
        } else if let Some(code) = buf.exit_code {
            if code == 0 {
                Line::from(format!(" {}", crate::apps::config::SUCCESS_SYMBOL))
                    .style(theme.success_style())
            } else {
                let mut spans = vec![Span::styled(
                    format!(" {}", crate::apps::config::ERROR_SYMBOL),
                    theme.error_style(),
                )];
                spans.push(Span::styled(format!(" exit {code}"), theme.muted_style()));
                Line::from(spans)
            }
        } else {
            Line::from("")
        };

        let badge = theme.mode_badge("OUTPUT", theme.info_background);

        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(badge.width() as u16),
                Constraint::Min(1),
                Constraint::Length(right.width() as u16 + 2),
            ])
            .split(area);

        frame.render_widget(
            Paragraph::new(badge)
                .block(Block::default().style(Style::default().bg(theme.background))),
            chunks[0],
        );
        frame.render_widget(theme.footer(left), chunks[1]);
        frame.render_widget(theme.footer(right).alignment(Alignment::Right), chunks[2]);
    }

    /// Returns the inner content area of the output pane (excluding borders).
    pub fn inner_area(&self) -> Rect {
        let area = self.last_area.get();
        Block::default().inner(area)
    }

    /// Maps output pane mouse coordinates to `(global_line, char_offset)`.
    fn position_at(&self, row: u16, col: u16, buf: &Task) -> Option<(usize, usize)> {
        let inner = self.inner_area();
        let rel_row = row.saturating_sub(inner.y) as usize;
        let target_col = col.saturating_sub(inner.x) as usize;
        let global_line = rel_row + buf.scroll as usize;
        let text = buf.parser.contents();
        let char_offset = text
            .lines
            .get(rel_row)
            .map(|line| {
                crate::apps::tui::selection::SelectionState::char_offset_from_col(line, target_col)
            })
            .unwrap_or(0);
        Some((global_line, char_offset))
    }

    /// Maps output-pane mouse coordinates to PTY-relative SGR coordinates.
    pub fn mouse_to_pty_coords(
        &self,
        mouse: &crossterm::event::MouseEvent,
        pty_cols: u16,
        pty_rows: u16,
    ) -> (u16, u16) {
        let inner = self.inner_area();
        let col = mouse
            .column
            .saturating_sub(inner.x)
            .saturating_add(1)
            .min(pty_cols);
        let row = mouse
            .row
            .saturating_sub(inner.y)
            .saturating_add(1)
            .min(pty_rows);
        (col, row)
    }
}

impl Component for Output {
    type Msg = Action;

    fn update(&mut self, msg: Self::Msg) -> Option<Cmd<Self::Msg>> {
        Some(Cmd::msg(msg))
    }
}

impl Input for Output {
    fn action(&self, event: crossterm::event::Event) -> Option<Self::Msg> {
        if let crossterm::event::Event::Key(key) = event {
            if let Some(action) = self.keymap.get(&key) {
                match action {
                    Action::BackIfDone | Action::Copy if !self.done => return None,
                    _ => return Some(*action),
                }
            }
        }
        None
    }
}
