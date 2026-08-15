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
use upmd_parser::{CodeId, Codes};
use upmd_runtime::{
    runtimes::tui::{Input, Output},
    Cmd, Component, Effect,
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

pub enum Outcome {
    Closed,
}

impl Dependencies {
    pub fn for_target(
        codes: &Codes,
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

    pub(crate) fn new(
        title: &'static str,
        codes: &Codes,
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

    pub fn set_theme(&mut self, theme: &Theme) {
        self.theme.clone_from(theme);
    }

    pub fn has_deps(&self) -> bool {
        self.graph.as_ref().is_some_and(DependencyGraph::has_deps)
    }

    fn name_of(&self, id: CodeId) -> String {
        self.names
            .get(&id)
            .cloned()
            .unwrap_or_else(|| id.to_string())
    }

    fn graph_lines(&self, graph: &DependencyGraph) -> Vec<Vec<(String, Style)>> {
        let layers = graph.layers();
        let spinner_char = self.spinner.render();
        let default_style = self.theme.style();
        let conn_style = self.theme.global_style().fg(self.theme.muted);
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
        let conn_w = 5usize;
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
            let empty_conn = " ".repeat(conn_w);
            for (row_index, row) in rows.iter_mut().enumerate() {
                let item_row = row_index / 2;
                let connector = if layer.len() <= 1 && next_layer.len() <= 1 {
                    if row_index == 0 {
                        "────→".to_string()
                    } else {
                        empty_conn.clone()
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
                    let between_next_items =
                        row_index % 2 == 1 && (row_index / 2 + 1) < next_layer.len();
                    if above_has_item || below_has_item || between_next_items {
                        let mut connector = String::new();
                        for position in 0..conn_w {
                            connector.push(if position == merge_col { '│' } else { ' ' });
                        }
                        connector
                    } else {
                        empty_conn.clone()
                    }
                };
                row.push((connector, conn_style));
            }
        }

        rows
    }

    fn render_graph(&self, frame: &mut Frame, area: Rect, graph: &DependencyGraph) {
        let layers = graph.layers();
        if graph.target().is_some() && layers.len() <= 1 {
            frame.render_widget(Paragraph::new("No dependencies"), area);
            return;
        }

        let global = self.theme.global_style();
        let rows = self.graph_lines(graph);
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

    /// Renders the dependency graph inline (no popup frame) at the given area.
    pub fn render_inline(&self, frame: &mut Frame, area: Rect) {
        frame.render_widget(Clear, area);
        if let Some(graph) = &self.graph {
            let lines = self.graph_lines(graph);
            let rows: Vec<Line> = lines
                .into_iter()
                .map(|row| {
                    Line::from(
                        row.into_iter()
                            .map(|(text, style)| Span::styled(text, style))
                            .collect::<Vec<_>>(),
                    )
                })
                .collect();
            let paragraph = Paragraph::new(Text::from(rows)).style(self.theme.global_style());
            frame.render_widget(paragraph, area);
        } else if let Some(message) = &self.message {
            frame.render_widget(Paragraph::new(message.clone()), area);
        }
    }

    /// Number of text rows the graph renders to.
    pub fn graph_rows(&self) -> u16 {
        self.graph.as_ref().map_or(1, |g| {
            let layers = g.layers();
            let h = layers.iter().map(Vec::len).max().unwrap_or(1);
            (h * 2 - 1).max(1) as u16
        })
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
    type Action = Action;
    type Outcome = Outcome;

    fn update(&mut self, action: Action) -> Option<Effect<Action, Outcome>> {
        match action {
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
            Action::Quit => return upmd_runtime::effect!(outcome: Outcome::Closed),
            Action::Exit => return upmd_runtime::effect!(Cmd::quit()),
        }
        None
    }
}

impl Input for Dependencies {
    fn action(&self, event: crossterm::event::Event) -> Option<Self::Action> {
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

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use insta::assert_snapshot;
    use keymap::DerivedConfig;
    use upmd_runtime::Component;

    use super::*;
    use crate::apps::workflow::DependencyGraph;

    fn empty_keymap() -> DerivedConfig<Action> {
        toml::from_str("").unwrap()
    }

    fn row_text(rows: &[Vec<(String, Style)>]) -> Vec<String> {
        rows.iter()
            .map(|row| row.iter().map(|(text, _)| text.clone()).collect::<String>())
            .collect()
    }

    fn deps_from(markdown: &str, target: Option<CodeId>) -> Dependencies {
        let codes = upmd_parser::new().parse(markdown).codes;
        let graph = target
            .map(|id| DependencyGraph::for_target(&codes, id))
            .unwrap_or_else(|| DependencyGraph::for_all(&codes))
            .unwrap();
        Dependencies::new(
            "Dependencies",
            &codes,
            Some(Ok(graph)),
            "No block selected",
            HashMap::new(),
            Theme::default(),
            empty_keymap(),
        )
    }

    #[test]
    fn snapshot_single_block() {
        let deps = deps_from("```sh [name:a]\n:\n```\n", None);
        let lines = row_text(&deps.graph_lines(deps.graph.as_ref().unwrap()));
        assert_snapshot!("single_block", lines.join("\n"));
    }

    #[test]
    fn snapshot_linear_chain() {
        let deps = deps_from(
            "\
```sh [name:a]\n:\n```\n\
```sh [name:b, deps:a]\n:\n```\n",
            None,
        );
        let lines = row_text(&deps.graph_lines(deps.graph.as_ref().unwrap()));
        assert_snapshot!("linear_chain", lines.join("\n"));
    }

    #[test]
    fn snapshot_linear_chain_3() {
        let deps = deps_from(
            "\
```sh [name:a]\n:\n```\n\
```sh [name:b, deps:a]\n:\n```\n\
```sh [name:c, deps:b]\n:\n```\n",
            None,
        );
        let lines = row_text(&deps.graph_lines(deps.graph.as_ref().unwrap()));
        assert_snapshot!("linear_chain_3", lines.join("\n"));
    }

    #[test]
    fn snapshot_fan_in() {
        let deps = deps_from(
            "\
```sh [name:a]\n:\n```\n\
```sh [name:b]\n:\n```\n\
```sh [name:c, deps:\"a|b\"]\n:\n```\n",
            None,
        );
        let lines = row_text(&deps.graph_lines(deps.graph.as_ref().unwrap()));
        assert_snapshot!("fan_in", lines.join("\n"));
    }

    #[test]
    fn snapshot_fan_out() {
        let deps = deps_from(
            "\
```sh [name:a]\n:\n```\n\
```sh [name:b, deps:a]\n:\n```\n\
```sh [name:c, deps:a]\n:\n```\n",
            None,
        );
        let lines = row_text(&deps.graph_lines(deps.graph.as_ref().unwrap()));
        assert_snapshot!("fan_out", lines.join("\n"));
    }

    #[test]
    fn snapshot_diamond() {
        let deps = deps_from(
            "\
```sh [name:a]\n:\n```\n\
```sh [name:b, deps:a]\n:\n```\n\
```sh [name:c, deps:a]\n:\n```\n\
```sh [name:d, deps:\"b|c\"]\n:\n```\n",
            None,
        );
        let lines = row_text(&deps.graph_lines(deps.graph.as_ref().unwrap()));
        assert_snapshot!("diamond", lines.join("\n"));
    }

    #[test]
    fn snapshot_3_fan_out() {
        let deps = deps_from(
            "\
```sh [name:a]\n:\n```\n\
```sh [name:b, deps:a]\n:\n```\n\
```sh [name:c, deps:a]\n:\n```\n\
```sh [name:d, deps:a]\n:\n```\n",
            None,
        );
        let lines = row_text(&deps.graph_lines(deps.graph.as_ref().unwrap()));
        assert_snapshot!("3_fan_out", lines.join("\n"));
    }

    #[test]
    fn snapshot_3_fan_in() {
        let deps = deps_from(
            "\
```sh [name:a]\n:\n```\n\
```sh [name:b]\n:\n```\n\
```sh [name:c]\n:\n```\n\
```sh [name:d, deps:\"a|b|c\"]\n:\n```\n",
            None,
        );
        let lines = row_text(&deps.graph_lines(deps.graph.as_ref().unwrap()));
        assert_snapshot!("3_fan_in", lines.join("\n"));
    }

    #[test]
    fn snapshot_mixed_top_target() {
        let codes = upmd_parser::new()
            .parse(
                "\
# Deps Graph Test

## Simple chain

```sh [name:leaf]
sleep 0.5 && echo leaf
```

```sh [name:mid, deps:leaf]
sleep 0.5 && echo mid
```

```sh [name:chain-top, deps:mid]
sleep 0.5 && echo top
```

## Fan-in from chain + parallel

```sh [name:x-dep]
sleep 0.5 && echo x
```

```sh [name:y-dep, deps:x-dep]
sleep 0.5 && echo y
```

```sh [name:a-dep]
sleep 0.5 && echo a
```

```sh [name:b-dep]
sleep 0.5 && echo b
```

```sh [name:z-dep, deps:\"y-dep, a-dep, b-dep\"]
sleep 0.5 && echo z
```

## Multi-level parallel groups

```sh [name:p-one]
sleep 0.5 && echo 1
```

```sh [name:p-two]
sleep 0.5 && echo 2
```

```sh [name:p-three]
sleep 0.5 && echo 3
```

```sh [name:p-four]
sleep 0.5 && echo 4
```

```sh [name:p-five]
sleep 0.5 && echo 5
```

```sh [name:p-six, deps:\"p-one | p-two, p-three | p-four, p-five\"]
sleep 0.5 && echo six
```

## Deep transitive chain

```sh [name:d-base]
sleep 0.5 && echo base
```

```sh [name:d-step1, deps:d-base]
sleep 0.5 && echo step1
```

```sh [name:d-step2, deps:d-step1]
sleep 0.5 && echo step2
```

```sh [name:d-step3, deps:d-step2]
sleep 0.5 && echo step3
```

```sh [name:d-step4, deps:d-step3]
sleep 0.5 && echo step4
```

## Mixed: chain + parallel

```sh [name:m-dep-a]
sleep 0.5 && echo a
```

```sh [name:m-dep-b]
sleep 0.5 && echo b
```

```sh [name:m-stage1, deps:\"m-dep-a, m-dep-b\"]
sleep 0.5 && echo stage1
```

```sh [name:m-stage2, deps:m-stage1]
sleep 0.5 && echo stage2
```

```sh [name:m-side-x]
sleep 0.5 && echo x
```

```sh [name:m-side-y]
sleep 0.5 && echo y
```

```sh [name:mixed-top, deps:\"m-stage2, m-side-x | m-side-y\"]
sleep 0.5 && echo top
```

## Diamond

```sh [name:seed]
sleep 0.5 && echo seed
```

```sh [name:fork-l, deps:seed]
sleep 0.5 && echo left
```

```sh [name:fork-r, deps:seed]
sleep 0.5 && echo right
```

```sh [name:diamond-join, deps:\"fork-l | fork-r\"]
sleep 0.5 && echo join
```",
            )
            .codes;
        let deps = Dependencies::for_target(
            &codes,
            Some(26),
            HashMap::new(),
            Theme::default(),
            empty_keymap(),
        );
        let lines = row_text(&deps.graph_lines(deps.graph.as_ref().unwrap()));
        insta::assert_snapshot!("mixed_top_target", lines.join("\n"));
    }

    #[test]
    fn uses_workflow_validation() {
        let markdown = "\
```sh [name:dup]\n:\n```\n\
```sh [name:dup]\n:\n```\n\
```sh [name:target, deps:dup]\n:\n```\n";
        let codes = upmd_parser::new().parse(markdown).codes;
        let deps = Dependencies::for_target(
            &codes,
            Some(3),
            HashMap::new(),
            Theme::default(),
            empty_keymap(),
        );

        let error = deps.message.as_ref().unwrap();
        assert!(error.contains("ambiguous"));
    }

    #[test]
    fn keeps_all_mode_graph() {
        let markdown = "\
```sh [name:a]\n:\n```\n\
```sh [name:b, deps:a]\n:\n```\n\
```sh [name:independent]\n:\n```\n";
        let codes = upmd_parser::new().parse(markdown).codes;
        let graph = DependencyGraph::for_all(&codes).unwrap();
        let deps = Dependencies::new(
            "Dependencies",
            &codes,
            Some(Ok(graph)),
            "No block selected",
            HashMap::new(),
            Theme::default(),
            empty_keymap(),
        );

        let graph = deps.graph.as_ref().unwrap();
        assert_eq!(graph.layers(), &[vec![1, 3], vec![2]]);
    }

    #[test]
    fn handles_both_scroll_axes() {
        let markdown = "```sh [name:a]\n:\n```\n";
        let codes = upmd_parser::new().parse(markdown).codes;
        let graph = DependencyGraph::for_all(&codes).unwrap();
        let mut deps = Dependencies::new(
            "Dependencies",
            &codes,
            Some(Ok(graph)),
            "No block selected",
            HashMap::new(),
            Theme::default(),
            empty_keymap(),
        );

        assert_eq!(
            deps.footer_shortcuts().to_string(),
            "↑↓←→ scroll  pgup/pgdn page  home reset  esc/q close"
        );

        let _ = Component::update(&mut deps, Action::Right);
        let _ = Component::update(&mut deps, Action::Down);
        assert_eq!(deps.scroll_x, 2);
        assert_eq!(deps.scroll_y, 1);

        let _ = Component::update(&mut deps, Action::Reset);
        assert_eq!(deps.scroll_x, 0);
        assert_eq!(deps.scroll_y, 0);
    }
}
