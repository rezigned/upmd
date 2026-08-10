//! Main TUI component and application-level event coordination.

use crate::apps::config::{self, Config as AppConfig};
use crate::apps::exec;
use crate::apps::tui;
use crate::apps::tui::{
    confirm, content, dependencies, file_picker, layout,
    overlay::{self, Overlay},
    tasks::Tasks,
    themes,
    workflow::State as WorkflowState,
    Shortcut,
};
use crate::apps::workflow::{Workflow, WorkflowTransition};
use crate::utils::key_to_bytes;
use color_eyre::Result;
use keymap::{DerivedConfig, KeyMap};
use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect, Spacing},
    style::Style,
    symbols::merge::MergeStrategy,
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};
use std::{cell::RefCell, collections::HashMap, path::PathBuf, process::ExitCode, time::Duration};
use upmd_parser::{CodeId, Parser};
use upmd_runtime::Effect;
use upmd_runtime::{
    runtimes::tui::{Input, Output},
    Cmd, Component, EffectExt,
};

#[derive(Clone, Copy, Default, PartialEq)]
enum View {
    #[default]
    Home,
    Input,
    Output,
}

/// Main TUI application component.
///
/// Owns the active content, tasks, and overlay components. Routes events,
/// manages execution state, and renders the full TUI layout.
pub struct App {
    config: AppConfig,
    content: content::Content,
    tasks: Tasks,
    view: View,
    overlay: Option<Overlay>,
    zen: bool,
    theme: crate::apps::theme::Theme,
    auto_input_paused: bool,
    layout: RefCell<layout::Area>,
    keymap: DerivedConfig<Action>,
    envs: tui::envs::EnvVars,
    /// Browse root preserved across file opens.
    /// Selecting a/b/c.md does not re-root the next picker to a/b/.
    file_picker_root: Option<PathBuf>,
    output: tui::output::Output,
    workflow: WorkflowState,
    last_cwd: Option<PathBuf>,
    started: bool,
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
    /// Show dependency diagram
    #[key(";", help = "deps")]
    ShowDeps,
    /// Toggle the inline workflow dependency graph
    #[key("'", help = "graph")]
    ToggleWorkflowGraph,
    /// Quit
    #[key("q", "ctrl-c", help = "quit")]
    Quit,
}

impl App {
    pub fn new(doc: upmd_parser::Document, config: AppConfig) -> std::result::Result<Self, String> {
        let selected = crate::apps::initial_code_id(&doc.codes, config.block.as_deref())?;
        Ok(Self::build(doc, config, selected))
    }

    fn build(doc: upmd_parser::Document, config: AppConfig, selected: Option<CodeId>) -> Self {
        let theme = config.theme.clone();
        let tasks = Tasks::new();
        let content = content::Content::new(doc, selected, &theme, tasks.buffers(), &config);

        App {
            config: config.clone(),
            content,
            tasks,
            view: View::Home,
            overlay: None,
            zen: false,
            theme: theme.clone(),
            auto_input_paused: false,
            layout: RefCell::new(layout::Area::default()),
            keymap: config.keymap.home(),
            envs: tui::envs::EnvVars::new(
                std::env::vars().collect(),
                theme.clone(),
                config.keymap.envs(),
                config.keymap.envs_edit(),
                config.keymap.menu(),
                config.keymap.search.clone(),
            ),
            file_picker_root: None,
            output: tui::output::Output::new(config.keymap.output()),
            workflow: WorkflowState::default(),
            last_cwd: None,
            notification: None,
            started: false,
        }
    }
}

// Code execution and workflow coordination.
impl App {
    fn execute_block(&mut self, id: CodeId) -> Option<Cmd<Msg>> {
        let code = self.content.code_by_id(id)?;
        let size = self.pty_size_for_code(id);
        let envs = self.envs.data();
        let command = self
            .tasks
            .run(
                code,
                size,
                envs,
                self.config.capture_state,
                &self.config.binaries,
                self.config
                    .working_dir
                    .clone()
                    .or_else(|| self.last_cwd.clone()),
            )
            .map(|receiver| exec::stream_rx(id, receiver, Msg::StreamUpdate));

        if command.is_none() {
            self.content.prefer_status_gutter_for(id);
            self.content.rebuild(self.tasks.buffers());
        }
        self.sync_task_statuses();
        command
    }

    fn launch_batch(&mut self, batch: Vec<CodeId>) -> Option<Cmd<Msg>> {
        let mut commands = Vec::new();
        let mut completed = None;

        for id in batch {
            let succeeded = self
                .tasks
                .get(id)
                .is_some_and(|task| task.exit_code == Some(0));
            let should_execute = self.workflow.should_execute(id, succeeded);
            if !should_execute {
                completed = self.workflow.advance(id, Some(0));
            } else if let Some(command) = self.execute_block(id) {
                commands.push(command);
            } else {
                completed = self.workflow.advance(id, Some(1));
            }
        }

        if let Some(result) = completed {
            if let Some(command) = self.handle_advance(result) {
                commands.push(command);
            }
        }

        match commands.len() {
            0 => None,
            1 => commands.pop(),
            _ => Some(Cmd::Batch(commands)),
        }
    }

