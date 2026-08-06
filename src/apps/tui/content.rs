use std::collections::HashMap;

use crossterm::event::{Event, MouseEvent};
use ratatui::{layout::Rect, Frame};
use upmd_parser::{nodes::Code, CodeId, Codes, Document};
use upmd_runtime::{
    runtimes::tui::{Input, Output},
    Cmd, Component,
};

use crate::apps::{
    config::Config,
    navigation::Navigation,
    task::{Task, TaskStatus},
    theme::Theme,
    tui::{menu, preview},
};

/// The menu and rendered Markdown for the active document.
pub struct Content {
    menu: menu::Menu,
    preview: preview::Preview,
}

pub enum Action {
    Menu(menu::Action),
    Preview(preview::Action),
}

impl Content {
    pub fn new(
        document: Document,
        selected: Option<CodeId>,
        theme: &Theme,
        outputs: &HashMap<CodeId, Task>,
        config: &Config,
    ) -> Self {
        let Document {
            nodes,
            codes,
            headings,
            ..
        } = document;

        let mut menu = menu::Menu::new(
            &codes,
            &headings,
            theme.clone(),
            config.keymap.menu::<Navigation>(),
        );
        let mut preview = preview::Preview::new(
            nodes,
            codes,
            theme.clone(),
            outputs,
            config.tui.inline_max_lines(),
            config.keymap.preview::<preview::Action>(),
        );

        if let Some(id) = selected {
            menu.select_by_id(id);
            preview.select_code(id);
        }

        Self { menu, preview }
    }

    pub fn select_code(&mut self, id: CodeId) {
        self.menu.select_by_id(id);
        self.preview.select_code(id);
    }

    pub fn select_code_in_place(&mut self, id: CodeId) {
        self.menu.select_by_id(id);
        self.preview.select_code_in_place(id);
    }

    pub fn sync_from_menu(&mut self) {
        match self.menu.mode() {
            menu::MenuMode::CodeBlocks => {
                if let Some(id) = self.menu.selected() {
                    self.select_code(id);
                }
            }
            menu::MenuMode::Toc => {
                if let Some(index) = self.menu.selected_toc_idx() {
                    self.select_heading(index);
                }
            }
        }
    }

    pub fn toggle_toc(&mut self) {
        match self.menu.mode() {
            menu::MenuMode::CodeBlocks => {
                self.menu.set_mode(menu::MenuMode::Toc);
                if let Some(logical) = self.preview.selected_logical_line() {
                    let index = self.preview.heading_count_at_line(logical);
                    self.menu.select_by_heading_idx(index);
                }
            }
            menu::MenuMode::Toc => {
                self.menu.set_mode(menu::MenuMode::CodeBlocks);
                if let Some(id) = self.preview.selected_code_id() {
                    self.menu.select_by_id(id);
                }
            }
        }
    }

    pub fn sync_from_preview(&mut self) {
        match self.menu.mode() {
            menu::MenuMode::CodeBlocks => {
                if let Some(id) = self.preview.selected_code_id() {
                    self.menu.select_by_id(id);
                } else {
                    self.menu.deselect();
                }
            }
            menu::MenuMode::Toc => {
                if let Some(logical) = self.preview.selected_logical_line() {
                    let index = self.preview.heading_count_at_line(logical);
                    self.menu.select_by_heading_idx(index);
                }
            }
        }
    }

    pub fn select_search_match(&mut self, index: usize) {
        self.preview.select_search_match(index);
        self.sync_from_preview();
    }

    pub fn select_heading(&mut self, index: usize) {
        self.menu.select_by_heading_idx(index);
        self.preview.select_heading(index);
    }

    pub fn set_theme(&mut self, theme: &Theme) {
        self.menu.set_theme(theme);
        self.preview.set_theme(theme);
    }

    pub fn selected_code_id(&self) -> Option<CodeId> {
        self.menu.selected()
    }

    pub fn focused_code_id(&self) -> Option<CodeId> {
        self.menu
            .selected()
            .or_else(|| self.preview.selected_code_id())
    }

    pub fn codes(&self) -> &Codes {
        self.preview.codes()
    }

    pub fn code_by_id(&self, id: CodeId) -> Option<&Code> {
        self.preview.code_by_id(id)
    }

    pub fn set_code_statuses(&mut self, statuses: HashMap<CodeId, TaskStatus>) {
        self.menu.set_code_statuses(statuses);
    }

    pub fn tick(&mut self) {
        self.menu.tick();
        self.preview.tick();
    }

    pub fn rebuild(&mut self, outputs: &HashMap<CodeId, Task>) {
        self.preview.rebuild_view(outputs);
    }

    pub fn prefer_status_gutter_for(&self, id: CodeId) {
        self.preview.prefer_status_gutter_for(id);
    }

    pub fn width(&self, total_width: u16) -> u16 {
        self.menu.width(total_width)
    }

    pub fn is_toc(&self) -> bool {
        matches!(self.menu.mode(), menu::MenuMode::Toc)
    }

    pub fn adjust_toc_width(&mut self, delta: i16, total_width: u16) {
        self.menu.adjust_toc_width(delta, total_width);
    }

    pub fn inline_max_lines(&self) -> usize {
        self.preview.inline_max_lines()
    }

    pub fn set_inline_max_lines(&self, height: usize) {
        self.preview.set_inline_max_lines(height);
    }

    pub fn code_prefix_overhead(&self, id: CodeId) -> usize {
        self.preview.code_prefix_overhead(id)
    }

    pub fn fit_inline_pty_rows(&self, id: CodeId, viewport: usize) -> Option<usize> {
        self.preview.fit_inline_pty_rows(id, viewport)
    }

    pub fn code_id_at_mouse(&self, mouse: &MouseEvent) -> Option<CodeId> {
        self.preview.code_id_at_mouse(mouse)
    }

    pub fn mouse_to_pty_coords(
        &self,
        id: CodeId,
        mouse: &MouseEvent,
        pty_cols: u16,
        pty_rows: u16,
    ) -> Option<(u16, u16)> {
        self.preview
            .mouse_to_pty_coords(id, mouse, pty_cols, pty_rows)
    }

    pub fn action(&self, event: Event) -> Option<Action> {
        if let Some(action) = self.preview.action(event.clone()) {
            Some(Action::Preview(action))
        } else {
            self.menu.action(event).map(Action::Menu)
        }
    }

    pub fn update_menu(&mut self, action: menu::Action) -> Option<Cmd<menu::Action>> {
        self.menu.update(action)
    }

    pub fn update_preview(&mut self, action: preview::Action) {
        self.preview.update(action);
    }

    pub fn take_copy_result(&self) -> Option<bool> {
        self.preview.take_copy_result()
    }

    pub fn search(&mut self, term: &str) -> Vec<usize> {
        self.preview.search(term)
    }

    pub fn render_preview(&self, frame: &mut Frame, area: Rect) {
        self.preview.render(frame, area);
    }

    pub fn render_menu(&self, frame: &mut Frame, area: Rect) {
        self.menu.render(frame, area);
    }
}
