use std::cell::Cell;
use std::collections::HashMap;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use ratatui::{
    layout::{Alignment, Rect},
    symbols::merge::MergeStrategy,
    text::Line,
    widgets::{Borders, List, ListItem},
    Frame,
};
use upmd_parser::nodes::Code;

use crate::apps::theme::Theme;
use crate::apps::tui::widgets::Spinner;
use crate::apps::{config, navigation::Navigation};

use crate::runner::CodeId;
use keymap::DerivedConfig;

use upmd_runtime::{
    runtimes::tui::{Input, Output},
    Cmd, Component,
};
pub enum MenuMode {
    CodeBlocks,
    Toc,
}

/// Task lifecycle status used to color menu items.
pub use crate::apps::task::TaskStatus as MenuTaskStatus;

/// Description of parsed code blocks for navigation.
pub struct Menu {
    model: Model,
    theme: Theme,
    code_statuses: HashMap<CodeId, MenuTaskStatus>,
    mode: MenuMode,
    toc_items: Vec<(u8, String)>,
    nav_keymap: DerivedConfig<Navigation>,
    spinner: Spinner,
    last_area: Cell<Rect>,
    toc_width_adjustment: i16,
}

/// The menu's data: code block IDs and selection index.
pub struct Model {
    pub items: Vec<CodeId>,
    pub state: ratatui::widgets::ListState,
}

#[derive(Clone, Debug)]
pub enum Action {
    Navigation(Navigation),
    /// User clicked a code block entry in the menu.
    Click(CodeId),
    /// User clicked a TOC heading entry in the menu.
    TocClick(usize),
}

fn toc_items(headings: &[upmd_parser::Heading]) -> Vec<(u8, String)> {
    headings
        .iter()
        .map(|heading| (heading.level, heading.text.clone()))
        .collect()
}

impl Menu {
    /// Creates a menu from parsed code blocks and headings.
    pub fn new(
        codes: &[Code],
        headings: &[upmd_parser::Heading],
        theme: Theme,
        nav_keymap: DerivedConfig<Navigation>,
    ) -> Self {
        let items: Vec<CodeId> = codes.iter().map(|c| c.id).collect();
        let toc_items = toc_items(headings);
        let mut state = ratatui::widgets::ListState::default();
        if !items.is_empty() {
            state.select(Some(0));
        }
        Self {
            model: Model { items, state },
            theme,
            code_statuses: HashMap::new(),
            mode: MenuMode::CodeBlocks,
            toc_items,
            nav_keymap,
            spinner: Spinner::dot(),
            last_area: Cell::new(Rect::default()),
            toc_width_adjustment: 0,
        }
    }
    /// Advances the spinner tick counter (driven by Msg::Tick).
    pub fn tick(&mut self) {
        self.spinner.tick();
    }

    pub fn set_mode(&mut self, mode: MenuMode) {
        self.mode = mode;
        self.model.state.select(Some(0));
    }

    pub fn mode(&self) -> &MenuMode {
        &self.mode
    }

    pub fn set_code_statuses(&mut self, statuses: HashMap<CodeId, MenuTaskStatus>) {
        self.code_statuses = statuses;
    }

    pub fn set_theme(&mut self, theme: Theme) {
        self.theme = theme;
    }

    pub fn selected(&self) -> Option<CodeId> {
        if matches!(self.mode, MenuMode::Toc) {
            return None;
        }
        self.model
            .state
            .selected()
            .and_then(|i| self.model.items.get(i).copied())
    }

    pub fn selected_toc_idx(&self) -> Option<usize> {
        if !matches!(self.mode, MenuMode::Toc) {
            return None;
        }
        self.model.state.selected()
    }

    pub fn next(&mut self) {
        let len = match self.mode {
            MenuMode::CodeBlocks => self.model.items.len(),
            MenuMode::Toc => self.toc_items.len(),
        };
        if len == 0 {
            return;
        }
        let i = match self.model.state.selected() {
            Some(i) => {
                if i >= len.saturating_sub(1) {
                    0
                } else {
                    i + 1
                }
            }
            None => 0,
        };
        self.model.state.select(Some(i));
    }

