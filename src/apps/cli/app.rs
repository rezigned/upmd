use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    io::{self, Write},
    path::PathBuf,
};

use upmd_runtime::{
    runtimes::cli::{Input, Output},
    Cmd, Component,
};

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
    /// Advance to the next block after each completes (--all).
    auto_advance: Vec<CodeId>,
    /// Captured environment variables, seeded from the parent process and
    /// updated incrementally as each code block runs.
    envs: Envs,
    /// Captured working directory, seeded from the parent process and
    /// updated incrementally as each code block runs.
    cwd: PathBuf,
    /// Lines written by the last render pass. Used to emit a move-up/clear
    /// escape sequence so the next render replaces the card in-place.
    prev_lines: Cell<u16>,
    /// When true, the next render skips the move-up/clear, leaving previous
    /// execution output on screen. Set after execution completes, cleared
    /// after one render cycle.
    reset_anchor: Cell<bool>,
    /// When set, the app starts in file-picker mode and transitions to
    /// code-block mode after the user selects a file.
    picker: Option<crate::apps::picker::PickerState>,
    picker_keymap: DerivedConfig<crate::apps::picker::PickerAction>,
}

#[derive(Clone, KeyMap, Debug, PartialEq, Eq, Hash)]
pub enum Action {
    /// Executes the selected code block.
    #[key("enter")]
    Run,
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

