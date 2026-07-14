//! Main TUI component and application-level event coordination.

use crate::apps::config::{self, Config as AppConfig};
use crate::apps::exec;
use crate::apps::navigation::Navigation;
use crate::apps::tui;
use crate::apps::tui::{
    confirm, file_picker, layout, menu, preview, tasks::Tasks, themes, Shortcut,
};
use crate::utils::key_to_bytes;
use color_eyre::Result;
use keymap::{DerivedConfig, KeyMap};
use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::Style,
    text::{Line, Span},
    widgets::{Block, Paragraph},
    Frame,
};
use std::{cell::RefCell, collections::HashMap, path::PathBuf, thread, time::Duration};
use upmd_parser::Parser;
use upmd_parser::{resolve_code_block, CodeId};
use upmd_runtime::{
    runtimes::tui::{Input, Output},
    Cmd, Component,
};

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Home,
    Input,
    Output,
    Confirm,
    Help,
    Envs,
    Search,
    Goto,
    Themes,
    FilePicker,
}

/// Main TUI application component.
///
/// Owns the menu, preview, tasks, and overlay components. Routes events,
/// manages execution state, and renders the full TUI layout.
pub struct App {
    config: AppConfig,
    menu: menu::Menu,
    preview: preview::Preview,
    tasks: Tasks,
    mode: Mode,
    zen: bool,
    theme: crate::apps::theme::Theme,
    auto_input_paused: bool,
    layout: RefCell<layout::Area>,
    keymap: DerivedConfig<Action>,
    confirm: Option<confirm::Confirm>,
    envs: tui::envs::EnvVars,
    help: Option<tui::help::Help>,
    search: Option<tui::search::Search>,
    goto: Option<tui::goto::Goto>,
    file_picker: Option<file_picker::FilePicker>,
    /// When true, picker cancel quits the app (startup mode).
    /// When false, picker cancel returns to the active document.
    file_picker_started_app: bool,
    /// Browse root preserved across file opens.
    /// Selecting a/b/c.md does not re-root the next picker to a/b/.
    file_picker_root: Option<PathBuf>,
    themes: Option<themes::ThemeSelector>,
    output: tui::output::Output,
    /// Advance to next code block after each completes (--all).
    auto_advance: Vec<CodeId>,
    /// Working directory captured from the most recent shell block that
    /// emitted `Stream::Cwd`. Used as the initial cwd for the next block.
    last_cwd: Option<PathBuf>,
    started: bool,
    /// Cached default footer right text (rebuilt when the theme changes).
    footer_right_text: Line<'static>,
    /// Transient flash notification at the bottom right.
    notification: Option<tui::notification::FlashMessage>,
}

#[derive(Clone, KeyMap, Debug, PartialEq, Eq, Hash)]
pub enum Action {
    /// Execute code block
    #[key("enter", symbol = "↵", help = "run")]
    Execute,
    /// View environment variables
    #[key("e", help = "envs")]
    Envs,
    /// Search text
    #[key("/", help = "search")]
    Search,
    /// Go to block by ID
    #[key("ctrl-g", help = "goto")]
    Goto,
    /// Open Markdown file picker
    #[key("f", help = "files")]
    OpenFilePicker,
    /// Switch theme
    #[key("t", help = "theme")]
    SwitchTheme,
    /// Toggle transparency
    #[key("ctrl-t", help = "transparent")]
    ToggleTransparency,
    /// Toggle zen mode
    #[key("z", help = "zen")]
    ToggleZen,
    /// Decrease TOC panel width
    #[key("<", help = "toc narrower")]
    DecreaseTocWidth,
    /// Increase TOC panel width
    #[key(">", help = "toc wider")]
    IncreaseTocWidth,
    /// View output
    #[key("o", help = "output")]
    ViewOutput,
    /// Enter input mode for the selected running block
    #[key("i", help = "input")]
    Input,
    /// Exit input mode
    #[key("ctrl-o", help = "exit input")]
    ExitInput,
    /// Reload the input file, clearing all output
    #[key("ctrl-r")]
    Reload,
    /// Paste clipboard content into the active PTY.
    #[key("ctrl-v", help = "paste")]
    Paste,
    /// Show/hide help
    #[key("?", help = "help")]
    Help,
    /// Quit
    #[key("q", "ctrl-c", help = "quit")]
    Quit,
}

impl App {
    /// Creates the app in file-picker mode before a document is loaded.
    pub fn from_file_picker(
        root: PathBuf,
        files: Vec<crate::markdown_files::MarkdownFile>,
        config: AppConfig,
    ) -> Self {
        let theme = config.theme.clone();
        let mut app = Self::new(upmd_parser::new().parse(""), config.clone());
        app.file_picker = Some(file_picker::FilePicker::new(
            files,
            theme,
            config.keymap.file_picker(),
        ));
        app.file_picker_root = Some(root);
        app.file_picker_started_app = true;
        app.mode = Mode::FilePicker;
        app
    }

    /// Creates the app from a parsed document.
    pub fn new(doc: upmd_parser::Document, config: AppConfig) -> Self {
        let upmd_parser::Document {
            nodes,
            codes,
            headings,
            ..
        } = doc;
        let theme = config.theme.clone();
        let tasks = Tasks::new();
        let auto_advance = if config.all {
            codes.iter().map(|c| c.id).collect()
        } else {
            Vec::new()
        };
        let selected = config
            .block
            .as_deref()
            .and_then(|spec| resolve_code_block(&codes, spec).first().copied());

        let mut menu = menu::Menu::new(
            &codes,
            &headings,
            theme.clone(),
            config.keymap.menu::<crate::apps::navigation::Navigation>(),
        );
        if let Some(id) = selected {
            menu.select_by_id(id);
        }

        let mut preview = preview::Preview::new(
            nodes,
            codes,
            theme.clone(),
            tasks.buffers(),
            config.tui.inline_max_lines(),
            config.keymap.preview::<preview::Action>(),
        );
        if let Some(id) = selected {
            preview.select_code(id);
        }

        App {
            config: config.clone(),
            preview,
            tasks,
            mode: Mode::Home,
            zen: false,
            theme: theme.clone(),
            auto_input_paused: false,
            layout: RefCell::new(layout::Area::default()),
            keymap: config.keymap.home(),
            confirm: None,
            envs: tui::envs::EnvVars::new(
                std::env::vars().collect(),
                theme.clone(),
                config.keymap.envs(),
                config.keymap.envs_edit(),
                config.keymap.menu(),
                config.keymap.search.clone(),
            ),
            help: None,
            search: None,
            goto: None,
            file_picker: None,
            file_picker_started_app: false,
            file_picker_root: None,
            menu,
            themes: None,
            output: tui::output::Output::new(config.keymap.output()),
            auto_advance,
            last_cwd: None,
            notification: None,
            started: false,
            footer_right_text: Self::build_footer_right_text(&theme),
        }
    }

