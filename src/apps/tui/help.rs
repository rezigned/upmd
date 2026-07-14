use crate::apps::{config::LOGO, tui::layout::centered_rect};
use keymap::{DerivedConfig, KeyMap};
use ratatui::{
    layout::{Alignment, Constraint, Layout, Rect},
    style::Style,
    text::{Line, Span, Text},
    widgets::{Cell, Clear, Paragraph, Row, Table},
    Frame,
};

use crate::apps::theme::Theme;

use upmd_runtime::{
    runtimes::tui::{Input, Output},
    Cmd, Component,
};

pub struct KeymapEntry {
    section: &'static str,
    symbol: String,
    description: String,
    search_text: String,
}

/// Displays all keybindings organised by section, with a live filter.
pub struct Help {
    theme: Theme,
    keymap: DerivedConfig<Action>,
    all_items: Vec<KeymapEntry>,
    query: String,
    matches: Vec<usize>,
    scroll: u16,
}

#[derive(KeyMap, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Action {
    /// Closes the help overlay.
    #[key("q", "esc", "?", help = "quit")]
    Quit,
    #[key("backspace", help = "delete")]
    Delete,
    #[key("down", "ctrl-n", help = "next")]
    Next,
    #[key("up", "ctrl-p", help = "prev")]
    Prev,
    /// Must be last because @any catches all unmatched keys.
    #[key("@any")]
    Input(char),
}

impl KeymapEntry {
    fn new(
        section: &'static str,
        symbol: String,
        description: String,
        search_text: String,
    ) -> Self {
        Self {
            section,
            symbol,
            description,
            search_text,
        }
    }

    fn matches(&self, query: &str) -> bool {
        self.search_text.contains(query)
    }
}

/// Extracts user-facing keymap entries from a parsed keymap config, skipping
/// implementation-only bindings (e.g. `@any`).
pub fn collect_keymap_entries<'a, T>(
    section: &'static str,
    config: &'a DerivedConfig<T>,
) -> impl Iterator<Item = KeymapEntry> + 'a {
    config.items.iter().filter_map(move |(_, item)| {
        let symbol = item
            .symbol
            .as_deref()
            .or_else(|| item.keys.first().map(|key| key.as_str()))
            .filter(|symbol| !symbol.is_empty() && *symbol != "@any")?;
        let description = if item.description.is_empty() {
            item.help.as_deref().unwrap_or_default()
        } else {
            &item.description
        };
        if description.is_empty() {
            return None;
        }
        let search_text = {
            let mut t = format!("{section} {symbol} {description}").to_lowercase();
            for key in &item.keys {
                if key.as_str() != symbol {
                    t.push(' ');
                    t.push_str(&key.to_lowercase());
                }
            }
            t
        };
        Some(KeymapEntry::new(
            section,
            symbol.to_string(),
            description.to_string(),
            search_text,
        ))
    })
}

impl Help {
    pub fn new(theme: Theme, keymap: DerivedConfig<Action>, all_items: Vec<KeymapEntry>) -> Self {
        let mut help = Self {
            theme,
            keymap,
            all_items,
            query: String::new(),
            matches: Vec::new(),
            scroll: 0,
        };
        help.rebuild();
        help
    }

    fn section_display(section: &str) -> &'static str {
        match section {
            "home" => "Home",
            "output" => "Output",
            "cli" => "CLI",
            "menu" => "Menu",
            "preview" => "Preview",
            "confirm" => "Confirm",
            "search" => "Search",
            "goto" => "Goto",
            "file_picker" => "File picker",
            "help" => "Help",
            "envs" => "Envs",
            "envs_edit" => "Envs (edit)",
            "themes" => "Themes",
            _ => "?",
        }
    }
    fn build_matches(&self) -> Vec<usize> {
        if self.query.is_empty() {
            return (0..self.all_items.len()).collect();
        }
        let query_lower = self.query.to_lowercase();
        self.all_items
            .iter()
            .enumerate()
            .filter(|(_, entry)| entry.matches(&query_lower))
            .map(|(i, _)| i)
            .collect()
    }

    fn rebuild(&mut self) {
        self.matches = self.build_matches();
        self.scroll = 0;
    }

    fn logo(&self, width: u16) -> Paragraph<'static> {
        let version = env!("CARGO_PKG_VERSION");
        let last = LOGO[LOGO.len() - 1];
        let pad = (width as usize).saturating_sub(last.chars().count()) / 2;
        let prefix = " ".repeat(pad);

        let up_style = self.theme.logo_style();
        let md_style = self.theme.style();

        let lines = LOGO[..LOGO.len() - 1]
            .iter()
            .map(|&s| {
                let padded = format!("{prefix}{s}");
                let split = prefix.chars().count() + 10;
                let up: String = padded.chars().take(split).collect();
                let md: String = padded.chars().skip(split).collect();
                Line::from(vec![Span::styled(up, up_style), Span::styled(md, md_style)])
            })
            .chain(std::iter::once({
                let padded = format!("{prefix}{last}");
                let split = prefix.chars().count() + 10;
                let up: String = padded.chars().take(split).collect();
                let md: String = padded.chars().skip(split).collect();
                Line::from(vec![
                    Span::styled(up, up_style),
                    Span::styled(md, md_style),
                    Span::raw(" "),
                    Span::raw(version),
                ])
            }))
            .chain(std::iter::once(
                Line::from(Span::styled(
                    env!("CARGO_PKG_DESCRIPTION"),
                    self.theme.muted_style(),
                ))
                .alignment(Alignment::Center),
            ));
        Paragraph::new(Text::from_iter(lines))
    }
}