    fn start_plan(&mut self, workflow: Workflow) -> Option<Cmd<Msg>> {
        let graph = workflow.graph().clone();
        let dependencies = dependencies::Dependencies::new(
            "Workflow",
            self.content.codes(),
            Some(Ok(graph)),
            "No active workflow",
            self.tasks.task_statuses(),
            self.theme.clone(),
            self.config.keymap.dependencies(),
        );
        let (batch, auto_run) = self.workflow.start(workflow, dependencies)?;
        self.select_batch(&batch);
        auto_run.then(|| self.launch_batch(batch)).flatten()
    }

    fn start_all(&mut self) -> Option<Cmd<Msg>> {
        match Workflow::for_all(self.content.codes(), self.config.yes) {
            Ok(workflow) => self.start_plan(workflow),
            Err(error) => {
                self.notify_error(error);
                None
            }
        }
    }

    fn start_target(&mut self, id: CodeId) -> Option<Cmd<Msg>> {
        match Workflow::for_target(self.content.codes(), id) {
            Ok(workflow) => self.start_plan(workflow),
            Err(error) => {
                self.notify_error(error);
                None
            }
        }
    }

    fn rerun_target(&mut self, id: CodeId) -> Option<Cmd<Msg>> {
        match Workflow::for_target_rerun(self.content.codes(), id) {
            Ok(workflow) => self.start_plan(workflow),
            Err(error) => {
                self.notify_error(error);
                None
            }
        }
    }

    fn execute(&mut self, id: CodeId) -> Option<Cmd<Msg>> {
        if self.workflow.is_active() {
            if !self.workflow.is_running(id) {
                return None;
            }
            return self.execute_block(id).or_else(|| {
                self.workflow
                    .advance(id, Some(1))
                    .and_then(|result| self.handle_advance(result))
            });
        }
        self.start_target(id)
    }

    fn handle_advance(&mut self, result: WorkflowTransition) -> Option<Cmd<Msg>> {
        match result {
            WorkflowTransition::NextBatch(batch) => {
                self.flush_pending_states();
                self.select_batch(&batch);
                self.workflow
                    .auto_run()
                    .then(|| self.launch_batch(batch))
                    .flatten()
            }
            WorkflowTransition::Pending | WorkflowTransition::Untracked => None,
            WorkflowTransition::Stopped(failure) => {
                self.flush_pending_states();
                self.workflow.finish();
                self.notify_error(format!(
                    "Block {} failed - stopping dependency chain",
                    failure.block
                ));
                None
            }
            WorkflowTransition::Finished { .. } => {
                self.flush_pending_states();
                self.workflow.finish();
                None
            }
        }
    }

    fn capture_state(&mut self, id: CodeId, capture_env: bool, capture_cwd: bool) {
        if !capture_env && !capture_cwd {
            return;
        }
        let task = self.tasks.get(id);
        let env = capture_env.then(|| task?.captured_envs.clone()).flatten();
        let cwd = capture_cwd
            .then(|| task?.captured_cwd.as_deref().map(PathBuf::from))
            .flatten();

        if !self.workflow.is_active() {
            if let Some(env) = env {
                self.envs.merge_envs(env);
            }
            if let Some(cwd) = cwd {
                self.last_cwd = Some(cwd);
            }
            return;
        }

        self.workflow.capture(id, env, cwd);
    }

    fn flush_pending_states(&mut self) {
        for (_, state) in self.workflow.take_captures() {
            if let Some(envs) = state.envs {
                self.envs.merge_envs(envs);
            }
            if let Some(cwd) = state.cwd {
                self.last_cwd = Some(cwd);
            }
        }
    }

    fn select_batch(&mut self, batch: &[CodeId]) {
        // Don't auto-navigate when running a specific target (stay on the
        // block the user originally selected.  In for_all mode (no target)
        // navigation follows the execution order.
        let has_target = self.workflow.has_target();
        if has_target {
            return;
        }
        if let Some(first) = batch.first() {
            self.content.select_code(*first);
        }
    }

    fn sync_task_statuses(&mut self) {
        self.content.set_code_statuses(self.tasks.task_statuses());
    }