    fn build_footer_right_text(theme: &crate::apps::theme::Theme) -> Line<'static> {
        Line::from(vec![
            Span::styled(config::APP_NAME, theme.active_fg_style()),
            Span::raw(" "),
            Span::styled(config::APP_VERSION, theme.muted_style()),
        ])
    }

    /// Synchronizes input mode with the currently selected code block.
    ///
    /// This method keeps input mode alive once entered, but never enters it
    /// from Home. New auto-entry happens from stream/tick paths when a selected
    /// process becomes ready for input.
    fn sync_input_mode(&mut self) {
        let is_input_mode = self.menu.selected().is_some_and(|id| {
            self.tasks.contains(id) && !self.tasks.is_done(id) && self.mode == Mode::Input
        });

        let new = match self.mode {
            Mode::Home | Mode::Input if is_input_mode => Mode::Input,
            Mode::Home | Mode::Input => Mode::Home,
            _ => return,
        };
        if self.mode != new {
            self.mode = new;
        }
    }

    fn pty_size_for_code(&self, id: CodeId) -> crate::pty::process::Size {
        self.layout
            .borrow()
            .pty_size(self.preview.code_prefix_overhead(id) as u16)
    }

    fn resize_tasks_for_preview(&mut self) {
        if let Some(id) = self.menu.selected() {
            let fitted = self.inline_pty_size_for_code(id);
            self.tasks.resize_task(id, fitted.width, fitted.height);
        }
    }

    /// Sizes the PTY to the remaining preview rows below the selected block's
    /// visible source lines, used when the process enters alternate screen.
    /// Normal inline output is capped by `inline_max_lines` and passes through.
    fn inline_pty_size_for_code(&self, id: CodeId) -> crate::pty::process::Size {
        let base = self.pty_size_for_code(id);
        let Some(task) = self.tasks.get(id) else {
            return base;
        };
        if !task.parser.is_alternate_screen() {
            return base;
        }

        let viewport = self
            .layout
            .borrow()
            .preview
            .height
            .saturating_sub(config::BORDER_HEIGHT as u16) as usize;
        let Some((start, end)) = self.preview.source_visual_extent(id) else {
            return base;
        };
        let source_rows = end.saturating_sub(start).saturating_add(1);
        let offset = self.preview.visual_offset();
        let (rows, new_offset) = preview::inline_pty_rows(viewport, end, source_rows, offset);
        if new_offset != offset {
            self.preview.set_visual_offset(new_offset);
        }
        crate::pty::process::Size::from((base.width, rows as u16))
    }

    fn execute(&mut self, id: CodeId) -> Option<Cmd<Msg>> {
        let code = self.preview.code_by_id(id)?;
        let size = self.pty_size_for_code(id);
        let envs = self.envs.data();

        if let Some(rx) = self.tasks.run(
            code,
            size,
            envs,
            self.config.capture_state,
            &self.config.binaries,
            self.config
                .working_dir
                .clone()
                .or_else(|| self.last_cwd.clone()),
        ) {
            self.sync_menu_running_state();
            Some(exec::stream_rx(id, rx, Msg::StreamUpdate))
        } else {
            self.preview.prefer_status_gutter_for(id);
            self.preview.rebuild_view(self.tasks.buffers());
            self.sync_menu_running_state();
            // The block failed to start (e.g. binary not found). Don't
            // leave it in the auto-advance queue or --all will stall.
            self.auto_advance.retain(|&i| i != id);
            self.run_next_pending()
        }
    }

    fn sync_menu_running_state(&mut self) {
        self.menu.set_code_statuses(self.tasks.task_statuses());
    }

    fn run_next_pending(&mut self) -> Option<Cmd<Msg>> {
        let mut next = None;
        for &id in &self.auto_advance {
            if self.tasks.get(id).is_none_or(|b| b.done()) {
                next = Some(id);
                break;
            }
        }
        if let Some(id) = next {
            self.auto_advance.retain(|&i| i != id);
            self.navigate_to_code(id);
            if self.config.yes {
                self.execute(id)
            } else {
                None
            }
        } else {
            None
        }
    }
    /// Sends raw text to the currently selected PTY as if the user typed it.
    fn send_text_to_pty(&mut self, text: &str) {
        if let Some(id) = self.menu.selected() {
            self.tasks.send_text(id, text);
        }
    }

    /// Forwards a keyboard event to the currently selected PTY process.
    ///
    /// In output mode, resets scrollback so the user sees fresh output.
    /// Converts the event to raw bytes and sends them as stdin input.
    fn forward_to_pty(&mut self, event: crossterm::event::Event) {
        if let Some(id) = self.menu.selected() {
            if let crossterm::event::Event::Key(key) = event {
                if self.mode == Mode::Output {
                    self.tasks.reset_scroll(id);
                }
                if let Some(bytes) = key_to_bytes(key) {
                    self.tasks.send_input(id, &bytes);
                }
            }
        }
    }

    /// Forwards a mouse event to a PTY application that requested SGR mouse input.
    ///
    /// Plain commands do not enable mouse reporting, so they keep using inline
    /// scroll. Full-screen TUIs such as Neovim typically enable `?1006` SGR
    /// mouse mode, and receive wheel/click/drag events through stdin.
    fn forward_mouse_to_pty(&mut self, mouse: &crossterm::event::MouseEvent) -> bool {
        let Some(id) = self.menu.selected() else {
            return false;
        };
        let Some(buf) = self.tasks.get(id) else {
            return false;
        };
        if buf.done || !buf.parser.sgr_mouse_enabled() {
            return false;
        }

        // Forward only clicks on the selected code block. Coordinate math
        // accounts for scroll offset and blockquote prefix overhead.
        let (pty_rows, pty_cols) = buf.parser.screen().size();
        let Some((col, row)) = self
            .preview
            .mouse_to_pty_coords(id, mouse, pty_cols, pty_rows)
        else {
            return false;
        };

        let Some(seq) = upmd_pty::mouse::encode_sgr_mouse(mouse, col, row) else {
            return false;
        };

        self.tasks.send_input(id, seq.as_bytes());
        true
    }

    /// Scrolls the inline output of the selected code block up or down.
    ///
    /// Positive delta scrolls up (showing earlier output), negative scrolls
    /// down (showing later output). Rebuilds the preview after scrolling.
    fn scroll_inline(&mut self, delta: isize) {
        if let Some(id) = self.menu.selected() {
            if let Some(buf) = self.tasks.get_mut(id) {
                if delta > 0 {
                    buf.scroll_inline_up(self.preview.inline_max_lines());
                } else {
                    buf.scroll_inline_down(self.preview.inline_max_lines());
                }
                self.preview.rebuild_view(self.tasks.buffers());
            }
        }
    }

    /// Updates input mode after clicking a different code block.
    ///
    /// Clicking another running block keeps input mode active for that new
    /// block. Clicking a completed or empty block exits input mode before the
    /// selection is changed.
    fn keep_input_for_running_click_target(&mut self, id: CodeId) {
        if self.mode == Mode::Input
            && self.menu.selected() != Some(id)
            && !self.tasks.get(id).is_some_and(|b| b.running())
        {
            self.mode = Mode::Home;
        }
    }

    /// Re-enters input mode when clicking a running block after pausing auto-entry.
    fn enter_input_for_running_click_target(&mut self, id: CodeId) {
        if self.mode == Mode::Home
            && self.auto_input_paused
            && self.tasks.get(id).is_some_and(|b| b.running())
        {
            self.auto_input_paused = false;
            self.mode = Mode::Input;
        }
    }

    /// Navigates the preview to a code block with smart-snap behavior.
    ///
    /// If the block is already visible in the viewport, only the selection
    /// highlight is updated (no scroll). If it is off-screen, the viewport
    /// snaps to the block's start line.
    fn navigate_to_code(&mut self, id: CodeId) {
        self.preview.select_code(id);
        self.menu.select_by_id(id);
    }

    /// Navigates the preview to a TOC heading with smart-snap behavior.
    ///
    /// If the heading is already visible in the viewport, only the selection
    /// highlight is updated (no scroll). If it is off-screen, the viewport
    /// scrolls to the heading.
    fn navigate_to_heading(&mut self, heading_idx: usize) {
        self.preview.select_heading(heading_idx);
        self.menu.select_by_heading_idx(heading_idx);
    }
}