        let auto_advance = if config.all {
            codes.iter().map(|c| c.id).collect()
        } else {
            Vec::new()
        };

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
            auto_advance,
            envs,
            cwd,
            prev_lines: Cell::new(0),
            reset_anchor: Cell::new(false),
            picker: None,
            picker_keymap: config.keymap.file_picker(),
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
        self.auto_advance = if self.config.all {
            self.codes.iter().map(|c| c.id).collect()
        } else {
            Vec::new()
        };
        self.outputs.borrow_mut().clear();
        self.prev_lines.set(0);
        self.reset_anchor.set(false);
    }

    fn auto_run_selected(&mut self) -> Option<Cmd<Msg>> {
        let id = self.codes.get(self.selected)?.id;
        self.execute(id)
    }

    fn execute(&mut self, id: CodeId) -> Option<Cmd<Msg>> {
        let code = self.codes.iter().find(|c| c.id == id)?;

        let (cols, rows) =
            crossterm::terminal::size().unwrap_or((PTY_DEFAULT_COLS, PTY_DEFAULT_ROWS));
        // Limit PTY size to leave room for our UI and prevent scrolling issues
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

        // Pass the accumulated env/cwd so state capture from prior blocks
        // is visible to later blocks in the same session.
        exec::run_code(
            code,
            size,
            self.envs.clone(),
            self.config.capture_state,
            &self.config.binaries,
            state,
            Some(self.cwd.clone()),
        )
        .map(|rx| exec::stream_rx(id, rx, Msg::StreamUpdate))
    }

    fn handle_stream_update(&mut self, id: CodeId, stream: Stream) -> Option<Cmd<Msg>> {
        // Pre-classify the stream so we can inspect by reference before moving
        // it into `exec::handle_stream`.
        let is_env = matches!(stream, Stream::Env(_));
        let is_cwd = matches!(stream, Stream::Cwd(_));
        let is_end = matches!(stream, Stream::End);
        let is_done = matches!(stream, Stream::End | Stream::Exit(_));

        self.apply_stream_to_output(id, &stream);
        self.apply_captured_state(id, is_env, is_cwd);

        // Execution finished. Reset the anchor so the next card renders
        // below this block's output rather than overwriting it.
        if is_done {
            self.reset_anchor.set(true);
        }

        // On completion, auto-advance or quit.
        if is_end && self.config.yes {
            return self.maybe_auto_advance(id);
        }
        None
    }

    /// Applies a stream message to the output state for the given block,
    /// writing PTY output directly to the terminal when in the alternate
    /// screen and resizing the PTY on alternate-screen transitions.
    fn apply_stream_to_output(&mut self, id: CodeId, stream: &Stream) {
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
            if was_alt || now_alt {
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
            let pty_size = if now_alt {
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

    /// Merges captured environment variables and cwd from a block into the
    /// session-wide state so later blocks inherit them.
    fn apply_captured_state(&mut self, id: CodeId, is_env: bool, is_cwd: bool) {
        if is_env {
            let outputs = self.outputs.borrow();
            if let Some(state) = outputs.get(&id) {
                if let Some(captured) = &state.captured_envs {
                    exec::merge_envs(&mut self.envs, captured);
                }
            }
        }
        if is_cwd {
            let outputs = self.outputs.borrow();
            if let Some(state) = outputs.get(&id) {
                if let Some(captured) = &state.captured_cwd {
                    self.cwd = PathBuf::from(captured);
                }
            }
        }
    }

    /// Auto-advances to the next pending block when `--yes` is enabled, or
    /// quits when all blocks are finished.
    fn maybe_auto_advance(&mut self, id: CodeId) -> Option<Cmd<Msg>> {
        if self.auto_advance.is_empty() {
            // Single block mode (--block --yes or --yes alone): quit when done.
            return Some(Cmd::quit());
        }
        if let Some(pos) = self.auto_advance.iter().position(|&i| i == id) {
            let next_pos = pos + 1;
            if next_pos < self.auto_advance.len() {
                let next = self.auto_advance[next_pos];
                if let Some(idx) = self.codes.iter().position(|c| c.id == next) {
                    self.selected = idx;
                }
                return self.execute(next);
            } else {
                // All blocks done. Auto-quit.
                return Some(Cmd::quit());
            }
        }
        None
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
            self.auto_run_selected()
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

    fn run(self) -> Result<()> {
        Ok(upmd_runtime::runtimes::cli::run(self)?)
    }
}

impl Component for App {
    type Msg = Msg;

    fn create(&mut self) -> Option<Cmd<Self::Msg>> {
        if self.picker.is_some() {
            return None;
        }
        if self.config.block.is_some() && self.config.yes {
            self.auto_run_selected()
        } else if !self.auto_advance.is_empty() && self.config.yes {
            let first = self.auto_advance[0];
            self.execute(first)
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
        let prev = self.prev_lines.get();
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

        self.prev_lines.set(lines);
        Ok(())
    }
}

impl Output for App {
    fn is_alternate_screen(&self) -> bool {
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
        // In picker mode, render the compact file picker list.
        if let Some(picker) = &self.picker {
            return self.render_picker(out, picker);
        }

        // Always overwrite the previous card in-place, even on the final
        // render after execution completes.  The reset_anchor flag tells
        // us to set prev_lines to 0 afterwards, so the next navigation
        // renders fresh below rather than overwriting the output.
        let anchor_reset = self.reset_anchor.get();
        let prev = self.prev_lines.get();
        if prev > 0 {
            write!(out, "\x1b[{prev}A\r\x1b[J")?;
        }
        self.reset_anchor.set(false);

        let term_width = crossterm::terminal::size()
            .map(|(w, _)| w as usize)
            .unwrap_or(80);

        // Wrap the output in a line counter so we can track prev_lines
        // for the next render.
        let mut counter = LineCounter::new(out, term_width as u16);
        if self.codes.is_empty() {
            writeln!(counter, "No code blocks found.")?;
            self.prev_lines
                .set(if anchor_reset { 0 } else { counter.lines });
            return Ok(());
        }

        let code = &self.codes[self.selected];
        let total = self.codes.len();
        let index = self.selected;
        let info_bg = ansi_bg(self.theme.info_background);
        let info_fg = ansi_fg(self.theme.info_foreground);
        let active_fg = ansi_fg(self.theme.active);
        let reset = "\x1b[0m";

        writeln!(counter)?;
        write!(
            counter,
            "{info_bg}{info_fg} [{active_fg}{}{reset}{info_bg}{info_fg}/{}] ",
            index + 1,
            total
        )?;
        let language = upmd_runner::find_language(&code.language);
        writeln!(counter, "{info_fg} {} {reset}", language.name)?;
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

        self.prev_lines
            .set(if anchor_reset { 0 } else { counter.lines });
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
    fn test_empty_codes_auto_run_does_not_panic() {
        let mut app = App::new(
            upmd_parser::Document {
                nodes: vec![],
                codes: Vec::new(),
                headings: Vec::new(),
                nodes_state: upmd_parser::NodesState::Full,
            },
            make_config(),
        );
        let result = app.auto_run_selected();
        assert!(
            result.is_none(),
            "auto_run_selected with no codes returns None"
        );
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
        app.prev_lines.set(0);
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
}
