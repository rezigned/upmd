use std::collections::HashMap;

use keymap::{DerivedConfig, KeyMap};
use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Clear, Paragraph},
    Frame,
};
use unicode_width::UnicodeWidthStr;
use upmd_parser::{nodes::Code, CodeId};
use upmd_runtime::{
    runtimes::tui::{Input, Output},
    Cmd, Component,
};

use crate::apps::{
    config::{ERROR_SYMBOL, SUCCESS_SYMBOL},
    task::TaskStatus,
    theme::Theme,
    tui::{layout::centered_rect, widgets::Spinner, Shortcut},
    workflow::DependencyGraph,
};

/// Scrollable dependency graph popup.
pub struct Dependencies {
    title: &'static str,
    graph: Option<DependencyGraph>,
    message: Option<String>,
    names: HashMap<CodeId, String>,
    statuses: HashMap<CodeId, TaskStatus>,
    spinner: Spinner,
    scroll_x: u16,
    scroll_y: u16,
    theme: Theme,
    keymap: DerivedConfig<Action>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, KeyMap)]
pub enum Action {
    #[key("up", "k", help = "up")]
    Up,
    #[key("down", "j", help = "down")]
    Down,
    #[key("left", "h", help = "left")]
    Left,
    #[key("right", "l", help = "right")]
    Right,
    #[key("pageup", help = "page up")]
    PageUp,
    #[key("pagedown", help = "page down")]
    PageDown,
    #[key("home", help = "reset")]
    Reset,
    #[key("esc", "q", help = "close")]
    Quit,
    #[key("ctrl-c")]
    Exit,
}

impl Dependencies {
    pub fn for_target(
        codes: &[Code],
        target: Option<CodeId>,
        statuses: HashMap<CodeId, TaskStatus>,
        theme: Theme,
        keymap: DerivedConfig<Action>,
    ) -> Self {
        let graph = target.map(|id| DependencyGraph::for_target(codes, id));
        Self::new(
            "Dependencies",
            codes,
            graph,
            "No block selected",
            statuses,
            theme,
            keymap,
        )
    }

    pub fn for_workflow(
        codes: &[Code],
        graph: Option<DependencyGraph>,
        statuses: HashMap<CodeId, TaskStatus>,
        theme: Theme,
        keymap: DerivedConfig<Action>,
    ) -> Self {
        Self::new(
            "Workflow Dependencies",
            codes,
            graph.map(Ok),
            "No active workflow",
            statuses,
            theme,
            keymap,
        )
    }

    fn new(
        title: &'static str,
        codes: &[Code],
        graph: Option<Result<DependencyGraph, String>>,
        empty_message: &'static str,
        statuses: HashMap<CodeId, TaskStatus>,
        theme: Theme,
        keymap: DerivedConfig<Action>,
    ) -> Self {
        let (graph, message) = match graph {
            Some(Ok(graph)) => (Some(graph), None),
            Some(Err(error)) => (None, Some(error)),
            None => (None, Some(empty_message.to_string())),
        };
        let names = codes
            .iter()
            .map(|code| {
                let name = if code.name.is_empty() {
                    code.id.to_string()
                } else {
                    code.name.clone()
                };
                (code.id, name)
            })
            .collect();

        Self {
            title,
            graph,
            message,
            names,
            statuses,
            spinner: Spinner::default(),
            scroll_x: 0,
            scroll_y: 0,
            theme,
            keymap,
        }
    }

    pub fn tick(&mut self, statuses: HashMap<CodeId, TaskStatus>) {
        self.statuses = statuses;
        self.spinner.tick();
    }

    fn name_of(&self, id: CodeId) -> String {
        self.names
            .get(&id)
            .cloned()
            .unwrap_or_else(|| id.to_string())
    }

    #[cfg(test)]
    pub(crate) fn graph(&self) -> Option<&DependencyGraph> {
        self.graph.as_ref()
    }

    #[cfg(test)]
    pub(crate) fn message(&self) -> Option<&str> {
        self.message.as_deref()
    }

    #[cfg(test)]
    pub(crate) fn scroll_offset(&self) -> (u16, u16) {
        (self.scroll_x, self.scroll_y)
    }