/// Messages handled by the main TUI component's event loop.
#[derive(Clone, Debug)]
pub enum Msg {
    Menu(menu::Action),
    Confirm(confirm::Action),
    Search(tui::search::Action),
    Goto(tui::goto::Action),
    Help(tui::help::Action),
    Envs(tui::envs::Action),
    Themes(themes::Action),
    FilePicker(file_picker::Action),
    OutputAction(tui::output::Action),
    StreamUpdate(CodeId, crate::pty::stream::Stream),
    Notify(tui::notification::FlashMessage),
    Tick,
    Event(crossterm::event::Event),
}

impl crate::RunApp for App {
    fn from_input(input: &str, config: AppConfig) -> Self {
        let doc = tracing::info_span!("parse").in_scope(|| upmd_parser::new().parse(input));
        tracing::info_span!("build").in_scope(|| Self::new(doc, config))
    }

    fn from_picker(
        root: PathBuf,
        files: Vec<crate::markdown_files::MarkdownFile>,
        config: AppConfig,
    ) -> Self {
        Self::from_file_picker(root, files, config)
    }

    fn run(self) -> Result<()> {
        Ok(upmd_runtime::runtimes::tui::run(self)?)
    }
}

impl Component for App {
    type Msg = Msg;

    fn create(&mut self) -> Option<Cmd<Msg>> {
        let tick_rate = self.config.tick_rate;
        Some(Cmd::stream(move |tx| loop {
            thread::sleep(Duration::from_millis(tick_rate));
            if tx.send(Msg::Tick).is_err() {
                break;
            }
        }))
    }

    fn update(&mut self, msg: Msg) -> Option<Cmd<Msg>> {
        match msg {
            Msg::Event(event) => self.handle_event(event),
            Msg::Menu(action) => self.handle_menu_msg(action),
            Msg::Confirm(action) => self.handle_confirm_msg(action),
            Msg::FilePicker(action) => self.handle_file_picker_msg(action),
            Msg::Search(action) => self.handle_search_msg(action),
            Msg::Goto(action) => self.handle_goto_msg(action),
            Msg::Help(action) => self.handle_help_msg(action),
            Msg::Envs(action) => self.handle_envs_msg(action),
            Msg::Themes(action) => self.handle_themes_msg(action),
            Msg::OutputAction(action) => self.handle_output_action(action),
            Msg::StreamUpdate(id, stream) => self.handle_stream_update(id, stream),
            Msg::Notify(flash) => {
                self.notification = Some(flash);
                None
            }
            Msg::Tick => {
                // Kick off auto mode on first tick.
                if !self.started {
                    self.started = true;
                    if self.config.block.is_some() && self.config.yes {
                        if let Some(id) = self.menu.selected() {
                            return self.execute(id);
                        }
                    } else if !self.auto_advance.is_empty() {
                        return self.run_next_pending();
                    }
                }
                if self.tasks.is_dirty() {
                    self.preview.rebuild_view(self.tasks.buffers());
                    self.tasks.clear_dirty();
                }
                self.sync_menu_running_state();
                self.sync_input_mode();
                self.menu.tick();
                if let Some(ref mut goto) = self.goto {
                    goto.tick();
                }
                self.preview.tick();
                self.output.tick();

                // Clear expired flash notification.
                if self
                    .notification
                    .as_ref()
                    .is_some_and(|n| n.is_expired(std::time::Instant::now()))
                {
                    self.notification = None;
                }
                None
            }
        }
    }
}

impl App {
    fn menu_width(&self, total_width: u16) -> u16 {
        if self.zen {
            0
        } else {
            self.menu.width(total_width)
        }
    }

    fn handle_action(&mut self, action: Action) -> Option<Cmd<Msg>> {
        match action {
            Action::Execute => {
                if let Some(id) = self
                    .menu
                    .selected()
                    .or_else(|| self.preview.selected_code_id())
                {
                    if self.tasks.contains(id) {
                        // Confirm re-run (will wipe existing output).
                        self.confirm = Some(confirm::Confirm::rerun(
                            id,
                            self.theme.clone(),
                            self.config.keymap.confirm(),
                        ));
                        self.mode = Mode::Confirm;
                        None
                    } else {
                        self.auto_input_paused = false;
                        self.execute(id)
                    }
                } else {
                    None
                }
            }
            Action::Quit => {
                self.confirm = Some(confirm::Confirm::quit(
                    self.theme.clone(),
                    self.config.keymap.confirm(),
                ));
                self.mode = Mode::Confirm;
                None
            }
            Action::Help => {
                self.help = Some(tui::help::Help::new(
                    self.theme.clone(),
                    self.config.keymap.help(),
                    self.help_keymap_items(),
                ));
                self.mode = Mode::Help;
                None
            }
            Action::Envs => {
                self.mode = Mode::Envs;
                None
            }
            Action::Search => {
                let search =
                    tui::search::Search::new(self.theme.clone(), self.config.keymap.search());
                self.search = Some(search);
                self.mode = Mode::Search;
                None
            }
            Action::OpenFilePicker => self.open_file_picker_for_current_dir(),
            Action::Goto => {
                use crate::apps::tui::goto::StatusKind;
                let mut all_blocks = Vec::new();
                let mut previews = HashMap::new();
                for c in self.preview.codes() {
                    let kind = match self.tasks.get(c.id) {
                        Some(buf) if !buf.done() => StatusKind::Running,
                        Some(buf) if buf.exit_code == Some(0) => StatusKind::Success,
                        Some(_) => StatusKind::Error,
                        None => StatusKind::None,
                    };
                    let label = if c.name.is_empty() {
                        format!("{}. {}", c.id, c.language)
                    } else {
                        format!("{}. {}", c.id, c.name)
                    };
                    all_blocks.push((c.id, label, kind));
                    previews.insert(c.id, (c.language.clone(), c.content.clone()));
                }
                self.goto = Some(tui::goto::Goto::new(
                    self.theme.clone(),
                    self.config.keymap.goto(),
                    all_blocks,
                    previews,
                ));
                self.mode = Mode::Goto;
                None
            }
            Action::SwitchTheme => {
                self.themes = Some(themes::ThemeSelector::new(
                    self.theme.clone(),
                    self.config.transparent,
                    self.config.keymap.themes(),
                    self.config.keymap.menu(),
                    self.config.keymap.search.clone(),
                ));
                self.mode = Mode::Themes;
                None
            }
            Action::ViewOutput => {
                if let Some(id) = self.menu.selected() {
                    if self.tasks.contains(id) {
                        self.mode = Mode::Output;
                        let area = self.layout.borrow().last_area;
                        let cols = area.width.max(config::PTY_DEFAULT_COLS);
                        let rows = area
                            .height
                            .saturating_sub(config::OUTPUT_FOOTER_HEIGHT)
                            .max(config::PTY_DEFAULT_ROWS);
                        self.tasks.resize(cols, rows);
                    }
                }
                None
            }
            Action::Input => {
                if let Some(id) = self.menu.selected() {
                    if self.tasks.contains(id) {
                        self.mode = Mode::Input;
                        self.auto_input_paused = false;
                    }
                }
                None
            }
            Action::ExitInput => {
                self.mode = Mode::Home;
                self.auto_input_paused = true;
                None
            }
            Action::Reload => {
                self.confirm = Some(confirm::Confirm::reload(
                    self.theme.clone(),
                    self.config.keymap.confirm(),
                ));
                self.mode = Mode::Confirm;
                None
            }
            Action::ToggleZen => {
                self.zen = !self.zen;
                let total = self.layout.borrow().total_width();
                self.layout
                    .borrow_mut()
                    .update_menu_width(self.menu_width(total));
                None
            }
            Action::DecreaseTocWidth | Action::IncreaseTocWidth => {
                if matches!(self.menu.mode(), menu::MenuMode::Toc) {
                    let delta = if matches!(action, Action::DecreaseTocWidth) {
                        -2
                    } else {
                        2
                    };
                    let total = self.layout.borrow().total_width();
                    self.menu.adjust_toc_width(delta, total);
                    self.layout
                        .borrow_mut()
                        .update_menu_width(self.menu_width(total));
                }
                None
            }
            Action::ToggleTransparency => self.toggle_transparency(),
            Action::Paste => {
                if let Some(text) = crate::utils::clipboard_paste() {
                    self.send_text_to_pty(&text);
                }
                None
            }
        }
    }

