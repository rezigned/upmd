//! PHP language runner

use anyhow::Result;

use super::PHP;
use crate::{CodeInput, ExecutionPlan, LanguageRunner};

impl LanguageRunner for PHP {
    fn plan<'a>(&self, code: &CodeInput<'a>) -> Result<ExecutionPlan<'a>> {
        let mut plan = ExecutionPlan::new();

        if self.language().supports_inline && code.content.lines().count() == 1 {
            let (binary, _) = self.resolve_binary()?;
            let mut args = self.options().extra_args.clone();
            args.push("-r".to_string());
            args.push(code.content.to_string());
            plan.executable(binary, args);
        } else {
            let filename = format!("script_{}.{}", code.id, self.language().file_extension);
            let php_content = if code.content.starts_with("<?php") {
                code.content.to_string()
            } else {
                format!("<?php\n{}", code.content)
            };

            let (binary, _) = self.resolve_binary()?;
            let mut args = self.options().extra_args.clone();
            args.push(filename.clone());
            plan.requires_file()
                .file(&filename, php_content)
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

    fn runner() -> PHP {
        PHP::with_options(RunnerOptions {
            bin: Some("php".into()),
            ..Default::default()
        })
    }

    #[test]
    fn test_inline_uses_dash_r() {
        let state = no_capture();
        let input = CodeInput {
            id: 1,
            content: "echo 'hello';",
            language: PHP::get().language(),
            state_capture: &state,
        };
        let plan = runner().plan(&input).unwrap();
        assert_eq!(executable_str(&plan), "php -r echo 'hello';");
        assert!(!plan.requires_file);
    }

    #[test]
    fn test_multiline_wraps_php_tag() {
        let state = no_capture();
        let input = CodeInput {
            id: 2,
            content: "echo 'a';\necho 'b';",
            language: PHP::get().language(),
            state_capture: &state,
        };
        let plan = runner().plan(&input).unwrap();
        assert!(plan.requires_file);
        let (_, content) = &plan.files[0];
        assert!(content.starts_with("<?php\n"), "got: {content}");
        assert_eq!(executable_str(&plan), "php script_2.php");
    }

    #[test]
    fn test_existing_php_tag_not_doubled() {
        let state = no_capture();
        let input = CodeInput {
            id: 3,
            content: "<?php\necho 'hi';",
            language: PHP::get().language(),
            state_capture: &state,
        };
        let plan = runner().plan(&input).unwrap();
        let (_, content) = &plan.files[0];
        assert!(
            content.starts_with("<?php\necho"),
            "tag doubled or missing: {content}"
        );
    }
}
