//! Python language runner with state capture

use anyhow::Result;

use super::Python;
use crate::{CodeId, CodeInput, ExecutionPlan, FifoPaths, LanguageRunner};

impl LanguageRunner for Python {
    fn supports_state_capture(&self) -> bool {
        true
    }

    fn generate_state_capture(&self, _code_id: CodeId) -> Option<String> {
        Some(include_str!("state.py").to_string())
    }

    fn check_name_collisions(&self, code: &str) -> bool {
        code.contains("upmd_capture_state")
            || code.contains("upmd_write_state")
            || code.contains("upmd_state_escape")
    }

    fn state_capture_env_vars(&self, fifos: &FifoPaths) -> Vec<(String, String)> {
        vec![(
            "UPMD_STATE_FIFO".to_string(),
            fifos.state_fifo.to_string_lossy().to_string(),
        )]
    }

    fn plan<'a>(&self, code: &CodeInput<'a>) -> Result<ExecutionPlan<'a>> {
        let mut plan = ExecutionPlan::new();

        // Multi-line, imports, or state capture require a file.
        // Single-line without capture runs inline via -c.
        if self.needs_file_execution(code) || code.state_capture.enabled {
            let filename = format!("script_{}.py", code.id);

            let content = if code.state_capture.enabled && self.supports_state_capture() {
                if self.check_name_collisions(code.content) {
                    tracing::warn!(
                        "Name collision detected in Python code, skipping state capture"
                    );
                    code.content.to_string()
                } else {
                    let state_capture = self
                        .generate_state_capture(code.state_capture.code_id)
                        .ok_or_else(|| anyhow::anyhow!("Failed to generate state capture code"))?;
                    format!(
                        "{}\n\n{}\n\nupmd_capture_state()",
                        code.content, state_capture
                    )
                }
            } else {
                code.content.to_string()
            };

            let (binary, _) = self.resolve_binary()?;
            let mut args = self.options().extra_args.clone();
            args.push(filename.clone());
            plan.requires_file()
                .file(&filename, content)
                .executable(binary, args)
                .cleanup(&filename);
        } else {
            let (binary, _) = self.resolve_binary()?;
            let mut args = self.options().extra_args.clone();
            args.push("-c".to_string());
            args.push(code.content.to_string());
            plan.executable(binary, args);
        }

        plan.apply_options(self.options());
        Ok(plan)
    }

    fn options(&self) -> &crate::RunnerOptions {
        &self.options
    }

    fn language(&self) -> &crate::Language {
        &self.language
    }
}

impl Python {
    fn needs_file_execution(&self, code: &CodeInput<'_>) -> bool {
        code.content.lines().count() > 1
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

    fn executable_str(plan: &ExecutionPlan) -> String {
        match &plan.executable {
            Some(prog) => {
                let mut s = prog.binary.clone();
                for arg in &prog.args {
                    s.push(' ');
                    s.push_str(arg);
                }
                s
            }
            None => String::new(),
        }
    }

    fn runner() -> Python {
        Python::with_options(RunnerOptions {
            bin: Some("python3".into()),
            ..Default::default()
        })
    }

    #[test]
    fn test_inline_single_line() {
        let state = no_capture();
        let input = CodeInput {
            id: 1,
            content: "print('hi')",
            language: Python::get().language(),
            state_capture: &state,
        };
        let plan = runner().plan(&input).unwrap();
        assert_eq!(executable_str(&plan), "python3 -c print('hi')");
        assert!(!plan.requires_file);
    }

    #[test]
    fn test_import_forces_file() {
        let state = no_capture();
        let input = CodeInput {
            id: 2,
            content: "import os\nprint(os.getcwd())",
            language: Python::get().language(),
            state_capture: &state,
        };
        let plan = runner().plan(&input).unwrap();
        assert!(plan.requires_file);
        assert_eq!(executable_str(&plan), "python3 script_2.py");
    }

    #[test]
    fn test_state_capture_injects_code() {
        let state = with_capture();
        let input = CodeInput {
            id: 1,
            content: "print('hello')",
            language: Python::get().language(),
            state_capture: &state,
        };
        let plan = runner().plan(&input).unwrap();
        assert!(
            plan.requires_file,
            "state capture should use file execution"
        );
        let (_, content) = &plan.files[0];
        assert!(
            content.contains("upmd_capture_state"),
            "missing capture call"
        );
        assert!(content.contains("upmd_write_state"), "missing state func");
    }
}
