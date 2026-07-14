//! Ruby language runner

use anyhow::Result;

use super::Ruby;
use crate::{CodeInput, ExecutionPlan, LanguageRunner};

impl LanguageRunner for Ruby {
    fn plan<'a>(&self, code: &CodeInput<'a>) -> Result<ExecutionPlan<'a>> {
        let mut plan = ExecutionPlan::new();

        if self.language().supports_inline && code.content.lines().count() == 1 {
            let (binary, _) = self.resolve_binary()?;
            let mut args = self.options().extra_args.clone();
            args.push("-e".to_string());
            args.push(code.content.to_string());
            plan.executable(binary, args);
        } else {
            let filename = format!("script_{}.{}", code.id, self.language().file_extension);
            let (binary, _) = self.resolve_binary()?;
            let mut args = self.options().extra_args.clone();
            args.push(filename.clone());
            plan.requires_file()
                .file(&filename, code.content)
                .executable(binary, args)
                .cleanup(&filename);
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

    fn runner() -> Ruby {
        Ruby::with_options(RunnerOptions {
            bin: Some("ruby".into()),
            ..Default::default()
        })
    }

    #[test]
    fn test_inline_uses_dash_e() {
        let state = no_capture();
        let input = CodeInput {
            id: 1,
            content: "puts 'hello'",
            language: Ruby::get().language(),
            state_capture: &state,
        };
        let plan = runner().plan(&input).unwrap();
        assert_eq!(executable_str(&plan), "ruby -e puts 'hello'");
        assert!(!plan.requires_file);
    }

    #[test]
    fn test_multiline_uses_file() {
        let state = no_capture();
        let input = CodeInput {
            id: 2,
            content: "x = 1\nputs x",
            language: Ruby::get().language(),
            state_capture: &state,
        };
        let plan = runner().plan(&input).unwrap();
        assert!(plan.requires_file);
        assert_eq!(executable_str(&plan), "ruby script_2.rb");
    }
}
