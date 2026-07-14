use crossterm::event::Event as CrosstermEvent;
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Clear,
    Frame,
};

use crate::apps::config::KeymapConfig;
use crate::apps::navigation::Navigation;
use crate::apps::theme::{self, Theme};
use crate::apps::tui::layout::centered_rect;
use crate::apps::tui::search::Action as SearchAction;
use crate::apps::tui::search::Search;
use crate::apps::tui::widgets::{highlight_text, render_popup_frame, render_table};
use keymap::{DerivedConfig, KeyMap};

use upmd_runtime::{
    runtimes::tui::{Input, Output},
    Cmd, Component,
};

/// Searchable table for switching application themes.
pub struct ThemeSelector {
    selected: Option<usize>,
    route: Route,
    search: Option<Search>,
    items: Vec<Item>,
    theme: Theme,
    transparent: bool,
    original_theme: Option<String>,
    scroll_acc: i32,

    main_keymap: DerivedConfig<MainAction>,
    nav_keymap: DerivedConfig<Navigation>,
    search_keymap_config: toml::Table,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, KeyMap)]
pub(crate) enum MainAction {
    /// Quit theme list
    #[key("esc", "q")]
    Quit,
    /// Enter search mode
    #[key("/")]
    Search,
    /// Select the current theme
    #[key("enter")]
    Select,
}

#[derive(Default, PartialEq, Eq, Hash, Clone, Copy)]
enum Route {
    #[default]
    List,
    Search,
}

#[derive(Clone)]
pub(crate) struct Item {
    pub name: String,
    pub theme: Theme,
    pub show: bool,
}

#[derive(Clone, Debug)]
pub enum Action {
    Main(MainAction),
    Search(SearchAction),
    Navigation(Navigation),
    Preview(Theme),
    Select(Theme),
    Restore(Theme),
    ScrollUp,
    ScrollDown,
}

impl ThemeSelector {
    pub fn new(
        current_theme: Theme,
        transparent: bool,
        main_keymap: DerivedConfig<MainAction>,
        nav_keymap: DerivedConfig<Navigation>,
        search_keymap_config: toml::Table,
    ) -> Self {
        let available = theme::available_themes();
        let items: Vec<Item> = available
            .into_iter()
            .map(|name| Item {
                theme: Theme::new(&name, transparent),
                name,
                show: true,
            })
            .collect();

        let selected = if let Some(pos) = items.iter().position(|i| i.name == current_theme.name())
        {
            Some(pos)
        } else if !items.is_empty() {
            Some(0)
        } else {
            None
        };

        Self {
            selected,
            route: Route::default(),
            search: None,
            items,
            theme: current_theme.clone(),
            transparent,
            original_theme: Some(current_theme.name().to_string()),
            scroll_acc: 0,
            main_keymap,
            nav_keymap,
            search_keymap_config,
        }
    }

    fn active_items(&self) -> Vec<&Item> {
        self.items.iter().filter(|i| i.show).collect()
    }

    pub fn selected_theme(&self) -> Option<String> {
        self.selected
            .and_then(|idx| self.active_items().get(idx).map(|i| i.name.clone()))
    }

    fn preview_selected(&mut self) -> Option<Cmd<Action>> {
        self.selected_theme().map(|name| {
            let theme = Theme::new(&name, self.transparent);
            self.theme = theme.clone();
            Cmd::msg(Action::Preview(theme))
        })
    }

    fn next(&mut self) {
        let active_len = self.active_items().len();
        if active_len == 0 {
            return;
        }
        let i = self.selected.map(|i| (i + 1) % active_len).unwrap_or(0);
        self.selected = Some(i);
    }

    fn prev(&mut self) {
        let active_len = self.active_items().len();
        if active_len == 0 {
            return;
        }
        let i = self
            .selected
            .map(|i| (i + active_len - 1) % active_len)
            .unwrap_or(0);
        self.selected = Some(i);
    }

    fn first(&mut self) {
        if !self.active_items().is_empty() {
            self.selected = Some(0);
        }
    }

    fn last(&mut self) {
        let active_len = self.active_items().len();
        if active_len > 0 {
            self.selected = Some(active_len.saturating_sub(1));
        }
    }

    fn page_up(&mut self) {
        for _ in 0..10 {
            self.prev();
        }
    }

    fn page_down(&mut self) {
        for _ in 0..10 {
            self.next();
        }
    }

    fn search_items(&mut self) {
        let term = self
            .search
            .as_ref()
            .map(|s| s.term().to_lowercase())
            .unwrap_or_default();
        for item in &mut self.items {
            item.show = term.is_empty() || item.name.to_lowercase().contains(&term);
        }
        let active = self.active_items();
        if active.is_empty() {
            self.selected = None;
        } else {
            self.selected = Some(0);
        }
    }

    fn update_list(&mut self, action: MainAction) -> Option<Cmd<Action>> {
        match action {
            MainAction::Quit => {
                let name = self.original_theme.clone().unwrap_or_default();
                let theme = Theme::new(&name, self.transparent);
                Some(Cmd::msg(Action::Restore(theme)))
            }
            MainAction::Search => {
                let search_keymap: DerivedConfig<SearchAction> =
                    KeymapConfig::parse_derived(&self.search_keymap_config);
                self.search = Some(Search::new(self.theme.clone(), search_keymap));
                self.route = Route::Search;
                None
            }
            MainAction::Select => self.selected_theme().map(|name| {
                let theme = Theme::new(&name, self.transparent);
                Cmd::msg(Action::Select(theme))
            }),
        }
    }