    fn handle_event(&mut self, event: crossterm::event::Event) -> Option<Cmd<Msg>> {
        if let crossterm::event::Event::Resize(cols, rows) = event {
            // Re-compute TUI layout dimensions for the new terminal size
            let area = Rect::new(0, 0, cols, rows);
            let mut layout = self.layout.borrow_mut();
            layout.update(area, self.menu_width(area.width));
            drop(layout);

            // Rebuild visual lines for new width BEFORE PTY sizing.
            self.preview.set_inline_max_lines(rows as usize);
            self.preview.rebuild_view(self.tasks.buffers());

            if self.mode == Mode::Output {
                let out_rows = rows
                    .saturating_sub(config::OUTPUT_FOOTER_HEIGHT)
                    .max(config::PTY_DEFAULT_ROWS);
                let out_cols = cols.max(config::PTY_DEFAULT_COLS);
                self.tasks.resize(out_cols, out_rows);
            } else {
                self.resize_tasks_for_preview();
            }

            return None;
        }

        match self.mode {
            Mode::Home | Mode::Input => self.handle_home_event(event),
            Mode::Output => self.handle_output_event(event),
            Mode::Envs => {
                let action = self.envs.action(event)?;
                let cmd = self.envs.update(action)?;
                Some(cmd.map(Msg::Envs))
            }
            Mode::Goto => {
                let action = self.goto.as_ref()?.action(event)?;
                let cmd = self.goto.as_mut()?.update(action)?;
                Some(cmd.map(Msg::Goto))
            }
            Mode::FilePicker => {
                let action = self.file_picker.as_ref()?.action(event)?;
                let cmd = self.file_picker.as_mut()?.update(action)?;
                Some(cmd.map(Msg::FilePicker))
            }
            _ => None,
        }
    }

    fn handle_home_event(&mut self, event: crossterm::event::Event) -> Option<Cmd<Msg>> {
        if self.mode == Mode::Input {
            let input_active = self
                .menu
                .selected()
                .and_then(|id| self.tasks.get(id))
                .is_some_and(|b| b.running());

            if let crossterm::event::Event::Key(key) = event {
                if let Some(action @ (Action::ExitInput | Action::Paste)) = self.keymap.get(&key) {
                    return self.handle_action(action.clone());
                }
            }

            // Mouse-aware PTY apps get SGR mouse events. Otherwise scroll
            // stays inline/input-mode and clicks outside the block exit input mode.
            if let crossterm::event::Event::Mouse(mouse) = &event {
                use crossterm::event::MouseEventKind;

                let inside_preview =
                    crate::utils::mouse_in_area(mouse, self.layout.borrow().preview);
                if input_active && inside_preview && self.forward_mouse_to_pty(mouse) {
                    return None;
                }
                let clicked_code = self.preview.code_id_at_mouse(mouse);
                match mouse.kind {
                    MouseEventKind::ScrollUp => {
                        self.scroll_inline(1);
                        return None;
                    }
                    MouseEventKind::ScrollDown => {
                        self.scroll_inline(-1);
                        return None;
                    }
                    MouseEventKind::Up(_) | MouseEventKind::Down(_)
                        if clicked_code != self.menu.selected() =>
                    {
                        self.mode = Mode::Home;
                        self.auto_input_paused = true;
                    }
                    // Stay in input when the selected running block is clicked again.
                    MouseEventKind::Up(_) | MouseEventKind::Down(_) if input_active => {
                        return None;
                    }
                    _ => {}
                }
            }

            // Non-mouse events with active input: forward to PTY.
            if input_active && !matches!(&event, crossterm::event::Event::Mouse(_)) {
                self.forward_to_pty(event);
                return None;
            }
        }

        if let Some(preview_action) = self.preview.action(event.clone()) {
            return self.handle_preview_msg(preview_action);
        }

        if let Some(menu_msg) = self.menu.action(event.clone()) {
            return self.handle_menu_msg(menu_msg);
        }

        if let crossterm::event::Event::Key(key) = event {
            if let Some(action) = self.keymap.get(&key) {
                return self.handle_action(action.clone());
            }
        }

        None
    }

    fn handle_output_event(&mut self, event: crossterm::event::Event) -> Option<Cmd<Msg>> {
        let id = self.menu.selected();

        if let Some(buf) = id.and_then(|id| self.tasks.get(id)) {
            self.output.update_state(buf);
        }

        if let Some(action) = self.output.action(event.clone()) {
            if let Some(cmd) = self.output.update(action) {
                return Some(cmd.map(Msg::OutputAction));
            }
        }

        // Forward mouse to PTY when the running block has SGR mouse enabled.
        // Otherwise handle as local scrollback / selection below.
        if let crossterm::event::Event::Mouse(mouse) = &event {
            if let Some(id) = id {
                let should_forward = self
                    .tasks
                    .get(id)
                    .is_some_and(|buf| buf.parser.sgr_mouse_enabled());
                if should_forward {
                    let (pty_cols, pty_rows) = self
                        .tasks
                        .get(id)
                        .map(|buf| buf.parser.screen().size())
                        .unwrap_or((0, 0));
                    let (col, row) = self.output.mouse_to_pty_coords(mouse, pty_cols, pty_rows);
                    if let Some(seq) = upmd_pty::mouse::encode_sgr_mouse(mouse, col, row) {
                        self.tasks.send_input(id, seq.as_bytes());
                        return None;
                    }
                }
            }
        }

        // Not forwarded → handle as local scrollback / selection.
        if let crossterm::event::Event::Mouse(mouse) = &event {
            if let Some(buf) = id.and_then(|id| self.tasks.get_mut(id)) {
                if self.output.handle_mouse_event(*mouse, buf) {
                    return Some(Cmd::msg(Msg::Notify(tui::notification::success("Copied"))));
                }
            }
        }
        self.forward_to_pty(event);
        None
    }

    fn handle_output_action(&mut self, action: tui::output::Action) -> Option<Cmd<Msg>> {
        match action {
            tui::output::Action::Back => {
                self.mode = Mode::Home;
                self.resize_tasks_for_preview();
            }
            tui::output::Action::BackIfDone => {
                if let Some(id) = self.menu.selected() {
                    if let Some(buf) = self.tasks.get(id) {
                        if buf.done() {
                            self.mode = Mode::Home;
                            self.resize_tasks_for_preview();
                        }
                    }
                }
            }
            tui::output::Action::Copy => {
                if let Some(id) = self.menu.selected() {
                    if let Some(buf) = self.tasks.get(id) {
                        if buf.done() && crate::utils::clipboard_copy(&buf.parser.contents_plain())
                        {
                            return Some(Cmd::msg(Msg::Notify(tui::notification::success(
                                "Copied",
                            ))));
                        }
                    }
                }
            }
            tui::output::Action::Paste => {
                if let Some(text) = crate::utils::clipboard_paste() {
                    self.send_text_to_pty(&text);
                }
            }
        }
        None
    }