    fn handle_stream_update(
        &mut self,
        id: CodeId,
        stream: crate::pty::stream::Stream,
    ) -> Option<Cmd<Msg>> {
        let capture_env = matches!(&stream, crate::pty::stream::Stream::Env(_));
        let capture_cwd = matches!(&stream, crate::pty::stream::Stream::Cwd(_));
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
        if entered_alternate_screen
            && self.view != View::Output
            && self.content.selected_code_id() == Some(id)
        {
            let size = self.inline_pty_size_for_code(id);
            self.tasks.resize_task(id, size.width, size.height);
            force_rebuild = true;
        }
        if matches!(
            &stream,
            crate::pty::stream::Stream::Exit(_) | crate::pty::stream::Stream::End
        ) {
            self.content.prefer_status_gutter_for(id);
        }

        self.capture_state(id, capture_env, capture_cwd);

        if force_rebuild {
            self.content.rebuild(self.tasks.buffers());
            self.tasks.clear_dirty();
        }
        self.sync_input_mode();

        // Auto-focus when the selected block becomes ready for input.
        if self.view != View::Input
            && self.overlay.is_none()
            && !self.auto_input_paused
            && self.content.selected_code_id() == Some(id)
            && self.tasks.is_waiting_for_input(id)
        {
            self.view = View::Input;
        }
        self.sync_task_statuses();

        if matches!(&stream, crate::pty::stream::Stream::End) {
            let exit_code = self.tasks.get(id).and_then(|task| task.exit_code);
            if exit_code != Some(0) {
                self.workflow.discard_capture(id);
            }
            let result = self.workflow.advance(id, exit_code)?;
            return self.handle_advance(result);
        }
        None
    }
}

// PTY geometry, input, and focus policy.
impl App {
    /// Synchronizes input mode with the currently selected code block.
    ///
    /// This method keeps input mode alive once entered, but never enters it
    /// from Home. New auto-entry happens from stream/tick paths when a selected
    /// process becomes ready for input.
    fn sync_input_mode(&mut self) {
        let is_input_mode = self.content.selected_code_id().is_some_and(|id| {
            self.tasks.contains(id) && !self.tasks.is_done(id) && self.view == View::Input
        });

        let new = match self.view {
            View::Home | View::Input if is_input_mode => View::Input,
            View::Home | View::Input => View::Home,
            _ => return,
        };
        if self.view != new {
            self.view = new;
        }
    }

    fn pty_size_for_code(&self, id: CodeId) -> crate::pty::process::Size {
        self.layout
            .borrow()
            .pty_size(self.content.code_prefix_overhead(id) as u16)
    }

