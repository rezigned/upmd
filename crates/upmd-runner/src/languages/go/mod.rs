//! Go language runner with state capture

use anyhow::Result;

use super::Go;
use crate::{CodeId, CodeInput, ExecutionPlan, FifoPaths, LanguageRunner};

impl LanguageRunner for Go {
    fn supports_state_capture(&self) -> bool {
        true
    }

    fn generate_state_capture(&self, _code_id: CodeId) -> Option<String> {
        Some(include_str!("state.go").to_string())
    }

    fn check_name_collisions(&self, code: &str) -> bool {
        code.contains("upmdCaptureState")
            || code.contains("upmdWriteState")
            || code.contains("upmdStateEscape")
    }

    fn state_capture_env_vars(&self, fifos: &FifoPaths) -> Vec<(String, String)> {
        vec![(
            "UPMD_STATE_FIFO".to_string(),
            fifos.state_fifo.to_string_lossy().to_string(),
        )]
    }

    fn plan<'a>(&self, code: &CodeInput<'a>) -> Result<ExecutionPlan<'a>> {
        let mut plan = ExecutionPlan::new();

        let dir = format!("go_project_{}", code.id);
        let file = format!("main_{}.go", code.id);
        let binary_name = format!("app_{}", code.id);

        let content = if code.state_capture.enabled && self.supports_state_capture() {
            if self.check_name_collisions(code.content) {
                tracing::warn!("Name collision detected in Go code, skipping state capture");
                wrap_go_code(code.content)
            } else {
                let stubs = self
                    .generate_state_capture(code.state_capture.code_id)
                    .ok_or_else(|| anyhow::anyhow!("Failed to generate state capture code"))?;
                // User provides own main: keep their code as-is, inject a
                // defer upmdCaptureState() into main, add missing imports,
                // and append the capture stubs.
                if code.content.contains("func main(") {
                    let base = wrap_go_code(code.content);
                    let main_start = base.find("func main").unwrap_or(base.len());
                    let header = &base[..main_start];
                    let main_body = inject_defer_into_main(&base[main_start..], "upmdCaptureState");
                    let imports = state_go_imports(code.content);
                    if imports.is_empty() {
                        format!("{}{}\n\n// state capture\n{}", header, main_body, stubs,)
                    } else {
                        format!(
                            "{}\n{}\n{}\n\n// state capture\n{}",
                            header.trim_end(),
                            imports,
                            main_body,
                            stubs,
                        )
                    }
                // No main function: wrap everything in a Go main program,
                // inject state imports and capture call.
                } else {
                    let imports = state_go_imports(code.content);
                    format!(
                        "package main\n\n{}\n\nfunc main() {{\n{}\n\tupmdCaptureState()\n}}\n\n// state capture\n{}",
                        imports,
                        code.content,
                        stubs,
                    )
                }
            }
        } else {
            wrap_go_code(code.content)
        };

        let go_mod = format!("module upmd_project_{}\n\ngo 1.21\n", code.id);

        let (binary, _) = self.resolve_binary()?;
        let mut build_args = vec![
            std::borrow::Cow::Borrowed(binary.as_str()),
            std::borrow::Cow::Borrowed("build"),
        ];
        build_args.extend(
            self.options()
                .extra_args
                .iter()
                .map(|s| std::borrow::Cow::Owned(s.clone())),
        );
        build_args.push(std::borrow::Cow::Owned("-o".to_string()));
        build_args.push(std::borrow::Cow::Owned(binary_name.clone()));
        build_args.push(std::borrow::Cow::Owned(file.clone()));

        plan.file(format!("{}/{}", dir, file), content)
            .file(format!("{}/go.mod", dir), go_mod)
            .working_dir(&dir)
            .command(build_args)
            .command(vec![format!("./{}", binary_name)])
            .cleanup(&binary_name)
            .cleanup(&dir)
            .apply_options(self.options());

        Ok(plan)
    }

    fn options(&self) -> &crate::RunnerOptions {
        &self.options
    }

    fn language(&self) -> &crate::Language {
        &self.language
    }
}

/// Builds the import block for state capture, skipping packages already imported by user code.
fn state_go_imports(code: &str) -> String {
    let mut pkgs = Vec::new();
    for pkg in ["fmt", "os", "strings"] {
        let quoted = format!("\"{}\"", pkg);
        if !code.contains(&quoted) {
            pkgs.push(pkg);
        }
    }
    if pkgs.is_empty() {
        return String::new();
    }
    let mut result = String::from("import (\n");
    for pkg in &pkgs {
        result.push_str(&format!("\t\"{}\"\n", pkg));
    }
    result.push(')');
    result
}

/// Wraps user code in a valid Go program.
/// - If code has `package main`, use as-is
/// - Otherwise, wrap in `package main` + `func main() { ... }`
fn wrap_go_code(content: &str) -> String {
    if content.contains("package main") {
        content.to_string()
    } else {
        format!("package main\n\nfunc main() {{\n{}\n}}", content)
    }
}

