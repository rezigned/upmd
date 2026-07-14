use crossterm::event::Event as CrosstermEvent;
use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::Modifier,
    text::{Line, Text},
    widgets::{Clear, Paragraph},
    Frame,
};

use crate::apps::tui::widgets::{highlight_text, render_popup_frame, render_table};

use crate::apps::config::{Envs, KeymapConfig};
use crate::apps::navigation::Navigation;
use crate::apps::theme::Theme;
use crate::apps::tui::layout::centered_rect;
use crate::apps::tui::search::Action as SearchAction;
use crate::apps::tui::search::Search;
use crate::apps::tui::Shortcut;
use keymap::{DerivedConfig, KeyMap};

use upmd_runtime::{
    runtimes::tui::{Input, Output},
    Cmd, Component,
};

const ENV_NAME: &str = "Name";
const ENV_VALUE: &str = "Value";

/// Editor for environment variables in a table.
pub struct EnvVars {
    selected: Option<usize>,
    mode: Mode,
    search: Option<Search>,
    items: Items,
    theme: Theme,

    // Inline editing state
    editing_key: Option<String>,
    edit_name: String,
    edit_value: String,
    editing_field: Option<String>,
    cursor_x: usize,

    main_keymap: DerivedConfig<MainAction>,
    edit_keymap: DerivedConfig<EditAction>,
    nav_keymap: DerivedConfig<Navigation>,
    search_keymap_config: toml::Table,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, KeyMap)]
pub(crate) enum MainAction {
    /// Quit environment list
    #[key("esc", "q", help = "quit")]
    Quit,
    /// Enter search mode
    #[key("/", help = "search")]
    Search,
    /// Select the current item
    #[key("enter", help = "edit")]
    Select,
    /// Add new environment variable
    #[key("a", "n", help = "new")]
    Add,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, KeyMap)]
pub(crate) enum EditAction {
    /// Quit editing mode
    #[key("esc", help = "cancel")]
    Quit,
    /// Navigate left in inline editing
    #[key("left", help = "left")]
    Left,
    /// Navigate right in inline editing
    #[key("right", help = "right")]
    Right,
    /// Save changes
    #[key("enter", help = "save")]
    Save,
    /// Switch between name and value fields
    #[key("tab", help = "switch")]
    SwitchField,
    /// Delete character
    #[key("backspace", help = "delete")]
    Delete,
    /// Paste from clipboard
    #[key("ctrl-v", help = "paste")]
    Paste,
    /// Input
    #[key("@any")]
    Input(char),
}

#[derive(Default, PartialEq, Eq, Hash, Clone, Copy)]
enum Mode {
    #[default]
    List,
    Search,
    Edit,
}

struct Items {
    map: Envs,
    term: String,
    lower_term: String,
}

impl Items {
    pub fn new(envs: &Envs) -> Self {
        Self {
            map: envs.clone(),
            term: String::new(),
            lower_term: String::new(),
        }
    }

    pub fn active(&self) -> Vec<(String, String)> {
        if self.lower_term.is_empty() {
            return self
                .map
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
        }
        self.map
            .iter()
            .filter(|(k, v)| contains(k, &self.lower_term) || contains(v, &self.lower_term))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    /// Returns the count of active (filtered) items without allocating a Vec.
    pub fn active_count(&self) -> usize {
        if self.lower_term.is_empty() {
            self.map.len()
        } else {
            self.map
                .iter()
                .filter(|(k, v)| contains(k, &self.lower_term) || contains(v, &self.lower_term))
                .count()
        }
    }

    pub fn search(&mut self, term: &str) {
        self.term = term.to_string();
        self.lower_term = if term.is_empty() {
            String::new()
        } else {
            term.to_lowercase()
        };
    }

    pub fn reset(&mut self) {
        self.term.clear();
        self.lower_term.clear();
    }

    pub fn add_new(&mut self) {
        self.map.insert(String::new(), String::new());
    }

    pub fn merge(&mut self, envs: Envs) {
        self.map.extend(envs);
    }
}

fn contains(s: &str, lower_term: &str) -> bool {
    s.to_lowercase().contains(lower_term)
}

#[derive(Clone, Debug)]
pub enum Action {
    Main(MainAction),
    Edit(EditAction),
    Search(SearchAction),
    Navigation(Navigation),
    ScrollUp,
    ScrollDown,
    Quit,
}

impl EnvVars {
    pub fn new(
        envs: Envs,
        theme: Theme,
        main_keymap: DerivedConfig<MainAction>,
        edit_keymap: DerivedConfig<EditAction>,
        nav_keymap: DerivedConfig<Navigation>,
        search_keymap_config: toml::Table,
    ) -> Self {
        Self {
            selected: None,
            mode: Mode::default(),
            search: None,
            items: Items::new(&envs),
            theme,
            editing_key: None,
            edit_name: String::new(),
            edit_value: String::new(),
            editing_field: None,
            cursor_x: 0,
            main_keymap,
            edit_keymap,
            nav_keymap,
            search_keymap_config,
        }
    }