impl Component for Help {
    type Msg = Action;

    fn update(&mut self, msg: Self::Msg) -> Option<Cmd<Self::Msg>> {
        match msg {
            Action::Quit => Some(Cmd::msg(Action::Quit)),
            Action::Input('\0') => None,
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
                    self.scroll = self.scroll.saturating_add(1);
                }
                None
            }
            Action::Prev => {
                self.scroll = self.scroll.saturating_sub(1);
                None
            }
        }
    }
}

impl Input for Help {
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

impl Help {
    fn render_filter(&self, frame: &mut Frame, area: Rect) {
        let filter_left = format!("Filter: {}", self.query);
        let filter_right = format!("({}/{})", self.matches.len(), self.all_items.len());
        let gap = (area.width as usize).saturating_sub(filter_left.len() + filter_right.len() + 2);

        frame.render_widget(
            Paragraph::new(Text::from(Line::from(vec![
                Span::raw(filter_left),
                Span::raw(" ".repeat(gap)),
                Span::styled(filter_right, Style::default().fg(self.theme.muted)),
            ]))),
            area,
        );
    }

    fn render_empty(&self, frame: &mut Frame, area: Rect) {
        let msg = if self.all_items.is_empty() {
            "No keybindings loaded"
        } else {
            "No matching keybindings"
        };

        frame.render_widget(
            Paragraph::new(Text::from(Line::from(Span::styled(
                msg,
                Style::default().fg(self.theme.muted),
            ))))
            .alignment(Alignment::Center),
            area,
        );
    }

    fn render_items(&self, frame: &mut Frame, area: Rect) {
        if self.matches.is_empty() {
            self.render_empty(frame, area);
            return;
        }

        let visible = visible_rows(self.build_rows(), self.scroll, area.height);
        frame.render_widget(
            Table::new(
                visible,
                [Constraint::Percentage(50), Constraint::Percentage(50)],
            )
            .column_spacing(1),
            area,
        );
    }

    fn render_footer(&self, frame: &mut Frame, area: Rect) {
        frame.render_widget(
            Paragraph::new(self.theme.keymap_shortcuts(&self.keymap.items, |action| {
                matches!(action, Action::Quit | Action::Delete)
            }))
            .alignment(Alignment::Center),
            area,
        );
    }

    fn build_rows(&self) -> Vec<Row<'_>> {
        let mut rows = Vec::new();
        let mut prev_section: Option<&'static str> = None;
        let mut pair_buf: Vec<&KeymapEntry> = Vec::new();

        for &idx in &self.matches {
            let entry = &self.all_items[idx];
            if prev_section != Some(entry.section) {
                if !pair_buf.is_empty() {
                    rows.push(make_pair_row(&pair_buf, &self.theme));
                    pair_buf.clear();
                }
                rows.push(
                    Row::new(vec![
                        Cell::from(Line::from(Span::styled(
                            format!("  {}", Self::section_display(entry.section)),
                            self.theme.code_info_style(),
                        ))),
                        Cell::from(""),
                    ])
                    .style(self.theme.code_info_style()),
                );
                prev_section = Some(entry.section);
            }

            pair_buf.push(entry);
            if pair_buf.len() == 2 {
                rows.push(make_pair_row(&pair_buf, &self.theme));
                pair_buf.clear();
            }
        }

        if !pair_buf.is_empty() {
            rows.push(make_pair_row(&pair_buf, &self.theme));
        }

        rows
    }
}

