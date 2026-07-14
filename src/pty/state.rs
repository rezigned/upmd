use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

#[cfg_attr(not(unix), allow(unused_imports))]
use anyhow::{Context, Result};
use crossbeam_channel::Sender;
use upmd_pty::ExitSignal;
use upmd_runner::FifoPaths;

use crate::apps::config::Envs;
use crate::pty::stream::Stream;

#[derive(Debug)]
struct CapturedState {
    env: Envs,
    cwd: Option<String>,
}

/// Reads state from the capture FIFO and sends it through the provided channel.
///
/// Two formats are supported:
/// - `version 1` structured state (non-shell runners: Python, Rust, etc.)
/// - Raw shell env dump with trailing cwd line (bash, fish, cmd, etc.)
pub fn read_state(
    state_dir: PathBuf,
    state_fifos: Option<FifoPaths>,
    tx: Sender<Stream>,
    child_exit: ExitSignal,
) {
    if let Some(fifos) = state_fifos {
        read_state_fifos(&fifos, &tx, &child_exit);
    }
    if let Err(e) = std::fs::remove_dir_all(&state_dir) {
        tracing::warn!("Failed to clean up state directory {state_dir:?}: {e}");
    }
}

fn read_state_fifos(fifos: &FifoPaths, tx: &Sender<Stream>, child_exit: &ExitSignal) {
    let Some(state_payload) = accept_fifo(&fifos.state_fifo, child_exit) else {
        return;
    };
    if let Some(state) = parse_state_output(&state_payload) {
        send_state(state, tx);
        return;
    }
    if let Some(state) = parse_shell_state_output(&state_payload) {
        send_state(state, tx);
    }
}

/// Creates FIFOs (or regular files on Windows) for state capture in the given
/// state directory.
#[cfg(unix)]
pub fn create_state_fifos(state_dir: &Path) -> Result<FifoPaths> {
    let state_fifo = state_dir.join("state.fifo");

    let _ = std::fs::remove_file(&state_fifo);

    create_fifo(&state_fifo)?;

    Ok(FifoPaths { state_fifo })
}

/// Creates FIFOs (or regular files on Windows) for state capture in the given
/// state directory.
#[cfg(not(unix))]
pub fn create_state_fifos(state_dir: &Path) -> Result<FifoPaths> {
    let state_fifo = state_dir.join("state");

    std::fs::write(&state_fifo, "")?;

    Ok(FifoPaths { state_fifo })
}