    pub fn data(&self) -> Envs {
        self.items.map.clone()
    }

    pub fn set_theme(&mut self, theme: Theme) {
        self.theme = theme;
    }

    pub fn merge_envs(&mut self, envs: Envs) {
        self.items.merge(envs);
    }

    fn selected(&self) -> Option<usize> {
        self.selected
    }

    fn next(&mut self) {
        let len = self.items.active_count();
        let i = self.selected.map(|i| (i + 1) % len.max(1)).unwrap_or(0);
        self.selected = Some(i);
    }

    fn prev(&mut self) {
        let len = self.items.active_count();
        let i = self
            .selected
            .map(|i| (i + len - 1) % len.max(1))
            .unwrap_or(0);
        self.selected = Some(i);
    }

    fn select_offset_up(&mut self, step: usize) {
        let current = self.selected.unwrap_or(0);
        self.selected = Some(current.saturating_sub(step));
    }

    fn select_offset_down(&mut self, step: usize) {
        let current = self.selected.unwrap_or(0);
        let max = self.items.active_count().saturating_sub(1);
        self.selected = Some((current + step).min(max));
    }

    fn update_list(&mut self, action: MainAction) -> Option<Cmd<Action>> {
        match action {
            MainAction::Quit => Some(Cmd::msg(Action::Quit)),
            MainAction::Search => {
                let search_keymap: DerivedConfig<SearchAction> =
                    KeymapConfig::parse_derived(&self.search_keymap_config);
                self.search = Some(Search::new(self.theme.clone(), search_keymap));
                self.mode = Mode::Search;
                None
            }
            MainAction::Select => {
                self.start_inline_edit();
                None
            }
            MainAction::Add => {
                self.items.add_new();
                let active = self.items.active();
                if let Some(index) = active
                    .iter()
                    .position(|(k, v)| k.is_empty() && v.is_empty())
                {
                    let (k, v) = &active[index];
                    self.editing_key = Some(k.clone());
                    self.edit_name = k.clone();
                    self.edit_value = v.clone();
                    self.editing_field = Some(ENV_NAME.to_string());
                    self.cursor_x = 0;
                    self.selected = Some(index);
                    self.mode = Mode::Edit;
                }
                None
            }
        }
    }

    fn update_search(&mut self, action: SearchAction) -> Option<Cmd<Action>> {
        match action {
            SearchAction::Prev => self.prev(),
            SearchAction::Next => self.next(),
            SearchAction::Quit => {
                self.search = None;
                self.mode = Mode::default();
                self.items.reset();
            }
            SearchAction::Select => {
                self.start_inline_edit();
                self.mode = Mode::Edit;
            }
            SearchAction::Input(_) | SearchAction::Delete => {
                if let Some(search) = self.search.as_mut() {
                    let _ = search.update(action);
                }
                self.search_items();
                if let Some(search) = self.search.as_mut() {
                    if search.term().is_empty() {
                        let _ = search.search::<()>(&[]);
                    } else {
                        let active = self.items.active();
                        let _ = search.search(&active);
                    }
                }
            }
        }
        None
    }