    pub fn previous(&mut self) {
        let len = match self.mode {
            MenuMode::CodeBlocks => self.model.items.len(),
            MenuMode::Toc => self.toc_items.len(),
        };
        if len == 0 {
            return;
        }
        let i = match self.model.state.selected() {
            Some(i) => {
                if i == 0 {
                    len.saturating_sub(1)
                } else {
                    i - 1
                }
            }
            None => 0,
        };
        self.model.state.select(Some(i));
    }

    pub fn first(&mut self) {
        if !self.model.items.is_empty() || !self.toc_items.is_empty() {
            self.model.state.select(Some(0));
        }
    }

    pub fn last(&mut self) {
        let len = match self.mode {
            MenuMode::CodeBlocks => self.model.items.len(),
            MenuMode::Toc => self.toc_items.len(),
        };
        if len > 0 {
            self.model.state.select(Some(len.saturating_sub(1)));
        }
    }

    fn page_size(&self) -> usize {
        let rows = self.last_area.get().height.saturating_sub(2);
        if rows < 1 {
            10
        } else {
            rows as usize
        }
    }

    fn adjust_selection(&mut self, f: impl FnOnce(usize, usize) -> usize) {
        let len = match self.mode {
            MenuMode::CodeBlocks => self.model.items.len(),
            MenuMode::Toc => self.toc_items.len(),
        };
        if len == 0 {
            return;
        }
        let i = self.model.state.selected().map(|i| f(i, len)).unwrap_or(0);
        self.model.state.select(Some(i));
    }

    pub fn page_up(&mut self) {
        let page = self.page_size();
        self.adjust_selection(|i, _| i.saturating_sub(page));
    }

    pub fn page_down(&mut self) {
        let page = self.page_size();
        self.adjust_selection(|i, len| i.saturating_add(page).min(len.saturating_sub(1)));
    }

    pub fn select_by_id(&mut self, id: CodeId) {
        if matches!(self.mode, MenuMode::Toc) {
            return;
        }
        if let Some(i) = self.model.items.iter().position(|&x| x == id) {
            self.model.state.select(Some(i));
        }
    }

    pub fn select_by_heading_idx(&mut self, heading_idx: usize) {
        if !matches!(self.mode, MenuMode::Toc) {
            return;
        }
        let idx = heading_idx.min(self.toc_items.len().saturating_sub(1));
        self.model.state.select(Some(idx));
    }

    pub fn deselect(&mut self) {
        self.model.state.select(None);
    }

    fn toc_base(&self, total_width: u16) -> i16 {
        let max = total_width / crate::apps::config::MENU_MAX_WIDTH_RATIO;
        24u16.min(max).clamp(15, 40) as i16
    }

    pub fn width(&self, total_width: u16) -> u16 {
        match self.mode {
            MenuMode::CodeBlocks => {
                if self.model.items.is_empty() {
                    return 0;
                }
                let len = self.model.items.len().checked_ilog10().unwrap_or(0) + 1;
                5.max(4 + len as u16)
            }
            MenuMode::Toc => {
                if self.toc_items.is_empty() {
                    return 0;
                }
                (self.toc_base(total_width) + self.toc_width_adjustment).clamp(
                    crate::apps::config::MENU_TOC_MIN_WIDTH as i16,
                    crate::apps::config::MENU_TOC_MAX_WIDTH as i16,
                ) as u16
            }
        }
    }

    pub fn adjust_toc_width(&mut self, delta: i16, total_width: u16) {
        let base = self.toc_base(total_width);
        let min_adj = (crate::apps::config::MENU_TOC_MIN_WIDTH as i16).saturating_sub(base);
        let max_adj = (crate::apps::config::MENU_TOC_MAX_WIDTH as i16).saturating_sub(base);
        self.toc_width_adjustment = (self.toc_width_adjustment + delta).clamp(min_adj, max_adj);
    }
}

impl Input for Menu {
    fn action(&self, event: crossterm::event::Event) -> Option<Self::Msg> {
        match event {
            crossterm::event::Event::Key(key) => {
                self.nav_keymap.get_bound(&key).map(Action::Navigation)
            }
            crossterm::event::Event::Mouse(mouse) => self.handle_mouse(mouse),
            _ => None,
        }
    }
}