    /// Applies a new theme to all UI components and rebuilds the preview.
    /// Skips expensive work when the theme name hasn't changed.
    fn apply_theme(&mut self, theme: crate::apps::theme::Theme) {
        if self.theme.name() == theme.name() {
            return;
        }
        self.footer_right_text = Self::build_footer_right_text(&theme);
        self.theme = theme.clone();
        self.preview.set_theme(theme.clone());
        self.menu.set_theme(theme.clone());
        self.envs.set_theme(theme);
        self.preview.rebuild_view(self.tasks.buffers());
    }

    /// Toggles the transparency setting, rebuilds the theme, and persists the preference.
    fn toggle_transparency(&mut self) -> Option<Cmd<Msg>> {
        self.config.transparent = !self.config.transparent;
        let theme = crate::apps::theme::Theme::new(self.theme.name(), self.config.transparent);
        self.apply_theme(theme);
        let transparent = self.config.transparent;
        Some(Cmd::stream(move |tx| {
            let flash = match config::UserConfig::update(|cfg| cfg.transparent = Some(transparent))
            {
                Ok(()) => {
                    tracing::info!("Saved transparency preference");
                    tui::notification::success("Transparency saved")
                }
                Err(e) => {
                    tracing::warn!("Failed to save transparency preference: {e}");
                    tui::notification::error("Failed to save transparency")
                }
            };
            let _ = tx.send(Msg::Notify(flash));
        }))
    }

    /// Saves the selected theme to the user config file on a background task.
    fn save_theme_preference(&self, theme: crate::apps::theme::Theme) -> Cmd<Msg> {
        let name = theme.name().to_string();
        Cmd::stream(move |tx| {
            let flash = match config::UserConfig::update(|cfg| cfg.theme = Some(name.clone())) {
                Ok(()) => {
                    tracing::info!("Saved theme preference: {name}");
                    tui::notification::success("Theme saved")
                }
                Err(e) => {
                    tracing::warn!("Failed to save theme preference: {e}");
                    tui::notification::error("Failed to save theme")
                }
            };
            let _ = tx.send(Msg::Notify(flash));
        })
    }