    fn update_edit(&mut self, action: EditAction) -> Option<Cmd<Action>> {
        match action {
            EditAction::Quit => self.exit_inline_edit(),
            EditAction::Save => self.save_inline_edit(),
            EditAction::Left => self.move_cursor_left(),
            EditAction::Right => self.move_cursor_right(),
            EditAction::SwitchField => self.switch_editing_field(),
            EditAction::Delete => self.delete_char(),
            EditAction::Paste => self.paste(),
            EditAction::Input(c) => self.insert_char(c),
        }
        None
    }

    fn search_items(&mut self) {
        let term = self.search.as_ref().map(|s| s.term()).unwrap_or("");
        self.items.search(term);
        if self.items.active_count() == 0 || term.is_empty() {
            self.selected = None;
        } else {
            self.selected = Some(0);
        }
    }

    fn footer_line(&self) -> Option<Line<'static>> {
        if self.mode != Mode::Search {
            return None;
        }
        self.search.as_ref().map(|s| s.footer_line())
    }

    fn start_inline_edit(&mut self) {
        if let Some(display_index) = self.selected() {
            let active = self.items.active();
            if let Some((k, v)) = active.get(display_index) {
                self.editing_key = Some(k.clone());
                self.edit_name = k.clone();
                self.edit_value = v.clone();
                self.editing_field = Some(ENV_NAME.to_string());
                self.cursor_x = k.chars().count();
                self.mode = Mode::Edit;
            }
        }
    }

    fn exit_inline_edit(&mut self) {
        if self.editing_key.as_deref() == Some("") && self.edit_value.is_empty() {
            self.items.map.remove("");
        }
        self.editing_key = None;
        self.edit_name.clear();
        self.edit_value.clear();
        self.editing_field = None;
        self.cursor_x = 0;
        self.mode = Mode::default();
    }

    fn save_inline_edit(&mut self) {
        if let Some(old_key) = self.editing_key.take() {
            self.items.map.remove(&old_key);
            self.items
                .map
                .insert(self.edit_name.clone(), self.edit_value.clone());
        }
        self.edit_name.clear();
        self.edit_value.clear();
        self.editing_field = None;
        self.cursor_x = 0;
        self.mode = Mode::default();
    }

    fn editing_field_content(&self) -> Option<&str> {
        let field = self.editing_field.as_deref()?;
        Some(match field {
            ENV_NAME => &self.edit_name,
            ENV_VALUE => &self.edit_value,
            _ => return None,
        })
    }

    fn editing_field_content_mut(&mut self) -> Option<(&mut String, &mut usize)> {
        let field = self.editing_field.as_deref()?;
        Some(match field {
            ENV_NAME => (&mut self.edit_name, &mut self.cursor_x),
            ENV_VALUE => (&mut self.edit_value, &mut self.cursor_x),
            _ => return None,
        })
    }

    fn move_cursor_left(&mut self) {
        if self.cursor_x > 0 {
            self.cursor_x -= 1;
        }
    }

    fn move_cursor_right(&mut self) {
        let Some(s) = self.editing_field_content() else {
            return;
        };
        if self.cursor_x < s.chars().count() {
            self.cursor_x += 1;
        }
    }

    fn switch_editing_field(&mut self) {
        let field = match self.editing_field.as_deref() {
            Some(ENV_NAME) => ENV_VALUE,
            Some(ENV_VALUE) => ENV_NAME,
            _ => return,
        };
        self.editing_field = Some(field.to_string());
        self.cursor_x = self
            .editing_field_content()
            .map(|s| s.chars().count())
            .unwrap_or(0);
    }

    fn insert_char(&mut self, c: char) {
        let Some((s, cursor)) = self.editing_field_content_mut() else {
            return;
        };
        let byte_idx = s
            .char_indices()
            .nth(*cursor)
            .map(|(i, _)| i)
            .unwrap_or(s.len());
        s.insert(byte_idx, c);
        *cursor += 1;
    }

    fn delete_char(&mut self) {
        let Some((s, cursor)) = self.editing_field_content_mut() else {
            return;
        };
        if *cursor > 0 {
            *cursor -= 1;
            let byte_idx = s
                .char_indices()
                .nth(*cursor)
                .map(|(i, _)| i)
                .unwrap_or(s.len());
            s.remove(byte_idx);
        }
    }