    fn render_graph(&self, frame: &mut Frame, area: Rect, graph: &DependencyGraph) {
        let layers = graph.layers();
        if graph.target().is_some() && layers.len() <= 1 {
            frame.render_widget(Paragraph::new("No dependencies"), area);
            return;
        }

        let spinner_char = self.spinner.render();
        let global = self.theme.global_style();
        let conn_style = global.fg(self.theme.muted);
        let default_style = self.theme.style();
        let running_style = self.theme.warning_fg_style();
        let success_style = self.theme.success_style();
        let error_style = self.theme.error_style();
        let selected_style = default_style.add_modifier(Modifier::BOLD);

        let max_name_len = |ids: &[CodeId]| -> usize {
            ids.iter()
                .map(|id| UnicodeWidthStr::width(self.name_of(*id).as_str()))
                .max()
                .unwrap_or(0)
        };
        let layer_widths = layers
            .iter()
            .map(|layer| max_name_len(layer))
            .collect::<Vec<_>>();
        let canvas_rows = layers
            .iter()
            .map(Vec::len)
            .max()
            .unwrap_or(1)
            .saturating_mul(2)
            .saturating_sub(1);
        let conn_w = 6usize;
        let mut rows: Vec<Vec<(String, Style)>> =
            (0..canvas_rows.max(1)).map(|_| Vec::new()).collect();

        for (column, layer) in layers.iter().enumerate() {
            let col_w = layer_widths[column];
            for (row_index, row) in rows.iter_mut().enumerate() {
                let item_row = row_index / 2;
                if row_index % 2 == 0 && item_row < layer.len() {
                    let id = layer[item_row];
                    let name = self.name_of(id);
                    let name_style = if graph.target() == Some(id) {
                        selected_style
                    } else {
                        default_style
                    };

                    match self.statuses.get(&id) {
                        Some(TaskStatus::Running) => {
                            row.push((format!(" {spinner_char} "), running_style));
                        }
                        Some(TaskStatus::Success) => {
                            row.push((format!(" {SUCCESS_SYMBOL} "), success_style));
                        }
                        Some(TaskStatus::Error) => {
                            row.push((format!(" {ERROR_SYMBOL} "), error_style));
                        }
                        _ => row.push((" · ".to_string(), conn_style)),
                    }
                    let padding = col_w.saturating_sub(UnicodeWidthStr::width(name.as_str()));
                    row.push((format!("{name}{} ", " ".repeat(padding)), name_style));
                } else {
                    row.push(("   ".to_string(), conn_style));
                    row.push((format!("{:width$} ", "", width = col_w), conn_style));
                }
            }

            if column + 1 >= layers.len() {
                break;
            }

            let next_layer = &layers[column + 1];
            let merge_col = 2usize;
            for (row_index, row) in rows.iter_mut().enumerate() {
                let item_row = row_index / 2;
                let connector = if layer.len() <= 1 && next_layer.len() <= 1 {
                    if row_index == 0 {
                        "─────→".to_string()
                    } else {
                        "      ".to_string()
                    }
                } else if row_index % 2 == 0 && item_row < layer.len() {
                    if item_row == 0 {
                        format!(
                            "{:─<merge_col$}┬{:─<width$}→",
                            "",
                            "",
                            merge_col = merge_col,
                            width = conn_w - merge_col - 2
                        )
                    } else {
                        let junction = if item_row == layer.len() - 1 {
                            '┘'
                        } else {
                            '┤'
                        };
                        let mut connector = String::new();
                        for position in 0..conn_w {
                            if position < merge_col {
                                connector.push('─');
                            } else if position == merge_col {
                                connector.push(junction);
                            } else {
                                connector.push(' ');
                            }
                        }
                        connector
                    }
                } else if row_index % 2 == 0 && layer.len() == 1 && item_row < next_layer.len() {
                    let junction = if item_row == next_layer.len() - 1 {
                        '└'
                    } else {
                        '├'
                    };
                    let mut connector = String::new();
                    for position in 0..conn_w {
                        if position < merge_col {
                            connector.push(' ');
                        } else if position == merge_col {
                            connector.push(junction);
                        } else if position == conn_w - 1 {
                            connector.push('→');
                        } else {
                            connector.push('─');
                        }
                    }
                    connector
                } else {
                    let above_has_item = row_index > 0 && ((row_index - 1) / 2) < layer.len();
                    let below_has_item =
                        row_index + 1 < canvas_rows && (row_index / 2 + 1) < layer.len();
                    if above_has_item || below_has_item {
                        let mut connector = String::new();
                        for position in 0..conn_w {
                            connector.push(if position == merge_col { '│' } else { ' ' });
                        }
                        connector
                    } else {
                        "      ".to_string()
                    }
                };
                row.push((connector, conn_style));
            }
        }

        let content_width = rows
            .iter()
            .map(|row| {
                row.iter()
                    .map(|(text, _)| UnicodeWidthStr::width(text.as_str()))
                    .sum::<usize>()
            })
            .max()
            .unwrap_or(0);
        let max_x = content_width.saturating_sub(area.width as usize);
        let max_y = rows.len().saturating_sub(area.height as usize);
        let scroll_x = self.scroll_x.min(u16::try_from(max_x).unwrap_or(u16::MAX));
        let scroll_y = self.scroll_y.min(u16::try_from(max_y).unwrap_or(u16::MAX));
        let text_lines = rows
            .into_iter()
            .map(|row| {
                Line::from(
                    row.into_iter()
                        .map(|(text, style)| Span::styled(text, style))
                        .collect::<Vec<_>>(),
                )
            })
            .collect::<Vec<_>>();
        let paragraph = Paragraph::new(Text::from(text_lines))
            .style(global)
            .scroll((scroll_y, scroll_x));
        frame.render_widget(paragraph, area);
    }
}

