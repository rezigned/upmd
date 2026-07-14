//! Executor module - provides language execution capabilities.
//!
//! This module provides a clean API for executing code blocks with automatic
//! workspace management, file creation, and state capture.

mod workspace;

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::{
    apps::config::Envs,
    pty::process::{Process, Size},
    pty::stream::Stream,
};
use anyhow::Result;
use crossbeam_channel::{bounded, Receiver};
use upmd_parser::nodes;

use upmd_runner::workspace::WorkspaceExecutionExt;

pub use upmd_runner::*;
use workspace::ExecutionContext;

/// Result of code execution.
///
/// This struct keeps the workspace alive for the duration of the execution,
/// ensuring temporary files and state directories are cleaned up when dropped.
pub struct Execution {
    rx: Receiver<Stream>,
    process: Process,
    #[allow(dead_code)]
    ctx: ExecutionContext, // held for Drop: cleans up temp files on destroy
}

impl Execution {
    /// Returns a clone of the receiver for output streams.
    /// Crossbeam receivers can be cloned to have multiple consumers.
    pub fn receiver(&self) -> Receiver<Stream> {
        self.rx.clone()
    }

    /// Returns a mutable reference to the process.
    pub fn process_mut(&mut self) -> &mut Process {
        &mut self.process
    }
}

/// Execute code in a PTY with optional state capture.
///
/// This is the main entry point for code execution. It handles:
/// - Creating a temporary workspace and state capture dirs
/// - Resolving the language runner and generating an execution plan
/// - Creating required temporary files and building the shell script
/// - Building the environment with state capture vars when enabled
/// - Spawning a PTY process with 3 threads (state, reader, process)
///
/// # Arguments
/// * `code` - The code block to execute
/// * `envs` - Environment variables to pass to the process
/// * `cwd` - Current working directory
/// * `size` - Terminal size for the PTY
/// * `capture_state` - Experimental state capture control
///
/// # Returns
/// An `Execution` struct containing the output receiver and process handle.
/// The workspace is automatically cleaned up when `Execution` is dropped.
pub fn execute(
    code: &nodes::Code,
    envs: Envs,
    cwd: PathBuf,
    size: Size,
    capture_state: bool,
    binaries: &HashMap<String, upmd_runner::RunnerOptions>,
) -> Result<Execution> {
    let ctx = ExecutionContext::new(code.id)?;
    let language = upmd_runner::find_language(&code.language);

    // Shell languages always capture. Non-shell languages require the explicit
    // --capture-state flag and must also be supported by the runner.
    let runner = resolve_runner(&language, binaries, code.options.attrs.get("bin").cloned())?;

    let should_capture_state = match language.kind {
        Kind::Shell => true,
        _ => capture_state && runner.supports_state_capture(),
    };

    // State capture setup: fifos + context for the runner's plan()
    let state_fifos = if should_capture_state {
        Some(ctx.create_state_fifos()?)
    } else {
        None
    };
    let state_capture = StateCaptureContext {
        enabled: should_capture_state,
        fifos: state_fifos.clone(),
        code_id: code.id,
    };
    let input = CodeInput {
        id: code.id,
        content: &code.content,
        language: &language,
        state_capture: &state_capture,
    };

    let plan = runner.plan(&input)?;

    // Files, working dir
    ctx.execute_plan(&plan)?;
    let working_dir = ctx.resolve_working_dir(&plan, cwd);

    // Environment
    let envs = build_envs(envs, &plan, &state_capture, &*runner, ctx.target_dir())?;

    // PTY execution
    let state = StateCapture {
        dir: ctx.state_dir().to_path_buf(),
        fifos: state_fifos,
    };
    let (tx, rx) = bounded(crate::apps::config::STREAM_CHANNEL_SIZE);

    let cmd = if let Some(program) = &plan.executable {
        // Direct exec: spawn the binary directly, no shell wrapper.
        // Resolve file args against the workspace target dir so the binary
        // can find scripts created by execute_plan().
        let target = ctx.target_dir();
        let mut cmd = vec![OsString::from(&program.binary)];
        for arg in &program.args {
            let resolved = plan
                .files
                .iter()
                .find(|(p, _)| p.to_string_lossy() == arg.as_str())
                .map(|(p, _)| target.join(p).into_os_string())
                .unwrap_or_else(|| OsString::from(arg));
            cmd.push(resolved);
        }
        cmd
    } else {
        // Script-based: build a shell script from commands and run via shell -c
        let script = WorkspaceExecutionExt::build_script(&*ctx, &plan)?;
        let shell_prefix = resolve_shell(code.id, &language, &*runner)?;
        let mut cmd: Vec<OsString> = shell_prefix;
        cmd.push(OsString::from(script));
        cmd
    };
    tracing::debug!(code_id = code.id, language = %language.name, argv = ?cmd, "executing");
    let mut p = Process::new(cmd, tx.clone(), size, working_dir, state)?;
    p.envs(envs);
    p.start()?;

    Ok(Execution {
        rx,
        process: p,
        ctx,
    })
}

