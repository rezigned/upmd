use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, HashMap},
    io::{self, Write},
    path::PathBuf,
    process::ExitCode,
    rc::Rc,
};

use upmd_runtime::{
    runtimes::cli::{Input, Output},
    Cmd, Component,
};

use super::output::BatchOutput;
use crate::{
    apps::config::Config as AppConfig,
    apps::config::{
        Envs, CLI_PREVIEW_LINES, CLI_PTY_COL_OVERHEAD, CLI_PTY_MIN_COLS, CLI_PTY_MIN_ROWS,
        CLI_PTY_ROW_OVERHEAD, PTY_DEFAULT_COLS, PTY_DEFAULT_ROWS,
    },
    apps::exec,
    apps::navigation::Navigation,
    apps::task,
    apps::theme::{ansi_bg, ansi_fg, ansi_style, Theme},
    apps::workflow::{Workflow, WorkflowTransition},
    pty::process::Size,
    pty::stream::Stream,
    utils::key_to_bytes,
};
use color_eyre::Result;
use keymap::{DerivedConfig, KeyMap};
use upmd_parser::nodes::{Code, CodeId};
use upmd_parser::{resolve_code_block, Parser};

/// For the CLI, manages code block execution and navigation.
pub struct App {
    codes: Vec<Code>,
    selected: usize,
    config: AppConfig,
    outputs: RefCell<HashMap<CodeId, task::Task>>,
    theme: Theme,
    keymap: DerivedConfig<Action>,
    nav_keymap: DerivedConfig<Navigation>,
    workflow: Option<Workflow>,
    envs: Envs,
    cwd: PathBuf,
    batch_output: BatchOutput,
    picker: Option<crate::apps::picker::PickerState>,
    picker_keymap: DerivedConfig<crate::apps::picker::PickerAction>,
    pending_states: BTreeMap<CodeId, (Option<Envs>, Option<PathBuf>)>,
    failed: Rc<Cell<bool>>,
}

#[derive(Clone, KeyMap, Debug, PartialEq, Eq, Hash)]
pub enum Action {
    /// Executes the selected code block.
    #[key("enter")]
    Run,
    /// Focuses the next block in a parallel batch.
    #[key("tab")]
    FocusNext,
    /// Quits the application.
    #[key("q", "ctrl-c")]
    Quit,
}

#[derive(Clone, Debug)]
pub enum Msg {
    Action(Action),
    Navigation(Navigation),
    StreamUpdate(CodeId, Stream),
    Picker(crate::apps::picker::PickerAction),
}

impl App {
    pub fn new(doc: upmd_parser::Document, config: AppConfig) -> Self {
        let upmd_parser::Document { codes, .. } = doc;

        let selected = match &config.block {
            Some(spec) => {
                let ids = resolve_code_block(&codes, spec);
                codes.iter().position(|c| ids.contains(&c.id)).unwrap_or(0)
            }
            None => 0,
        };

        let failed = Rc::new(Cell::new(false));

        let keymap: DerivedConfig<Action> = config.keymap.cli::<Action>();

        // Seed env and cwd from the parent process so consecutive blocks
        // see mutations from prior blocks (like cd or export).
        let envs = std::env::vars().collect();
        let cwd = config
            .working_dir
            .clone()
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());