    fn resize_tasks_for_preview(&mut self) {
        if let Some(id) = self.content.selected_code_id() {
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

        let viewport = self.layout.borrow().preview_viewport_rows();
        let rows = self
            .content
            .fit_inline_pty_rows(id, viewport)
            .unwrap_or(base.height as usize);
        crate::pty::process::Size::from((base.width, rows as u16))
    }

    /// Sends raw text to the currently selected PTY as if the user typed it.
    fn send_text_to_pty(&mut self, text: &str) {
        if let Some(id) = self.content.selected_code_id() {
            self.tasks.send_text(id, text);
        }
    }

    /// Forwards a keyboard event to the currently selected PTY process.
    ///
    /// In output mode, resets scrollback so the user sees fresh output.
    /// Converts the event to raw bytes and sends them as stdin input.
    fn forward_to_pty(&mut self, event: crossterm::event::Event) {
        if let Some(id) = self.content.selected_code_id() {
            if let crossterm::event::Event::Key(key) = event {
                if self.view == View::Output {
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
        let Some(id) = self.content.selected_code_id() else {
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
            .content
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
        if let Some(id) = self.content.selected_code_id() {
            if let Some(buf) = self.tasks.get_mut(id) {
                if delta > 0 {
                    buf.scroll_inline_up(self.content.inline_max_lines());
                } else {
                    buf.scroll_inline_down(self.content.inline_max_lines());
                }
                self.content.rebuild(self.tasks.buffers());
            }
        }
    }

    /// Updates input mode after clicking a different code block.
    ///
    /// Clicking another running block keeps input mode active for that new
    /// block. Clicking a completed or empty block exits input mode before the
    /// selection is changed.
    fn keep_input_for_running_click_target(&mut self, previous: Option<CodeId>, id: CodeId) {
        if self.view == View::Input
            && previous != Some(id)
            && !self.tasks.get(id).is_some_and(|b| b.running())
        {
            self.view = View::Home;
        }
    }

    /// Re-enters input mode when clicking a running block after pausing auto-entry.
    fn enter_input_for_running_click_target(&mut self, id: CodeId) {
        if self.view == View::Home
            && self.auto_input_paused
            && self.tasks.get(id).is_some_and(|b| b.running())
        {
            self.auto_input_paused = false;
            self.view = View::Input;
        }
    }
}

/// Messages handled by the main TUI component's event loop.
#[derive(Debug)]
pub enum Msg {
    Content(content::Action),
    Overlay(overlay::Action),
    StreamUpdate(CodeId, crate::pty::stream::Stream),
    Notify(tui::notification::FlashMessage),
    ImageDecoded(tui::preview::DecodedImage),
    Tick,
    Event(crossterm::event::Event),
}

impl crate::RunApp for App {
    fn from_input(input: &str, config: AppConfig) -> std::result::Result<Self, String> {
        let doc = tracing::info_span!("parse").in_scope(|| upmd_parser::new().parse(input));
        tracing::info_span!("build").in_scope(|| Self::new(doc, config))
    }

    fn from_picker(
        root: PathBuf,
        files: Vec<crate::markdown_files::MarkdownFile>,
        config: AppConfig,
    ) -> Self {
        let theme = config.theme.clone();
        let mut app = Self::build(upmd_parser::Document::default(), config.clone(), None);
        app.overlay = Some(Overlay::FilePicker {
            picker: file_picker::FilePicker::new(files, theme, config.keymap.file_picker()),
            quit_on_cancel: true,
        });
        app.file_picker_root = Some(root);
        app
    }

    fn run(self) -> Result<ExitCode> {
        upmd_runtime::runtimes::tui::run(self)?;
        Ok(ExitCode::SUCCESS)
    }
}

impl Component for App {
    type Action = Msg;
    type Outcome = upmd_runtime::NoOutcome;

    fn update(&mut self, msg: Msg) -> Option<Effect<Self::Action, Self::Outcome>> {
        let command = match msg {
            Msg::Event(event) => self.handle_event(event),
            Msg::Content(action) => self.handle_content_msg(action),
            Msg::Overlay(message) => self.handle_overlay_msg(message),
            Msg::StreamUpdate(id, stream) => self.handle_stream_update(id, stream),
            Msg::ImageDecoded(decoded) => {
                self.content.complete_image(decoded);
                None
            }
            Msg::Notify(flash) => {
                self.notification = Some(flash);
                None
            }
            Msg::Tick => self.handle_tick(),
        };

        command.map(Effect::Command)
    }
}

// Runtime lifecycle, actions, and event routing.

impl App {
    fn handle_tick(&mut self) -> Option<Cmd<Msg>> {
        if !self.started {
            self.started = true;
            if self.config.block.is_some() && self.config.yes {
                if let Some(id) = self.content.selected_code_id() {
                    return self.execute(id);
                }
            } else if self.config.all {
                return self.start_all();
            }
        }
        if self.tasks.is_dirty() {
            self.content.rebuild(self.tasks.buffers());
            self.tasks.clear_dirty();
        }

        self.sync_task_statuses();
        self.sync_input_mode();
        self.content.tick();

        let statuses = self.tasks.task_statuses();
        if let Some(overlay) = &mut self.overlay {
            overlay.tick(&statuses);
        }
        self.output.tick();
        self.workflow.tick(statuses);

        // Clear expired flash notification.
        if self
            .notification
            .as_ref()
            .is_some_and(|n| n.is_expired(std::time::Instant::now()))
        {
            self.notification = None;
        }
        let requests = self.content.take_image_requests();
        if requests.is_empty() {
            None
        } else {
            Some(Cmd::stream(move |tx| {
                for path in requests {
                    if tx
                        .send(Msg::ImageDecoded(tui::preview::decode_image(path)))
                        .is_err()
                    {
                        break;
                    }
                }
            }))
        }
    }

    fn is_action_enabled(&self, action: &Action) -> bool {
        let selected_task = self
            .content
            .selected_code_id()
            .and_then(|id| self.tasks.get(id));

        match action {
            Action::ViewOutput => selected_task.is_some(),
            Action::Input => selected_task.is_some_and(|task| task.running()),
            Action::ExitInput => self.view == View::Input,
            Action::Paste => {
                self.view == View::Input && selected_task.is_some_and(|task| task.running())
            }
            Action::ToggleWorkflowGraph => self.workflow.has_graph(),
            _ => true,
        }
    }

    fn handle_action(&mut self, action: Action) -> Option<Cmd<Msg>> {
        if !self.is_action_enabled(&action) {
            return None;
        }

        match action {
            Action::Execute => {
                if let Some(id) = self.content.focused_code_id() {
                    if self.tasks.contains(id) {
                        self.overlay = Some(Overlay::Confirm(confirm::Confirm::rerun(
                            id,
                            self.theme.clone(),
                            self.config.keymap.confirm(),
                        )));
                        None
                    } else {
                        self.auto_input_paused = false;
                        self.execute(id)
                    }
                } else {
                    None
                }
            }
            Action::ToggleWorkflowGraph => {
                self.workflow.toggle_graph();
                None
            }
            Action::Quit => {
                self.overlay = Some(Overlay::Confirm(confirm::Confirm::quit(
                    self.theme.clone(),
                    self.config.keymap.confirm(),
                )));
                None
            }
            Action::Help => {
                self.overlay = Some(Overlay::Help(tui::help::Help::from_keymaps(
                    self.theme.clone(),
                    &self.config.keymap,
                )));
                None
            }
            Action::Envs => {
                self.overlay = Some(Overlay::Envs);
                None
            }
            Action::Search => {
                self.overlay = Some(Overlay::Search(tui::search::Search::new(
                    self.theme.clone(),
                    self.config.keymap.search(),
                )));
                None
            }
            Action::OpenFilePicker => self.open_file_picker_for_current_dir(),
            Action::Goto => {
                use crate::apps::tui::goto::StatusKind;
                let mut all_blocks = Vec::new();
                let mut previews = HashMap::new();
                for c in self.content.codes() {
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
                self.overlay = Some(Overlay::Goto(tui::goto::Goto::new(
                    self.theme.clone(),
                    self.config.keymap.goto(),
                    all_blocks,
                    previews,
                )));
                None
            }
            Action::SwitchTheme => {
                self.overlay = Some(Overlay::Themes(Box::new(themes::ThemeSelector::new(
                    self.theme.clone(),
                    self.config.transparent,
                    self.config.keymap.themes(),
                    self.config.keymap.menu(),
                    self.config.keymap.search.clone(),
                ))));
                None
            }
            Action::ViewOutput => {
                if let Some(id) = self.content.selected_code_id() {
                    if self.tasks.contains(id) {
                        self.view = View::Output;
                        let size = self.layout.borrow().output_pty_size();
                        self.tasks.resize(size.width, size.height);
                    }
                }
                None
            }
            Action::Input => {
                if let Some(id) = self.content.selected_code_id() {
                    if self.tasks.contains(id) {
                        self.view = View::Input;
                        self.auto_input_paused = false;
                    }
                }
                None
            }
            Action::ExitInput => {
                self.view = View::Home;
                self.auto_input_paused = true;
                None
            }
            Action::Reload => {
                self.overlay = Some(Overlay::Confirm(confirm::Confirm::reload(
                    self.theme.clone(),
                    self.config.keymap.confirm(),
                )));
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
                if self.content.is_toc() {
                    let delta = if matches!(action, Action::DecreaseTocWidth) {
                        -2
                    } else {
                        2
                    };
                    let total = self.layout.borrow().total_width();
                    self.content.adjust_toc_width(delta, total);
                    self.layout
                        .borrow_mut()
                        .update_menu_width(self.menu_width(total));
                }
                None
            }
            Action::ShowDeps => {
                self.overlay = Some(Overlay::Dependencies(
                    dependencies::Dependencies::for_target(
                        self.content.codes(),
                        self.content.selected_code_id(),
                        self.tasks.task_statuses(),
                        self.theme.clone(),
                        self.config.keymap.dependencies(),
                    ),
                ));
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
            self.content.set_inline_max_lines(rows as usize);
            self.content.rebuild(self.tasks.buffers());

            if self.view == View::Output {
                let size = self.layout.borrow().output_pty_size();
                self.tasks.resize(size.width, size.height);
            } else {
                self.resize_tasks_for_preview();
            }

            return None;
        }

        match self.view {
            View::Home | View::Input => self.handle_home_event(event),
            View::Output => self.handle_output_event(event),
        }
    }

    fn handle_home_event(&mut self, event: crossterm::event::Event) -> Option<Cmd<Msg>> {
        if self.view == View::Input {
            let input_active = self
                .content
                .selected_code_id()
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
                let clicked_code = self.content.code_id_at_mouse(mouse);
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
                        if clicked_code != self.content.selected_code_id() =>
                    {
                        self.view = View::Home;
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

        if let Some(action) = self.content.action(event.clone()) {
            return Some(Cmd::msg(Msg::Content(action)));
        }

        if let crossterm::event::Event::Key(key) = event {
            if let Some(action) = self.keymap.get(&key) {
                return self.handle_action(action.clone());
            }
        }

        None
    }

    fn handle_output_event(&mut self, event: crossterm::event::Event) -> Option<Cmd<Msg>> {
        let id = self.content.selected_code_id();

        if let Some(buf) = id.and_then(|id| self.tasks.get(id)) {
            self.output.update_state(buf);
        }

        if let Some(action) = self.output.action(event.clone()) {
            let (_, outcome) = self.output.update(action).into_parts();
            if let Some(outcome) = outcome {
                return self.handle_output_action(outcome);
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
                self.view = View::Home;
                self.resize_tasks_for_preview();
            }
            tui::output::Action::BackIfDone => {
                if let Some(id) = self.content.selected_code_id() {
                    if let Some(buf) = self.tasks.get(id) {
                        if buf.done() {
                            self.view = View::Home;
                            self.resize_tasks_for_preview();
                        }
                    }
                }
            }
            tui::output::Action::Copy => {
                if let Some(id) = self.content.selected_code_id() {
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
}

impl Input for App {
    fn action(&self, event: crossterm::event::Event) -> Option<Msg> {
        if let Some(overlay) = &self.overlay {
            overlay.action(event, &self.envs).map(Msg::Overlay)
        } else {
            Some(Msg::Event(event))
        }
    }

    fn tick_rate(&self) -> Option<Duration> {
        Some(Duration::from_millis(self.config.tick_rate))
    }

    fn tick_action(&self) -> Option<Msg> {
        Some(Msg::Tick)
    }
}

// Theme preferences and transient notifications.
impl App {
    /// Applies a new theme to all UI components and rebuilds the preview.
    /// Skips expensive work when the theme name hasn't changed.
    fn apply_theme(&mut self, theme: crate::apps::theme::Theme) {
        if self.theme.name() == theme.name() {
            return;
        }
        self.content.set_theme(&theme);
        if let Some(overlay) = &mut self.overlay {
            overlay.set_theme(&theme);
        }
        self.workflow.set_theme(&theme);
        self.envs.set_theme(&theme);
        self.theme = theme;
        self.content.rebuild(self.tasks.buffers());
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

// Child component coordination.
impl App {
    fn handle_content_msg(&mut self, message: content::Action) -> Option<Cmd<Msg>> {
        let (command, outcome) = self.content.update(message).into_parts();

        match outcome {
            Some(content::Outcome::CodeClicked {
                previous,
                selected,
                copied,
            }) => {
                self.keep_input_for_running_click_target(previous, selected);
                self.enter_input_for_running_click_target(selected);
                self.notify_copy_result(copied);
            }
            Some(content::Outcome::PreviewInteracted { copied }) => {
                if self.view == View::Input {
                    self.view = View::Home;
                }
                self.notify_copy_result(copied);
            }
            None => {}
        }

        self.sync_input_mode();
        command.map(|cmd| cmd.map(Msg::Content))
    }

    fn notify_copy_result(&mut self, copied: Option<bool>) {
        match copied {
            Some(true) => self.notify_success("Copied"),
            Some(false) => self.notify_error("Failed to copy"),
            None => {}
        }
    }

    fn handle_overlay_msg(&mut self, message: overlay::Action) -> Option<Cmd<Msg>> {
        let (command, outcome) = self
            .overlay
            .as_mut()?
            .update(message, &mut self.envs)
            .into_parts();
        let command = command.map(|command| command.map(Msg::Overlay));
        let outcome_command = outcome.and_then(|outcome| self.apply_overlay_outcome(outcome));
        outcome_command.or(command)
    }

    fn apply_overlay_outcome(&mut self, outcome: overlay::Outcome) -> Option<Cmd<Msg>> {
        match outcome {
            overlay::Outcome::Close => {
                self.overlay = None;
                self.view = View::Home;
                None
            }
            overlay::Outcome::Quit => Some(Cmd::quit()),
            overlay::Outcome::Reload => {
                self.overlay = None;
                self.view = View::Home;
                self.reload()
            }
            overlay::Outcome::Rerun(id) => {
                self.overlay = None;
                self.view = View::Home;
                self.auto_input_paused = false;
                self.rerun_target(id)
            }
            overlay::Outcome::ClearSearch => {
                self.content.search("");
                self.overlay = None;
                self.view = View::Home;
                None
            }
            overlay::Outcome::RefreshSearch(term) => {
                let results = self.content.search(&term);
                let selected = self
                    .overlay
                    .as_mut()
                    .and_then(|overlay| overlay.apply_search_results(&results));
                if let Some(index) = selected {
                    self.content.select_search_match(index);
                }
                None
            }
            overlay::Outcome::SelectCode(id) => {
                self.content.select_code(id);
                self.overlay = None;
                self.view = View::Home;
                None
            }
            overlay::Outcome::OpenFile(path) => self.open_markdown_file(path),
            overlay::Outcome::PreviewTheme(theme) => {
                self.apply_theme(theme);
                None
            }
            overlay::Outcome::SaveTheme(theme) => {
                self.apply_theme(theme.clone());
                self.overlay = None;
                self.view = View::Home;
                Some(self.save_theme_preference(theme))
            }
            overlay::Outcome::RestoreTheme(theme) => {
                self.apply_theme(theme);
                self.overlay = None;
                self.view = View::Home;
                self.notify_info("Theme restored");
                None
            }
        }
    }
}

// File picker and active document lifecycle.
impl App {
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
                self.overlay = Some(Overlay::FilePicker {
                    picker: file_picker::FilePicker::new(
                        files,
                        self.theme.clone(),
                        self.config.keymap.file_picker(),
                    ),
                    quit_on_cancel: false,
                });
                self.file_picker_root = Some(root);
            }
            Err(err) => {
                self.notify_error(err.to_string());
            }
        }
    }

    /// Reads and loads a Markdown file, replacing the active document.
    /// Updates `config.file` because reload reads the active path from config.
    fn open_markdown_file(&mut self, path: PathBuf) -> Option<Cmd<Msg>> {
        self.config.block = None;
        match crate::reader::read_from_path(&path) {
            Ok(input) => {
                let doc = upmd_parser::new().parse(&input);
                match self.load_document(doc) {
                    Ok(()) => {
                        self.config.file = Some(path.display().to_string());
                        self.overlay = None;
                        self.view = View::Home;
                    }
                    Err(error) => self.notify_error(error),
                }
            }
            Err(err) => {
                self.notify_error(format!("Failed to open {}: {err}", path.display()));
            }
        }
        None
    }

    /// Replaces the document and resets document-derived execution and UI state.
    fn load_document(&mut self, doc: upmd_parser::Document) -> Result<(), String> {
        let selected = crate::apps::initial_code_id(&doc.codes, self.config.block.as_deref())?;
        self.tasks.clear();
        self.auto_input_paused = false;
        self.content = content::Content::new(
            doc,
            selected,
            &self.theme,
            self.tasks.buffers(),
            &self.config,
        );
        self.workflow.clear();
        self.overlay = None;
        self.started = false;

        Ok(())
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

        match self.load_document(doc) {
            Ok(()) => {
                self.view = View::Home;
                tracing::info!("File reloaded successfully");
            }
            Err(error) => self.notify_error(error),
        }
        None
    }
}

// Root layout and rendering.
impl Output for App {
    fn render(&self, frame: &mut Frame, area: Rect) {
        if self.view == View::Output {
            if let Some(id) = self.content.selected_code_id() {
                if let Some(buf) = self.tasks.get(id) {
                    self.output.render(frame, area, buf, &self.theme);
                }
            }
            self.render_notification(frame, area);
            return;
        }

        let mut layout = self.layout.borrow_mut();
        layout.update(area, self.menu_width(area.width));

        let workflow_graph = self.workflow.visible_graph();
        let (preview_area, graph_area) = if let Some(deps) = workflow_graph {
            let graph_rows = deps
                .graph_rows()
                .min(layout.preview.height.saturating_sub(2));
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Min(0),
                    Constraint::Length(graph_rows.saturating_add(2)),
                ])
                .spacing(Spacing::Overlap(1))
                .split(layout.preview);
            (chunks[0], Some(chunks[1]))
        } else {
            (layout.preview, None)
        };
        self.content.render_preview(frame, preview_area);
        if let (Some(deps), Some(graph_area)) = (workflow_graph, graph_area) {
            let block = self
                .theme
                .block()
                .borders(Borders::ALL)
                .border_style(self.theme.inactive_style())
                .merge_borders(MergeStrategy::Exact);
            let graph_inner = block.inner(graph_area);
            frame.render_widget(block, graph_area);
            deps.render_inline(frame, graph_inner);
        }
        // Render the menu last so its right border merges with both content panes.
        if !self.zen {
            self.content.render_menu(frame, layout.menu);
        }
        self.render_footer(frame, layout.footer);

        if let Some(overlay) = &self.overlay {
            overlay.render(frame, area, &self.envs);
        }

        self.render_notification(frame, area);
    }
}

// Rendering and footer helpers.
impl App {
    fn render_notification(&self, frame: &mut Frame, area: Rect) {
        if let Some(ref flash) = self.notification {
            flash.render(frame, area, &self.theme);
        }
    }

    fn menu_width(&self, total_width: u16) -> u16 {
        if self.zen {
            0
        } else {
            self.content.width(total_width)
        }
    }

    /// Renders shortcuts for the active mode and selected task state.
    fn render_footer(&self, frame: &mut Frame, area: Rect) {
        let (badge, left, right) = self.footer_content();

        let right_text = right.or_else(|| self.footer_right()).unwrap_or_default();
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

    fn is_action_visible_in_footer(&self, action: &Action) -> bool {
        if !self.is_action_enabled(action) {
            return false;
        }

        match self.view {
            View::Input => matches!(action, Action::ExitInput | Action::Paste),
            View::Home | View::Output => matches!(
                action,
                Action::Execute
                    | Action::Input
                    | Action::ViewOutput
                    | Action::OpenFilePicker
                    | Action::Search
                    | Action::Goto
                    | Action::SwitchTheme
                    | Action::ToggleWorkflowGraph
                    | Action::ToggleZen
                    | Action::Help
            ),
        }
    }

    fn footer_content(&self) -> (Line<'static>, Line<'static>, Option<Line<'static>>) {
        if let Some(content) = self
            .overlay
            .as_ref()
            .and_then(|overlay| overlay.footer_content(&self.theme))
        {
            return content;
        }

        match self.view {
            View::Input => {
                let is_running = self
                    .content
                    .selected_code_id()
                    .and_then(|id| self.tasks.get(id))
                    .is_some_and(|b| b.running());
                let badge = if is_running {
                    self.theme.mode_badge("INPUT", self.theme.success)
                } else {
                    self.theme.mode_badge("NORMAL", self.theme.accent)
                };
                (badge, self.footer_shortcuts(), None)
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
                (badge, self.footer_shortcuts(), None)
            }
        }
    }
}

impl Shortcut for App {
    fn footer_shortcuts(&self) -> Line<'static> {
        let shortcuts = self.theme.keymap_shortcuts(&self.keymap.items, |action| {
            self.is_action_visible_in_footer(action)
        });

        if self.view == View::Input {
            return shortcuts;
        }

        let mut spans = vec![
            Span::styled("↑↓", self.theme.active_fg_style()),
            Span::styled(" ", self.theme.inactive_style()),
            Span::raw("move").style(self.theme.muted_style()),
            Span::styled("  ", self.theme.inactive_style()),
        ];
        spans.extend(shortcuts.spans);
        Line::from(spans)
    }

    fn footer_right(&self) -> Option<Line<'static>> {
        Some(Line::from(vec![
            Span::styled(config::APP_NAME, self.theme.active_fg_style()),
            Span::raw(" "),
            Span::styled(config::APP_VERSION, self.theme.muted_style()),
        ]))
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
        <App as crate::RunApp>::from_picker(
            PathBuf::from("/repo"),
            vec![
                markdown_file("/repo/README.md", "README.md"),
                markdown_file("/repo/docs/install.md", "docs/install.md"),
            ],
            AppConfig::default(),
        )
    }

    fn selected_picker_path(app: &App) -> Option<&Path> {
        match &app.overlay {
            Some(Overlay::FilePicker { picker, .. }) => picker.selected_path(),
            _ => None,
        }
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
        .unwrap()
    }

    #[test]
    fn file_picker_message_next_moves_selection() {
        let mut app = app_in_file_picker_mode();

        assert_eq!(
            selected_picker_path(&app),
            Some(Path::new("/repo/README.md"))
        );

        let _ = app.update(Msg::Overlay(overlay::Action::FilePicker(
            file_picker::Action::Next,
        )));
        assert_eq!(
            selected_picker_path(&app),
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
        )
        .unwrap();

        app.handle_action(Action::OpenFilePicker);

        assert!(matches!(app.overlay, Some(Overlay::FilePicker { .. })));
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

        let mut app = <App as crate::RunApp>::from_picker(
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

        let _ = app.update(Msg::Overlay(overlay::Action::FilePicker(
            file_picker::Action::Next,
        )));
        assert_eq!(selected_picker_path(&app), Some(install_path.as_path()));

        let _ = app.update(Msg::Overlay(overlay::Action::FilePicker(
            file_picker::Action::Select,
        )));

        assert!(app.overlay.is_none());
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
            "# Original\n\n```sh [name:first]\necho old\n```\n\n```sh [name:setup]\necho setup\n```\n",
            true,
            Some("setup"),
        );
        app.workflow
            .set_graph(dependencies::Dependencies::for_target(
                app.content.codes(),
                Some(2),
                HashMap::new(),
                app.theme.clone(),
                app.config.keymap.dependencies(),
            ));

        app.reload();

        assert!(!app.workflow.is_active());
        assert!(!app.workflow.has_graph());
        assert!(!app.started);
        assert_eq!(app.content.selected_code_id(), Some(2));
    }

    #[test]
    fn reload_failure_preserves_document_state_and_notifies_error() {
        let markdown = "```sh\necho first\n```\n\n```sh\necho second\n```\n";
        let mut app = app_for_reload(Path::new("unused.md"), markdown, true, Some("2"));
        app.config.file = None;
        let selected_before = app.content.selected_code_id();
        let had_plan = app.workflow.is_active();

        app.reload();

        assert_eq!(app.content.selected_code_id(), selected_before);
        assert_eq!(app.workflow.is_active(), had_plan);
        let notification = app.notification.as_ref().unwrap();
        assert_eq!(notification.kind, tui::notification::FlashKind::Error);
        assert_eq!(notification.text, "No file path in config, cannot reload");
    }

    #[cfg(unix)]
    #[test]
    fn alternate_screen_is_resized_to_rows_below_source_block() {
        let markdown =
            "```bash\nprintf '\\033[?1049h'\nprintf 'TUI'\nsleep 1\nprintf '\\033[?1049l'\n```\n";
        let mut app = App::new(upmd_parser::new().parse(markdown), AppConfig::default()).unwrap();
        let area = Rect::new(0, 0, 80, 43);
        let menu_width = app.menu_width(area.width);
        app.layout.borrow_mut().update(area, menu_width);
        app.content.rebuild(app.tasks.buffers());

        let code = app.content.code_by_id(1).unwrap().clone();
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

    #[test]
    fn inline_dependency_graph_preserves_collapsed_pane_borders() {
        let markdown = "\
```sh [name:prepare]\n:\n```\n\
```sh [name:build]\n:\n```\n\
```sh [name:target, deps:\"prepare|build\"]\n:\n```\n";
        let mut app = App::new(
            upmd_parser::new().parse(markdown),
            AppConfig {
                block: Some("target".to_string()),
                ..Default::default()
            },
        )
        .unwrap();
        app.workflow
            .set_graph(dependencies::Dependencies::for_target(
                app.content.codes(),
                Some(3),
                HashMap::new(),
                app.theme.clone(),
                app.config.keymap.dependencies(),
            ));

        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| app.render(frame, frame.area()))
            .unwrap();

        let buffer = terminal.backend().buffer();
        let rows: Vec<String> = (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect();
        let output = rows
            .join("\n")
            .replace(&format!("upmd {}", config::APP_VERSION), "upmd VERSION");
        insta::assert_snapshot!(output);
    }
}
