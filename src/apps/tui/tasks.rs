//! Tasks manager that owns output buffers and handles all PTY I/O.

use std::collections::HashMap;
use std::path::PathBuf;

use crossbeam_channel::Receiver;

use crate::apps::config::Envs;
use crate::apps::task::TaskStatus;
use crate::{apps::exec, apps::task::Task, pty::process::Size as PtySize, pty::stream::Stream};
use upmd_parser::{nodes, CodeId};

/// Manages the lifecycle of code block executions.
/// Owns output buffers and handles all PTY I/O.
pub struct Tasks {
    outputs: HashMap<CodeId, Task>,
}

impl Tasks {
    pub fn new() -> Self {
        Self {
            outputs: HashMap::new(),
        }
    }

    /// Starts a task. Creates a new [`Task`], spawns the process.
    /// Returns `Some(rx)` on success (caller should start a stream thread),
    /// `None` on error (error text is already written to the parser).
    pub fn run(
        &mut self,
        code: &nodes::Code,
        size: PtySize,
        envs: Envs,
        capture_state: bool,
        binaries: &HashMap<String, upmd_runner::RunnerOptions>,
        working_dir: Option<PathBuf>,
    ) -> Option<Receiver<Stream>> {
        let task = self
            .outputs
            .entry(code.id)
            .or_insert_with(|| Task::new(size.width, size.height, 500));
        task.reset();

        match exec::run_code(code, size, envs, capture_state, binaries, task, working_dir) {
            Some(rx) => {
                task.dirty = true;
                Some(rx)
            }
            None => None,
        }
    }

    /// Handles a stream message from a running task.
    /// Returns `true` if the preview should be rebuilt.
    pub fn handle_stream(&mut self, id: CodeId, stream: &Stream) -> bool {
        let Some(task) = self.outputs.get_mut(&id) else {
            return false;
        };
        let is_out = matches!(stream, Stream::Out(_));
        let force_rebuild = exec::handle_stream(task, stream);
        if is_out {
            task.dirty = true;
        }
        force_rebuild
    }

    /// Sends raw bytes to a task's PTY stdin.
    pub fn send_input(&mut self, id: CodeId, bytes: &[u8]) {
        if let Some(task) = self.outputs.get_mut(&id) {
            if let Some(exec) = &mut task.execution {
                if !task.done {
                    let _ = exec.process_mut().write(bytes);
                }
            }
        }
    }

    /// Sends text to a task's PTY as if typed character-by-character.
    pub fn send_text(&mut self, id: CodeId, text: &str) {
        if let Some(task) = self.outputs.get_mut(&id) {
            if let Some(exec) = &mut task.execution {
                if !task.done {
                    for ch in text.chars() {
                        let mut b = [0; 4];
                        let encoded = ch.encode_utf8(&mut b);
                        let _ = exec.process_mut().write(encoded.as_bytes());
                    }
                }
            }
        }
    }

    /// Resets scroll for a task (only when an execution is active).
    pub fn reset_scroll(&mut self, id: CodeId) {
        if let Some(task) = self.outputs.get_mut(&id) {
            if task.execution.is_some() && !task.done {
                task.scroll = 0;
                task.parser.scroll(0);
            }
        }
    }

    /// Resizes all running PTYs and parser screens.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        for task in self.outputs.values_mut() {
            Self::resize_task_buffer(task, cols, rows);
        }
    }

    /// Resizes one running PTY and parser screen.
    pub fn resize_task(&mut self, id: CodeId, cols: u16, rows: u16) {
        if let Some(task) = self.outputs.get_mut(&id) {
            Self::resize_task_buffer(task, cols, rows);
        }
    }

    fn resize_task_buffer(task: &mut Task, cols: u16, rows: u16) {
        if task.parser.screen().size() == (rows, cols) {
            return;
        }
        task.parser.resize(rows, cols);
        if let Some(ref mut execution) = task.execution {
            execution.process_mut().resize(PtySize::from((cols, rows)));
        }
    }

    /// Accesses all tasks (needed by Preview for rebuild_view).
    pub fn buffers(&self) -> &HashMap<CodeId, Task> {
        &self.outputs
    }

    /// Returns a reference to a single task.
    pub fn get(&self, id: CodeId) -> Option<&Task> {
        self.outputs.get(&id)
    }

    /// Returns a mutable reference to a single task.
    pub fn get_mut(&mut self, id: CodeId) -> Option<&mut Task> {
        self.outputs.get_mut(&id)
    }

    /// Checks if a task exists for the given code ID.
    pub fn contains(&self, id: CodeId) -> bool {
        self.outputs.contains_key(&id)
    }

    /// Returns `true` if the task is done.
    pub fn is_done(&self, id: CodeId) -> bool {
        self.outputs.get(&id).is_some_and(|t| t.done)
    }

    /// Returns `true` if the task is waiting for input (cursor visible).
    pub fn is_waiting_for_input(&self, id: CodeId) -> bool {
        self.outputs
            .get(&id)
            .is_some_and(|t| t.execution.is_some() && !t.parser.screen().hide_cursor())
    }

    /// Returns a map of every code ID to its task lifecycle status.
    pub fn task_statuses(&self) -> HashMap<CodeId, TaskStatus> {
        self.outputs
            .iter()
            .map(|(&id, task)| (id, task.status()))
            .collect()
    }

    /// Returns `true` if any task has unprocessed output.
    pub fn is_dirty(&self) -> bool {
        self.outputs.values().any(|t| t.dirty)
    }

    /// Clears the dirty flag on all tasks.
    pub fn clear_dirty(&mut self) {
        for task in self.outputs.values_mut() {
            task.dirty = false;
        }
    }

    /// Removes all tasks and clears outputs.
    pub fn clear(&mut self) {
        self.outputs.clear();
    }
}
