use std::collections::HashMap;

use crate::apps::config::{ERROR_SYMBOL, SUCCESS_SYMBOL};
use crate::apps::theme::Theme;
use crate::apps::tui::layout::centered_rect;
use crate::apps::tui::widgets::spinner::Spinner;
use crate::apps::tui::Shortcut;
use keymap::{DerivedConfig, KeyMap};
use ratatui::style::Style;
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::Paragraph;

use upmd_runtime::{
    runtimes::tui::{Input, Output},
    Cmd, Component,
};

const MIN_PREVIEW_LAYOUT_WIDTH: u16 = 72;

/// Execution status kind for a code block in the goto list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StatusKind {
    None,
    Running,
    Success,
    Error,
}

/// Goto dialog with query filtering and code-block name search.
///
/// Opens with all code blocks listed. Type a query (ID or name substring)
/// to filter the list, navigate with up/down, and press Enter to jump.
/// Running blocks display an animated spinner; completed/failed blocks
/// show a success or error symbol in the theme's semantic color.
/// The right panel shows a syntax-highlighted preview of the selected block.
pub struct Goto {
    query: String,
    selected: usize,
    matches: Vec<(u32, String, StatusKind)>,
    all_blocks: Vec<(u32, String, StatusKind)>,
    /// Raw code content and language keyed by code ID, shown as a mini-preview.
    previews: HashMap<u32, (String, String)>,
    spinner: Spinner,
    theme: Theme,
    keymap: DerivedConfig<Action>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, KeyMap)]
pub enum Action {
    #[key("backspace", help = "delete")]
    Delete,
    #[key("enter", help = "go")]
    Select,
    #[key("down", "ctrl-n", help = "next")]
    Next,
    #[key("up", "ctrl-p", help = "prev")]
    Prev,
    #[key("esc", help = "cancel")]
    Quit,
    #[key("@any")]
    Input(char),
}

impl Goto {
    pub fn new(
        theme: Theme,
        keymap: DerivedConfig<Action>,
        all_blocks: Vec<(u32, String, StatusKind)>,
        previews: HashMap<u32, (String, String)>,
    ) -> Self {
        let mut s = Self {
            query: String::new(),
            selected: 0,
            matches: Vec::new(),
            all_blocks,
            previews,
            spinner: Spinner::braille(),
            theme,
            keymap,
        };
        s.rebuild();
        s
    }

    pub fn selected_code_id(&self) -> Option<u32> {
        self.matches.get(self.selected).map(|(id, _, _)| *id)
    }

    pub fn tick(&mut self) {
        self.spinner.tick();
    }

    fn rebuild(&mut self) {
        self.matches = self.build_matches();
        self.selected = self.selected.min(self.matches.len().saturating_sub(1));
    }

    fn build_matches(&self) -> Vec<(u32, String, StatusKind)> {
        let query_lower = self.query.to_lowercase();

        self.all_blocks
            .iter()
            .filter(|(id, label, _)| {
                if self.query.is_empty() {
                    return true;
                }
                id.to_string().contains(&self.query) || label.to_lowercase().contains(&query_lower)
            })
            .cloned()
            .collect()
    }
}

impl Shortcut for Goto {
    fn footer_shortcuts(&self) -> ratatui::text::Line<'static> {
        self.theme.shortcuts(&[
            ("↑↓".to_string(), "move".to_string()),
            ("↵".to_string(), "go".to_string()),
            ("esc".to_string(), "cancel".to_string()),
        ])
    }
}

impl Component for Goto {
    type Msg = Action;

    fn update(&mut self, msg: Self::Msg) -> Option<Cmd<Self::Msg>> {
        self.spinner.tick();
        match msg {
            Action::Input(ch) => {
                self.query.push(ch);
                self.rebuild();
                None
            }
            Action::Delete => {
                self.query.pop();
                self.rebuild();
                None
            }
            Action::Next => {
                if !self.matches.is_empty() {
                    self.selected = (self.selected + 1).min(self.matches.len().saturating_sub(1));
                    self.rebuild();
                }
                None
            }
            Action::Prev => {
                if !self.matches.is_empty() {
                    self.selected = self.selected.saturating_sub(1);
                    self.rebuild();
                }
                None
            }
            Action::Select => Some(Cmd::msg(Action::Select)),
            Action::Quit => Some(Cmd::msg(Action::Quit)),
        }
    }
}