impl Output for Help {
    fn render(&self, frame: &mut Frame, area: Rect) {
        let popup_area = centered_rect(80, 50, area);
        frame.render_widget(Clear, popup_area);

        let block = self.theme.popup_block("Help");
        let inner = block.inner(popup_area);
        frame.render_widget(block, popup_area);

        let [logo_area, filter_area, items_area, footer_area] = Layout::vertical([
            Constraint::Length((LOGO.len() + 2) as u16),
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .areas(inner);

        frame.render_widget(self.logo(inner.width), logo_area);
        self.render_filter(frame, filter_area);
        self.render_items(frame, items_area);
        self.render_footer(frame, footer_area);
    }
}

/// Builds a 2-column table row from 1-2 entries, styled like the footer.
fn make_pair_row<'a>(entries: &[&'a KeymapEntry], theme: &Theme) -> Row<'a> {
    let mut cells: Vec<Cell<'a>> = entries
        .iter()
        .map(|e| {
            Cell::from(Line::from(vec![
                Span::styled(format!(" {}", e.symbol), theme.active_fg_style()),
                Span::styled(format!(" {}", e.description), Style::default()),
            ]))
        })
        .collect();
    while cells.len() < 2 {
        cells.push(Cell::from(""));
    }
    Row::new(cells)
}

fn visible_rows<'a>(mut rows: Vec<Row<'a>>, scroll: u16, height: u16) -> Vec<Row<'a>> {
    let visible = height as usize;
    let total = rows.len();
    if visible == 0 || total == 0 {
        return Vec::new();
    }

    let start = (scroll as usize).min(total.saturating_sub(visible));
    let end = (start + visible).min(total);
    rows.drain(start..end).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

    fn help() -> Help {
        Help::new(Theme::default(), toml::from_str("").unwrap(), Vec::new())
    }

    fn help_with_items() -> Help {
        Help::new(
            Theme::default(),
            toml::from_str("").unwrap(),
            vec![
                KeymapEntry::new(
                    "help",
                    "?".to_string(),
                    "Open help overlay".to_string(),
                    "help ? open help overlay".to_string(),
                ),
                KeymapEntry::new(
                    "search",
                    "/".to_string(),
                    "Find text".to_string(),
                    "search / find text".to_string(),
                ),
                KeymapEntry::new(
                    "goto",
                    "g".to_string(),
                    "Jump to code block".to_string(),
                    "goto g jump to code block".to_string(),
                ),
                KeymapEntry::new(
                    "output",
                    "x".to_string(),
                    "Clear completed output".to_string(),
                    "output x clear completed output".to_string(),
                ),
            ],
        )
    }

    fn key(code: KeyCode, modifiers: KeyModifiers) -> Event {
        Event::Key(KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            state: crossterm::event::KeyEventState::empty(),
        })
    }

    fn matched_symbols(help: &Help) -> Vec<&str> {
        help.matches
            .iter()
            .map(|&idx| help.all_items[idx].symbol.as_str())
            .collect()
    }

    #[test]
    fn printable_key_is_routed_to_input_action() {
        let help = help();

        assert_eq!(
            help.action(key(KeyCode::Char('x'), KeyModifiers::NONE)),
            Some(Action::Input('x'))
        );
    }

    #[test]
    fn navigation_keys_are_not_swallowed_by_any_binding() {
        let help = help();

        assert_eq!(
            help.action(key(KeyCode::Down, KeyModifiers::NONE)),
            Some(Action::Next)
        );
        assert_eq!(
            help.action(key(KeyCode::Up, KeyModifiers::NONE)),
            Some(Action::Prev)
        );
    }

    #[test]
    fn input_appends_query_and_rebuilds_matches() {
        let mut help = help_with_items();

        help.update(Action::Input('j'));

        assert_eq!(help.query, "j");
        assert_eq!(matched_symbols(&help), vec!["g"]);
    }

    #[test]
    fn navigation_scrolls_rows_without_selecting_items() {
        let mut help = help_with_items();

        help.update(Action::Next);
        help.update(Action::Next);
        assert_eq!(help.scroll, 2);

        help.update(Action::Prev);
        assert_eq!(help.scroll, 1);

        help.update(Action::Input('j'));
        assert_eq!(help.scroll, 0);
        assert_eq!(matched_symbols(&help), vec!["g"]);
    }

    #[test]
    fn filters_match_section_symbol_and_description() {
        for (query, expected_symbols) in
            [("search", vec!["/"]), ("/", vec!["/"]), ("jump", vec!["g"])]
        {
            let mut help = help_with_items();
            for ch in query.chars() {
                help.update(Action::Input(ch));
            }

            assert_eq!(matched_symbols(&help), expected_symbols, "query {query:?}");
        }
    }
}