fn accept_fifo(path: &Path, child_exit: &ExitSignal) -> Option<String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;

        // Cancel thread: waits for the child to exit, then opens the FIFO for
        // writing (O_WRONLY|O_NONBLOCK) to unblock the reader's blocking open().
        // Uses O_NONBLOCK so it returns ENXIO if the reader already finished.
        let cancel_path = path.to_owned();
        let cancel_signal = child_exit.clone();
        std::thread::spawn(move || {
            cancel_signal.wait();
            let _ = std::fs::OpenOptions::new()
                .write(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(&cancel_path);
        });

        // Blocking open: waits until either the child opens the FIFO for
        // writing (normal data) or the cancel thread opens it (child exited
        // without writing).
        let mut content = String::new();
        let mut file = File::open(path).ok()?;
        if file.read_to_string(&mut content).is_err() {
            return None;
        }

        // Empty content means the cancel thread unblocked the FIFO because
        // the child exited without writing state data.
        if content.is_empty() {
            None
        } else {
            Some(content)
        }
    }

    #[cfg(windows)]
    {
        use std::time::{Duration, Instant};

        const STATE_TIMEOUT: Duration = Duration::from_secs(5);

        let start = Instant::now();
        loop {
            // Read before checking child_exit to avoid a race where the child
            // writes and exits between the flag check and the file read.
            let mut content = String::new();
            if let Ok(mut f) = File::open(path) {
                if f.read_to_string(&mut content).is_ok() && !content.is_empty() {
                    return Some(content);
                }
            }
            if child_exit.is_done() {
                // Final read attempt in case data was written just before exit.
                let mut content = String::new();
                if let Ok(mut f) = File::open(path) {
                    if f.read_to_string(&mut content).is_ok() && !content.is_empty() {
                        return Some(content);
                    }
                }
                return None;
            }
            if start.elapsed() > STATE_TIMEOUT {
                return None;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

fn send_state(state: CapturedState, tx: &Sender<Stream>) {
    let _ = tx.send(Stream::Env(state.env));
    if let Some(cwd) = state.cwd {
        // The state format parser already strips record newlines.
        // Only use trim_end_matches to guard trailing whitespace from edge cases.
        let cwd = cwd.trim_end_matches(&['\n', '\r'][..]).to_string();
        if !cwd.is_empty() {
            let _ = tx.send(Stream::Cwd(cwd));
        }
    }
}

/// Parses upmd state v1 output.
///
/// Format:
/// - first non-empty line: `version 1`
/// - `cwd "<escaped-path>"`
/// - `env "<escaped-key>" "<escaped-value>"`
///
/// The same quoted-string grammar is used for cwd, keys, and values.
///
/// Example:
/// ```text
/// version 1
/// cwd "/tmp/project"
/// env "PATH" "/usr/bin"
/// env "NAME" "hello\nworld"
/// ```
fn parse_state_output(content: &str) -> Option<CapturedState> {
    let mut env = Envs::new();
    let mut cwd = None;
    let mut saw_version = false;

    // State v1 records are line-based: `cwd "path"` and `env "key" "value"`.
    // Both key and value are quoted so env names do not need a separate grammar.
    for line in content.lines().map(|line| line.trim_end_matches('\r')) {
        if line.trim().is_empty() {
            continue;
        }
        if !saw_version {
            if line != "version 1" {
                return None;
            }
            saw_version = true;
            continue;
        }

        if let Some(raw_cwd) = line.strip_prefix("cwd ") {
            if cwd.is_some() {
                return None;
            }
            cwd = Some(parse_state_string(raw_cwd)?);
        } else {
            let raw_env = line.strip_prefix("env ")?;
            let (key, raw_value) = parse_state_string_pair(raw_env)?;
            env.insert(key, parse_state_string(raw_value)?);
        }
    }

    saw_version.then_some(CapturedState { env, cwd })
}

/// Parses state output produced by shell runners.
///
/// Shell runners (bash, fish, cmd, powershell) write env output followed by
/// a cwd path as the last line into the FIFO. This function strips the cwd
/// line and feeds the remaining env output to [`parse_env_output`].
///
/// Heuristics for detecting the cwd line:
/// - Line starting with `cwd "..."` (from earlier v1-aware shells)
/// - Line that does not contain `=`, `export `, or `declare -x ` (bare path)
fn parse_shell_state_output(content: &str) -> Option<CapturedState> {
    let mut lines: Vec<&str> = content
        .lines()
        .map(|line| line.trim_end_matches('\r'))
        .filter(|line| !line.trim().is_empty())
        .collect();

    let cwd = if let Some(last) = lines.last() {
        if let Some(raw_cwd) = last.strip_prefix("cwd ") {
            let cwd = parse_state_string(raw_cwd)?;
            lines.pop();
            Some(cwd)
        } else if !last.contains('=')
            && !last.starts_with("export ")
            && !last.starts_with("declare -x ")
        {
            let cwd = (*last).to_string();
            lines.pop();
            Some(cwd)
        } else {
            None
        }
    } else {
        None
    };

    let env = parse_env_output(&lines.join("\n"))?;
    Some(CapturedState { env, cwd })
}

fn parse_state_string(input: &str) -> Option<String> {
    let mut chars = input.chars();
    if chars.next()? != '"' {
        return None;
    }

    let mut output = String::new();
    while let Some(ch) = chars.next() {
        match ch {
            '"' => return chars.next().is_none().then_some(output),
            '\\' => match chars.next()? {
                '\\' => output.push('\\'),
                '"' => output.push('"'),
                'n' => output.push('\n'),
                'r' => output.push('\r'),
                't' => output.push('\t'),
                _ => return None,
            },
            _ => output.push(ch),
        }
    }

    None
}

fn parse_state_string_pair(input: &str) -> Option<(String, &str)> {
    let mut escaped = false;
    let mut chars = input.char_indices();
    if chars.next()?.1 != '"' {
        return None;
    }

    for (idx, ch) in chars {
        if escaped {
            escaped = false;
            continue;
        }
        match ch {
            '\\' => escaped = true,
            '"' => {
                let key = parse_state_string(&input[..=idx])?;
                let raw_value = input[idx + 1..].strip_prefix(' ')?;
                return Some((key, raw_value));
            }
            _ => {}
        }
    }

    None
}

#[cfg(unix)]
fn create_fifo(path: &Path) -> Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let c_path = CString::new(path.as_os_str().as_bytes())
        .with_context(|| format!("Path contains null byte: {}", path.display()))?;

    let ret = unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) };
    if ret != 0 {
        let err = std::io::Error::last_os_error();
        anyhow::bail!("Failed to create FIFO {}: {}", path.display(), err);
    }

    Ok(())
}

