//! OSC 7501 program status reporting, child framing, and ID scoping.
//!
//! Reports block lifecycle status (`upmd/block-N`), frames child OSC 7501
//! sequences across PTY chunks (scoped to `upmd/block-N/<child-id>`, or
//! `upmd/block-N` when the child sets no ID), and replies to child capability
//! queries. Status sequences are consumed while all other bytes pass through
//! unchanged.

use std::{
    fmt::{Arguments, Write as _},
    io::{self, IsTerminal, Write},
    sync::atomic::{AtomicBool, Ordering},
};

use upmd_parser::CodeId;

use crate::apps::task::TaskStatus;

static ENABLED: AtomicBool = AtomicBool::new(false);

pub(crate) fn set_enabled(enabled: bool) {
    ENABLED.store(enabled, Ordering::Relaxed);
}

const MAX_SEQUENCE: usize = 4096;
const MAX_ID: usize = 128;
const MAX_ID_SEGMENTS: usize = 5;
const OSC_PREFIX: &[u8] = b"\x1b]7501;";
const MAX_BODY: usize = MAX_SEQUENCE - OSC_PREFIX.len() - 1;
const QUERY_REPLY: &[u8] =
    b"\x1b]7501;?:states=idle,working,done,blocked,error:kinds=permission,question,auth\x1b\\";

/// Per-task OSC 7501 status reporting, child framing, and record cleanup.
pub(crate) struct ProgramStatus {
    id: CodeId,
    state: ParseState,
    input: Vec<u8>,
    reported: bool,
}

enum ChildEvent<'a> {
    Output(&'a [u8]),
    Query,
    /// An OSC 7501 body with a block-scoped ID; other fields are passed through.
    Report(String),
}

#[derive(Clone, Copy, Default)]
enum ParseState {
    #[default]
    Ground,
    /// Matched `&OSC_PREFIX[..len]` (`1..OSC_PREFIX.len()`).
    Prefix(usize),
    /// Inside an `OSC 7501` body.
    Status { escaped: bool, discard: bool },
}

/// Frames `body` as an `OSC 7501` sequence and writes it to `stdout` when attached to a TTY.
fn write_osc(body: Arguments<'_>) -> bool {
    let stdout = io::stdout();
    if !ENABLED.load(Ordering::Relaxed) || !stdout.is_terminal() {
        return false;
    }
    let mut stdout = stdout.lock();
    let _ = write!(stdout, "\x1b]7501;{body}\x1b\\");
    let _ = stdout.flush();
    true
}

impl ProgramStatus {
    pub(crate) fn new(id: CodeId) -> Self {
        Self {
            id,
            state: ParseState::Ground,
            input: Vec::new(),
            reported: false,
        }
    }

    /// Reports a block's lifecycle status to the outer terminal, clearing any
    /// active child record when the block is no longer running.
    pub(crate) fn report_task(&mut self, status: TaskStatus) {
        if status != TaskStatus::Running {
            self.clear();
        }
        let state = match status {
            TaskStatus::Idle => "idle",
            TaskStatus::Running => "working",
            TaskStatus::Success => "done",
            TaskStatus::Error => "error",
        };
        write_osc(format_args!(
            "state={state}:app=upmd:id=upmd/block-{}",
            self.id
        ));
    }

    /// Consumes OSC 7501 sequences from a PTY chunk and, when `passthrough` is
    /// true (CLI fullscreen/alternate-screen mode), writes the remaining bytes to `stdout`.
    ///
    /// While `active`, valid status reports are scoped under `upmd/block-{id}` (or
    /// `upmd/block-{id}/{child}` when the report sets an ID) and written to the outer
    /// terminal, and capability queries (`?`) invoke `reply`.
    pub(crate) fn process(
        &mut self,
        bytes: &[u8],
        active: bool,
        passthrough: bool,
        mut reply: impl FnMut(&[u8]),
    ) {
        let mut stdout = io::stdout();
        if !ENABLED.load(Ordering::Relaxed) {
            if passthrough {
                let _ = stdout.write_all(bytes);
                let _ = stdout.flush();
            }
            return;
        }
        let mut reported = false;
        self.parse(bytes, active, |event| match event {
            ChildEvent::Output(bytes) if passthrough => {
                let _ = stdout.write_all(bytes);
            }
            ChildEvent::Output(_) => {}
            ChildEvent::Query => reply(QUERY_REPLY),
            ChildEvent::Report(body) => {
                reported |= write_osc(format_args!("{body}"));
            }
        });
        if passthrough {
            let _ = stdout.flush();
        }
        self.reported |= reported;
    }

