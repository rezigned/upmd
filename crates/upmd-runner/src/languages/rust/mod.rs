//! Rust language runner
//!
//! Resolves the compiler via `which` in order:
//! 1. Explicit user config (`[bin:...]` attr or `binaries.rust` in config)
//! 2. `rustc` (direct compile)
//! 3. `cargo rustc --` (wraps rustc when only cargo is available)

use anyhow::Result;

use super::Rust;
use crate::{CodeId, CodeInput, ExecutionPlan, LanguageRunner};

// Rust language runner implementation
impl LanguageRunner for Rust {
    fn supports_state_capture(&self) -> bool {
        true
    }

    fn generate_state_capture(&self, _code_id: CodeId) -> Option<String> {
        Some(include_str!("state.rs").to_string())
    }

    fn check_name_collisions(&self, code: &str) -> bool {
        code.contains("upmd_capture_state")
            || code.contains("upmd_write_state")
            || code.contains("upmd_state_escape")
    }

    fn state_capture_env_vars(&self, fifos: &crate::FifoPaths) -> Vec<(String, String)> {
        vec![(
            "UPMD_STATE_FIFO".to_string(),
            fifos.state_fifo.to_string_lossy().to_string(),
        )]
    }

    fn plan<'a>(&self, code: &CodeInput<'a>) -> Result<ExecutionPlan<'a>> {
        let mut plan = ExecutionPlan::new();

        let (binary, prefix) = self.resolve_binary()?;
        let rs_file = format!("main_{}.rs", code.id);
        let binary_name = format!("app_{}", code.id);

        // State capture wraps user code with capture functions and call.
        // Without capture, just wraps in a valid Rust main if needed.
        let rust_content = if code.state_capture.enabled && self.supports_state_capture() {
            if self.check_name_collisions(code.content) {
                tracing::warn!("Name collision detected in Rust code, skipping state capture");
                wrap_rust_code(code.content)
            } else {
                let state_capture = self
                    .generate_state_capture(code.state_capture.code_id)
                    .ok_or_else(|| anyhow::anyhow!("Failed to generate state capture code"))?;
                let base = if code.content.contains("fn main(") {
                    // Inject capture call before the last } of main
                    inject_rust_main_call(code.content)
                } else {
                    format!("fn main() {{\n{}\nupmd_capture_state();\n}}", code.content)
                };
                format!("{}\n\n{}", base, state_capture)
            }
        } else {
            wrap_rust_code(code.content)
        };

        let mut compile_args = vec![std::borrow::Cow::Borrowed(binary.as_str())];
        for arg in &prefix {
            compile_args.push(std::borrow::Cow::Borrowed(arg.as_str()));
        }
        compile_args.push(std::borrow::Cow::Borrowed(rs_file.as_str()));
        compile_args.extend(
            self.options()
                .extra_args
                .iter()
                .map(|s| std::borrow::Cow::Owned(s.clone())),
        );
        compile_args.push(std::borrow::Cow::Borrowed("-o"));
        compile_args.push(std::borrow::Cow::Borrowed(binary_name.as_str()));

        plan.requires_file()
            .file(&rs_file, rust_content)
            .working_dir(".")
            .command(compile_args)
            .command([format!("./{}", binary_name)])
            .cleanup(&binary_name)
            .cleanup(&rs_file)
            .apply_options(self.options());

        Ok(plan)
    }

    fn options(&self) -> &crate::RunnerOptions {
        &self.options
    }

    fn language(&self) -> &crate::Language {
        &self.language
    }

    fn resolve_binary(&self) -> Result<(String, Vec<String>)> {
        // 1. Explicit user config.
        if let Some(bin) = &self.options().bin {
            let prefix = if bin.contains("cargo") {
                vec!["rustc".to_string(), "--".to_string()]
            } else {
                Vec::new()
            };
            return Ok((bin.clone(), prefix));
        }

        // 2. rustc (direct compile)
        if which::which("rustc").is_ok() {
            return Ok(("rustc".to_string(), Vec::new()));
        }

        // 3. cargo (wraps rustc)
        if which::which("cargo").is_ok() {
            return Ok((
                "cargo".to_string(),
                vec!["rustc".to_string(), "--".to_string()],
            ));
        }

        Err(anyhow::anyhow!(
            "No Rust compiler found. Install rustc or cargo."
        ))
    }
}