impl Shortcut for Dependencies {
    fn footer_shortcuts(&self) -> Line<'static> {
        self.theme.shortcuts(&[
            ("↑↓←→".to_string(), "scroll".to_string()),
            ("pgup/pgdn".to_string(), "page".to_string()),
            ("home".to_string(), "reset".to_string()),
            ("esc/q".to_string(), "close".to_string()),
        ])
    }
}

impl Component for Dependencies {
    type Msg = Action;

    fn update(&mut self, msg: Self::Msg) -> Option<Cmd<Self::Msg>> {
        match msg {
            Action::Up => self.scroll_y = self.scroll_y.saturating_sub(1),
            Action::Down => self.scroll_y = self.scroll_y.saturating_add(1),
            Action::Left => self.scroll_x = self.scroll_x.saturating_sub(2),
            Action::Right => self.scroll_x = self.scroll_x.saturating_add(2),
            Action::PageUp => self.scroll_y = self.scroll_y.saturating_sub(10),
            Action::PageDown => self.scroll_y = self.scroll_y.saturating_add(10),
            Action::Reset => {
                self.scroll_x = 0;
                self.scroll_y = 0;
            }
            Action::Quit => return Some(Cmd::msg(Action::Quit)),
            Action::Exit => return Some(Cmd::quit()),
        }
        None
    }
}

impl Input for Dependencies {
    fn action(&self, event: crossterm::event::Event) -> Option<Self::Msg> {
        let crossterm::event::Event::Key(key) = event else {
            return None;
        };
        self.keymap.get_bound(&key)
    }
}

impl Output for Dependencies {
    fn render(&self, frame: &mut Frame, area: Rect) {
        let popup_height = if self.graph.is_some() { 40 } else { 20 };
        let popup_area = centered_rect(60, popup_height, area);
        frame.render_widget(Clear, popup_area);

        let block = self.theme.popup_block(self.title);
        let inner = block.inner(popup_area);
        frame.render_widget(block, popup_area);
        let [content_area, footer_area] =
            Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(inner);
        frame.render_widget(self.theme.footer(self.footer_shortcuts()), footer_area);

        if let Some(graph) = &self.graph {
            self.render_graph(frame, content_area, graph);
        } else if let Some(message) = &self.message {
            frame.render_widget(Paragraph::new(message.clone()), content_area);
        }
    }
}
