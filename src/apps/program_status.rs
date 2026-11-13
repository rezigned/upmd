//! Child-process OSC 7501 status framing and ID scoping.
//!
//! Frames OSC 7501 sequences across PTY chunks, scopes child IDs under the
//! owning block, and replies to capability queries. Status sequences are
//! consumed while all other bytes pass through unchanged.

use std::{
    fmt::{Arguments, Write as _},
    io::{self, IsTerminal, Write},
};

use upmd_parser::CodeId;

const MAX_SEQUENCE: usize = 4096;
const MAX_ID: usize = 128;
const MAX_ID_SEGMENTS: usize = 5;
const OSC_PREFIX: &[u8] = b"\x1b]7501;";
const MAX_BODY: usize = MAX_SEQUENCE - OSC_PREFIX.len() - 1;
const QUERY_REPLY: &[u8] =
    b"\x1b]7501;?:states=idle,working,done,blocked,error:kinds=permission,question,auth\x1b\\";

/// Per-execution child status framing, forwarding, and record cleanup.
#[derive(Default)]
pub(crate) struct ChildStatus {
    state: ParseState,
    input: Vec<u8>,
    reported_id: Option<CodeId>,
}

enum ChildEvent<'a> {
    Output(&'a [u8]),
    Query,
    /// An OSC sequence with a block-scoped ID; other fields are passed through.
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

/// Returns whether a terminal write was attempted. Reporting is best-effort.
pub(crate) fn write(text: Arguments<'_>) -> bool {
    let stdout = io::stdout();
    if !stdout.is_terminal() {
        return false;
    }
    let mut stdout = stdout.lock();
    let _ = stdout.write_fmt(text);
    let _ = stdout.flush();
    true
}

impl ChildStatus {
    /// Consumes OSC 7501 sequences from a PTY chunk and optionally forwards
    /// the remaining bytes to `stdout`.
    ///
    /// While `active`, valid status reports are scoped under `upmd/block-{id}/child` and
    /// written to the outer terminal, and capability queries (`?`) invoke `reply`.
    pub(crate) fn feed(
        &mut self,
        id: CodeId,
        bytes: &[u8],
        active: bool,
        forward_to_stdout: bool,
        mut reply: impl FnMut(&[u8]),
    ) {
        let mut stdout = io::stdout();
        let mut reported = false;
        self.parse(id, bytes, active, |event| match event {
            ChildEvent::Output(bytes) if forward_to_stdout => {
                let _ = stdout.write_all(bytes);
            }
            ChildEvent::Output(_) => {}
            ChildEvent::Query => reply(QUERY_REPLY),
            ChildEvent::Report(sequence) => {
                reported |= write(format_args!("{sequence}"));
            }
        });
        if forward_to_stdout {
            let _ = stdout.flush();
        }
        if reported {
            self.reported_id = Some(id);
        }
    }

    pub(crate) fn reset(&mut self) {
        self.clear();
        self.state = ParseState::Ground;
        self.input.clear();
    }

    /// Clears records without losing framing for output queued after process exit.
    pub(crate) fn clear(&mut self) {
        if let Some(id) = self.reported_id.take() {
            write(format_args!(
                "\x1b]7501;state=clear:id=upmd/block-{id}/child\x1b\\"
            ));
        }
    }

    fn parse(
        &mut self,
        id: CodeId,
        bytes: &[u8],
        active: bool,
        mut emit: impl FnMut(ChildEvent<'_>),
    ) {
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
                            } else if let Some(report) = scope_id(id, &self.input) {
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
    let mut output = String::from("\x1b]7501;");
    let mut has_id = false;
    for (i, pair) in body.split(':').enumerate() {
        if i > 0 {
            output.push(':');
        }
        match pair.split_once('=') {
            Some((key, value)) if key.trim() == "id" => {
                let id = value.trim();
                let scoped = format!("upmd/block-{block}/child/{id}");
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
        let _ = write!(output, "{separator}id=upmd/block-{block}/child");
    }
    output.push_str("\x1b\\");
    (output.len() <= MAX_SEQUENCE).then_some(output)
}

fn valid_id(id: &str) -> bool {
    id.split('/').count() <= MAX_ID_SEGMENTS
        && id.split('/').all(|s| {
            (1..=32).contains(&s.len())
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'+' | b'-'))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect(decoder: &mut ChildStatus, bytes: &[u8]) -> (Vec<u8>, Vec<String>) {
        let mut output = Vec::new();
        let mut events = Vec::new();
        decoder.parse(1, bytes, true, |event| match event {
            ChildEvent::Output(bytes) => output.extend_from_slice(bytes),
            ChildEvent::Query => events.push("query".to_owned()),
            ChildEvent::Report(report) => events.push(report),
        });
        (output, events)
    }

    #[test]
    fn scopes_ids_and_rejects_invalid_reports() {
        let cases = [
            ("state=clear", Some("state=clear:id=upmd/block-1/child")),
            (
                "id=old:state=done: id = worker/sub ",
                Some("id=upmd/block-1/child/old:state=done: id =upmd/block-1/child/worker/sub"),
            ),
            (
                "state=done:future=literal id=untouched:id=worker:msg=SGk=::",
                Some("state=done:future=literal id=untouched:id=upmd/block-1/child/worker:msg=SGk=::"),
            ),
            ("state=clear:id=", None),
            ("state=clear:id=bad%2Fid", None),
            ("state=clear:id=a//b", None),
            ("state=clear:id=a/b/c/d/e/f", None),
            ("state=clear\x18outside", None),
        ];
        for (body, expected) in cases {
            let expected = expected.map(|b| format!("\x1b]7501;{b}\x1b\\"));
            assert_eq!(scope_id(1, body.as_bytes()), expected, "{body}");
        }
    }

    #[test]
    fn frames_status_and_filters_passthrough_across_chunks() {
        let input = b"before\x1b]7501;?\x07\x1b]0;title\x07\x1b]75\x07\x1b\x1b[0m\x1b]7501;state=blocked:id=worker\x1b\\after";
        let expected_out = b"before\x1b]0;title\x07\x1b]75\x07\x1b\x1b[0mafter";
        let expected_events = [
            "query",
            "\x1b]7501;state=blocked:id=upmd/block-1/child/worker\x1b\\",
        ];

        for split in 0..=input.len() {
            let mut decoder = ChildStatus::default();
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
        let mut decoder = ChildStatus::default();
        let mut out = Vec::new();
        decoder.parse(1, oversized.as_bytes(), false, |event| match event {
            ChildEvent::Output(b) => out.extend_from_slice(b),
            _ => panic!("inactive event"),
        });
        assert_eq!(out, b"tail");
    }
}