/// Wraps user code in a fn main if it doesn't already have one.
fn wrap_rust_code(content: &str) -> String {
    if content.contains("fn main(") {
        content.to_string()
    } else {
        format!("fn main() {{\n{}\n}}", content)
    }
}

/// Injects `upmd_capture_state()` before the last `}` of the main function.
/// Pragmatic: works for typical single-main programs.
fn inject_rust_main_call(content: &str) -> String {
    if let Some(pos) = content.trim_end().rfind('}') {
        format!(
            "{}\n\tupmd_capture_state();\n{}",
            &content[..pos],
            &content[pos..]
        )
    } else {
        format!("fn main() {{\n{}\n\tupmd_capture_state();\n}}", content)
    }
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

    fn commands_str(plan: &ExecutionPlan) -> String {
        plan.commands
            .iter()
            .map(|cmd| cmd.iter().map(|a| a.as_ref()).collect::<Vec<_>>().join(" "))
            .collect::<Vec<_>>()
            .join(" && ")
    }

    fn runner() -> Rust {
        Rust::with_options(RunnerOptions {
            bin: Some("rustc".into()),
            ..Default::default()
        })
    }

    #[test]
    fn test_simple_code_wrapped_in_main() {
        let state = no_capture();
        let input = CodeInput {
            id: 1,
            content: "println!(\"hi\");",
            language: Rust::get().language(),
            state_capture: &state,
        };
        let plan = runner().plan(&input).unwrap();
        let (_, content) = &plan.files[0];
        assert!(content.contains("fn main()"), "got: {content}");
    }

    #[test]
    fn test_existing_main_not_double_wrapped() {
        let state = no_capture();
        let src = "fn main() { println!(\"hi\"); }";
        let input = CodeInput {
            id: 2,
            content: src,
            language: Rust::get().language(),
            state_capture: &state,
        };
        let plan = runner().plan(&input).unwrap();
        let (_, content) = &plan.files[0];
        assert_eq!(
            content.matches("fn main").count(),
            1,
            "double-wrapped: {content}"
        );
    }

    #[test]
    fn test_state_capture_injects_code() {
        let state = with_capture();
        let input = CodeInput {
            id: 1,
            content: "fn main() { println!(\"hi\"); }",
            language: Rust::get().language(),
            state_capture: &state,
        };
        let plan = runner().plan(&input).unwrap();
        let (_, content) = &plan.files[0];
        assert!(
            content.contains("upmd_capture_state"),
            "missing capture func"
        );
        assert!(content.contains("upmd_write_state"), "missing state func");
    }

    #[test]
    fn test_stdlib_use_does_not_need_cargo() {
        let state = no_capture();
        let input = CodeInput {
            id: 3,
            content: "use std::collections::HashMap;\nfn main() {}",
            language: Rust::get().language(),
            state_capture: &state,
        };
        let plan = runner().plan(&input).unwrap();
        assert_eq!(commands_str(&plan), "rustc main_3.rs -o app_3 && ./app_3");
    }

    #[test]
    fn test_compile_then_run_commands() {
        let state = no_capture();
        let input = CodeInput {
            id: 4,
            content: "fn main() { println!(\"hi\"); }",
            language: Rust::get().language(),
            state_capture: &state,
        };
        let plan = runner().plan(&input).unwrap();
        assert_eq!(commands_str(&plan), "rustc main_4.rs -o app_4 && ./app_4");
    }

    #[test]
    fn test_cargo_fallback_inserts_rustc_flag() {
        let state = no_capture();
        let input = CodeInput {
            id: 5,
            content: "fn main() { println!(\"hi\"); }",
            language: Rust::get().language(),
            state_capture: &state,
        };
        let runner = Rust::with_options(RunnerOptions {
            bin: Some("cargo".into()),
            ..Default::default()
        });
        let plan = runner.plan(&input).unwrap();
        assert_eq!(
            commands_str(&plan),
            "cargo rustc -- main_5.rs -o app_5 && ./app_5"
        );
    }
}