/// Resolves the runner (respecting binary overrides from config and code block
/// attributes) without calling `plan()`.
///
/// Returns the runner so callers can inspect its capabilities (e.g.
/// `supports_state_capture()`) before generating the execution plan.
fn resolve_runner(
    language: &Language,
    binaries: &HashMap<String, RunnerOptions>,
    bin_attr: Option<String>,
) -> Result<Box<dyn LanguageRunner + Send + Sync>> {
    let mut opts = binaries
        .get(language.name.as_str())
        .cloned()
        .unwrap_or_default();
    if let Some(ref bin) = bin_attr {
        opts.bin = Some(bin.clone());
    }
    if opts.bin.is_some() || !opts.extra_args.is_empty() || !opts.env.is_empty() {
        create_runner(language, opts)
    } else {
        let _ = upmd_runner::find_by_language(language)?;
        create_runner(language, RunnerOptions::default())
    }
}

/// Returns `[shell_path, exec_flag]` for executing a script string.
///
/// Only called for script-based plans (shell languages and multi-command
/// compiled languages). Shell languages use `resolve_binary()` to find
/// the correct shell, then map the shell name to the expected exec flag:
/// - `-c` for POSIX shells (sh, bash, zsh, fish)
/// - `/c` for cmd.exe
/// - `-Command` for PowerShell
///
/// For non-shell languages this path is never reached - their plans use
/// `executable` for direct exec instead.
fn resolve_shell(
    code_id: upmd_parser::CodeId,
    language: &Language,
    static_runner: &dyn LanguageRunner,
) -> Result<Vec<OsString>> {
    let (shell_path, flag) = if language.kind == Kind::Shell {
        // Shell languages must have their binary available. Do not silently
        // fall back to sh/cmd. That would run Windows scripts on Unix (and vice versa).
        let (path, _) = static_runner.resolve_binary().inspect_err(|e| {
            let bin = e
                .downcast_ref::<upmd_runner::ExecutionError>()
                .and_then(|err| match err {
                    upmd_runner::ExecutionError::BinaryNotFound(b) => Some(b.as_str()),
                    _ => None,
                })
                .unwrap_or("?");
            tracing::error!(code_id = code_id, language = %language.name, bin = %bin, "binary not found");
        })?;
        let flag = exec_flag_for_shell(&path);
        (path, flag)
    } else {
        (default_shell(), default_shell_flag())
    };

    Ok(vec![OsString::from(shell_path), OsString::from(flag)])
}

#[cfg(unix)]
fn default_shell() -> String {
    "sh".to_string()
}

#[cfg(windows)]
fn default_shell() -> String {
    "cmd.exe".to_string()
}

#[cfg(unix)]
fn default_shell_flag() -> &'static str {
    "-c"
}

#[cfg(windows)]
fn default_shell_flag() -> &'static str {
    "/c"
}

fn exec_flag_for_shell(path: &str) -> &'static str {
    let name = std::path::Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(path);
    match name {
        "cmd" => "/c",
        "powershell" | "pwsh" => "-Command",
        _ => "-c",
    }
}