/// Parses environment variable output from various shell formats.
///
/// Supports:
/// - `export -p` format (bash/zsh): `declare -x KEY="value"` or `export KEY="value"`
/// - `env` format (POSIX sh): `KEY=value`
/// - `set -x` format (fish): Similar to env format
///
/// Double-quoted values have standard shell escape sequences unescaped
/// (`\n`, `\t`, `\\`, `\"`) so multi-line values are preserved correctly.
///
/// # Arguments
/// * `content` - Raw output from shell environment export command
///
/// # Returns
/// * `Some(Envs)` if any environment variables were parsed
/// * `None` if no valid environment variables were found
pub fn parse_env_output(content: &str) -> Option<Envs> {
    let mut env_map = Envs::new();
    let mut chars = content.chars().peekable();

    // We parse character-by-character so we can handle multi-line quoted
    // values that span more than one line in the output.
    while chars.peek().is_some() {
        // Collect one logical line (may span multiple physical lines if quoted)
        let line = collect_logical_line(&mut chars);
        if line.trim().is_empty() {
            continue;
        }

        // Strip common prefixes from different shell formats
        let clean_line = if let Some(s) = line.strip_prefix("export ") {
            s.trim_start().to_string()
        } else if let Some(s) = line.strip_prefix("declare -x ") {
            s.trim_start().to_string()
        } else {
            line.clone()
        };

        // Parse KEY=value
        let Some((key, raw_value)) = clean_line.split_once('=') else {
            continue;
        };

        // Validate key: must be a valid identifier (not empty, not starting with digit)
        if key.is_empty()
            || key.chars().next().is_none_or(|c| c.is_ascii_digit())
            || !key.chars().all(|c| c.is_alphanumeric() || c == '_')
        {
            continue;
        }

        let value = unescape_value(raw_value);
        env_map.insert(key.to_string(), value);
    }

    if env_map.is_empty() {
        None
    } else {
        Some(env_map)
    }
}

/// Collects one logical assignment line from the character stream.
///
/// A logical line ends at a newline that is not inside a quoted string.
/// This handles values like `KEY="line1\nline2"` where the shell has
/// encoded the newline as `\n` - those stay on one physical line - as
/// well as the rarer case where a shell emits a literal newline inside
/// a quoted value.
fn collect_logical_line(chars: &mut std::iter::Peekable<std::str::Chars>) -> String {
    let mut line = String::new();
    let mut in_double = false;
    let mut in_single = false;

    while let Some(&c) = chars.peek() {
        chars.next();
        match c {
            '"' if !in_single => {
                in_double = !in_double;
                line.push(c);
            }
            '\'' if !in_double => {
                in_single = !in_single;
                line.push(c);
            }
            '\\' if in_double => {
                // Keep the backslash and the next char verbatim for unescape_value
                line.push(c);
                if let Some(&next) = chars.peek() {
                    chars.next();
                    line.push(next);
                }
            }
            '\n' if !in_double && !in_single => break,
            _ => line.push(c),
        }
    }

    line
}