    fn handle_menu_msg(&mut self, msg: menu::Action) -> Option<Cmd<Msg>> {
        let cmd = self.menu.update(msg.clone());

        match msg {
            menu::Action::Click(id) => {
                self.keep_input_for_running_click_target(id);
                self.navigate_to_code(id);
                self.enter_input_for_running_click_target(id);
                self.sync_input_mode();
                return None;
            }
            menu::Action::TocClick(heading_idx) => {
                self.navigate_to_heading(heading_idx);
                self.sync_input_mode();
                return None;
            }
            menu::Action::Navigation(_) => {}
        }

        // Sync preview to menu's new selection after keyboard navigation.
        match self.menu.mode() {
            menu::MenuMode::CodeBlocks => {
                if let Some(id) = self.menu.selected() {
                    self.navigate_to_code(id);
                }
            }
            menu::MenuMode::Toc => {
                if let Some(idx) = self.menu.selected_toc_idx() {
                    self.navigate_to_heading(idx);
                }
            }
        }

        self.sync_input_mode();
        cmd.map(|c| c.map(Msg::Menu))
    }
    fn handle_preview_msg(&mut self, action: preview::Action) -> Option<Cmd<Msg>> {
        match action {
            preview::Action::ToggleToc => {
                match self.menu.mode() {
                    menu::MenuMode::CodeBlocks => {
                        self.menu.set_mode(menu::MenuMode::Toc);
                        if let Some(logical) = self.preview.selected_logical_line() {
                            let heading_idx = self.preview.heading_count_at_line(logical);
                            self.menu.select_by_heading_idx(heading_idx);
                        }
                    }
                    menu::MenuMode::Toc => {
                        self.menu.set_mode(menu::MenuMode::CodeBlocks);
                        if let Some(id) = self.preview.selected_code_id() {
                            self.menu.select_by_id(id);
                        }
                    }
                }
                self.sync_input_mode();
                return None;
            }
            preview::Action::SelectCodeBlock(id) => {
                self.keep_input_for_running_click_target(id);
                self.preview.select_code(id);
                if let Some(id) = self.preview.selected_code_id() {
                    self.menu.select_by_id(id);
                }
                self.enter_input_for_running_click_target(id);
                self.sync_input_mode();
                let copied = self.preview.take_copy_result();
                if copied == Some(true) {
                    self.notify_success("Copied");
                } else if copied == Some(false) {
                    self.notify_error("Failed to copy");
                }
                return None;
            }
            _ => {
                // Non-code click (heading, paragraph): always unfocus.
                if self.mode == Mode::Input {
                    self.mode = Mode::Home;
                }
                self.preview.update(action);
                if let Some(ok) = self.preview.take_copy_result() {
                    if ok {
                        self.notify_success("Copied");
                    } else {
                        self.notify_error("Failed to copy");
                    }
                }
            }
        }

        // Sync menu selection to whatever is now visible in preview
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
                    let heading_idx = self.preview.heading_count_at_line(logical);
                    self.menu.select_by_heading_idx(heading_idx);
                }
            }
        }
        self.sync_input_mode();
        None
    }

    fn handle_confirm_msg(&mut self, action: confirm::Action) -> Option<Cmd<Msg>> {
        let cmd = self.confirm.as_mut().and_then(|c| c.update(action));
        match action {
            confirm::Action::Confirmed(confirm::ConfirmAction::Quit) => Some(Cmd::quit()),
            confirm::Action::Confirmed(confirm::ConfirmAction::ReloadFile) => {
                self.confirm = None;
                self.mode = Mode::Home;
                self.reload()
            }
            confirm::Action::Confirmed(confirm::ConfirmAction::ReRun(id)) => {
                self.confirm = None;
                self.mode = Mode::Home;
                self.auto_input_paused = false;
                self.execute(id)
            }
            confirm::Action::Cancelled => {
                self.confirm = None;
                self.mode = Mode::Home;
                None
            }
            _ => cmd.map(|c| c.map(Msg::Confirm)),
        }
    }

    fn handle_search_msg(&mut self, action: tui::search::Action) -> Option<Cmd<Msg>> {
        let search = self.search.as_mut()?;
        let cmd = search.update(action);

        if cmd.is_some() {
            match action {
                tui::search::Action::Quit => {
                    self.preview.set_search_term("");
                    self.search = None;
                    self.mode = Mode::Home;
                }
                tui::search::Action::Select => {
                    self.search = None;
                    self.mode = Mode::Home;
                }
                _ => {}
            }
            return None;
        }

        self.preview.set_search_term(search.term());
        let result = self.preview.matches(search.term());
        if let Some(index) = search.search(&result) {
            self.preview.select_line(*index);

            if let Some(code) = self.preview.selected_code() {
                if let Some(id) = code.code_id {
                    self.menu.select_by_id(id);
                }
            } else {
                self.menu.deselect();
            }
        }

        None
    }
    /// Handles picker actions that change application-level state.
    /// The picker owns navigation; the app owns file loading and cancellation.
    fn handle_file_picker_msg(&mut self, action: file_picker::Action) -> Option<Cmd<Msg>> {
        let cmd = self.file_picker.as_mut()?.update(action);
        cmd.as_ref()?;

        match action {
            file_picker::Action::Select => {
                let path = self.file_picker.as_ref()?.selected_path()?.to_path_buf();
                self.open_markdown_file(path)
            }
            file_picker::Action::Quit => {
                self.file_picker = None;
                if self.file_picker_started_app {
                    Some(Cmd::quit())
                } else {
                    self.mode = Mode::Home;
                    None
                }
            }
            _ => cmd.map(|c| c.map(Msg::FilePicker)),
        }
    }
    fn help_keymap_items(&self) -> Vec<tui::help::KeymapEntry> {
        let mut items = Vec::new();
        Self::append_help_entries(&mut items, "home", self.config.keymap.home::<Action>());
        Self::append_help_entries(
            &mut items,
            "output",
            self.config.keymap.output::<tui::output::Action>(),
        );
        Self::append_help_entries(
            &mut items,
            "cli",
            self.config.keymap.cli::<crate::apps::cli::app::Action>(),
        );
        Self::append_help_entries(&mut items, "menu", self.config.keymap.menu::<Navigation>());
        Self::append_help_entries(
            &mut items,
            "preview",
            self.config.keymap.preview::<tui::preview::Action>(),
        );
        Self::append_help_entries(
            &mut items,
            "confirm",
            self.config.keymap.confirm::<tui::confirm::Action>(),
        );
        Self::append_help_entries(
            &mut items,
            "search",
            self.config.keymap.search::<tui::search::Action>(),
        );
        Self::append_help_entries(
            &mut items,
            "goto",
            self.config.keymap.goto::<tui::goto::Action>(),
        );
        Self::append_help_entries(
            &mut items,
            "file_picker",
            self.config.keymap.file_picker::<tui::file_picker::Action>(),
        );
        Self::append_help_entries(
            &mut items,
            "help",
            self.config.keymap.help::<tui::help::Action>(),
        );
        Self::append_help_entries(
            &mut items,
            "envs",
            self.config.keymap.envs::<tui::envs::MainAction>(),
        );
        Self::append_help_entries(
            &mut items,
            "envs_edit",
            self.config.keymap.envs_edit::<tui::envs::EditAction>(),
        );
        Self::append_help_entries(
            &mut items,
            "themes",
            self.config.keymap.themes::<tui::themes::MainAction>(),
        );
        items
    }

    fn append_help_entries<T>(
        items: &mut Vec<tui::help::KeymapEntry>,
        section: &'static str,
        config: DerivedConfig<T>,
    ) {
        items.extend(tui::help::collect_keymap_entries(section, &config));
    }

    /// Resolves picker root then opens the picker.
    /// Fallback chain: file_picker_root, config.file parent, cwd.
    /// `a/b/c.md` does not narrow the next picker to `a/b/`.
    fn open_file_picker_for_current_dir(&mut self) -> Option<Cmd<Msg>> {
        let root = self
            .file_picker_root
            .clone()
            .or_else(|| {
                self.config.file.as_deref().and_then(|file| {
                    std::path::Path::new(file)
                        .parent()
                        .filter(|parent| !parent.as_os_str().is_empty())
                        .map(PathBuf::from)
                })
            })
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
        self.open_file_picker(root);
        None
    }

    fn open_file_picker(&mut self, root: PathBuf) {
        match crate::markdown_files::find_markdown_files(
            &root,
            crate::markdown_files::MarkdownSearchOptions::default(),
        ) {
            Ok(files) if files.is_empty() => {
                self.notify_error(format!("No Markdown files found under {}", root.display()));
            }
            Ok(files) => {
                self.file_picker = Some(file_picker::FilePicker::new(
                    files,
                    self.theme.clone(),
                    self.config.keymap.file_picker(),
                ));
                self.mode = Mode::FilePicker;
                self.file_picker_root = Some(root);
                self.file_picker_started_app = false;
            }
            Err(err) => {
                self.notify_error(err.to_string());
            }
        }
    }

    /// Reads and loads a Markdown file, replacing the active document.
    /// Updates `config.file` because reload reads the active path from config.
    fn open_markdown_file(&mut self, path: PathBuf) -> Option<Cmd<Msg>> {
        match crate::reader::read_from_path(&path) {
            Ok(input) => {
                let doc = upmd_parser::new().parse(&input);
                self.config.file = Some(path.display().to_string());
                self.file_picker = None;
                self.file_picker_started_app = false;
                self.load_document(doc);
                self.mode = Mode::Home;
            }
            Err(err) => {
                self.notify_error(format!("Failed to open {}: {err}", path.display()));
            }
        }
        None
    }
    /// Replaces the document and resets document-derived execution and UI state.
    fn load_document(&mut self, doc: upmd_parser::Document) {
        let upmd_parser::Document {
            nodes,
            codes,
            headings,
            ..
        } = doc;
        self.tasks.clear();
        self.auto_input_paused = false;
        self.menu = menu::Menu::new(
            &codes,
            &headings,
            self.theme.clone(),
            self.config
                .keymap
                .menu::<crate::apps::navigation::Navigation>(),
        );
        self.preview = preview::Preview::new(
            nodes,
            codes.clone(),
            self.theme.clone(),
            self.tasks.buffers(),
            self.config.tui.inline_max_lines(),
            self.config.keymap.preview::<preview::Action>(),
        );
        self.auto_advance = if self.config.all {
            codes.iter().map(|c| c.id).collect()
        } else {
            Vec::new()
        };

        // Reapply --block selection when a new document is loaded after
        // startup (e.g. from the directory file picker).
        if let Some(ref spec) = self.config.block {
            let ids = upmd_parser::resolve_code_block(&codes, spec);
            if let Some(&id) = ids.first() {
                self.menu.select_by_id(id);
                self.navigate_to_code(id);
            }
        }
    }

    fn handle_goto_msg(&mut self, action: tui::goto::Action) -> Option<Cmd<Msg>> {
        let goto = self.goto.as_mut()?;
        let cmd = goto.update(action);

        if cmd.is_some() {
            match action {
                tui::goto::Action::Select => {
                    if let Some(id) = goto.selected_code_id() {
                        self.navigate_to_code(id);
                    }
                    self.goto = None;
                    self.mode = Mode::Home;
                }
                tui::goto::Action::Quit => {
                    self.goto = None;
                    self.mode = Mode::Home;
                }
                _ => {}
            }
            return None;
        }

        None
    }

    fn handle_help_msg(&mut self, action: tui::help::Action) -> Option<Cmd<Msg>> {
        if let Some(help) = self.help.as_mut() {
            if let Some(_cmd) = help.update(action) {
                self.help = None;
                self.mode = Mode::Home;
            }
        }
        None
    }

    fn handle_envs_msg(&mut self, action: tui::envs::Action) -> Option<Cmd<Msg>> {
        let cmd = self.envs.update(action.clone());
        if let tui::envs::Action::Quit = action {
            self.mode = Mode::Home;
            None
        } else {
            cmd.map(|c| c.map(Msg::Envs))
        }
    }

    fn handle_themes_msg(&mut self, action: themes::Action) -> Option<Cmd<Msg>> {
        let cmd = self.themes.as_mut()?.update(action.clone());
        match action {
            themes::Action::Preview(theme) => {
                self.apply_theme(theme);
                None
            }
            themes::Action::Select(theme) => {
                self.apply_theme(theme.clone());
                self.themes = None;
                self.mode = Mode::Home;
                Some(self.save_theme_preference(theme))
            }
            themes::Action::Restore(theme) => {
                self.apply_theme(theme);
                self.themes = None;
                self.mode = Mode::Home;
                self.notify_info("Theme restored");
                None
            }
            _ => cmd.map(|c| c.map(Msg::Themes)),
        }
    }

    fn handle_stream_update(
        &mut self,
        id: CodeId,
        stream: crate::pty::stream::Stream,
    ) -> Option<Cmd<Msg>> {
        let was_alternate_screen = self
            .tasks
            .get(id)
            .is_some_and(|task| task.parser.is_alternate_screen());
        let mut force_rebuild = self.tasks.handle_stream(id, &stream);
        let entered_alternate_screen = !was_alternate_screen
            && self
                .tasks
                .get(id)
                .is_some_and(|task| task.parser.is_alternate_screen());
        if entered_alternate_screen && self.mode != Mode::Output && self.menu.selected() == Some(id)
        {
            let size = self.inline_pty_size_for_code(id);
            self.tasks.resize_task(id, size.width, size.height);
            force_rebuild = true;
        }
        if matches!(
            &stream,
            crate::pty::stream::Stream::Exit(_) | crate::pty::stream::Stream::End
        ) {
            self.preview.prefer_status_gutter_for(id);
        }

        if matches!(&stream, crate::pty::stream::Stream::Env(_)) {
            if let Some(buf) = self.tasks.get(id) {
                if let Some(envs) = &buf.captured_envs {
                    self.envs.merge_envs(envs.clone());
                }
            }
        }

        if let crate::pty::stream::Stream::Cwd(_) = &stream {
            if let Some(buf) = self.tasks.get(id) {
                if let Some(cwd) = &buf.captured_cwd {
                    self.last_cwd = Some(std::path::PathBuf::from(cwd));
                }
            }
        }

        if force_rebuild {
            self.preview.rebuild_view(self.tasks.buffers());
            self.tasks.clear_dirty();
        }
        self.sync_input_mode();

        // Auto-focus when the selected block becomes ready for input.
        if self.mode != Mode::Input
            && !self.auto_input_paused
            && self.menu.selected() == Some(id)
            && self.tasks.is_waiting_for_input(id)
        {
            self.mode = Mode::Input;
        }
        self.sync_menu_running_state();

        // Auto-advance to next block in --all mode.
        if matches!(&stream, crate::pty::stream::Stream::End) && !self.auto_advance.is_empty() {
            self.run_next_pending()
        } else {
            None
        }
    }

    /// Reloads the active file and replaces all document-derived state.
    fn reload(&mut self) -> Option<Cmd<Msg>> {
        let doc = match exec::reload_document(self.config.file.as_deref()) {
            Ok(doc) => doc,
            Err(err) => {
                tracing::warn!("{err}");
                self.notify_error(err.to_string());
                return None;
            }
        };

        self.load_document(doc);
        self.mode = Mode::Home;
        tracing::info!("File reloaded successfully");
        None
    }
}

