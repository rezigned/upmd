use crate::apps::theme::Theme;
use crate::apps::tui::Shortcut;
use keymap::{DerivedConfig, KeyMap};

use upmd_runtime::{
    runtimes::tui::{Input, Output},
    Cmd, Component,
};

impl Shortcut for Search {
    fn footer_shortcuts(&self) -> ratatui::text::Line<'static> {
        self.theme.keymap_shortcuts(&self.keymap.items, |action| {
            matches!(action, Action::Quit | Action::Select)
        })
    }

    fn footer_right(&self) -> Option<ratatui::text::Line<'static>> {
        Some(self.footer_line())
    }
}

/// Text search across code block output.
pub struct Search {
    term: String,
    index: usize,
    total: usize,
    keymap: DerivedConfig<Action>,
    theme: Theme,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, KeyMap)]
pub enum Action {
    /// Deletes the last character from the search query.
    #[key("backspace", help = "delete")]
    Delete,
    /// Selects the current search match and exits search mode.
    #[key("enter", help = "select")]
    Select,
    /// Moves to the next search result.
    #[key("down", "ctrl-n", help = "next")]
    Next,
    /// Moves to the previous search result.
    #[key("up", "ctrl-p", help = "prev")]
    Prev,
    /// Exits search mode without selecting.
    #[key("esc", help = "quit")]
    Quit,
    /// Appends a character to the search query.
    #[key("@any")]
    Input(char),
}

impl Search {
    pub fn new(theme: Theme, keymap: DerivedConfig<Action>) -> Self {
        Self {
            term: String::new(),
            index: 0,
            total: 0,
            theme,
            keymap,
        }
    }

    pub fn term(&self) -> &str {
        &self.term
    }

    pub fn index(&self) -> usize {
        if self.total > 0 {
            self.index.saturating_add(1)
        } else {
            self.index
        }
    }

    pub fn total(&self) -> usize {
        self.total
    }

    pub fn search<'a, T>(&mut self, items: &'a [T]) -> Option<&'a T> {
        if items.is_empty() {
            self.index = 0;
            self.total = 0;
            return None;
        }
        self.total = items.len();
        self.index = self.index.min(self.total.saturating_sub(1));
        items.get(self.index)
    }

    pub fn footer_line(&self) -> ratatui::text::Line<'static> {
        use ratatui::text::Line;
        let info = format!("'{}' {}/{}", self.term(), self.index(), self.total());
        Line::raw(info)
    }
}

impl Component for Search {
    type Msg = Action;

    fn update(&mut self, msg: Self::Msg) -> Option<Cmd<Self::Msg>> {
        match msg {
            Action::Input(c) => {
                self.term.push(c);
                None
            }
            Action::Delete => {
                self.term.pop();
                None
            }
            Action::Next => {
                self.index = self.index.saturating_add(1);
                None
            }
            Action::Prev => {
                self.index = self.index.saturating_sub(1);
                None
            }
            Action::Select => Some(Cmd::msg(Action::Select)),
            Action::Quit => Some(Cmd::msg(Action::Quit)),
        }
    }
}

impl Input for Search {
    fn action(&self, event: crossterm::event::Event) -> Option<Self::Msg> {
        match event {
            crossterm::event::Event::Key(key) => self.keymap.get_bound(&key),
            _ => None,
        }
    }
}

impl Output for Search {
    fn render(&self, _frame: &mut ratatui::Frame, _area: ratatui::layout::Rect) {}
}