/// Builds the environment variable list for the PTY process: filters out
/// internal shell vars, injects UPMD_DIR / UPMD_FILE / UPMD_FILE_PATH, and
/// adds state capture env vars when enabled.
fn build_envs(
    envs: Envs,
    plan: &ExecutionPlan,
    state_capture: &StateCaptureContext,
    static_runner: &dyn LanguageRunner,
    target_dir: &Path,
) -> Result<Envs> {
    let mut envs: Envs = envs
        .into_iter()
        .filter(|(k, _)| !matches!(k.as_str(), "_" | "SHLVL" | "PWD" | "OLDPWD"))
        .collect();

    envs.insert(
        "UPMD_DIR".to_string(),
        crate::apps::target_dir()?.to_string_lossy().to_string(),
    );
    if let Some((file_path, _)) = plan.files.first() {
        let abs_path = target_dir.join(file_path);
        envs.insert(
            "UPMD_FILE".to_string(),
            file_path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string(),
        );
        envs.insert(
            "UPMD_FILE_PATH".to_string(),
            abs_path.to_string_lossy().to_string(),
        );
    }

    if let Some(fifos) = &state_capture.fifos {
        for (key, value) in static_runner.state_capture_env_vars(fifos) {
            envs.insert(key, value);
        }
    }

    // Merge plan-level env vars (e.g. PATH overrides from venv setup)
    for (key, value) in &plan.env_vars {
        envs.insert(key.clone(), value.clone());
    }

    Ok(envs)
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use super::*;

    #[cfg(unix)]
    mod unix {
        use super::*;
        use std::time::Duration;

        /// Runs a bash block that exports an env var and verifies the captured
        /// state arrives through the stream.
        #[test]
        fn test_state_capture_roundtrip() {
            let code = nodes::Code {
                id: 999,
                language: "bash".into(),
                content: r#"export TEST_UPMD_STATE="hello_from_shell""#.into(),
                ..Default::default()
            };

            let size = Size {
                width: 80,
                height: 24,
            };
            let cwd = std::env::current_dir().unwrap();
            let envs = Envs::new();

            let exec = execute(&code, envs, cwd, size, true, &HashMap::new()).unwrap();
            let rx = exec.receiver();

            // Collect all stream messages until End.
            let mut captured_env: Option<crate::apps::config::Envs> = None;
            let mut captured_cwd: Option<String> = None;
            let mut exit_code: Option<i32> = None;

            while let Ok(msg) = rx.recv_timeout(Duration::from_secs(10)) {
                match msg {
                    Stream::Env(envs) => captured_env = Some(envs),
                    Stream::Cwd(cwd) => captured_cwd = Some(cwd),
                    Stream::Exit(code) => exit_code = Some(code),
                    Stream::End => break,
                    Stream::Out(_) => {}
                }
            }

            assert_eq!(exit_code, Some(0), "process should exit successfully");

            let envs =
                captured_env.expect("should have received Stream::Env with captured env vars");
            assert_eq!(
                envs.get("TEST_UPMD_STATE"),
                Some(&"hello_from_shell".to_string()),
                "captured env should contain the exported variable"
            );

            let cwd = captured_cwd.expect("should have received Stream::Cwd");
            assert!(!cwd.is_empty(), "captured cwd should be a non-empty path");
        }

        /// Runs a Python block that sets env vars via `os.environ` and verifies
        /// the captured state arrives through the stream.
        #[test]
        fn test_python_state_capture_roundtrip() {
            let code = nodes::Code {
                id: 998,
                language: "python".into(),
                content: r#"
import os
os.environ["FROM_PYTHON"] = "set by python"
os.environ["PY_UNICODE"] = "café"
os.environ["PY_NEWLINE"] = "first\nsecond"
print("python ran")
"#
                .into(),
                ..Default::default()
            };

            let size = Size {
                width: 80,
                height: 24,
            };
            let cwd = std::env::current_dir().unwrap();
            let envs = Envs::new();

            let exec = execute(&code, envs, cwd, size, true, &HashMap::new()).unwrap();
            let rx = exec.receiver();

            let mut captured_env: Option<crate::apps::config::Envs> = None;
            let mut captured_cwd: Option<String> = None;
            let mut exit_code: Option<i32> = None;

            while let Ok(msg) = rx.recv_timeout(Duration::from_secs(15)) {
                match msg {
                    Stream::Env(envs) => {
                        println!("python: got Stream::Env with {} vars", envs.len());
                        captured_env = Some(envs);
                    }
                    Stream::Cwd(cwd) => {
                        println!("python: got Stream::Cwd: {}", cwd);
                        captured_cwd = Some(cwd);
                    }
                    Stream::Exit(code) => {
                        println!("python: got Stream::Exit: {}", code);
                        exit_code = Some(code);
                    }
                    Stream::End => {
                        println!("python: got Stream::End");
                        break;
                    }
                    Stream::Out(s) => {
                        print!("{}", s);
                    }
                }
            }

            assert_eq!(
                exit_code,
                Some(0),
                "python process should exit successfully"
            );

            let envs = captured_env.expect("should have received Stream::Env from python");
            assert_eq!(
                envs.get("FROM_PYTHON"),
                Some(&"set by python".to_string()),
                "captured env should contain FROM_PYTHON"
            );
            assert_eq!(
                envs.get("PY_UNICODE"),
                Some(&"café".to_string()),
                "captured env should contain PY_UNICODE"
            );
            assert_eq!(
                envs.get("PY_NEWLINE"),
                Some(&"first\nsecond".to_string()),
                "captured env should contain PY_NEWLINE with actual newline"
            );

            let _cwd = captured_cwd.expect("should have received Stream::Cwd from python");
        }

        #[test]
        fn test_binary_override_picked_up_by_runner() {
            let language = upmd_runner::find_language("bash");
            let opts = upmd_runner::RunnerOptions {
                bin: Some("/bin/sh".into()),
                ..Default::default()
            };
            let runner = upmd_runner::create_runner(&language, opts).unwrap();
            assert_eq!(
                runner.options().bin.as_deref(),
                Some("/bin/sh"),
                "custom runner should carry the overridden binary"
            );
            let (validated, _) = runner.resolve_binary().unwrap();
            assert_eq!(validated, "/bin/sh");
        }

        #[test]
        fn test_bin_attr_takes_precedence_over_config() {
            // Config override sets bash → /bin/sh, but code block says bash → /bin/bash
            let code = nodes::Code {
                id: 1,
                language: "bash".into(),
                content: "echo hello".into(),
                options: upmd_parser::nodes::Options {
                    attrs: [("bin".into(), "/bin/bash".into())].into(),
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut binaries: HashMap<String, upmd_runner::RunnerOptions> = HashMap::new();
            binaries.insert(
                "Bash".into(),
                upmd_runner::RunnerOptions {
                    bin: Some("/bin/sh".into()),
                    ..Default::default()
                },
            );

            let size = Size {
                width: 80,
                height: 24,
            };
            let cwd = std::env::current_dir().unwrap();
            let envs = Envs::new();

            let exec = execute(&code, envs, cwd, size, false, &binaries).unwrap();
            let rx = exec.receiver();
            let mut exit_code = None;
            while let Ok(msg) = rx.recv_timeout(Duration::from_secs(10)) {
                if let Stream::Exit(code) = msg {
                    exit_code = Some(code);
                    break;
                }
            }
            assert_eq!(exit_code, Some(0), "/bin/bash should execute the block");
        }

        #[test]
        fn test_bin_attr_without_config() {
            // Code block says bash → /bin/bash, no config override
            let code = nodes::Code {
                id: 2,
                language: "bash".into(),
                content: "echo hello".into(),
                options: upmd_parser::nodes::Options {
                    attrs: [("bin".into(), "/bin/bash".into())].into(),
                    ..Default::default()
                },
                ..Default::default()
            };

            let size = Size {
                width: 80,
                height: 24,
            };
            let cwd = std::env::current_dir().unwrap();
            let envs = Envs::new();

            let exec = execute(&code, envs, cwd, size, false, &HashMap::new()).unwrap();
            let rx = exec.receiver();
            let mut exit_code = None;
            while let Ok(msg) = rx.recv_timeout(Duration::from_secs(10)) {
                if let Stream::Exit(code) = msg {
                    exit_code = Some(code);
                    break;
                }
            }
            assert_eq!(exit_code, Some(0), "/bin/bash should execute the block");
        }
    }
}
