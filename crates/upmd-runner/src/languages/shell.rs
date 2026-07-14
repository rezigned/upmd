//! Shell language runners (bash, fish, sh, zsh).
//!
//! # Platform notes
//!
//! These runners are Unix-only. On Windows, `bash`/`sh`/`zsh`/`fish` are not
//! available by default. To support Windows, add `Cmd` and `PowerShell`
//! runners using the same `impl_shell_runner!` macro with `cmd /c` and
//! `powershell -File` as the source commands.

use super::{Bash, Fish, Shell, Zsh};
use crate::{CodeInput, ExecutionPlan, LanguageRunner, RunnerOptions};
use anyhow::Result;

macro_rules! impl_shell_runner {
    ($type:ty, $source_cmd:expr, $env_cmd:expr) => {
        impl LanguageRunner for $type {
            fn plan<'a>(&self, code: &CodeInput<'a>) -> Result<ExecutionPlan<'a>> {
                let mut plan = ExecutionPlan::new();
                plan.resolve_paths();

                if self.language().supports_inline && code.content.lines().count() == 1 {
                    plan.command([code.content.to_string()]);
                } else {
                    // Filename is stable per code_id so sequential re-runs overwrite
                    // safely. Concurrent re-runs of the same block could collide;
                    // that would require a unique execution ID in CodeInput.
                    let filename = format!("script_{}.{}", code.id, self.language().file_extension);

                    plan.requires_file()
                        .file(&filename, code.content)
                        .command([$source_cmd.to_string(), format!("./{}", filename)])
                        .cleanup(&filename);
                }

                // State capture: wrap the assembled commands so that export
                // and pwd run in the same shell context as the user code.
                if code.state_capture.enabled {
                    if let Some(fifos) = &code.state_capture.fifos {
                        let state_fifo =
                            super::posix_quote(&fifos.state_fifo.display().to_string());
                        let env_cmd = $env_cmd;
                        plan.wrap(move |cmds| {
                            format!(
                                "{cmds}\nRET=$?\n{{ {env_cmd}; pwd; }} > {state_fifo}\nexit $RET"
                            )
                        });
                    }
                }

                plan.apply_options(self.options());
                Ok(plan)
            }

            fn options(&self) -> &RunnerOptions {
                &self.options
            }

            fn language(&self) -> &crate::Language {
                &self.language
            }
        }
    };
}

impl_shell_runner!(Bash, ".", "export -p");
impl_shell_runner!(Shell, ".", "export");
impl_shell_runner!(Zsh, ".", "export -p");

