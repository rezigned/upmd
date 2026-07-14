//! Shared execution engine used by both TUI and CLI frontends.
//!
//! Owns the per-code-block output state and provides shared functions
//! for running code, processing PTY output streams, and reloading documents.

use std::path::PathBuf;

use crossbeam_channel::Receiver;

use crate::apps::config::Envs;
use crate::apps::task::Task;
use crate::{pty::process::Size as PtySize, pty::stream::Stream, runner};
use std::collections::HashMap;
use upmd_parser::{nodes, CodeId, Parser};
use upmd_runtime::Cmd;

/// Executes a code block, populating the given [`Task`].
///
/// Returns a `Receiver` for stream messages on success,
/// or `None` on error (the error text is already written to the parser).
pub fn run_code(
    code: &nodes::Code,
    size: PtySize,
    envs: Envs,
    capture_state: bool,
    binaries: &HashMap<String, upmd_runner::RunnerOptions>,
    output: &mut Task,
    working_dir: Option<PathBuf>,
) -> Option<Receiver<Stream>> {
    output.reset();
    let cwd = match working_dir {
        Some(d) => d,
        None => match std::env::current_dir() {
            Ok(d) => d,
            Err(e) => {
                output
                    .parser
                    .parse(&format!("Failed to determine current directory: {e}"));
                output.done = true;
                return None;
            }
        },
    };
    match runner::execute(code, envs, cwd, size, capture_state, binaries) {
        Ok(exec) => {
            let rx = exec.receiver();
            output.execution = Some(exec);
            Some(rx)
        }
        Err(err) => {
            output.parser.parse(&err.to_string());
            output.done = true;
            None
        }
    }
}

/// Processes a single [`Stream`] message for the given [`Task`].
///
/// Returns `true` if the full view should be rebuilt (exit or end).
pub fn handle_stream(output: &mut Task, stream: &Stream) -> bool {
    let mut force_rebuild = false;
    match stream {
        Stream::Out(s) => {
            output.parser.parse(s);
        }
        Stream::Env(envs) => {
            output.captured_envs = Some(envs.clone());
        }
        Stream::Cwd(cwd) => {
            output.captured_cwd = Some(cwd.clone());
        }
        Stream::Exit(code) => {
            output.exit_code = Some(*code);
            output.done = true;
            force_rebuild = true;
        }
        Stream::End => {
            force_rebuild = !output.done;
            output.done = true;
        }
    }
    force_rebuild
}

/// Reads a file from disk and parses it into markdown nodes.
///
/// Returns the parsed document on success, or an error message on failure.
pub fn reload_document(file: Option<&str>) -> Result<upmd_parser::Document, String> {
    let content = match file {
        Some(path) => crate::reader::read_from_file(path).map_err(|e| format!("{e}"))?,
        None => return Err("No file path in config, cannot reload".into()),
    };
    let parser = upmd_parser::new();
    Ok(parser.parse(&content))
}

/// Merges captured environment variables into a session-wide list.
///
/// Updates existing entries in place and appends new ones.
pub fn merge_envs(dest: &mut Envs, captured: &Envs) {
    dest.extend(captured.iter().map(|(k, v)| (k.clone(), v.clone())));
}

/// Creates a stream command that forwards process output and control separately.
///
/// PTY output can be effectively infinite (`yes` is the canonical case), so
/// `Out` is best-effort on the low-priority queue. Lifecycle/state messages go
/// to the high-priority queue so `Exit`/`End` cannot sit behind stale output.
pub fn stream_rx<M: Clone + Send + 'static>(
    id: CodeId,
    rx: Receiver<Stream>,
    mk_msg: impl Fn(CodeId, Stream) -> M + Send + 'static,
) -> Cmd<M> {
    Cmd::priority_stream(move |output_tx, control_tx| {
        while let Ok(msg) = rx.recv() {
            if matches!(msg, Stream::Out(_)) {
                let _ = output_tx.try_send(mk_msg(id, msg));
            } else if control_tx.send(mk_msg(id, msg)).is_err() {
                break;
            }
        }
    })
}