        Self {
            codes,
            selected,
            config: config.clone(),
            outputs: RefCell::new(HashMap::new()),
            theme: config.theme.clone(),
            keymap,
            nav_keymap: config.keymap.cli::<Navigation>(),
            workflow: None,
            envs,
            cwd,
            batch_output: BatchOutput::new(),
            picker: None,
            picker_keymap: config.keymap.file_picker(),
            pending_states: BTreeMap::new(),
            failed,
        }
    }

    /// Creates the app in file-picker mode before a document is loaded.
    pub fn from_file_picker(
        files: Vec<crate::markdown_files::MarkdownFile>,
        config: AppConfig,
    ) -> Self {
        let mut app = Self::new(upmd_parser::new().parse(""), config);
        app.picker = Some(crate::apps::picker::PickerState::new(files));
        app
    }

    /// Reads and loads a Markdown file, replacing the active document.
    fn open_markdown_file(&mut self, path: &std::path::Path) -> Option<Cmd<Msg>> {
        match crate::reader::read_from_path(path) {
            Ok(input) => {
                let doc = upmd_parser::new().parse(&input);
                self.load_document(doc);
                self.picker = None;
            }
            Err(err) => {
                // In picker mode, errors are fatal since there's no
                // notification system like the TUI.
                eprintln!("Failed to open {}: {err}", path.display());
                return Some(Cmd::quit());
            }
        }
        None
    }

    fn load_document(&mut self, doc: upmd_parser::Document) {
        let upmd_parser::Document { codes, .. } = doc;
        self.codes = codes;
        self.selected = match &self.config.block {
            Some(spec) => {
                let ids = resolve_code_block(&self.codes, spec);
                self.codes
                    .iter()
                    .position(|c| ids.contains(&c.id))
                    .unwrap_or(0)
            }
            None => 0,
        };
        self.workflow = None;
        self.outputs.borrow_mut().clear();
        self.batch_output.reset();
        self.pending_states.clear();
    }

    fn launch_batch(&mut self, batch: Vec<CodeId>) -> Option<Cmd<Msg>> {
        let mut commands = Vec::new();
        let mut completed = None;

        for id in batch {
            let succeeded = self
                .outputs
                .borrow()
                .get(&id)
                .is_some_and(|task| task.exit_code == Some(0));
            let should_execute = self
                .workflow
                .as_ref()
                .is_none_or(|workflow| workflow.should_execute(id, succeeded));
            if !should_execute {
                completed = self.advance_workflow(id, Some(0));
            } else if let Some(command) = self.execute_block(id) {
                commands.push(command);
            } else {
                completed = self.advance_workflow(id, Some(1));
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

    fn start_plan(&mut self, mut workflow: Workflow) -> Option<Cmd<Msg>> {
        let auto_run = workflow.auto_run();
        let Some(batch) = workflow.start() else {
            return auto_run.then(Cmd::quit);
        };
        self.workflow = Some(workflow);
        self.select_batch(&batch);
        auto_run.then(|| self.launch_batch(batch)).flatten()
    }

    fn start_all(&mut self) -> Option<Cmd<Msg>> {
        match Workflow::for_all(&self.codes, self.config.yes) {
            Ok(workflow) => self.start_plan(workflow),
            Err(error) => self.plan_error(error),
        }
    }

    fn start_target(&mut self, id: CodeId) -> Option<Cmd<Msg>> {
        match Workflow::for_target(&self.codes, id) {
            Ok(workflow) => self.start_plan(workflow),
            Err(error) => self.plan_error(error),
        }
    }

    fn plan_error(&self, error: String) -> Option<Cmd<Msg>> {
        eprintln!("{error}");
        if self.config.yes {
            self.failed.set(true);
            Some(Cmd::quit())
        } else {
            None
        }
    }

    fn execute_block(&mut self, id: CodeId) -> Option<Cmd<Msg>> {
        if self
            .workflow
            .as_ref()
            .is_some_and(|workflow| workflow.is_running(id))
        {
            self.batch_output.track(id);
        }
        let code = self.codes.iter().find(|code| code.id == id)?;
        let (cols, rows) =
            crossterm::terminal::size().unwrap_or((PTY_DEFAULT_COLS, PTY_DEFAULT_ROWS));
        let pty_rows = rows
            .saturating_sub(CLI_PTY_ROW_OVERHEAD)
            .max(CLI_PTY_MIN_ROWS);
        let pty_cols = cols
            .saturating_sub(CLI_PTY_COL_OVERHEAD)
            .max(CLI_PTY_MIN_COLS);
        let size = Size::from((pty_cols, pty_rows));
        let mut outputs = self.outputs.borrow_mut();
        let state = outputs
            .entry(id)
            .or_insert_with(|| task::Task::new(pty_cols, pty_rows, 1024));
        let command = exec::run_code(
            code,
            size,
            self.envs.clone(),
            self.config.capture_state,
            &self.config.binaries,
            state,
            Some(self.cwd.clone()),
        )
        .map(|receiver| exec::stream_rx(id, receiver, Msg::StreamUpdate));
        drop(outputs);

        if command.is_none() {
            eprintln!("Block {id} failed to start");
        }
        command
    }

    fn execute(&mut self, id: CodeId) -> Option<Cmd<Msg>> {
        if let Some(workflow) = &self.workflow {
            if !workflow.is_running(id) {
                return None;
            }
            return self.execute_block(id).or_else(|| {
                self.advance_workflow(id, Some(1))
                    .and_then(|r| self.handle_advance(r))
            });
        }
        self.start_target(id)
    }

    fn advance_workflow(
        &mut self,
        id: CodeId,
        exit_code: Option<i32>,
    ) -> Option<WorkflowTransition> {
        let result = self
            .workflow
            .as_mut()
            .map(|workflow| workflow.advance(id, exit_code));
        if let Some(result) = &result {
            self.batch_output.complete(result);
        }
        result
    }

    fn handle_advance(&mut self, result: WorkflowTransition) -> Option<Cmd<Msg>> {
        match result {
            WorkflowTransition::NextBatch(batch) => {
                self.flush_pending_states();
                self.select_batch(&batch);
                self.workflow
                    .as_ref()
                    .is_some_and(Workflow::auto_run)
                    .then(|| self.launch_batch(batch))
                    .flatten()
            }
            WorkflowTransition::Pending | WorkflowTransition::Untracked => None,
            WorkflowTransition::Stopped(failure) => {
                self.flush_pending_states();
                self.workflow = None;
                eprintln!("Block {} failed - stopping dependency chain", failure.block);
                if self.config.yes {
                    self.failed.set(true);
                    Some(Cmd::quit())
                } else {
                    None
                }
            }
            WorkflowTransition::Finished { failed } => {
                self.flush_pending_states();
                self.workflow = None;
                if self.config.yes {
                    self.failed.set(failed);
                    Some(Cmd::quit())
                } else {
                    None
                }
            }
        }
    }

    fn select_batch(&mut self, batch: &[CodeId]) {
        self.batch_output.select(batch);
        if let Some(first) = batch.first() {
            if let Some(index) = self.codes.iter().position(|code| code.id == *first) {
                self.selected = index;
            }
        }
    }

    fn focus_next_batch_block(&mut self) {
        let Some(selected) = self.codes.get(self.selected).map(|code| code.id) else {
            return;
        };
        let Some(next) = self.batch_output.focus_next(selected) else {
            return;
        };
        if let Some(index) = self.codes.iter().position(|code| code.id == next) {
            self.selected = index;
        }
    }

    fn handle_stream_update(&mut self, id: CodeId, stream: Stream) -> Option<Cmd<Msg>> {
        let is_env = matches!(stream, Stream::Env(_));
        let is_cwd = matches!(stream, Stream::Cwd(_));
        let is_end = matches!(stream, Stream::End);

        self.apply_stream_to_output(id, &stream);
        self.apply_captured_state(id, is_env, is_cwd);

        if is_end {
            let exit_code = self
                .outputs
                .borrow()
                .get(&id)
                .and_then(|task| task.exit_code);
            if exit_code != Some(0) {
                self.pending_states.remove(&id);
            }
            let result = self.advance_workflow(id, exit_code)?;
            return self.handle_advance(result);
        }
        None
    }

    /// Applies a stream message to the output state for the given block,
    /// writing PTY output directly to the terminal when in the alternate
    /// screen and resizing the PTY on alternate-screen transitions.
    fn apply_stream_to_output(&mut self, id: CodeId, stream: &Stream) {
        let focused = self
            .codes
            .get(self.selected)
            .is_some_and(|code| code.id == id);
        let mut outputs = self.outputs.borrow_mut();
        let Some(state) = outputs.get_mut(&id) else {
            return;
        };

        let was_alt = state.is_alternate_screen();
        exec::handle_stream(state, stream);
        let now_alt = state.is_alternate_screen();

        // When a full-screen TUI app (vim, less, htop) is active in the
        // alternate screen buffer, write its PTY output directly to the
        // real terminal so escape sequences (cursor positioning, alt
        // screen entry/exit) are processed natively.
        if let Stream::Out(s) = stream {
            if self.batch_output.is_terminal() && focused && (was_alt || now_alt) {
                let _ = io::stdout().write_all(s.as_bytes());
                let _ = io::stdout().flush();
            }
        }

        // Resize PTY when entering/exiting alternate screen so TUI apps
        // get the full terminal dimensions and line-oriented programs get
        // card-compatible dimensions.
        if was_alt != now_alt {
            let (cols, rows) =
                crossterm::terminal::size().unwrap_or((PTY_DEFAULT_COLS, PTY_DEFAULT_ROWS));
            let pty_size = if now_alt && self.batch_output.is_terminal() && focused {
                Size::from((cols, rows))
            } else {
                let pty_rows = rows
                    .saturating_sub(CLI_PTY_ROW_OVERHEAD)
                    .max(CLI_PTY_MIN_ROWS);
                let pty_cols = cols
                    .saturating_sub(CLI_PTY_COL_OVERHEAD)
                    .max(CLI_PTY_MIN_COLS);
                Size::from((pty_cols, pty_rows))
            };
            if let Some(exec) = &mut state.execution {
                exec.process_mut().resize(pty_size);
            }
        }
    }

    fn apply_captured_state(&mut self, id: CodeId, is_env: bool, is_cwd: bool) {
        if !is_env && !is_cwd {
            return;
        }
        let outputs = self.outputs.borrow();
        let env = is_env
            .then(|| outputs.get(&id)?.captured_envs.clone())
            .flatten();
        let cwd = is_cwd
            .then(|| outputs.get(&id)?.captured_cwd.clone())
            .flatten();
        drop(outputs);

        if self.workflow.is_none() {
            if let Some(captured) = env {
                exec::merge_envs(&mut self.envs, &captured);
            }
            if let Some(captured) = cwd {
                self.cwd = PathBuf::from(captured);
            }
            return;
        }

        let entry = self.pending_states.entry(id).or_default();
        if let Some(env) = env {
            entry.0 = Some(env);
        }
        if let Some(cwd) = cwd {
            entry.1 = Some(PathBuf::from(cwd));
        }
    }

    fn flush_pending_states(&mut self) {
        for (_, (env, cwd)) in std::mem::take(&mut self.pending_states) {
            if let Some(captured) = env {
                exec::merge_envs(&mut self.envs, &captured);
            }
            if let Some(captured) = cwd {
                self.cwd = captured;
            }
        }
    }

    /// Handles picker actions when in file-picker mode.
    /// Navigation actions (Input, Delete, Next, Prev) are delegated to
    /// PickerState. Select loads the file and transitions to normal mode.
    /// Quit exits the app.
    fn handle_picker_msg(&mut self, action: crate::apps::picker::PickerAction) -> Option<Cmd<Msg>> {
        let picker = self.picker.as_mut()?;
        if picker.handle_navigation(&action) {
            return None;
        }
        match action {
            crate::apps::picker::PickerAction::Select => {
                let path = picker
                    .selected_file_idx()
                    .map(|i| picker.files[i].path.clone());
                match path {
                    Some(path) => self.open_markdown_file(&path),
                    None => None,
                }
            }
            crate::apps::picker::PickerAction::Quit => Some(Cmd::quit()),
            _ => None,
        }
    }

    fn handle_action(&mut self, action: Action) -> Option<Cmd<Msg>> {
        match action {
            Action::Run => {
                let id = self.codes.get(self.selected)?.id;
                self.execute(id)
            }
            Action::FocusNext => {
                self.focus_next_batch_block();
                None
            }
            Action::Quit => Some(Cmd::quit()),
        }
    }

    fn handle_nav(&mut self, nav: Navigation) -> Option<Cmd<Msg>> {
        let total = self.codes.len();
        match nav {
            Navigation::Next if self.selected + 1 < total => {
                self.selected += 1;
            }
            Navigation::Prev if self.selected > 0 => {
                self.selected -= 1;
            }
            Navigation::First => {
                self.selected = 0;
            }
            Navigation::Last => {
                self.selected = total.saturating_sub(1);
            }
            Navigation::PageUp => {
                self.selected = self.selected.saturating_sub(5);
            }
            Navigation::PageDown => {
                self.selected = (self.selected + 5).min(total.saturating_sub(1));
            }
            _ => {}
        }
        if self.config.yes {
            if let Some(id) = self.codes.get(self.selected).map(|c| c.id) {
                self.execute(id)
            } else {
                None
            }
        } else {
            None
        }
    }
}

impl crate::RunApp for App {
    fn from_input(input: &str, config: AppConfig) -> Self {
        let doc = upmd_parser::new().parse(input);
        Self::new(doc, config)
    }

    fn from_picker(
        _root: PathBuf,
        files: Vec<crate::markdown_files::MarkdownFile>,
        config: AppConfig,
    ) -> Self {
        Self::from_file_picker(files, config)
    }

    fn run(mut self) -> Result<ExitCode> {
        let failed = Rc::clone(&self.failed);
        let runtime = upmd_runtime::runtimes::cli::Runtime::new();
        self.batch_output.set_terminal(runtime.is_terminal());
        upmd_runtime::Runtime::run(runtime, upmd_runtime::Engine::new(self))?;
        Ok(if failed.get() {
            ExitCode::FAILURE
        } else {
            ExitCode::SUCCESS
        })
    }
}

impl Component for App {
    type Msg = Msg;

    fn create(&mut self) -> Option<Cmd<Self::Msg>> {
        if self.picker.is_some() {
            return None;
        }
        if self.config.block.is_some() && self.config.yes {
            let id = self.codes.get(self.selected)?.id;
            self.execute(id)
        } else if self.config.all {
            self.start_all()
        } else {
            None
        }
    }

    fn update(&mut self, msg: Msg) -> Option<Cmd<Msg>> {
        match msg {
            Msg::Action(action) => self.handle_action(action),
            Msg::Navigation(nav) => self.handle_nav(nav),
            Msg::StreamUpdate(id, stream) => self.handle_stream_update(id, stream),
            Msg::Picker(action) => self.handle_picker_msg(action),
        }
    }
}

impl Input for App {
    fn action(&self, event: crossterm::event::Event) -> Option<Msg> {
        if let crossterm::event::Event::Key(key) = event {
            // In picker mode, route all keys through the picker keymap.
            if self.picker.is_some() {
                if let Some(action) = self.picker_keymap.get_bound(&key) {
                    return Some(Msg::Picker(action));
                }
                return None;
            }

            if self.batch_output.visible().len() > 1
                && self.keymap.get(&key) == Some(&Action::FocusNext)
            {
                return Some(Msg::Action(Action::FocusNext));
            }

            // Forward input to process if one is running. Write directly
            // via RefCell so keystrokes don't trigger a render cycle.
            let Some(code) = self.codes.get(self.selected) else {
                // No code blocks; only quit actions are meaningful.
                if let Some(action) = self.keymap.get(&key) {
                    if action == &Action::Quit {
                        return Some(Msg::Action(action.clone()));
                    }
                }
                return None;
            };
            {
                let mut outputs = self.outputs.borrow_mut();
                if let Some(state) = outputs.get_mut(&code.id) {
                    if !state.done {
                        if let Some(exec) = &mut state.execution {
                            if let Some(bytes) = key_to_bytes(key) {
                                let _ = exec.process_mut().write(&bytes);
                                return None;
                            }
                        }
                    }
                }
            }

            if let Some(action) = self.keymap.get(&key) {
                return Some(Msg::Action(action.clone()));
            }

            if let Some(nav) = self.nav_keymap.get(&key) {
                return Some(Msg::Navigation(*nav));
            }
        }
        None
    }
}

impl App {
    /// Renders the compact file picker list in-place.
    fn render_picker<W: Write>(
        &self,
        out: &mut W,
        picker: &crate::apps::picker::PickerState,
    ) -> std::io::Result<()> {
        let prev = self.batch_output.previous_lines();
        if prev > 0 {
            write!(out, "\x1b[{prev}A\r\x1b[J")?;
        } else {
            write!(out, "\r\x1b[J")?;
        }

        let reset = "\x1b[0m";
        let muted = ansi_fg(self.theme.muted);
        let active = ansi_fg(self.theme.active);
        let inactive = ansi_fg(self.theme.info_background);
        let foreground = ansi_fg(self.theme.foreground);
        let mut lines: u16 = 0;

        // Header: "File: <query>  (matched/total)"
        let has_query = !picker.query.is_empty();
        let query_display = if has_query {
            picker.query.as_str()
        } else {
            "type to filter..."
        };
        let query_color = if has_query { &active } else { &muted };
        write!(
            out,
            "File: {query_color}{query_display}{reset}  ({}/{})\r\n",
            picker.matches.len(),
            picker.files.len(),
        )?;
        lines += 1;

        // List entries
        if picker.matches.is_empty() {
            write!(out, "{muted}  (no matching files){reset}\r\n")?;
            lines += 1;
        } else {
            for (i, &file_idx) in picker.matches.iter().enumerate() {
                let is_sel = i == picker.selected;
                let file = &picker.files[file_idx];
                if is_sel {
                    write!(
                        out,
                        "{active}\u{25b8} {}  {}{reset}\r\n",
                        i + 1,
                        file.display
                    )?;
                } else {
                    write!(out, "{foreground}  {}  {}{reset}\r\n", i + 1, file.display)?;
                }
                lines += 1;
            }
        }

        // Footer: key hints, matching theme.shortcuts() styling.
        // Key symbols in active, descriptions in muted, separators in inactive.
        let shortcuts: &[(&str, &str)] = &[
            ("\u{2191}\u{2193}", "move"),
            ("\u{21b5}", "open"),
            ("esc", "cancel"),
        ];
        for (i, (key, desc)) in shortcuts.iter().enumerate() {
            if i > 0 {
                write!(out, "{inactive}  {reset}")?;
            }
            write!(out, "{active}{key}{reset} {muted}{desc}{reset}")?;
        }
        write!(out, "\r\n")?;
        lines += 1;

        self.batch_output.set_previous_lines(lines);
        Ok(())
    }

    fn code_label(&self, id: CodeId) -> String {
        self.codes
            .iter()
            .find(|code| code.id == id)
            .map(|code| {
                if code.name.is_empty() {
                    code.id.to_string()
                } else {
                    code.name.clone()
                }
            })
            .unwrap_or_else(|| id.to_string())
    }

    fn render_plain_transcript<W: Write>(&self, out: &mut W, id: CodeId) -> io::Result<()> {
        let label = self.code_label(id);
        writeln!(out, "==> {label} [block {id}]")?;
        if let Some(task) = self.outputs.borrow().get(&id) {
            let text = task.parser.inline_contents(false);
            for line in text.lines {
                for span in line.spans {
                    write!(out, "{}", span.content)?;
                }
                writeln!(out)?;
            }
            match task.exit_code {
                Some(exit_code) => writeln!(out, "<== {label} exited with code {exit_code}")?,
                None => writeln!(out, "<== {label} failed to start")?,
            }
        } else {
            writeln!(out, "<== {label} produced no output")?;
        }
        Ok(())
    }

    fn render_terminal_transcript<W: Write>(&self, out: &mut W, id: CodeId) -> io::Result<()> {
        let label = self.code_label(id);
        let active = ansi_fg(self.theme.active);
        let reset = "\x1b[0m";
        writeln!(out, "\n{active}==> {label} [block {id}]{reset}")?;
        if let Some(task) = self.outputs.borrow().get(&id) {
            let text = task.parser.inline_contents(false);
            for line in text.lines {
                write!(out, "  ")?;
                for span in line.spans {
                    write!(out, "{}{}{}", ansi_style(span.style), span.content, reset)?;
                }
                writeln!(out)?;
            }
            let (symbol, color, result) = match task.exit_code {
                Some(0) => (
                    crate::apps::config::SUCCESS_SYMBOL,
                    ansi_fg(self.theme.success),
                    "exited with code 0".to_string(),
                ),
                Some(exit_code) => (
                    crate::apps::config::ERROR_SYMBOL,
                    ansi_fg(self.theme.error),
                    format!("exited with code {exit_code}"),
                ),
                None => (
                    crate::apps::config::ERROR_SYMBOL,
                    ansi_fg(self.theme.error),
                    "failed to start".to_string(),
                ),
            };
            writeln!(out, "  {color}{symbol} {result}{reset}")?;
        }
        Ok(())
    }

    fn render_plain<W: Write>(&self, out: &mut W) -> io::Result<()> {
        for batch in self.batch_output.take_pending() {
            for id in batch {
                self.render_plain_transcript(out, id)?;
            }
        }
        Ok(())
    }

    fn render_batch_tabs<W: Write>(&self, out: &mut W) -> io::Result<()> {
        let visible = self.batch_output.visible();
        if visible.len() <= 1 {
            return Ok(());
        }

        let selected = self.codes.get(self.selected).map(|code| code.id);
        let outputs = self.outputs.borrow();
        let active = ansi_fg(self.theme.active);
        let muted = ansi_fg(self.theme.muted);
        let reset = "\x1b[0m";
        write!(out, "  Parallel:")?;
        for &id in visible {
            let status = match outputs.get(&id) {
                Some(task) if !task.done => "●",
                Some(task) if task.exit_code == Some(0) => crate::apps::config::SUCCESS_SYMBOL,
                Some(_) => crate::apps::config::ERROR_SYMBOL,
                None => "·",
            };
            let color = if selected == Some(id) {
                &active
            } else {
                &muted
            };
            write!(out, " {color}[{} {status}]{reset}", self.code_label(id))?;
        }
        writeln!(out, "  {active}tab{reset} {muted}switch{reset}")?;
        Ok(())
    }
}

impl Output for App {
    fn is_alternate_screen(&self) -> bool {
        if !self.batch_output.is_terminal() {
            return false;
        }
        if self.codes.is_empty() {
            return false;
        }
        let id = self.codes[self.selected].id;
        self.outputs
            .borrow()
            .get(&id)
            .map(|s| s.is_alternate_screen())
            .unwrap_or(false)
    }

    fn render<W: Write>(&self, out: &mut W) -> std::io::Result<()> {
        if !self.batch_output.is_terminal() {
            return self.render_plain(out);
        }

        if let Some(picker) = &self.picker {
            return self.render_picker(out, picker);
        }

        let prev = self.batch_output.previous_lines();
        if prev > 0 {
            write!(out, "\x1b[{prev}A\r\x1b[J")?;
        }

        let term_width = crossterm::terminal::size()
            .map(|(w, _)| w as usize)
            .unwrap_or(80);
        let pending = self.batch_output.take_pending();
        let committed = !pending.is_empty();
        if committed {
            let mut transcript = LineCounter::new(out, term_width as u16);
            for batch in pending {
                for id in batch {
                    self.render_terminal_transcript(&mut transcript, id)?;
                }
            }
        }

        if committed && self.batch_output.visible().is_empty() {
            self.batch_output.set_previous_lines(0);
            return Ok(());
        }

        let mut counter = LineCounter::new(out, term_width as u16);
        if self.codes.is_empty() {
            writeln!(counter, "No code blocks found.")?;
            self.batch_output.set_previous_lines(counter.lines);
            return Ok(());
        }

        let code = &self.codes[self.selected];
        let total = self.codes.len();
        let index = self.selected;
        let info_bg = ansi_bg(self.theme.info_background);
        let info_fg = ansi_fg(self.theme.info_foreground);
        let active_fg = ansi_fg(self.theme.active);
        let reset = "\x1b[0m";

        self.render_batch_tabs(&mut counter)?;
        writeln!(counter)?;
        let deps_display = format!("{}", code.deps);
        let deps_str = if deps_display.is_empty() {
            String::new()
        } else {
            let muted = ansi_fg(self.theme.muted);
            format!(" {muted}{deps_display}")
        };
        // Note: `deps_display` has no trailing reset so `info_bg` stays active.
        // The `{reset}` in the write below closes all styles.
        let language = upmd_runner::find_language(&code.language);
        write!(
            counter,
            "{info_bg}{info_fg} [{active_fg}{idx}{reset}{info_bg}{info_fg}/{total}]{info_fg} {lang}{deps}{reset}",
            idx = index + 1,
            total = total,
            lang = language.name,
            deps = deps_str,
        )?;
        writeln!(counter)?;

        // Code Preview
        let preview_lines = self.config.cli.preview_lines.unwrap_or(CLI_PREVIEW_LINES);
        let total_lines = code.content.lines().count();
        let excerpt: String = code
            .content
            .lines()
            .take(preview_lines)
            .collect::<Vec<_>>()
            .join("\n");
        let highlighted = self.theme.highlight(&excerpt, &code.language);
        for hl_line in &highlighted.lines {
            write!(counter, "  ")?;
            for span in &hl_line.spans {
                write!(
                    counter,
                    "{}{}{}",
                    ansi_style(span.style),
                    span.content,
                    reset
                )?;
            }
            writeln!(counter)?;
        }

        if total_lines > preview_lines {
            let remaining = total_lines - preview_lines;
            let muted = ansi_fg(self.theme.muted);
            writeln!(counter, "{muted}  ... {} more lines{reset}", remaining)?;
        }

        // Separator (term_width-1 avoids cursor wrap on the last column)
        let muted = ansi_fg(self.theme.muted);
        let sep_width = term_width.saturating_sub(1);
        writeln!(counter, "\n{muted}{}{reset}", "─".repeat(sep_width))?;

        // Command Output
        let outputs = self.outputs.borrow();
        if let Some(state) = outputs.get(&code.id) {
            let text = state.parser.inline_contents(!state.done);
            for line in text.lines {
                write!(counter, "  ")?;
                for span in line.spans {
                    write!(counter, "{}", ansi_style(span.style))?;
                    write!(counter, "{}", span.content)?;
                    write!(counter, "{}", reset)?;
                }
                writeln!(counter)?;
            }

            if state.done {
                if let Some(exit_code) = state.exit_code {
                    let (sym, color) = if exit_code == 0 {
                        (
                            crate::apps::config::SUCCESS_SYMBOL,
                            ansi_fg(self.theme.success),
                        )
                    } else {
                        (crate::apps::config::ERROR_SYMBOL, ansi_fg(self.theme.error))
                    };
                    writeln!(
                        counter,
                        "\n  {color}{sym} exited with code {exit_code}{reset}"
                    )?;
                }
            }
        }

        // Footer is only shown when no block has run yet (pure navigation).
        let has_output = outputs.contains_key(&code.id);
        let is_executing = outputs.get(&code.id).map(|s| !s.done).unwrap_or(false);
        drop(outputs);
        if !is_executing && !has_output {
            let keys_color = ansi_style(self.theme.active_fg_style());
            let desc_color = ansi_style(self.theme.muted_style());
            writeln!(counter)?;
            writeln!(
                counter,
                "  {keys_color}j/k {desc_color}navigate  {keys_color}enter {desc_color}run  {keys_color}q {desc_color}quit{reset}"
            )?;
        }

        self.batch_output.set_previous_lines(counter.lines);
        Ok(())
    }
}

/// Counts rendered lines while passing bytes through to the inner writer
/// with CRLF conversion for raw-mode terminals.  Handles ANSI escape
/// sequences and line wrapping so prev_lines correctly reflects the number
/// of visual rows the card occupies.
struct LineCounter<'a, W: io::Write> {
    inner: &'a mut W,
    lines: u16,
    col: u16,
    term_width: u16,
    esc: EscState,
}

#[derive(PartialEq)]
enum EscState {
    Normal,
    Esc,
    Csi,
}

impl<'a, W: io::Write> LineCounter<'a, W> {
    fn new(inner: &'a mut W, term_width: u16) -> Self {
        Self {
            inner,
            lines: 0,
            col: 0,
            term_width,
            esc: EscState::Normal,
        }
    }
}

impl<'a, W: io::Write> io::Write for LineCounter<'a, W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut last = 0;
        let mut i = 0;
        while i < buf.len() {
            let b = buf[i];
            match self.esc {
                EscState::Esc => {
                    self.esc = if b == b'[' {
                        EscState::Csi
                    } else {
                        EscState::Normal
                    };
                    i += 1;
                    continue;
                }
                EscState::Csi => {
                    if (0x40..=0x7E).contains(&b) {
                        self.esc = EscState::Normal;
                    }
                    i += 1;
                    continue;
                }
                EscState::Normal => {}
            }
            match b {
                b'\x1b' => self.esc = EscState::Esc,
                b'\r' => self.col = 0,
                b'\n' => {
                    self.lines += 1;
                    self.col = 0;
                    // Emit the content before this newline, with CRLF
                    // conversion for raw-mode terminals.
                    self.inner.write_all(&buf[last..i])?;
                    if i == 0 || buf[i - 1] != b'\r' {
                        self.inner.write_all(b"\r")?;
                    }
                    self.inner.write_all(b"\n")?;
                    last = i + 1;
                }
                0x80..=0xBF => {} // UTF-8 continuation, no column advance
                _ => {
                    let width = if b < 0x80 {
                        1u16
                    } else {
                        // Decode the full character from the leading byte
                        let slice = &buf[i..];
                        let width = std::str::from_utf8(slice)
                            .ok()
                            .and_then(|s| s.chars().next())
                            .map(|c| unicode_width::UnicodeWidthChar::width(c).unwrap_or(0) as u16)
                            .unwrap_or(1);
                        // Skip continuation bytes of this multi-byte character
                        let char_len = slice
                            .first()
                            .map(|&first| {
                                let l = first.leading_ones() as usize;
                                if l > 1 && l < 7 {
                                    l.min(slice.len())
                                } else {
                                    1
                                }
                            })
                            .unwrap_or(1);
                        i += char_len.saturating_sub(1);
                        width
                    };
                    self.col += width;
                    if self.col >= self.term_width {
                        self.lines += 1;
                        self.col = 0;
                    }
                }
            }
            i += 1;
        }
        self.inner.write_all(&buf[last..])?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use upmd_parser::Parser;

    fn make_config() -> AppConfig {
        AppConfig::new(crate::apps::config::ConfigArgs {
            file: None,
            theme: "base16-ocean.dark".into(),
            capture_state: false,
            block: None,
            yes: false,
            all: false,
            tick_rate: 66,
            tui: crate::apps::config::TuiConfig::default(),
            cli: crate::apps::config::CliConfig::default(),
            transparent: false,
            keymap: crate::apps::config::KeymapConfig::default(),
            binaries: HashMap::new(),
            working_dir: None,
        })
    }

    fn make_two_block_app() -> App {
        let input = r#"# First

```python
print("hello")
```

## Second

```bash
echo world
```
"#;
        let doc = upmd_parser::new().parse(input);
        App::new(doc, make_config())
    }

    fn make_parallel_app() -> App {
        let input = "\
```sh [name:a]\n:\n```\n\
```sh [name:b]\n:\n```\n\
```sh [name:target, deps:\"a | b\"]\n:\n```\n";
        App::new(upmd_parser::new().parse(input), make_config())
    }

    fn insert_completed_output(app: &App, id: CodeId, text: &str, exit_code: i32) {
        let mut output = task::Task::new(80, 24, 1024);
        exec::handle_stream(&mut output, &Stream::Out(text.to_string()));
        exec::handle_stream(&mut output, &Stream::Exit(exit_code));
        exec::handle_stream(&mut output, &Stream::End);
        app.outputs.borrow_mut().insert(id, output);
    }

    #[test]
    fn test_write_card_contains_block_header() {
        let app = make_two_block_app();
        let mut buf = Vec::new();
        app.render(&mut buf).unwrap();
        let out = String::from_utf8(buf).unwrap();
        // ANSI codes split "[1/2]", so check for the index pattern instead.
        assert!(out.contains("/2]"), "card should show block index");
        assert!(out.contains("Python"), "card should show language");
    }

    #[test]
    fn test_write_card_contains_code_and_separator() {
        let app = make_two_block_app();
        let mut buf = Vec::new();
        app.render(&mut buf).unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(out.contains("print"), "card should contain code content");
        assert!(out.contains('─'), "card should contain separator line");
    }

    #[test]
    fn test_create_with_no_code_blocks_shows_message() {
        let mut app = App::new(
            upmd_parser::Document {
                nodes: vec![],
                codes: Vec::new(),
                headings: Vec::new(),
                nodes_state: upmd_parser::NodesState::Full,
            },
            make_config(),
        );
        let _ = app.create();
        assert_eq!(app.codes.len(), 0);
    }

    #[test]
    fn test_write_footer_contains_help_text() {
        let app = make_two_block_app();
        let mut buf = Vec::new();
        app.render(&mut buf).unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(out.contains("j/k"), "footer should show navigation");
        assert!(out.contains("enter"), "footer should show run key");
        assert!(out.contains("q"), "footer should show quit key");
    }

    #[test]
    fn test_navigation_updates_selected() {
        let mut app = make_two_block_app();
        assert_eq!(app.selected, 0, "starts at first block");

        app.handle_nav(Navigation::Next);
        assert_eq!(app.selected, 1, "navigates to second block");

        app.handle_nav(Navigation::Prev);
        assert_eq!(app.selected, 0, "navigates back to first block");
    }

    #[test]
    fn test_navigation_first_and_last() {
        let mut app = make_two_block_app();
        app.handle_nav(Navigation::Last);
        assert_eq!(app.selected, 1, "goes to last block");

        app.handle_nav(Navigation::First);
        assert_eq!(app.selected, 0, "goes to first block");
    }

    #[test]
    fn test_navigation_clamps() {
        let mut app = make_two_block_app();
        // Already at 0, prev should stay
        app.handle_nav(Navigation::Prev);
        assert_eq!(app.selected, 0);

        // Go to last, next should stay
        app.handle_nav(Navigation::Last);
        app.handle_nav(Navigation::Next);
        assert_eq!(app.selected, 1);
    }

    #[test]
    fn test_create_with_empty_codes() {
        let mut app = App::new(
            upmd_parser::Document {
                nodes: vec![],
                codes: Vec::new(),
                headings: Vec::new(),
                nodes_state: upmd_parser::NodesState::Full,
            },
            make_config(),
        );
        let result = app.create();
        assert!(result.is_none(), "empty codes returns None");
    }

    #[test]
    fn test_cli_config_preview_lines_used() {
        let mut config = make_config();
        config.cli.preview_lines = Some(1);
        let input = r#"# Test

```python
print("1")
print("2")
print("3")
```
"#;
        let doc = upmd_parser::new().parse(input);
        let app = App::new(doc, config);
        let mut buf = Vec::new();
        app.render(&mut buf).unwrap();
        let out = String::from_utf8(buf).unwrap();
        // Only one line should be shown + "2 more lines" indicator.
        assert!(
            out.contains("2 more lines"),
            "should respect preview_lines=1; got:\n{out}"
        );
    }

    #[test]
    fn test_cli_preview_highlight_has_no_embedded_line_endings() {
        let mut config = make_config();
        config.cli.preview_lines = Some(2);
        let input = r#"# Test

```python
print("1")
print("2")
```
"#;
        let doc = upmd_parser::new().parse(input);
        let app = App::new(doc, config);
        let mut buf = Vec::new();
        app.render(&mut buf).unwrap();
        let out = String::from_utf8(buf).unwrap();

        assert!(
            !out.contains("print(\"1\")\n\x1b[0m\n"),
            "highlight span leaked a line ending into CLI output:\n{out}"
        );
    }

    #[test]
    fn test_execute_with_wrong_id_returns_none() {
        let mut app = make_two_block_app();
        let wrong_id: upmd_parser::nodes::CodeId = 9999;
        let result = app.execute(wrong_id);
        assert!(result.is_none(), "execute with unknown id returns None");
    }

    #[test]
    fn test_empty_codes_action_does_not_panic() {
        let app = App::new(
            upmd_parser::Document {
                nodes: vec![],
                codes: Vec::new(),
                headings: Vec::new(),
                nodes_state: upmd_parser::NodesState::Full,
            },
            make_config(),
        );
        // Simulate a quit keypress on an empty document.
        let event = crossterm::event::Event::Key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('q'),
            crossterm::event::KeyModifiers::empty(),
        ));
        let _ = app.action(event);
    }

    #[test]
    fn test_empty_all_plan_does_not_panic() {
        let mut app = App::new(
            upmd_parser::Document {
                nodes: vec![],
                codes: Vec::new(),
                headings: Vec::new(),
                nodes_state: upmd_parser::NodesState::Full,
            },
            make_config(),
        );
        assert!(app.start_all().is_none());
    }

    #[test]
    fn test_empty_codes_create_with_yes_does_not_panic() {
        let mut config = make_config();
        config.yes = true;
        let mut app = App::new(
            upmd_parser::Document {
                nodes: vec![],
                codes: Vec::new(),
                headings: Vec::new(),
                nodes_state: upmd_parser::NodesState::Full,
            },
            config,
        );
        let result = app.create();
        assert!(
            result.is_none(),
            "create with --yes and no codes returns None"
        );
    }

    #[test]
    fn test_write_card_twice_gives_same_output() {
        let app = make_two_block_app();
        let mut buf1 = Vec::new();
        app.render(&mut buf1).unwrap();
        // Reset anchor so the second render doesn't emit a move-up escape
        // sequence.  This simulates two independent render sessions.
        app.batch_output.set_previous_lines(0);
        let mut buf2 = Vec::new();
        app.render(&mut buf2).unwrap();
        assert_eq!(
            String::from_utf8(buf1).unwrap(),
            String::from_utf8(buf2).unwrap(),
            "write_card is deterministic"
        );
    }

    #[test]
    fn test_write_card_different_blocks() {
        let mut app = make_two_block_app();

        // First block card
        let mut buf1 = Vec::new();
        app.render(&mut buf1).unwrap();
        let card1 = String::from_utf8(buf1).unwrap();

        // Navigate to second block and get its card
        app.selected = 1;
        let mut buf2 = Vec::new();
        app.render(&mut buf2).unwrap();
        let card2 = String::from_utf8(buf2).unwrap();

        assert!(card1.contains("print"), "first block contains python code");
        assert!(card2.contains("echo"), "second block contains bash code");
        assert_ne!(card1, card2, "different blocks produce different cards");
    }

    #[test]
    fn non_terminal_parallel_output_is_plain_and_deterministic() {
        let mut app = make_parallel_app();
        insert_completed_output(&app, 1, "from a\n", 0);
        insert_completed_output(&app, 2, "from b\n", 0);
        app.batch_output.set_terminal(false);
        app.batch_output.select(&[1, 2]);
        app.batch_output.track(1);
        app.batch_output.track(2);
        app.batch_output
            .complete(&WorkflowTransition::NextBatch(vec![3]));

        let mut buf = Vec::new();
        app.render(&mut buf).unwrap();
        let output = String::from_utf8(buf).unwrap();
        assert!(!output.contains('\u{1b}'));
        assert!(output.find("==> a").unwrap() < output.find("==> b").unwrap());
        assert!(output.contains("from a"));
        assert!(output.contains("from b"));
        assert!(output.contains("<== a exited with code 0"));
        assert!(output.contains("<== b exited with code 0"));

        let mut second_render = Vec::new();
        app.render(&mut second_render).unwrap();
        assert!(second_render.is_empty());
    }

    #[test]
    fn terminal_parallel_output_supports_focus_and_commits_every_block() {
        let mut app = make_parallel_app();
        insert_completed_output(&app, 1, "from a\n", 0);
        insert_completed_output(&app, 2, "from b\n", 0);
        app.batch_output.select(&[1, 2]);
        app.batch_output.track(1);
        app.batch_output.track(2);

        let mut live = Vec::new();
        app.render(&mut live).unwrap();
        let live = String::from_utf8(live).unwrap();
        assert!(live.contains("Parallel:"));
        assert!(live.contains("tab"));
        assert_eq!(app.codes[app.selected].id, 1);
        app.focus_next_batch_block();
        assert_eq!(app.codes[app.selected].id, 2);

        app.batch_output
            .complete(&WorkflowTransition::Finished { failed: false });
        app.batch_output.set_previous_lines(0);
        let mut committed = Vec::new();
        app.render(&mut committed).unwrap();
        let committed = String::from_utf8(committed).unwrap();
        assert!(committed.find("==> a").unwrap() < committed.find("==> b").unwrap());
        assert!(committed.contains("from a"));
        assert!(committed.contains("from b"));
        assert_eq!(app.batch_output.previous_lines(), 0);
    }
}