impl LanguageRunner for Fish {
    fn plan<'a>(&self, code: &CodeInput<'a>) -> Result<ExecutionPlan<'a>> {
        let mut plan = ExecutionPlan::new();
        plan.resolve_paths();

        if self.language().supports_inline && code.content.lines().count() == 1 {
            plan.command([code.content.to_string()]);
        } else {
            let filename = format!("script_{}.{}", code.id, self.language().file_extension);
            plan.requires_file()
                .file(&filename, code.content)
                .command(["source".to_string(), format!("./{}", filename)])
                .cleanup(&filename);
        }

        if code.state_capture.enabled {
            if let Some(fifos) = &code.state_capture.fifos {
                let state_fifo = super::posix_quote(&fifos.state_fifo.display().to_string());
                plan.wrap(move |cmds| {
                    format!(
                        "{cmds}\nset RET $status\nbegin\nenv\npwd\nend > {state_fifo}\nexit $RET"
                    )
                });
            }
        }

        plan.apply_options(self.options());
        Ok(plan)
    }

    fn options(&self) -> &RunnerOptions {
        &self.options
    }

    fn language(&self) -> &crate::Language {
        &self.language
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        workspace::{TempWorkspace, WorkspaceExecutionExt},
        FifoPaths, Kind, Language, StateCaptureContext,
    };
    use std::path::PathBuf;

    // Thin wrapper: build_script now lives in WorkspaceExecutionExt, so we
    // just point a TempWorkspace at `root` and delegate. No more duplicate logic.
    fn assemble(plan: &ExecutionPlan, root: &str) -> String {
        let ws = TempWorkspace::from_path(root);
        ws.build_script(plan).expect("build_script failed")
    }

    fn no_capture() -> StateCaptureContext {
        StateCaptureContext {
            enabled: false,
            fifos: None,
            code_id: 1,
        }
    }

    fn with_capture() -> StateCaptureContext {
        StateCaptureContext {
            enabled: true,
            code_id: 1,
            fifos: Some(FifoPaths {
                state_fifo: PathBuf::from("/tmp/state/state.fifo"),
            }),
        }
    }

    fn with_capture_paths(state_fifo: &str) -> StateCaptureContext {
        StateCaptureContext {
            enabled: true,
            code_id: 1,
            fifos: Some(FifoPaths {
                state_fifo: PathBuf::from(state_fifo),
            }),
        }
    }

    fn bash_lang() -> Language {
        Language {
            name: "Bash".into(),
            kind: Kind::Shell,
            aliases: &["bash"],
            binaries: &["bash"],
            syntax: "sh".into(),
            file_extension: "sh",
            supports_inline: true,
            supports_file: true,
            package_manager: None,
        }
    }

    #[test]
    fn test_inline_no_capture() {
        let lang = bash_lang();
        let state = no_capture();
        let input = CodeInput {
            id: 1,
            content: "echo hello",
            language: &lang,
            state_capture: &state,
        };

        let plan = Bash::new().plan(&input).unwrap();
        let script = assemble(&plan, "/tmp/ws");

        // Single-line inline: no file, no wrapper
        assert_eq!(script, "echo hello");
    }

    #[test]
    fn test_multiline_no_capture() {
        let lang = bash_lang();
        let state = no_capture();
        let input = CodeInput {
            id: 1,
            content: "echo hello\necho world",
            language: &lang,
            state_capture: &state,
        };

        let plan = Bash::new().plan(&input).unwrap();
        let script = assemble(&plan, "/tmp/ws");

        // Multi-line: sourced from file, path resolved to absolute
        assert_eq!(script, ". /tmp/ws/script_1.sh");
    }

    #[test]
    fn test_multiline_with_capture() {
        let lang = bash_lang();
        let state = with_capture();
        let input = CodeInput {
            id: 1,
            content: "echo hello\necho world",
            language: &lang,
            state_capture: &state,
        };

        let plan = Bash::new().plan(&input).unwrap();
        let script = assemble(&plan, "/tmp/ws");

        let expected = "\
. /tmp/ws/script_1.sh\n\
RET=$?\n\
{ export -p; pwd; } > '/tmp/state/state.fifo'\n\
exit $RET";

        assert_eq!(script, expected);
    }

    #[test]
    fn test_fish_uses_source_and_set_x() {
        let lang = Language {
            name: "Fish".into(),
            kind: Kind::Shell,
            aliases: &["fish"],
            binaries: &["fish"],
            syntax: "sh".into(),
            file_extension: "fish",
            supports_inline: true,
            supports_file: true,
            package_manager: None,
        };
        let state = with_capture();
        let input = CodeInput {
            id: 2,
            content: "echo hello\necho world",
            language: &lang,
            state_capture: &state,
        };

        let plan = Fish::new().plan(&input).unwrap();
        let script = assemble(&plan, "/tmp/ws");

        // Fish uses `source`, `$status`, and `begin … end` for POSIX-compatible output.
        let expected = "\
source /tmp/ws/script_2.fish\n\
set RET $status\n\
begin\n\
env\n\
pwd\n\
end > '/tmp/state/state.fifo'\n\
exit $RET";

        assert_eq!(script, expected);
    }

    // --- Inline bypass tests ---
    // These cover the fix in build_script / assemble: single-element commands on
    // non-file plans must be emitted verbatim, not quoted. Before the fix,
    // `echo hello` would become `'echo hello'` (a literal command name) and fail.

    #[test]
    fn test_inline_with_spaces_not_quoted() {
        // Regression: "echo hello world" has spaces. Must NOT be wrapped in quotes.
        let lang = bash_lang();
        let state = no_capture();
        let input = CodeInput {
            id: 1,
            content: "echo hello world",
            language: &lang,
            state_capture: &state,
        };

        let plan = Bash::new().plan(&input).unwrap();
        assert!(!plan.requires_file, "single-line should be inline");

        let script = assemble(&plan, "/tmp/ws");
        assert_eq!(script, "echo hello world");
    }

    #[test]
    fn test_inline_with_special_chars_not_quoted() {
        // Content with single quotes and exclamation marks must also pass through raw.
        let lang = bash_lang();
        let state = no_capture();
        let input = CodeInput {
            id: 1,
            content: "echo 'it works!'",
            language: &lang,
            state_capture: &state,
        };

        let plan = Bash::new().plan(&input).unwrap();
        assert!(!plan.requires_file);

        let script = assemble(&plan, "/tmp/ws");
        assert_eq!(script, "echo 'it works!'");
    }

    #[test]
    fn test_inline_with_capture_not_quoted() {
        // State capture wraps the command but must not quote the inline content.
        let lang = bash_lang();
        let state = with_capture();
        let input = CodeInput {
            id: 1,
            content: "echo hello world",
            language: &lang,
            state_capture: &state,
        };

        let plan = Bash::new().plan(&input).unwrap();
        let script = assemble(&plan, "/tmp/ws");

        let expected = "\
echo hello world\n\
RET=$?\n\
{ export -p; pwd; } > '/tmp/state/state.fifo'\n\
exit $RET";
        assert_eq!(script, expected);
    }

    #[test]
    fn test_state_capture_fifo_paths_are_posix_quoted() {
        let lang = bash_lang();
        let state = with_capture_paths("/tmp/state/state';$HOME`.fifo");
        let input = CodeInput {
            id: 1,
            content: "echo hello",
            language: &lang,
            state_capture: &state,
        };

        let plan = Bash::new().plan(&input).unwrap();
        let script = assemble(&plan, "/tmp/ws");

        assert!(script.contains("{ export -p; pwd; } > '/tmp/state/state'\\'';$HOME`.fifo'"));
    }

    #[test]
    fn test_file_based_args_still_quoted() {
        // File-based (multi-line) commands go through format_args and should
        // still have their paths resolved. The bypass must not affect them.
        let lang = bash_lang();
        let state = no_capture();
        let input = CodeInput {
            id: 3,
            content: "echo hello\necho world",
            language: &lang,
            state_capture: &state,
        };

        let plan = Bash::new().plan(&input).unwrap();
        assert!(plan.requires_file, "multi-line should use a file");

        let script = assemble(&plan, "/tmp/ws");
        // Source command + resolved absolute path, no spurious quoting.
        assert_eq!(script, ". /tmp/ws/script_3.sh");
    }
}