    fn paste(&mut self) {
        if let Some(text) = crate::utils::clipboard_paste() {
            for c in text.chars() {
                if c == '\n' || c == '\r' {
                    continue;
                }
                self.insert_char(c);
            }
        }
    }

    fn render_envs(&self, frame: &mut Frame, area: Rect) {
        let active = self.items.active();
        let search_term = self.search.as_ref().map(|s| s.term()).unwrap_or("");
        let style = self.theme.style();

        let (rows_area, footer_area, inner_width) = render_popup_frame(
            frame,
            area,
            "Environment Variables",
            &self.theme,
            &format!(" {ENV_NAME}"),
            ENV_VALUE,
        );
        let value_width = inner_width / 2;

        let editing_display_index = self
            .editing_key
            .as_ref()
            .and_then(|key| active.iter().position(|(k, _)| k == key));

        fn line_count(text: &str, width: u16) -> u16 {
            text.lines()
                .map(|line| (line.len() as u16).div_ceil(width))
                .sum::<u16>()
                .max(1)
        }

        let row_h: Vec<u16> = active
            .iter()
            .enumerate()
            .map(|(i, item)| {
                if Some(i) == editing_display_index {
                    line_count(&item.1, value_width)
                } else {
                    1
                }
            })
            .collect();

        render_table(
            frame,
            rows_area,
            &active,
            self.selected,
            &row_h,
            |item, _i, is_selected| {
                let is_editing = self.editing_key.as_ref().is_some_and(|key| *key == item.0);
                let row_style = if is_selected {
                    style.add_modifier(Modifier::REVERSED)
                } else {
                    style
                };

                let (name_text, value_text) = if is_editing {
                    let editing_name = self.editing_field.as_ref().is_some_and(|f| *f == ENV_NAME);
                    let mut name = self.edit_name.clone();
                    let mut value = self.edit_value.clone();
                    if editing_name {
                        insert_cursor(&mut name, self.cursor_x);
                    } else {
                        insert_cursor(&mut value, self.cursor_x);
                    }
                    (name, value)
                } else {
                    (item.0.clone(), item.1.clone())
                };

                let padded_name = format!(" {} ", name_text);

                (
                    highlight_text(
                        &padded_name,
                        search_term,
                        self.theme.search_highlight_style(),
                    ),
                    highlight_text(
                        &value_text,
                        search_term,
                        self.theme.search_highlight_style(),
                    ),
                    row_style,
                )
            },
        );

        self.render_footer(frame, footer_area);
    }

    fn render_footer(&self, frame: &mut Frame, footer_area: Rect) {
        if self.mode != Mode::Search && self.mode != Mode::List && self.mode != Mode::Edit {
            return;
        }
        let left = match self.mode {
            Mode::Search => self
                .search
                .as_ref()
                .map(|s| s.footer_shortcuts())
                .unwrap_or_default(),
            Mode::Edit => self
                .theme
                .keymap_shortcuts(&self.edit_keymap.items, |action| {
                    matches!(
                        action,
                        EditAction::Quit | EditAction::Save | EditAction::SwitchField
                    )
                }),
            _ => self
                .theme
                .keymap_shortcuts(&self.main_keymap.items, |action| {
                    matches!(
                        action,
                        MainAction::Quit
                            | MainAction::Search
                            | MainAction::Select
                            | MainAction::Add
                    )
                }),
        };
        let right = self.footer_line();
        let right_width = right.as_ref().map(|r| r.width() as u16).unwrap_or(0) + 2;
        let footer_layout = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(10), Constraint::Length(right_width.max(20))])
            .split(footer_area);
        frame.render_widget(self.theme.footer(left), footer_layout[0]);
        if let Some(footer) = right {
            let text = Text::from(footer);
            let paragraph = Paragraph::new(text).alignment(Alignment::Right);
            frame.render_widget(paragraph, footer_layout[1]);
        }
    }
}

impl Component for EnvVars {
    type Msg = Action;