/// Unescapes a raw shell value string.
///
/// - Double-quoted: strips outer quotes and expands `\n`, `\t`, `\\`, `\"`
/// - Single-quoted: strips outer quotes, no escape processing
/// - Unquoted: returned as-is
fn unescape_value(raw: &str) -> String {
    let raw = raw.trim();

    if raw.len() >= 2 && raw.starts_with('"') && raw.ends_with('"') {
        // Double-quoted: iterate char by char expanding escape sequences
        let inner = &raw[1..raw.len() - 1];
        let mut out = String::with_capacity(inner.len());
        let mut chars = inner.chars();
        while let Some(c) = chars.next() {
            if c == '\\' {
                match chars.next() {
                    Some('n') => out.push('\n'),
                    Some('t') => out.push('\t'),
                    Some('\\') => out.push('\\'),
                    Some('"') => out.push('"'),
                    // Unknown escape: preserve both chars verbatim
                    Some(other) => {
                        out.push('\\');
                        out.push(other);
                    }
                    None => out.push('\\'),
                }
            } else {
                out.push(c);
            }
        }
        out
    } else if raw.len() >= 2 && raw.starts_with('\'') && raw.ends_with('\'') {
        // Single-quoted: strip quotes, no escape processing per POSIX
        raw[1..raw.len() - 1].to_string()
    } else {
        // Unquoted: return as-is
        raw.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_env_bash_export() {
        let input = r#"export PATH="/usr/bin:/bin"
export HOME="/home/user"
export TEST="value with spaces""#;

        let result = parse_env_output(input).unwrap();
        assert_eq!(result.get("PATH"), Some(&"/usr/bin:/bin".to_string()));
        assert_eq!(result.get("HOME"), Some(&"/home/user".to_string()));
        assert_eq!(result.get("TEST"), Some(&"value with spaces".to_string()));
    }

    #[test]
    fn test_parse_env_bash_declare() {
        let input = r#"declare -x PATH="/usr/bin:/bin"
declare -x HOME='/home/user'
declare -x SIMPLE=value"#;

        let result = parse_env_output(input).unwrap();
        assert_eq!(result.get("PATH"), Some(&"/usr/bin:/bin".to_string()));
        assert_eq!(result.get("HOME"), Some(&"/home/user".to_string()));
        assert_eq!(result.get("SIMPLE"), Some(&"value".to_string()));
    }

    #[test]
    fn test_parse_env_posix() {
        let input = r#"PATH=/usr/bin:/bin
HOME=/home/user
TEST=value"#;

        let result = parse_env_output(input).unwrap();
        assert_eq!(result.get("PATH"), Some(&"/usr/bin:/bin".to_string()));
        assert_eq!(result.get("HOME"), Some(&"/home/user".to_string()));
        assert_eq!(result.get("TEST"), Some(&"value".to_string()));
    }

    #[test]
    fn test_parse_env_empty() {
        let input = "";
        let result = parse_env_output(input);
        assert!(result.is_none());
    }

    #[test]
    fn test_parse_env_mixed_formats() {
        let input = r#"export PATH="/usr/bin"
declare -x HOME="/home/user"
SIMPLE=value"#;

        let result = parse_env_output(input).unwrap();
        assert_eq!(result.get("PATH"), Some(&"/usr/bin".to_string()));
        assert_eq!(result.get("HOME"), Some(&"/home/user".to_string()));
        assert_eq!(result.get("SIMPLE"), Some(&"value".to_string()));
    }

    #[test]
    fn test_parse_env_escaped_newline_in_value() {
        // bash encodes newlines as \n inside double-quoted declare -x output
        let input = r#"declare -x MULTILINE="line1\nline2\nline3""#;
        let result = parse_env_output(input).unwrap();
        assert_eq!(
            result.get("MULTILINE"),
            Some(&"line1\nline2\nline3".to_string())
        );
    }

    #[test]
    fn test_parse_env_escaped_tab_and_backslash() {
        let input = r#"export TABS="col1\tcol2"
export BACK="a\\b""#;
        let result = parse_env_output(input).unwrap();
        assert_eq!(result.get("TABS"), Some(&"col1\tcol2".to_string()));
        assert_eq!(result.get("BACK"), Some(&"a\\b".to_string()));
    }

    #[test]
    fn test_parse_env_literal_newline_in_quoted_value() {
        // Some shells emit literal newlines inside quoted values
        let input = "export KEY=\"line1\nline2\"\nexport OTHER=val";
        let result = parse_env_output(input).unwrap();
        assert_eq!(result.get("KEY"), Some(&"line1\nline2".to_string()));
        assert_eq!(result.get("OTHER"), Some(&"val".to_string()));
    }

    #[test]
    fn test_parse_env_invalid_key_ignored() {
        // Lines that don't look like valid assignments should be skipped
        let input = "VALID=yes\n123INVALID=no\n=nokey\nOK=1";
        let result = parse_env_output(input).unwrap();
        assert!(result.contains_key("VALID"));
        assert!(result.contains_key("OK"));
        assert!(!result.contains_key("123INVALID"));
        assert!(!result.contains_key(""));
    }

    #[test]
    fn test_parse_state_output() {
        let input = r#"version 1
cwd "/tmp/project"
env "PATH" "/usr/bin"
env "MULTILINE" "line1\nline2""#;
        let state = parse_state_output(input).unwrap();
        assert_eq!(state.env.get("PATH"), Some(&"/usr/bin".to_string()));
        assert_eq!(
            state.env.get("MULTILINE"),
            Some(&"line1\nline2".to_string())
        );
        assert_eq!(state.cwd.as_deref(), Some("/tmp/project"));
    }

    #[test]
    fn test_parse_state_output_rejects_unknown_version() {
        let input = r#"version 2
cwd "/tmp/project"
env "PATH" "/usr/bin""#;
        assert!(parse_state_output(input).is_none());
    }

    #[test]
    fn test_parse_state_output_rejects_invalid_escape() {
        let input = r#"version 1
env "PATH" "bad\qescape""#;
        assert!(parse_state_output(input).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn test_accept_fifo_normal_data() {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        use std::time::Duration;
        use upmd_pty::ExitSignal;

        let dir = std::env::temp_dir().join(format!("upmd_test_fifo_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let fifo_path = dir.join("test_fifo");
        let c_path = CString::new(fifo_path.as_os_str().as_bytes()).unwrap();
        let ret = unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) };
        assert_eq!(ret, 0, "mkfifo failed");

        let child_exit = ExitSignal::new();
        let writer_child_exit = child_exit.clone();
        let _fifo = fifo_path.clone();

        // Writer thread: write data and then immediately notify child exit.
        let writer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            let data = b"hello\nworld\n";
            let mut f = std::fs::File::create(&_fifo).unwrap();
            std::io::Write::write_all(&mut f, data).unwrap();
            drop(f);
            // Simulate child exit after writing.
            writer_child_exit.notify();
        });

        let result = accept_fifo(&fifo_path, &child_exit);
        assert_eq!(result.as_deref(), Some("hello\nworld\n"));

        writer.join().unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn test_accept_fifo_child_exits_without_data() {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        use std::time::Duration;
        use upmd_pty::ExitSignal;

        let dir =
            std::env::temp_dir().join(format!("upmd_test_fifo_cancel_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let fifo_path = dir.join("test_fifo");
        let c_path = CString::new(fifo_path.as_os_str().as_bytes()).unwrap();
        let ret = unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) };
        assert_eq!(ret, 0, "mkfifo failed");

        let child_exit = ExitSignal::new();
        let cancel_sig = child_exit.clone();

        // Simulate child exits without writing: notify exit signal after a delay.
        let cancel_handle = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            cancel_sig.notify();
        });

        let start = std::time::Instant::now();
        let result = accept_fifo(&fifo_path, &child_exit);
        let elapsed = start.elapsed();

        // Should return None when child exits without writing data.
        assert!(result.is_none());
        // Should complete in reasonable time (cancel thread triggers within ~200ms).
        assert!(
            elapsed < Duration::from_secs(5),
            "took too long: {elapsed:?}"
        );

        cancel_handle.join().unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