    fn update_search(&mut self, action: SearchAction) -> Option<Cmd<Action>> {
        match action {
            SearchAction::Prev => {
                self.prev();
                return self.preview_selected();
            }
            SearchAction::Next => {
                self.next();
                return self.preview_selected();
            }
            SearchAction::Quit => {
                self.search = None;
                self.route = Route::default();
                self.search_items();
                return None;
            }
            SearchAction::Select => {
                return self.selected_theme().map(|name| {
                    let theme = Theme::new(&name, self.transparent);
                    Cmd::msg(Action::Select(theme))
                });
            }
            SearchAction::Input(_) | SearchAction::Delete => {}
        }

        if let Some(search) = self.search.as_mut() {
            let _ = search.update(action);
        }
        self.search_items();
        let active: Vec<_> = self.active_items().into_iter().cloned().collect();
        if let Some(search) = self.search.as_mut() {
            if search.term().is_empty() {
                let _ = search.search::<()>(&[]);
            } else {
                let _ = search.search(&active);
            }
        }
        self.preview_selected()
    }

    fn render_themes(&self, frame: &mut Frame, area: Rect) {
        let active = self.active_items();
        let row_h = vec![1u16; active.len()];
        let search_term = self.search.as_ref().map(|s| s.term()).unwrap_or("");
        let (rows_area, footer_area, _) =
            render_popup_frame(frame, area, "Themes", &self.theme, " Name ", " Swatches ");

        let selected_bg = self.theme.code_background;

        render_table(
            frame,
            rows_area,
            &active,
            self.selected,
            &row_h,
            |item, _i, is_selected| {
                let swatch_bg = if is_selected { Some(selected_bg) } else { None };

                let mut spans = Vec::new();
                let colors = [
                    item.theme.background,
                    item.theme.foreground,
                    item.theme.accent,
                    item.theme.active,
                ];
                for color in colors {
                    let mut style = Style::default().fg(color);
                    if let Some(bg) = swatch_bg {
                        style = style.bg(bg);
                    }
                    spans.push(Span::styled(" ⬤ ", style));
                }

                let style = if is_selected {
                    self.theme
                        .code_style()
                        .fg(self.theme.foreground)
                        .add_modifier(Modifier::BOLD)
                } else {
                    self.theme.style()
                };

                (
                    highlight_text(
                        &format!(" {} ", item.name),
                        search_term,
                        self.theme.search_highlight_style(),
                    ),
                    Line::from(spans),
                    style,
                )
            },
        );

        if self.route == Route::Search || self.route == Route::List {
            let left = if self.route == Route::Search {
                self.theme.shortcuts(&[
                    ("esc".to_string(), "quit".to_string()),
                    ("↵".to_string(), "select".to_string()),
                ])
            } else {
                self.theme.shortcuts(&[
                    ("esc".to_string(), "quit".to_string()),
                    ("/".to_string(), "search".to_string()),
                    ("↵".to_string(), "select".to_string()),
                ])
            };
            let right = self.search.as_ref().map(|s| s.footer_line());
            let right_width = right.as_ref().map(|r| r.width() as u16).unwrap_or(0) + 2;
            let footer_layout = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Min(10), Constraint::Length(right_width.max(20))])
                .split(footer_area);
            frame.render_widget(self.theme.footer(left), footer_layout[0]);
            if let Some(footer) = right {
                let text = ratatui::text::Text::from(footer);
                let paragraph = ratatui::widgets::Paragraph::new(text)
                    .alignment(ratatui::layout::Alignment::Right);
                frame.render_widget(paragraph, footer_layout[1]);
            }
        }
    }
}

impl Component for ThemeSelector {
    type Msg = Action;

    fn update(&mut self, msg: Self::Msg) -> Option<Cmd<Self::Msg>> {
        match msg {
            Action::Main(action) => self.update_list(action),
            Action::Search(action) => self.update_search(action),
            Action::Navigation(nav) => {
                match nav {
                    Navigation::First => self.first(),
                    Navigation::Last => self.last(),
                    Navigation::Prev => self.prev(),
                    Navigation::Next => self.next(),
                    Navigation::PageUp => self.page_up(),
                    Navigation::PageDown => self.page_down(),
                };
                self.preview_selected()
            }
            Action::Preview(_) => None,
            Action::Select(_) => None,
            Action::Restore(_) => None,
            Action::ScrollUp => {
                self.scroll_acc -= 1;
                if self.scroll_acc <= -3 {
                    self.scroll_acc = 0;
                    self.prev();
                    return self.preview_selected();
                }
                None
            }
            Action::ScrollDown => {
                self.scroll_acc += 1;
                if self.scroll_acc >= 3 {
                    self.scroll_acc = 0;
                    self.next();
                    return self.preview_selected();
                }
                None
            }
        }
    }
}

impl Input for ThemeSelector {
    fn action(&self, event: CrosstermEvent) -> Option<Self::Msg> {
        match event {
            CrosstermEvent::Mouse(mouse) => {
                if mouse.kind == crossterm::event::MouseEventKind::ScrollUp {
                    Some(Action::ScrollUp)
                } else if mouse.kind == crossterm::event::MouseEventKind::ScrollDown {
                    Some(Action::ScrollDown)
                } else {
                    None
                }
            }
            CrosstermEvent::Key(key) => match self.route {
                Route::List => self
                    .main_keymap
                    .get_bound(&key)
                    .map(Action::Main)
                    .or_else(|| self.nav_keymap.get_bound(&key).map(Action::Navigation)),
                Route::Search => self
                    .search
                    .as_ref()
                    .and_then(|s| s.action(CrosstermEvent::Key(key)))
                    .map(Action::Search),
            },
            _ => None,
        }
    }
}

impl Output for ThemeSelector {
    fn render(&self, frame: &mut Frame, area: Rect) {
        let popup_area = centered_rect(60, 60, area);
        frame.render_widget(Clear, popup_area);
        self.render_themes(frame, popup_area);
    }
}
