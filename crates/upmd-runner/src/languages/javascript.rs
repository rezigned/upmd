//! JavaScript language runner

use anyhow::Result;

use super::JavaScript;
use crate::{CodeInput, ExecutionPlan, LanguageRunner};

impl LanguageRunner for JavaScript {
    fn plan<'a>(&self, code: &CodeInput<'a>) -> Result<ExecutionPlan<'a>> {
        let mut plan = ExecutionPlan::new();

        // Multi-line code or imports/requires need a file; single-line runs inline via -e.
        if self.needs_file_execution(code) {
            let filename = format!("script_{}.js", code.id);

            let (binary, _) = self.resolve_binary()?;
            let mut args = self.options().extra_args.clone();
            args.push(filename.clone());
            plan.requires_file()
                .file(&filename, code.content)
                .executable(binary, args)
                .cleanup(filename);
        } else {
            let (binary, _) = self.resolve_binary()?;
            let mut args = self.options().extra_args.clone();
            args.push("-e".to_string());
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

impl JavaScript {
    fn needs_file_execution(&self, code: &CodeInput<'_>) -> bool {
        code.content.lines().count() > 1 || code.content.contains("import ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RunnerOptions, StateCaptureContext};

    fn no_capture() -> StateCaptureContext {
        StateCaptureContext {
            enabled: false,
            fifos: None,
            code_id: 1,
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

    fn runner() -> JavaScript {
        JavaScript::with_options(RunnerOptions {
            bin: Some("node".into()),
            ..Default::default()
        })
    }

    #[test]
    fn test_inline_single_line() {
        let state = no_capture();
        let input = CodeInput {
            id: 1,
            content: "console.log('hi')",
            language: JavaScript::get().language(),
            state_capture: &state,
        };
        let plan = runner().plan(&input).unwrap();
        assert_eq!(executable_str(&plan), "node -e console.log('hi')");
        assert!(!plan.requires_file);
    }

    #[test]
    fn test_multiline_uses_file() {
        let state = no_capture();
        let input = CodeInput {
            id: 2,
            content: "const x = 1;\nconsole.log(x);",
            language: JavaScript::get().language(),
            state_capture: &state,
        };
        let plan = runner().plan(&input).unwrap();
        assert!(plan.requires_file);
        assert_eq!(executable_str(&plan), "node script_2.js");
    }

    #[test]
    fn test_import_forces_file() {
        let state = no_capture();
        let input = CodeInput {
            id: 3,
            content: "import fs from 'fs'",
            language: JavaScript::get().language(),
            state_capture: &state,
        };
        let plan = runner().plan(&input).unwrap();
        assert!(plan.requires_file);
        assert_eq!(executable_str(&plan), "node script_3.js");
    }

    #[test]
    fn test_extra_args_prepended() {
        let state = no_capture();
        let input = CodeInput {
            id: 1,
            content: "console.log('hi')",
            language: JavaScript::get().language(),
            state_capture: &state,
        };
        let runner = JavaScript::with_options(RunnerOptions {
            bin: Some("node".into()),
            extra_args: vec!["--no-warnings".into()],
            ..Default::default()
        });
        let plan = runner.plan(&input).unwrap();
        assert_eq!(
            executable_str(&plan),
            "node --no-warnings -e console.log('hi')"
        );
    }

    #[test]
    fn test_env_merged() {
        let state = no_capture();
        let input = CodeInput {
            id: 1,
            content: "console.log('hi')",
            language: JavaScript::get().language(),
            state_capture: &state,
        };
        let runner = JavaScript::with_options(RunnerOptions {
            bin: Some("node".into()),
            env: [("NODE_ENV".into(), "test".into())].into(),
            ..Default::default()
        });
        let plan = runner.plan(&input).unwrap();
        assert_eq!(plan.env_vars.get("NODE_ENV"), Some(&"test".to_string()));
    }
}
