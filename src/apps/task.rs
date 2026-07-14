//! Per-code-block execution state with display state (scroll, gutter, dirty).
//!
//! [`Task`] combines the low-level process handle and PTY parser with the
//! viewport state (scroll positions, dirty flag) that the TUI and CLI need
//! for rendering.  A `Task` is created when a code block is executed and
//! persists until the document is reloaded.

use crate::apps::config::Envs;
use crate::runner;
use upmd_pty::parser::Parser as PtyParser;

/// Lifecycle status of a code block execution.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum TaskStatus {
    Idle,
    /// Process is alive (may be waiting for input or producing output).
    Running,
    /// Process exited with code 0.
    Success,
    /// Process exited with a non-zero code or failed to start.
    Error,
}

/// Full state of one code-block execution: PTY output, scroll, and flags.
pub struct Task {
    pub parser: PtyParser,
    pub execution: Option<runner::Execution>,
    pub done: bool,
    pub exit_code: Option<i32>,
    pub captured_envs: Option<Envs>,
    pub captured_cwd: Option<String>,
    /// Output-mode scroll position (lines from bottom of full scrollback).
    pub scroll: u16,
    /// Inline output scroll offset (how many lines to skip from the top).
    pub inline_scroll: usize,
    /// Set when new output has arrived since last rebuild.
    pub dirty: bool,
}

impl Task {
    pub fn new(cols: u16, rows: u16, scrollback: usize) -> Self {
        Self {
            parser: PtyParser::new(rows, cols, scrollback),
            execution: None,
            done: false,
            exit_code: None,
            captured_envs: None,
            captured_cwd: None,
            scroll: 0,
            inline_scroll: 0,
            dirty: true,
        }
    }

    pub fn running(&self) -> bool {
        self.execution.is_some() && !self.done
    }

    pub fn done(&self) -> bool {
        self.done
    }

    pub fn status(&self) -> TaskStatus {
        if !self.done && self.execution.is_some() {
            TaskStatus::Running
        } else if self.done {
            if self.exit_code == Some(0) {
                TaskStatus::Success
            } else {
                TaskStatus::Error
            }
        } else {
            TaskStatus::Idle
        }
    }

    /// Resets execution state while preserving PTY parser dimensions.
    pub fn reset(&mut self) {
        self.execution = None;
        self.done = false;
        self.exit_code = None;
        self.captured_envs = None;
        self.captured_cwd = None;
        self.parser.reset();
        self.scroll = 0;
        self.inline_scroll = 0;
        self.dirty = true;
    }

    /// Returns whether the PTY output is currently in the alternate screen
    /// buffer (e.g. vim, less, htop are running).
    ///
    /// Delegates to the VT100 parser which tracks `?1049h`/`?1049l` (and
    /// `?47h`/`?47l`) sequences accurately.
    pub fn is_alternate_screen(&self) -> bool {
        self.parser.is_alternate_screen()
    }

    /// Scrolls the inline output up by one line.
    /// Advances the parser's scrollback so deeper history becomes visible.
    pub fn scroll_inline_up(&mut self, inline_max_lines: usize) {
        self.inline_scroll = self.inline_scroll.saturating_add(1);
        self.sync_inline_scrollback(inline_max_lines);
    }

    /// Scrolls the inline output down by one line.
    pub fn scroll_inline_down(&mut self, inline_max_lines: usize) {
        self.inline_scroll = self.inline_scroll.saturating_sub(1);
        self.sync_inline_scrollback(inline_max_lines);
    }

    /// Aligns the vt100 parser scrollback with `inline_scroll` so that
    /// `inline_contents()` returns content from the right depth.
    pub fn sync_inline_scrollback(&mut self, inline_max_lines: usize) {
        self.parser.scroll(usize::MAX);
        let max_scrollback = self.parser.screen().scrollback();
        let rows = self.parser.screen().size().0 as usize;
        let total_lines = max_scrollback + rows;
        // Clamp inline_scroll so we never scroll past the point where the
        // window would be at the very first line of output.
        let max_scroll = total_lines.saturating_sub(inline_max_lines);
        self.inline_scroll = self.inline_scroll.min(max_scroll);
        let n = self.inline_scroll.min(max_scrollback);
        self.parser.scroll(n);
    }
}