impl Input for Goto {
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

impl Output for Goto {
    fn render(&self, frame: &mut ratatui::Frame, area: ratatui::layout::Rect) {
        use ratatui::{
            layout::{Constraint, Direction, Layout},
            widgets::{Clear, List, ListItem},
        };

        let block = self.theme.popup_block("Go to block");
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

        // Query input line.
        let has_query = !self.query.is_empty();
        let placeholder = if has_query { "" } else { "type to filter..." };
        let input_text = Text::from(Line::from(vec![
            Span::raw("Block: "),
            Span::styled(
                if has_query { &self.query } else { placeholder },
                Style::default().fg(if has_query {
                    self.theme.active
                } else {
                    self.theme.muted
                }),
            ),
        ]));
        frame.render_widget(Paragraph::new(input_text), vert[0]);

        frame.render_widget(self.theme.footer(self.footer_shortcuts()), vert[2]);

        // Use the full body for the list on narrow terminals. The preview is
        // useful only when both columns have enough room to remain readable.
        let body_area = vert[1];
        let show_preview = body_area.width >= MIN_PREVIEW_LAYOUT_WIDTH;
        let (list_area, preview_area) = if show_preview {
            let horiz = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
                .split(body_area);
            (horiz[0], horiz[1])
        } else {
            (body_area, ratatui::layout::Rect::default())
        };

        // ---- Left: filterable list ----
        let visible_rows = list_area.height as usize;
        let total = self.matches.len();

        let cap = visible_rows.saturating_sub(1);
        let scroll_offset = if total > cap && self.selected >= cap {
            self.selected - cap + 1
        } else {
            0
        };
        let visible_end = (scroll_offset + visible_rows).min(total);

        if scroll_offset < visible_end {
            let items: Vec<ListItem> = self.matches[scroll_offset..visible_end]
                .iter()
                .enumerate()
                .map(|(i, (_, label, kind))| {
                    let abs = scroll_offset + i;
                    let is_sel = abs == self.selected;
                    let prefix = if is_sel { "▸ " } else { "  " };

                    let base_fg = if is_sel {
                        self.theme.active
                    } else {
                        self.theme.foreground
                    };

                    if *kind == StatusKind::None {
                        return ListItem::new(Line::from(Span::styled(
                            format!("{prefix}{label}"),
                            Style::default().fg(base_fg),
                        )));
                    }

                    let (status, status_fg) = match kind {
                        StatusKind::Success => (SUCCESS_SYMBOL.to_string(), self.theme.success),
                        StatusKind::Error => (ERROR_SYMBOL.to_string(), self.theme.error),
                        StatusKind::Running => {
                            (self.spinner.render().to_string(), self.theme.active)
                        }
                        StatusKind::None => unreachable!(),
                    };

                    ListItem::new(Line::from(vec![
                        Span::styled(format!("{prefix}{label} "), Style::default().fg(base_fg)),
                        Span::styled(status, Style::default().fg(status_fg)),
                    ]))
                })
                .collect();

            frame.render_widget(List::new(items), list_area);
        }

        // ---- Right: syntax-highlighted mini-preview ----
        if show_preview {
            if let Some((lang, content)) = self
                .selected_code_id()
                .and_then(|id| self.previews.get(&id))
            {
                let ph = preview_area.height as usize;

                let highlighted = self.theme.highlight(content, lang);
                let lines: Vec<Line<'static>> = highlighted.lines.into_iter().take(ph).collect();

                frame.render_widget(Paragraph::new(Text::from(lines)), preview_area);
            }
        }
    }
}
