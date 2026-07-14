//! C language runner

use anyhow::Result;

use super::C;
use crate::{CodeInput, ExecutionPlan, LanguageRunner};

impl LanguageRunner for C {
    fn plan<'a>(&self, code: &CodeInput<'a>) -> Result<ExecutionPlan<'a>> {
        let mut plan = ExecutionPlan::new();

        let (binary, _) = self.resolve_binary()?;
        let c_file = format!("main_{}.c", code.id);
        let binary_name = format!("app_{}", code.id);

        let content = wrap_c_code(code.content);

        let mut compile_args = vec![
            std::borrow::Cow::Borrowed(binary.as_str()),
            std::borrow::Cow::Borrowed(c_file.as_str()),
        ];
        compile_args.extend(
            self.options()
                .extra_args
                .iter()
                .map(|s| std::borrow::Cow::Owned(s.clone())),
        );
        compile_args.push(std::borrow::Cow::Borrowed("-o"));
        compile_args.push(std::borrow::Cow::Borrowed(binary_name.as_str()));

        plan.requires_file()
            .file(&c_file, content)
            .working_dir(".")
            .command(compile_args)
            .command([format!("./{}", binary_name)])
            .cleanup(&binary_name)
            .cleanup(&c_file)
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

fn wrap_c_code(content: &str) -> String {
    if content.contains("main(") {
        content.to_string()
    } else {
        format!(
            "#include <stdio.h>\n#include <stdlib.h>\nint main() {{\n{}\nreturn 0;\n}}",
            content
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CodeInput, RunnerOptions, StateCaptureContext};

    fn no_capture() -> StateCaptureContext {
        StateCaptureContext {
            enabled: false,
            fifos: None,
            code_id: 1,
        }
    }

    fn runner() -> C {
        C::with_options(RunnerOptions {
            bin: Some("gcc".into()),
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
    fn test_bare_code_wrapped_in_main() {
        let state = no_capture();
        let input = CodeInput {
            id: 1,
            content: "printf(\"hello\\n\");",
            language: C::get().language(),
            state_capture: &state,
        };
        let plan = runner().plan(&input).unwrap();
        let (_, content) = &plan.files[0];
        assert!(content.contains("#include <stdio.h>"), "got: {content}");
        assert!(content.contains("#include <stdlib.h>"), "got: {content}");
        assert!(content.contains("int main()"), "got: {content}");
        assert!(content.contains("printf"), "got: {content}");
        assert!(content.contains("return 0;"), "got: {content}");
    }

    #[test]
    fn test_existing_main_not_double_wrapped() {
        let state = no_capture();
        let src = "#include <stdio.h>\nint main() { printf(\"hi\"); return 0; }";
        let input = CodeInput {
            id: 2,
            content: src,
            language: C::get().language(),
            state_capture: &state,
        };
        let plan = runner().plan(&input).unwrap();
        let (_, content) = &plan.files[0];
        assert_eq!(content, src);
    }

    #[test]
    fn test_compile_then_run_commands() {
        let state = no_capture();
        let input = CodeInput {
            id: 3,
            content: "int main() { return 0; }",
            language: C::get().language(),
            state_capture: &state,
        };
        let plan = runner().plan(&input).unwrap();
        assert_eq!(commands_str(&plan), "gcc main_3.c -o app_3 && ./app_3");
    }

    #[test]
    fn test_void_main_not_double_wrapped() {
        let state = no_capture();
        let src = "void main() { }";
        let input = CodeInput {
            id: 4,
            content: src,
            language: C::get().language(),
            state_capture: &state,
        };
        let plan = runner().plan(&input).unwrap();
        let (_, content) = &plan.files[0];
        assert_eq!(content, src);
    }
}
