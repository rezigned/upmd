use super::{cmd_quote, powershell_quote, Cmd, PowerShell};
use crate::{CodeInput, ExecutionPlan, FifoPaths, LanguageRunner, RunnerOptions, ShellQuoteStyle};
use anyhow::Result;

macro_rules! impl_windows_runner {
    ($type:ty, $source_cmd:expr, $env_cmd:expr, $cwd_cmd:expr, $quote_fn:expr, $sep:expr) => {
        impl LanguageRunner for $type {
            fn plan<'a>(&self, code: &CodeInput<'a>) -> Result<ExecutionPlan<'a>> {
                let mut plan = ExecutionPlan::new();
                plan.resolve_paths();
                plan.quote_style(<$type>::quote_style());

                if self.language().supports_inline && code.content.lines().count() == 1 {
                    plan.command([code.content.to_string()]);
                } else {
                    plan.requires_file();
                    let filename = format!("script_{}.{}", code.id, self.language().file_extension);
                    plan.file(&filename, code.content);
                    // The shell prefix (cmd.exe /c, powershell -Command) is
                    // added by resolve_shell() in the execution engine, so
                    // the plan only needs the script invocation itself.
                    if $source_cmd.is_empty() {
                        plan.command([filename.clone()]);
                    } else {
                        plan.command([$source_cmd.to_string(), filename.clone()]);
                    }
                    plan.cleanup(filename);
                }

                if code.state_capture.enabled {
                    if let Some(fifos) = &code.state_capture.fifos {
                        let state_fifo = $quote_fn(&fifos.state_fifo.display().to_string());
                        plan.wrap(move |cmds| {
                            format!(
                                "{cmds}\n$({} {} {}) > {}\n",
                                $env_cmd, $sep, $cwd_cmd, state_fifo
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

            fn supports_state_capture(&self) -> bool {
                true
            }

            fn state_capture_env_vars(&self, fifos: &FifoPaths) -> Vec<(String, String)> {
                vec![(
                    "UPMD_STATE_FIFO".to_string(),
                    fifos.state_fifo.to_string_lossy().to_string(),
                )]
            }
        }
    };
}

// `&` is the unconditional command separator in cmd.exe; `;` is the statement
// separator in PowerShell and works on PowerShell 5.1 as well as 7+.
impl_windows_runner!(Cmd, "", "set", "echo %cd%", cmd_quote, "&");
impl_windows_runner!(
    PowerShell,
    ".",
    "Get-ChildItem Env: | ForEach-Object { \"$($_.Name)=$($_.Value)\" }",
    "(Get-Location).Path",
    powershell_quote,
    ";"
);

impl Cmd {
    fn quote_style() -> ShellQuoteStyle {
        ShellQuoteStyle::Cmd
    }
}

impl PowerShell {
    fn quote_style() -> ShellQuoteStyle {
        ShellQuoteStyle::PowerShell
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

    fn with_capture_path(state_fifo: &str) -> StateCaptureContext {
        StateCaptureContext {
            enabled: true,
            code_id: 1,
            fifos: Some(FifoPaths {
                state_fifo: PathBuf::from(state_fifo),
            }),
        }
    }

    fn assemble(plan: &ExecutionPlan, root: &str) -> String {
        let ws = TempWorkspace::from_path(root);
        ws.build_script(plan).expect("build_script failed")
    }

    fn cmd_lang() -> Language {
        Language {
            name: "Cmd".into(),
            kind: Kind::Shell,
            aliases: &["cmd", "bat", "batch"],
            binaries: &["cmd"],
            syntax: "batch".into(),
            file_extension: "bat",
            supports_inline: true,
            supports_file: true,
            package_manager: None,
        }
    }

    fn ps_lang() -> Language {
        Language {
            name: "PowerShell".into(),
            kind: Kind::Shell,
            aliases: &["powershell", "ps1", "pwsh"],
            binaries: &["powershell", "pwsh"],
            syntax: "powershell".into(),
            file_extension: "ps1",
            supports_inline: true,
            supports_file: true,
            package_manager: None,
        }
    }

    // --- Cmd tests ---

    #[test]
    fn test_cmd_inline_no_capture() {
        let lang = cmd_lang();
        let state = no_capture();
        let input = CodeInput {
            id: 1,
            content: "echo hello",
            language: &lang,
            state_capture: &state,
        };
        let plan = Cmd::new().plan(&input).unwrap();
        let script = assemble(&plan, "/tmp/ws");
        assert_eq!(script, "echo hello");
    }

    #[test]
    fn test_cmd_multiline_no_capture() {
        let lang = cmd_lang();
        let state = no_capture();
        let input = CodeInput {
            id: 1,
            content: "echo hello\necho world",
            language: &lang,
            state_capture: &state,
        };
        let plan = Cmd::new().plan(&input).unwrap();
        let script = assemble(&plan, "/tmp/ws");
        assert_eq!(script, "\"/tmp/ws/script_1.bat\"");
    }

    #[test]
    fn test_cmd_multiline_with_capture() {
        let lang = cmd_lang();
        let state = with_capture();
        let input = CodeInput {
            id: 1,
            content: "echo hello\necho world",
            language: &lang,
            state_capture: &state,
        };
        let plan = Cmd::new().plan(&input).unwrap();
        let script = assemble(&plan, "/tmp/ws");
        let expected = "\
\"/tmp/ws/script_1.bat\"\n\
(set & echo %cd%) > \"/tmp/state/state.fifo\"\n";
        assert_eq!(script, expected);
    }

    #[test]
    fn test_cmd_inline_with_capture() {
        let lang = cmd_lang();
        let state = with_capture();
        let input = CodeInput {
            id: 1,
            content: "echo hello",
            language: &lang,
            state_capture: &state,
        };
        let plan = Cmd::new().plan(&input).unwrap();
        let script = assemble(&plan, "/tmp/ws");
        let expected = "\
echo hello\n\
(set & echo %cd%) > \"/tmp/state/state.fifo\"\n";
        assert_eq!(script, expected);
    }

    #[test]
    fn test_cmd_capture_fifo_paths_escape_expansion_chars() {
        let lang = cmd_lang();
        let state = with_capture_path(r"C:\tmp\state %USERPROFILE% & more\state.fifo");
        let input = CodeInput {
            id: 1,
            content: "echo hello",
            language: &lang,
            state_capture: &state,
        };

        let plan = Cmd::new().plan(&input).unwrap();
        let script = assemble(&plan, r"C:\tmp\ws");

        assert!(script
            .contains(r#"(set & echo %cd%) > "C:\tmp\state %%USERPROFILE%% & more\state.fifo""#));
    }

    #[test]
    fn test_ps_inline_no_capture() {
        let lang = ps_lang();
        let state = no_capture();
        let input = CodeInput {
            id: 1,
            content: "Write-Host 'hello'",
            language: &lang,
            state_capture: &state,
        };
        let plan = PowerShell::new().plan(&input).unwrap();
        let script = assemble(&plan, "/tmp/ws");
        assert_eq!(script, "Write-Host 'hello'");
    }

    #[test]
    fn test_ps_multiline_no_capture() {
        let lang = ps_lang();
        let state = no_capture();
        let input = CodeInput {
            id: 1,
            content: "Write-Host 'hello'\nWrite-Host 'world'",
            language: &lang,
            state_capture: &state,
        };
        let plan = PowerShell::new().plan(&input).unwrap();
        let script = assemble(&plan, "/tmp/ws");
        assert_eq!(script, ". /tmp/ws/script_1.ps1");
    }

    #[test]
    fn test_ps_multiline_with_capture() {
        let lang = ps_lang();
        let state = with_capture();
        let input = CodeInput {
            id: 1,
            content: "Write-Host 'hello'\nWrite-Host 'world'",
            language: &lang,
            state_capture: &state,
        };
        let plan = PowerShell::new().plan(&input).unwrap();
        let script = assemble(&plan, "/tmp/ws");
        let expected = "\
. /tmp/ws/script_1.ps1\n\
(Get-ChildItem Env: | ForEach-Object { \"$($_.Name)=$($_.Value)\" } ; (Get-Location).Path) > '/tmp/state/state.fifo'\n";
        assert_eq!(script, expected);
    }

    #[test]
    fn test_ps_inline_with_capture() {
        let lang = ps_lang();
        let state = with_capture();
        let input = CodeInput {
            id: 1,
            content: "Write-Host 'hello'",
            language: &lang,
            state_capture: &state,
        };
        let plan = PowerShell::new().plan(&input).unwrap();
        let script = assemble(&plan, "/tmp/ws");
        let expected = "\
Write-Host 'hello'\n\
(Get-ChildItem Env: | ForEach-Object { \"$($_.Name)=$($_.Value)\" } ; (Get-Location).Path) > '/tmp/state/state.fifo'\n";
        assert_eq!(script, expected);
    }

    #[test]
    fn test_powershell_capture_fifo_paths_escape_single_quotes() {
        let lang = ps_lang();
        let state = with_capture_path(r"C:\tmp\state 'quoted'\state.fifo");
        let input = CodeInput {
            id: 1,
            content: "Write-Host 'hello'",
            language: &lang,
            state_capture: &state,
        };

        let plan = PowerShell::new().plan(&input).unwrap();
        let script = assemble(&plan, r"C:\tmp\ws");

        assert!(script.contains(
            r#"(Get-ChildItem Env: | ForEach-Object { "$($_.Name)=$($_.Value)" } ; (Get-Location).Path) > 'C:\tmp\state ''quoted''\state.fifo'"#
        ));
    }

    #[test]
    fn test_cmd_multiline_quotes_paths_with_spaces() {
        let lang = cmd_lang();
        let state = no_capture();
        let input = CodeInput {
            id: 1,
            content: "echo hello\necho world",
            language: &lang,
            state_capture: &state,
        };
        let plan = Cmd::new().plan(&input).unwrap();
        let script = assemble(&plan, "/tmp/ws with space");
        assert_eq!(script, "\"/tmp/ws with space/script_1.bat\"");
    }

    #[test]
    fn test_ps_multiline_quotes_paths_with_spaces() {
        let lang = ps_lang();
        let state = no_capture();
        let input = CodeInput {
            id: 1,
            content: "Write-Host 'hello'\nWrite-Host 'world'",
            language: &lang,
            state_capture: &state,
        };
        let plan = PowerShell::new().plan(&input).unwrap();
        let script = assemble(&plan, "/tmp/ws with space");
        assert_eq!(script, ". '/tmp/ws with space/script_1.ps1'");
    }
}