impl Menu {
    /// Hit-tests a mouse click against the list items.
    ///
    /// Row 0 of the area is the top border, so item 0 starts at area.y + 1.
    /// Accounts for the list scroll offset so clicks work even when the list
    /// is scrolled down.
    fn handle_mouse(&self, mouse: crossterm::event::MouseEvent) -> Option<Action> {
        use crossterm::event::{MouseButton, MouseEventKind};
        if !matches!(mouse.kind, MouseEventKind::Up(MouseButton::Left)) {
            return None;
        }
        let area = self.last_area.get();
        // Must be inside the menu area.
        if !crate::utils::mouse_in_area(&mouse, area) {
            return None;
        }
        // Row relative to the inner list (skip top border at area.y).
        let inner_row = mouse.row.saturating_sub(area.y + 1) as usize;
        let offset = self.model.state.offset();
        let item_idx = offset + inner_row;

        match self.mode {
            MenuMode::CodeBlocks => {
                let id = self.model.items.get(item_idx).copied()?;
                Some(Action::Click(id))
            }
            MenuMode::Toc => {
                if item_idx < self.toc_items.len() {
                    Some(Action::TocClick(item_idx))
                } else {
                    None
                }
            }
        }
    }
}

impl Component for Menu {
    type Msg = Action;

    fn update(&mut self, msg: Self::Msg) -> Option<Cmd<Self::Msg>> {
        match msg {
            Action::Navigation(Navigation::Prev) => self.previous(),
            Action::Navigation(Navigation::Next) => self.next(),
            Action::Navigation(Navigation::First) => self.first(),
            Action::Navigation(Navigation::Last) => self.last(),
            Action::Navigation(Navigation::PageUp) => self.page_up(),
            Action::Navigation(Navigation::PageDown) => self.page_down(),
            // Click actions are handled by the parent app; nothing to do here.
            Action::Click(_) | Action::TocClick(_) => {}
        }
        None
    }
}

impl Output for Menu {
    fn render(&self, frame: &mut Frame, area: Rect) {
        // Menu width can be 0 when there are no code blocks; ratatui panics
        // rendering borders in areas smaller than 2×2.
        if area.width < 2 || area.height < 2 {
            return;
        }
        self.last_area.set(area);

        let items: Vec<ListItem> = match self.mode {
            MenuMode::CodeBlocks => {
                let len = self.model.items.len().checked_ilog10().unwrap_or(0);
                self.model
                    .items
                    .iter()
                    .enumerate()
                    .map(|(i, &id)| {
                        let padding = len.saturating_sub(id.checked_ilog10().unwrap_or(0)) as usize;
                        let mut content = format!(" {:>padding$}{id} ", "");
                        let mut style = self.theme.muted_style();
                        match self.code_statuses.get(&id) {
                            Some(MenuTaskStatus::Running) => {
                                content.replace_range(0..1, &self.spinner.render().to_string());
                                style = self.theme.running_style();
                            }
                            Some(MenuTaskStatus::Success) => {
                                style = self.theme.success_style();
                            }
                            Some(MenuTaskStatus::Error) => {
                                style = self.theme.error_style();
                            }
                            _ => {}
                        }

                        if Some(i) == self.model.state.selected() {
                            ListItem::new(content).style(self.theme.active_style())
                        } else {
                            ListItem::new(content).style(style)
                        }
                    })
                    .collect()
            }
            MenuMode::Toc => {
                let item_width = area.width.saturating_sub(config::MENU_BORDER_SIZE) as usize;
                let min_level = self.toc_items.iter().map(|(l, _)| *l).min().unwrap_or(1);
                self.toc_items
                    .iter()
                    .enumerate()
                    .map(|(i, (level, text))| {
                        let indent =
                            " ".repeat((*level as usize).saturating_sub(min_level as usize) * 2);
                        let max_width = item_width.saturating_sub(indent.len() + 2);
                        let display = truncate_to_width(text, max_width);
                        let line = Line::from(format!(" {indent}{display} "));
                        if Some(i) == self.model.state.selected() {
                            ListItem::new(line).style(self.theme.active_style())
                        } else {
                            ListItem::new(line).style(self.theme.style())
                        }
                    })
                    .collect()
            }
        };

        let title = match self.mode {
            MenuMode::CodeBlocks => "",
            MenuMode::Toc => " TOC ",
        };

        let list = List::new(items)
            .block(
                self.theme
                    .block()
                    .title(title)
                    .title_alignment(Alignment::Center)
                    .borders(Borders::ALL)
                    .border_style(self.theme.inactive_style())
                    .merge_borders(MergeStrategy::Exact),
            )
            .highlight_style(self.theme.active_style());

        let mut state = self.model.state;
        frame.render_stateful_widget(list, area, &mut state);
    }
}