impl Input for App {
    fn action(&self, event: crossterm::event::Event) -> Option<Msg> {
        match self.mode {
            Mode::Home | Mode::Input | Mode::Output => Some(Msg::Event(event)),
            Mode::Confirm => self
                .confirm
                .as_ref()
                .and_then(|c| c.action(event))
                .map(Msg::Confirm),
            Mode::Search => self
                .search
                .as_ref()
                .and_then(|s| s.action(event))
                .map(Msg::Search),
            Mode::Goto => self
                .goto
                .as_ref()
                .and_then(|g| g.action(event))
                .map(Msg::Goto),
            Mode::FilePicker => self
                .file_picker
                .as_ref()
                .and_then(|p| p.action(event))
                .map(Msg::FilePicker),
            Mode::Help => self
                .help
                .as_ref()
                .and_then(|h| h.action(event))
                .map(Msg::Help),
            Mode::Envs => self.envs.action(event).map(Msg::Envs),
            Mode::Themes => self
                .themes
                .as_ref()
                .and_then(|t| t.action(event))
                .map(Msg::Themes),
        }
    }
}

impl Output for App {
    fn render(&self, frame: &mut Frame, area: Rect) {
        if self.mode == Mode::Output {
            if let Some(id) = self.menu.selected() {
                if let Some(buf) = self.tasks.get(id) {
                    self.output.render(frame, area, buf, &self.theme);
                }
            }
            self.render_notification(frame, area);
            return;
        }

        let mut layout = self.layout.borrow_mut();
        layout.update(area, self.menu_width(area.width));

        if !self.zen {
            self.menu.render(frame, layout.menu);
        }

        self.preview.render(frame, layout.preview);
        self.render_footer(frame, layout.footer);

        match self.mode {
            Mode::Help => {
                if let Some(help) = &self.help {
                    help.render(frame, area);
                }
            }
            Mode::Confirm => {
                if let Some(confirm) = &self.confirm {
                    confirm.render(frame, area);
                }
            }
            Mode::Envs => {
                self.envs.render(frame, area);
            }
            Mode::Themes => {
                if let Some(themes) = &self.themes {
                    themes.render(frame, area);
                }
            }
            Mode::Goto => {
                if let Some(goto) = &self.goto {
                    goto.render(frame, area);
                }
            }
            Mode::FilePicker => {
                if let Some(picker) = &self.file_picker {
                    picker.render(frame, area);
                }
            }
            _ => {}
        }

        self.render_notification(frame, area);
    }
}

impl App {
    fn render_notification(&self, frame: &mut Frame, area: Rect) {
        if let Some(ref flash) = self.notification {
            flash.render(frame, area, &self.theme);
        }
    }