/// Injects `defer <call>()` as the first statement of the `main` function.
///
/// This is a best-effort text transformation: it finds the opening brace of
/// `func main` and inserts the defer call on the next line. It assumes the
/// user's main is formatted in a typical way (the opening brace is present
/// and not inside a string/comment).
fn inject_defer_into_main(body: &str, call: &str) -> String {
    // Find the opening brace of func main.
    let Some(brace_idx) = body.find('{') else {
        return body.to_string();
    };
    let before = &body[..=brace_idx];
    let after = &body[brace_idx + 1..];
    format!("{}\n\tdefer {}();{}", before, call, after)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FifoPaths, RunnerOptions, StateCaptureContext};
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

    fn runner() -> Go {
        Go::with_options(RunnerOptions {
            bin: Some("go".into()),
            ..Default::default()
        })
    }

    fn commands_str(plan: &ExecutionPlan) -> String {
        plan.commands
            .iter()
            .map(|cmd| cmd.iter().map(|a| a.as_ref()).collect::<Vec<_>>().join(" "))
            .collect::<Vec<_>>()
            .join(" && ")
    }

    #[test]
    fn test_simple_code_wrapped_in_main() {
        let lang = Go::get().language();
        let state = no_capture();
        let input = CodeInput {
            id: 1,
            content: "println(\"hi\")",
            language: lang,
            state_capture: &state,
        };
        let plan = runner().plan(&input).unwrap();
        let (_, content) = &plan.files[0];
        assert!(content.contains("func main()"), "got: {content}");
    }

    #[test]
    fn test_full_package_left_as_is() {
        let lang = Go::get().language();
        let state = no_capture();
        let src = "package main\n\nimport \"fmt\"\n\nfunc main() { fmt.Println(\"hi\") }";
        let input = CodeInput {
            id: 2,
            content: src,
            language: lang,
            state_capture: &state,
        };
        let plan = runner().plan(&input).unwrap();
        let (_, content) = &plan.files[0];
        assert_eq!(content, src);
    }

    #[test]
    fn test_state_capture_injects_code() {
        let state = with_capture();
        let input = CodeInput {
            id: 1,
            content: "package main\nfunc main() { println(\"hi\") }",
            language: Go::get().language(),
            state_capture: &state,
        };
        let plan = runner().plan(&input).unwrap();
        let (_, content) = &plan.files[0];
        assert!(content.contains("upmdCaptureState"), "missing capture func");
        assert!(content.contains("upmdWriteState"), "missing state func");
    }

    #[test]
    fn test_state_capture_defers_in_user_main() {
        let state = with_capture();
        let input = CodeInput {
            id: 1,
            content: "package main\nfunc main() { println(\"hi\") }",
            language: Go::get().language(),
            state_capture: &state,
        };
        let plan = runner().plan(&input).unwrap();
        let (_, content) = &plan.files[0];
        // The user's main must defer the capture call so state is written
        // even when main returns normally.
        assert!(
            content.contains("defer upmdCaptureState()"),
            "main should defer upmdCaptureState; got:\n{content}"
        );
    }

    #[test]
    fn test_state_capture_simple_statement() {
        let state = with_capture();
        let input = CodeInput {
            id: 2,
            content: "println(\"hi\")",
            language: Go::get().language(),
            state_capture: &state,
        };
        let plan = runner().plan(&input).unwrap();
        let (_, content) = &plan.files[0];
        assert!(content.contains("upmdCaptureState"), "missing capture func");
        assert!(content.contains("func main()"), "missing func main");
    }

    #[test]
    fn test_compile_then_run_commands() {
        let lang = Go::get().language();
        let state = no_capture();
        let input = CodeInput {
            id: 3,
            content: "println(\"hi\")",
            language: lang,
            state_capture: &state,
        };
        let plan = runner().plan(&input).unwrap();
        let cmd = commands_str(&plan);
        assert_eq!(cmd, "go build -o app_3 main_3.go && ./app_3");
    }

    #[test]
    fn test_bin_override_is_used_for_build() {
        let lang = Go::get().language();
        let state = no_capture();
        let input = CodeInput {
            id: 3,
            content: "println(\"hi\")",
            language: lang,
            state_capture: &state,
        };
        let runner = Go::with_options(RunnerOptions {
            bin: Some("/custom/go".into()),
            ..Default::default()
        });
        let plan = runner.plan(&input).unwrap();
        let cmd = commands_str(&plan);
        assert_eq!(cmd, "/custom/go build -o app_3 main_3.go && ./app_3");
    }

    #[test]
    fn test_go_mod_generated() {
        let lang = Go::get().language();
        let state = no_capture();
        let input = CodeInput {
            id: 3,
            content: "println(\"hi\")",
            language: lang,
            state_capture: &state,
        };
        let plan = runner().plan(&input).unwrap();
        let go_mod = plan
            .files
            .iter()
            .find(|(p, _)| p.to_string_lossy().ends_with("go.mod"))
            .map(|(_, c)| c.as_str())
            .expect("go.mod should be generated");
        assert!(go_mod.contains("module upmd_project_3"));
        assert!(go_mod.contains("go 1.21"));
    }
}