    pub(crate) fn reset(&mut self) {
        self.clear();
        self.state = ParseState::Ground;
        self.input.clear();
    }

    /// Clears records without losing framing for output queued after process exit.
    pub(crate) fn clear(&mut self) {
        if std::mem::take(&mut self.reported) {
            write_osc(format_args!("state=clear:id=upmd/block-{}", self.id));
        }
    }

    fn parse(&mut self, bytes: &[u8], active: bool, mut emit: impl FnMut(ChildEvent<'_>)) {
        let mut raw_start = 0;
        let mut index = 0;
        while index < bytes.len() {
            let byte = bytes[index];
            match self.state {
                ParseState::Ground => {
                    let Some(offset) = bytes[index..].iter().position(|&b| b == 0x1b) else {
                        break;
                    };
                    let esc = index + offset;
                    if raw_start < esc {
                        emit(ChildEvent::Output(&bytes[raw_start..esc]));
                    }
                    raw_start = esc + 1;
                    index = esc + 1;
                    self.state = ParseState::Prefix(1);
                    continue;
                }
                // Withhold bytes while matching `\x1b]7501;`.
                ParseState::Prefix(len) => {
                    raw_start = index + 1;
                    if byte == OSC_PREFIX[len] {
                        if len + 1 == OSC_PREFIX.len() {
                            self.input.clear();
                            self.state = ParseState::Status {
                                escaped: false,
                                discard: !active,
                            };
                        } else {
                            self.state = ParseState::Prefix(len + 1);
                        }
                    } else {
                        // Mismatch: flush the withheld prefix and re-check this byte in Ground.
                        emit(ChildEvent::Output(&OSC_PREFIX[..len]));
                        raw_start = index;
                        self.state = ParseState::Ground;
                        continue;
                    }
                }
                // Consume OSC 7501 body until BEL (`\x07`) or ST (`\x1b\`).
                ParseState::Status {
                    escaped,
                    mut discard,
                } => {
                    raw_start = index + 1;
                    let ended = (escaped && byte == b'\\') || byte == 0x07;
                    // Any ESC not followed by `\` is malformed.
                    discard |= !active || (escaped && byte != b'\\');

                    if ended {
                        self.state = ParseState::Ground;
                        if !discard {
                            if self.input.trim_ascii() == b"?" {
                                emit(ChildEvent::Query);
                            } else if let Some(report) = scope_id(self.id, &self.input) {
                                emit(ChildEvent::Report(report));
                            }
                        }
                    } else {
                        // Buffer payload bytes (hold `0x1b` in case it starts `\x1b\`).
                        if !discard && byte != 0x1b {
                            if self.input.len() < MAX_BODY {
                                self.input.push(byte);
                            } else {
                                discard = true;
                            }
                        }
                        self.state = ParseState::Status {
                            escaped: byte == 0x1b,
                            discard,
                        };
                    }
                }
            }
            index += 1;
        }
        if raw_start < bytes.len() {
            emit(ChildEvent::Output(&bytes[raw_start..]));
        }
    }
}

/// Replace only ID values, preserving field order and all other body bytes.
fn scope_id(block: CodeId, input: &[u8]) -> Option<String> {
    let body = std::str::from_utf8(input).ok()?;
    // Keep the forwarded body inside its OSC frame.
    if body.chars().any(char::is_control) {
        return None;
    }
    let mut output = String::with_capacity(body.len() + 32);
    let mut has_id = false;
    for (i, pair) in body.split(':').enumerate() {
        if i > 0 {
            output.push(':');
        }
        match pair.split_once('=') {
            Some((key, value)) if key.trim() == "id" => {
                let id = value.trim();
                let scoped = format!("upmd/block-{block}/{id}");
                if !valid_id(id) || scoped.len() > MAX_ID {
                    return None;
                }
                let _ = write!(output, "{key}={scoped}");
                has_id = true;
            }
            _ => output.push_str(pair),
        }
    }
    if !has_id {
        let separator = if body.is_empty() { "" } else { ":" };
        let _ = write!(output, "{separator}id=upmd/block-{block}");
    }
    (output.len() + OSC_PREFIX.len() + 2 <= MAX_SEQUENCE).then_some(output)
}

fn valid_id(id: &str) -> bool {
    id.split('/').count() <= MAX_ID_SEGMENTS
        && id.split('/').all(|s| {
            (1..=32).contains(&s.len())
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_.+-".contains(&b))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect(decoder: &mut ProgramStatus, bytes: &[u8]) -> (Vec<u8>, Vec<String>) {
        let mut output = Vec::new();
        let mut events = Vec::new();
        decoder.parse(bytes, true, |event| match event {
            ChildEvent::Output(bytes) => output.extend_from_slice(bytes),
            ChildEvent::Query => events.push("query".to_owned()),
            ChildEvent::Report(report) => events.push(report),
        });
        (output, events)
    }

    #[test]
    fn scopes_ids_and_rejects_invalid_reports() {
        let cases = [
            ("state=clear", Some("state=clear:id=upmd/block-1")),
            (
                "id=old:state=done: id = worker/sub ",
                Some("id=upmd/block-1/old:state=done: id =upmd/block-1/worker/sub"),
            ),
            (
                "state=done:future=literal id=untouched:id=worker:msg=SGk=::",
                Some("state=done:future=literal id=untouched:id=upmd/block-1/worker:msg=SGk=::"),
            ),
            ("state=clear:id=", None),
            ("state=clear:id=bad%2Fid", None),
            ("state=clear:id=a//b", None),
            ("state=clear:id=a/b/c/d/e/f", None),
            ("state=clear\x18outside", None),
        ];
        for (body, expected) in cases {
            assert_eq!(scope_id(1, body.as_bytes()).as_deref(), expected, "{body}");
        }
    }

    #[test]
    fn frames_status_and_filters_passthrough_across_chunks() {
        let input = b"before\x1b]7501;?\x07\x1b]0;title\x07\x1b]75\x07\x1b\x1b[0m\x1b]7501;state=blocked:id=worker\x1b\\after";
        let expected_out = b"before\x1b]0;title\x07\x1b]75\x07\x1b\x1b[0mafter";
        let expected_events = ["query", "state=blocked:id=upmd/block-1/worker"];

        for split in 0..=input.len() {
            let mut decoder = ProgramStatus::new(1);
            let (mut out, mut events) = collect(&mut decoder, &input[..split]);
            let (out2, events2) = collect(&mut decoder, &input[split..]);
            out.extend(out2);
            events.extend(events2);
            assert_eq!(out, expected_out, "split {split}");
            assert_eq!(events, expected_events, "split {split}");
        }

        // Oversized sequences and inactive (post-exit) streams still strip OSC 7501 from output.
        let oversized = format!(
            "\x1b]7501;msg={}\x07\x1b]7501;?\x07tail",
            "a".repeat(MAX_SEQUENCE)
        );
        let mut decoder = ProgramStatus::new(1);
        let mut out = Vec::new();
        decoder.parse(oversized.as_bytes(), false, |event| match event {
            ChildEvent::Output(b) => out.extend_from_slice(b),
            _ => panic!("inactive event"),
        });
        assert_eq!(out, b"tail");
    }
}
