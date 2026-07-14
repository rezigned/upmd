//! Zig language runner

use anyhow::Result;

use super::Zig;
use crate::{CodeInput, ExecutionPlan, LanguageRunner};

impl LanguageRunner for Zig {
    fn plan<'a>(&self, code: &CodeInput<'a>) -> Result<ExecutionPlan<'a>> {
        let mut plan = ExecutionPlan::new();

        let (binary, _) = self.resolve_binary()?;
        let zig_file = format!("main_{}.zig", code.id);
        let binary_name = format!("app_{}", code.id);

        let content = wrap_zig_code(code.content);

        let mut compile_args = vec![
            std::borrow::Cow::Borrowed(binary.as_str()),
            std::borrow::Cow::Borrowed("build-exe"),
            std::borrow::Cow::Borrowed(zig_file.as_str()),
        ];
        compile_args.extend(
            self.options()
                .extra_args
                .iter()
                .map(|s| std::borrow::Cow::Owned(s.clone())),
        );
        compile_args.push(std::borrow::Cow::Borrowed("--name"));
        compile_args.push(std::borrow::Cow::Borrowed(binary_name.as_str()));

        plan.requires_file()
            .file(&zig_file, content)
            .working_dir(".")
            .command(compile_args)
            .command([format!("./{}", binary_name)])
            .cleanup(&binary_name)
            .cleanup(format!("{}.o", binary_name))
            .cleanup(&zig_file)
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

fn wrap_zig_code(content: &str) -> String {
    if content.contains("pub fn main(") {
        content.to_string()
    } else {
        let prefix = if content.contains("@import(\"std\")") || content.contains("const std") {
            String::new()
        } else {
            "const std = @import(\"std\");\n".to_string()
        };
        format!("{}pub fn main() !void {{\n{}\n}}", prefix, content)
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

    fn runner() -> Zig {
        Zig::with_options(RunnerOptions {
            bin: Some("zig".into()),
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
            content: "std.debug.print(\"hello\\n\", .{{}});",
            language: Zig::get().language(),
            state_capture: &state,
        };
        let plan = runner().plan(&input).unwrap();
        let (_, content) = &plan.files[0];
        assert!(
            content.contains("const std = @import(\"std\")"),
            "got: {content}"
        );
        assert!(content.contains("pub fn main()"), "got: {content}");
        assert_eq!(
            content.matches("const std = @import(\"std\")").count(),
            1,
            "std import should appear exactly once: {content}"
        );
    }

    #[test]
    fn test_bare_code_with_existing_std_import() {
        let state = no_capture();
        let src = "const std = @import(\"std\");\nstd.debug.print(\"hi\\n\", .{});";
        let input = CodeInput {
            id: 4,
            content: src,
            language: Zig::get().language(),
            state_capture: &state,
        };
        let plan = runner().plan(&input).unwrap();
        let (_, content) = &plan.files[0];
        assert!(content.contains("pub fn main()"), "got: {content}");
        assert_eq!(
            content.matches("const std = @import(\"std\")").count(),
            1,
            "should not duplicate existing import: {content}"
        );
    }

    #[test]
    fn test_existing_main_not_double_wrapped() {
        let state = no_capture();
        let src = "const std = @import(\"std\");\npub fn main() !void { std.debug.print(\"hi\\n\", .{}); }";
        let input = CodeInput {
            id: 2,
            content: src,
            language: Zig::get().language(),
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
            content: "pub fn main() !void {}",
            language: Zig::get().language(),
            state_capture: &state,
        };
        let plan = runner().plan(&input).unwrap();
        assert_eq!(
            commands_str(&plan),
            "zig build-exe main_3.zig --name app_3 && ./app_3"
        );
    }
}