    fn update(&mut self, msg: Self::Msg) -> Option<Cmd<Self::Msg>> {
        match msg {
            Action::Main(action) => self.update_list(action),
            Action::Edit(action) => self.update_edit(action),
            Action::Search(action) => self.update_search(action),
            Action::Navigation(nav) => {
                match nav {
                    Navigation::Next => self.next(),
                    Navigation::Prev => self.prev(),
                    Navigation::First => self.selected = Some(0),
                    Navigation::Last => {
                        self.selected = Some(self.items.active_count().saturating_sub(1));
                    }
                    Navigation::PageUp => self.select_offset_up(10),
                    Navigation::PageDown => self.select_offset_down(10),
                }
                None
            }
            Action::ScrollUp => {
                self.select_offset_up(3);
                None
            }
            Action::ScrollDown => {
                self.select_offset_down(3);
                None
            }
            Action::Quit => Some(Cmd::msg(Action::Quit)),
        }
    }
}

impl Input for EnvVars {
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
            CrosstermEvent::Key(key) => match self.mode {
                Mode::List => self
                    .main_keymap
                    .get_bound(&key)
                    .map(Action::Main)
                    .or_else(|| self.nav_keymap.get_bound(&key).map(Action::Navigation)),
                Mode::Search => self
                    .search
                    .as_ref()
                    .and_then(|s| s.action(CrosstermEvent::Key(key)))
                    .map(Action::Search),
                Mode::Edit => self.edit_keymap.get_bound(&key).map(Action::Edit),
            },
            _ => None,
        }
    }
}

impl Output for EnvVars {
    fn render(&self, frame: &mut Frame, area: ratatui::layout::Rect) {
        let popup_area = centered_rect(80, 70, area);
        frame.render_widget(Clear, popup_area);
        self.render_envs(frame, popup_area);
    }
}

fn insert_cursor(s: &mut String, cursor_x: usize) {
    // cursor_x is a character index; convert to a byte index before inserting
    // the cursor block so multi-byte UTF-8 characters are not split.
    let byte_idx = s
        .char_indices()
        .nth(cursor_x)
        .map(|(i, _)| i)
        .unwrap_or(s.len());
    if byte_idx <= s.len() {
        s.insert(byte_idx, '█');
    } else {
        s.push('█');
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_env_vars() -> EnvVars {
        let keymap: DerivedConfig<MainAction> = toml::from_str("").unwrap();
        let edit_keymap: DerivedConfig<EditAction> = toml::from_str("").unwrap();
        let nav_keymap: DerivedConfig<Navigation> = toml::from_str("").unwrap();
        EnvVars::new(
            Envs::new(),
            Theme::default(),
            keymap,
            edit_keymap,
            nav_keymap,
            toml::Table::new(),
        )
    }

    #[test]
    fn test_insert_multibyte_characters() {
        let mut envs = empty_env_vars();
        envs.edit_name = "ab".to_string();
        envs.editing_field = Some(ENV_NAME.to_string());
        envs.cursor_x = 1; // between 'a' and 'b'

        envs.insert_char('é');
        assert_eq!(envs.edit_name, "aéb");
        assert_eq!(envs.cursor_x, 2);

        envs.insert_char('ก');
        assert_eq!(envs.edit_name, "aéกb");
        assert_eq!(envs.cursor_x, 3);

        envs.insert_char('a');
        assert_eq!(envs.edit_name, "aéกab");
        assert_eq!(envs.cursor_x, 4);
    }

    #[test]
    fn test_delete_multibyte_characters() {
        let mut envs = empty_env_vars();
        envs.edit_name = "aéกb".to_string();
        envs.editing_field = Some(ENV_NAME.to_string());
        envs.cursor_x = 3; // after 'ก'
        envs.delete_char();
        assert_eq!(envs.edit_name, "aéb");
        envs.delete_char();
        assert_eq!(envs.edit_name, "ab");
    }

    #[test]
    fn test_insert_cursor_multibyte() {
        let mut s = "aéกb".to_string();
        insert_cursor(&mut s, 2); // after 'é'
        assert_eq!(s, "aé█กb");
    }

    #[test]
    fn test_cursor_does_not_exceed_char_count() {
        let mut envs = empty_env_vars();
        envs.edit_name = "é".to_string();
        envs.editing_field = Some(ENV_NAME.to_string());
        envs.cursor_x = 1;

        envs.move_cursor_right();
        assert_eq!(envs.cursor_x, 1, "cursor should stop at char count");
    }
}