/// Truncates a string to fit within `max_width` display columns,
/// appending "…" when truncation occurs. Handles CJK wide characters.
fn truncate_to_width(text: &str, max_width: usize) -> String {
    const ELLIPSIS: &str = "…";
    let ellipsis_width = ELLIPSIS.width();

    if max_width == 0 {
        return String::new();
    }
    if text.width() <= max_width {
        return text.to_string();
    }
    if max_width <= ellipsis_width {
        // Not enough room for any content, just signal truncation.
        return ELLIPSIS.chars().take(max_width).collect();
    }

    let target = max_width - ellipsis_width;
    let mut w = 0;
    let mut end = 0;

    for (i, c) in text.char_indices() {
        let cw = c.width().unwrap_or(0);
        if w + cw > target {
            break;
        }
        w += cw;
        end = i + c.len_utf8();
    }

    let mut out = String::with_capacity(end + ELLIPSIS.len());
    out.push_str(&text[..end]);
    out.push_str(ELLIPSIS);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use upmd_parser as parser;
    use upmd_parser::Parser;

    #[test]
    fn test_extract_code_ids_finds_nested_in_blockquote() {
        let md =
            "# Welcome\n\n> Blockquote\n> ```sh\n> ls\n> ```\n\n## Other\n```sh\necho hi\n```\n";
        let doc = parser::new().parse(md);
        let ids: Vec<CodeId> = doc.codes.iter().map(|c| c.id).collect();
        assert_eq!(ids.len(), 2, "should find both code blocks");
    }

    #[test]
    fn test_extract_code_ids_from_list_item() {
        let md = "- item with code:\n  ```rust\n  fn f() {}\n  ```\n";
        let doc = parser::new().parse(md);
        let ids: Vec<CodeId> = doc.codes.iter().map(|c| c.id).collect();
        assert_eq!(ids.len(), 1);
    }

    #[test]
    fn test_document_headings_include_nested_heading() {
        let md = "> # Nested heading\n> some text\n\n# Top heading\n";
        let doc = parser::new().parse(md);
        assert_eq!(doc.headings.len(), 2);
        assert_eq!(doc.headings[0].text, "Nested heading");
    }

    #[test]
    fn test_menu_page_up_down() {
        let codes: Vec<upmd_parser::nodes::Code> = (1..=20u32)
            .map(|id| upmd_parser::nodes::Code {
                id,
                ..Default::default()
            })
            .collect();
        let nav_keymap: keymap::DerivedConfig<Navigation> = toml::from_str("").unwrap();
        let mut menu = Menu::new(
            &codes,
            &[],
            crate::apps::theme::Theme::default(),
            nav_keymap,
        );
        menu.last_area.set(Rect::new(0, 0, 30, 12)); // 10 visible rows

        // Exercise the update() match arms that were previously no-ops.
        menu.model.state.select(Some(10));
        menu.update(Action::Navigation(Navigation::PageUp));
        assert_eq!(menu.model.state.selected(), Some(0), "page_up from middle");

        menu.model.state.select(Some(5));
        menu.update(Action::Navigation(Navigation::PageDown));
        assert_eq!(
            menu.model.state.selected(),
            Some(15),
            "page_down from middle"
        );

        menu.model.state.select(Some(18));
        menu.update(Action::Navigation(Navigation::PageDown));
        assert_eq!(
            menu.model.state.selected(),
            Some(19),
            "page_down near end clamps to last"
        );

        menu.model.state.select(Some(0));
        menu.update(Action::Navigation(Navigation::PageUp));
        assert_eq!(
            menu.model.state.selected(),
            Some(0),
            "page_up at first item stays"
        );

        // page_size fallback when last_area is zero.
        menu.last_area.set(Rect::new(0, 0, 0, 0));
        menu.model.state.select(Some(9));
        menu.update(Action::Navigation(Navigation::PageUp));
        assert_eq!(
            menu.model.state.selected(),
            Some(0),
            "page_size fallback to 10"
        );
    }
}