    /// Renders shortcuts for the active mode and selected task state.
    fn render_footer(&self, frame: &mut Frame, area: Rect) {
        let (badge, left, right) = self.footer_content();

        let right_text = right.unwrap_or_else(|| self.footer_right_text.clone());
        let badge_width = badge.width() as u16;

        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(badge_width),
                Constraint::Min(1),
                Constraint::Length(right_text.width() as u16 + 2),
            ])
            .split(area);

        frame.render_widget(
            Paragraph::new(badge)
                .block(Block::default().style(Style::default().bg(self.theme.background))),
            chunks[0],
        );
        frame.render_widget(self.theme.footer(left), chunks[1]);
        frame.render_widget(
            self.theme.footer(right_text).alignment(Alignment::Right),
            chunks[2],
        );
    }

    fn footer_content(&self) -> (Line<'static>, Line<'static>, Option<Line<'static>>) {
        match self.mode {
            Mode::Search => {
                let badge = self.theme.mode_badge("SEARCH", self.theme.active);
                if let Some(s) = &self.search {
                    (badge, s.footer_shortcuts(), s.footer_right())
                } else {
                    (badge, self.keymap_footer(), None)
                }
            }
            Mode::Goto => {
                let badge = self.theme.mode_badge("GOTO", self.theme.active);
                (badge, Line::default(), None)
            }
            Mode::FilePicker => {
                let badge = self.theme.mode_badge("OPEN", self.theme.active);
                if let Some(picker) = &self.file_picker {
                    (badge, picker.footer_shortcuts(), None)
                } else {
                    (badge, Line::default(), None)
                }
            }
            Mode::Input => {
                let is_running = self
                    .menu
                    .selected()
                    .and_then(|id| self.tasks.get(id))
                    .is_some_and(|b| b.running());
                let badge = if is_running {
                    self.theme.mode_badge("INPUT", self.theme.success)
                } else {
                    self.theme.mode_badge("NORMAL", self.theme.accent)
                };
                let left = self
                    .theme
                    .keymap_shortcuts(&self.keymap.items, |action| match action {
                        Action::ExitInput => is_running,
                        Action::Paste => is_running,
                        _ => false,
                    });
                (badge, left, None)
            }
            _ if self.zen => {
                let badge = self.theme.mode_badge("ZEN", self.theme.logo);
                let left = self.theme.shortcuts(&[
                    ("z".to_string(), "exit zen".to_string()),
                    ("q".to_string(), "quit".to_string()),
                ]);
                (badge, left, None)
            }
            _ => {
                let badge = self.theme.mode_badge("NORMAL", self.theme.accent);
                let left = match self.menu.selected().and_then(|id| self.tasks.get(id)) {
                    Some(buf) => self.keymap_footer_with(|action| match action {
                        Action::ViewOutput => true,
                        Action::Input => buf.running(),
                        _ => false,
                    }),
                    None => self.keymap_footer(),
                };
                (badge, left, None)
            }
        }
    }

    fn keymap_footer(&self) -> ratatui::text::Line<'static> {
        self.keymap_footer_with(|_| false)
    }

    fn keymap_footer_with(&self, extra: impl Fn(&Action) -> bool) -> ratatui::text::Line<'static> {
        let shortcuts = self.theme.keymap_shortcuts(&self.keymap.items, |action| {
            extra(action)
                || matches!(
                    action,
                    Action::Execute
                        | Action::OpenFilePicker
                        | Action::Search
                        | Action::Goto
                        | Action::SwitchTheme
                        | Action::ToggleZen
                        | Action::Help
                )
        });

        let nav_spans = vec![
            Span::styled("↑↓", self.theme.active_fg_style()),
            Span::styled(" ", self.theme.inactive_style()),
            Span::raw("move").style(self.theme.muted_style()),
        ];

        let mut all_spans = nav_spans;
        all_spans.push(Span::styled("  ", self.theme.inactive_style()));
        all_spans.extend(shortcuts.spans);

        ratatui::text::Line::from(all_spans)
    }

    /// Shows an info flash notification at the bottom right.
    pub fn notify_info(&mut self, text: impl Into<String>) {
        self.notification = Some(tui::notification::info(text));
    }

    /// Shows a success flash notification at the bottom right.
    pub fn notify_success(&mut self, text: impl Into<String>) {
        self.notification = Some(tui::notification::success(text));
    }

    /// Shows an error flash notification at the bottom right.
    pub fn notify_error(&mut self, text: impl Into<String>) {
        self.notification = Some(tui::notification::error(text));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::markdown_files::MarkdownFile;
    use std::fs;
    use std::path::{Path, PathBuf};
    use upmd_runtime::Component;

    fn markdown_file(path: &str, display: &str) -> MarkdownFile {
        MarkdownFile {
            path: PathBuf::from(path),
            display: display.to_string(),
        }
    }

    fn app_in_file_picker_mode() -> App {
        App::from_file_picker(
            PathBuf::from("/repo"),
            vec![
                markdown_file("/repo/README.md", "README.md"),
                markdown_file("/repo/docs/install.md", "docs/install.md"),
            ],
            AppConfig::default(),
        )
    }

    fn app_for_reload(path: &Path, markdown: &str, all: bool, block: Option<&str>) -> App {
        App::new(
            upmd_parser::new().parse(markdown),
            AppConfig {
                file: Some(path.display().to_string()),
                all,
                block: block.map(str::to_string),
                ..Default::default()
            },
        )
    }

    #[test]
    fn file_picker_message_next_moves_selection() {
        let mut app = app_in_file_picker_mode();

        assert_eq!(
            app.file_picker
                .as_ref()
                .and_then(file_picker::FilePicker::selected_path),
            Some(Path::new("/repo/README.md"))
        );

        app.update(Msg::FilePicker(file_picker::Action::Next));
        assert_eq!(
            app.file_picker
                .as_ref()
                .and_then(file_picker::FilePicker::selected_path),
            Some(Path::new("/repo/docs/install.md"))
        );
    }

    #[test]
    fn file_picker_for_bare_relative_file_opens_from_current_directory() {
        let expected_root = std::env::current_dir().unwrap();
        let mut app = App::new(
            upmd_parser::new().parse("# Read me\n"),
            AppConfig {
                file: Some("README.md".to_string()),
                ..Default::default()
            },
        );

        app.handle_action(Action::OpenFilePicker);

        assert!(matches!(app.mode, Mode::FilePicker));
        assert_eq!(
            app.file_picker_root.as_deref(),
            Some(expected_root.as_path())
        );
    }

    #[test]
    fn file_picker_select_keeps_original_picker_root_for_nested_file() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let readme_path = root.join("README.md");
        let docs_dir = root.join("docs");
        let install_path = docs_dir.join("install.md");

        fs::create_dir(&docs_dir).unwrap();
        fs::write(&readme_path, "# Read me\n").unwrap();
        fs::write(&install_path, "# Install\n\nUse this guide.\n").unwrap();

        let mut app = App::from_file_picker(
            root.clone(),
            vec![
                MarkdownFile {
                    path: readme_path,
                    display: "README.md".to_string(),
                },
                MarkdownFile {
                    path: install_path.clone(),
                    display: "docs/install.md".to_string(),
                },
            ],
            AppConfig::default(),
        );

        app.update(Msg::FilePicker(file_picker::Action::Next));
        assert_eq!(
            app.file_picker
                .as_ref()
                .and_then(file_picker::FilePicker::selected_path),
            Some(install_path.as_path())
        );

        app.update(Msg::FilePicker(file_picker::Action::Select));

        assert!(app.file_picker.is_none());
        assert_eq!(app.file_picker_root.as_deref(), Some(root.as_path()));
    }
    #[test]
    fn reload_success_replaces_document_state_and_reapplies_run_configuration() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("runbook.md");
        fs::write(
            &path,
            "# Updated\n\n```sh [name:first]\necho first\n```\n\n```sh [name:setup]\necho setup\n```\n",
        )
        .unwrap();
        let mut app = app_for_reload(
            &path,
            "# Original\n\n```sh\necho old\n```\n",
            true,
            Some("setup"),
        );

        app.reload();

        assert_eq!(app.auto_advance, vec![1, 2]);
        assert_eq!(app.menu.selected(), Some(2));
    }

    #[test]
    fn reload_failure_preserves_document_state_and_notifies_error() {
        let markdown = "```sh\necho first\n```\n\n```sh\necho second\n```\n";
        let mut app = app_for_reload(Path::new("unused.md"), markdown, true, Some("2"));
        app.config.file = None;
        let selected_before = app.menu.selected();
        let auto_advance_before = app.auto_advance.clone();

        app.reload();

        assert_eq!(app.menu.selected(), selected_before);
        assert_eq!(app.auto_advance, auto_advance_before);
        let notification = app.notification.as_ref().unwrap();
        assert_eq!(notification.kind, tui::notification::FlashKind::Error);
        assert_eq!(notification.text, "No file path in config, cannot reload");
    }
    #[cfg(unix)]
    #[test]
    fn alternate_screen_is_resized_to_rows_below_source_block() {
        let markdown =
            "```bash\nprintf '\\033[?1049h'\nprintf 'TUI'\nsleep 1\nprintf '\\033[?1049l'\n```\n";
        let mut app = App::new(upmd_parser::new().parse(markdown), AppConfig::default());
        let area = Rect::new(0, 0, 80, 43);
        let menu_width = app.menu_width(area.width);
        app.layout.borrow_mut().update(area, menu_width);
        app.preview.rebuild_view(app.tasks.buffers());

        let code = app.preview.code_by_id(1).unwrap().clone();
        let size = app.pty_size_for_code(1);
        let rx = app
            .tasks
            .run(
                &code,
                size,
                app.envs.data(),
                false,
                &app.config.binaries,
                None,
            )
            .expect("bash task should start");

        let mut entered_alternate_screen = false;
        while let Ok(stream) = rx.recv_timeout(std::time::Duration::from_secs(2)) {
            app.handle_stream_update(1, stream);
            if app
                .tasks
                .get(1)
                .is_some_and(|task| task.parser.is_alternate_screen())
            {
                entered_alternate_screen = true;
                break;
            }
        }

        assert!(entered_alternate_screen);
        assert_eq!(app.tasks.get(1).unwrap().parser.screen().size().0, 35);
        app.tasks.send_input(1, b"\x03");
    }
}
