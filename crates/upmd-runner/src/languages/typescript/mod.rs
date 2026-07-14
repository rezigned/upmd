//! TypeScript language runner with state capture
//!
//! When no explicit [`bin`](crate::RunnerOptions::bin) is configured, the
//! runner probes candidates via `which` in the following order:
//!
//! 1. Explicit user config (`[bin:...]` attr or `binaries.typescript` in config)
//! 2. `node --experimental-strip-types` (Node.js 22.6+ native runner)
//! 3. `npx tsx` (discovers project-local tsx. May prompt to install if missing.)
//! 4. `ts-node --compiler-options ...` (legacy, direct global install)
//!
//! If none are found the plan step returns a clear error with installation
//! instructions.

use anyhow::Result;

use super::TypeScript;
use crate::{CodeId, CodeInput, ExecutionPlan, FifoPaths, LanguageRunner};

impl LanguageRunner for TypeScript {
    fn supports_state_capture(&self) -> bool {
        true
    }

    fn generate_state_capture(&self, _code_id: CodeId) -> Option<String> {
        Some(include_str!("state.ts").to_string())
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

        let ts_file = format!("script_{}.ts", code.id);

        let ts_content = if code.state_capture.enabled && self.supports_state_capture() {
            if self.check_name_collisions(code.content) {
                tracing::warn!(
                    "Name collision detected in TypeScript code, skipping state capture"
                );
                code.content.to_string()
            } else {
                let state_capture = self
                    .generate_state_capture(code.state_capture.code_id)
                    .ok_or_else(|| anyhow::anyhow!("Failed to generate state capture code"))?;
                format!(
                    "{}\n\n{}\n\nupmdCaptureState();",
                    code.content, state_capture
                )
            }
        } else {
            code.content.to_string()
        };

        let (binary, mut args) = self.resolve_binary()?;

        args.extend(self.options().extra_args.iter().cloned());
        args.push(ts_file.clone());

        plan.requires_file()
            .file(&ts_file, ts_content)
            .executable(binary, args)
            .cleanup(&ts_file);

        for (key, value) in &self.options().env {
            plan.env(key.clone(), value.clone());
        }

        Ok(plan)
    }

    fn options(&self) -> &crate::RunnerOptions {
        &self.options
    }

    fn language(&self) -> &crate::Language {
        &self.language
    }

    fn resolve_binary(&self) -> Result<(String, Vec<String>)> {
        // 1. Explicit user config ([bin:...] attr or config.toml).
        //    Also covers bun: [bin:bun] produces "bun script.ts" which runs
        //    TypeScript natively.
        if let Some(bin) = &self.options().bin {
            return Ok((bin.clone(), Vec::new()));
        }

        // 2. Node.js native strip-types (22.6+)
        if let Ok(node_path) = which::which("node") {
            if let Ok(output) = std::process::Command::new(&node_path)
                .arg("--version")
                .output()
            {
                let v = String::from_utf8_lossy(&output.stdout);
                let parts: Vec<u32> = v
                    .trim()
                    .strip_prefix('v')
                    .unwrap_or(&v)
                    .split('.')
                    .filter_map(|s| s.parse().ok())
                    .collect();
                let supported = parts.first().is_some_and(|&m| m > 22)
                    || (parts.first() == Some(&22) && parts.get(1).is_some_and(|&p| p >= 6));
                if supported {
                    return Ok((
                        node_path.to_string_lossy().to_string(),
                        vec!["--experimental-strip-types".to_string()],
                    ));
                }
            }
        }

        // 3. npx tsx (finds project-local tsx. May prompt to install.)
        if which::which("npx").is_ok() {
            return Ok(("npx".to_string(), vec!["tsx".to_string()]));
        }

        // 4. ts-node direct (legacy, avoids npx install prompt)
        if which::which("ts-node").is_ok() {
            return Ok((
                "ts-node".to_string(),
                vec![
                    "--compiler-options".to_string(),
                    // ts-node v10 on Node.js >=26 silently discards stdout
                    // with ESM module resolution. Force CommonJS output.
                    r#"{"module":"commonjs"}"#.to_string(),
                ],
            ));
        }

        Err(anyhow::anyhow!(
            "No TypeScript runner found. Install Node.js 22.6+ or run `npm install -D tsx`."
        ))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FifoPaths, StateCaptureContext};
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

    #[test]
    fn explicit_bin_bypasses_resolver() {
        let state = no_capture();
        let input = CodeInput {
            id: 1,
            content: "console.log('hi')",
            language: TypeScript::get().language(),
            state_capture: &state,
        };
        let runner = TypeScript::with_options(crate::RunnerOptions {
            bin: Some("tsx".into()),
            ..Default::default()
        });
        let plan = runner.plan(&input).unwrap();
        assert_eq!(executable_str(&plan), "tsx script_1.ts");
    }

    #[test]
    fn state_capture_injects_code() {
        let state = with_capture();
        let input = CodeInput {
            id: 1,
            content: "console.log('hi')",
            language: TypeScript::get().language(),
            state_capture: &state,
        };
        let plan = TypeScript::new().plan(&input).unwrap();
        let (_, content) = &plan.files[0];
        assert!(content.contains("upmdCaptureState"), "missing capture func");
        assert!(content.contains("upmdWriteState"), "missing state func");
    }

    #[test]
    fn extra_args_are_prepended() {
        let state = no_capture();
        let input = CodeInput {
            id: 1,
            content: "console.log('hi')",
            language: TypeScript::get().language(),
            state_capture: &state,
        };
        let runner = TypeScript::with_options(crate::RunnerOptions {
            extra_args: vec!["--transpile-only".into()],
            ..Default::default()
        });
        let plan = runner.plan(&input).unwrap();
        let s = executable_str(&plan);
        assert!(s.contains("--transpile-only"), "missing extra arg in: {s}");
    }

    #[test]
    fn env_is_merged() {
        let state = no_capture();
        let input = CodeInput {
            id: 1,
            content: "console.log('hi')",
            language: TypeScript::get().language(),
            state_capture: &state,
        };
        let runner = TypeScript::with_options(crate::RunnerOptions {
            env: [("NODE_ENV".into(), "test".into())].into(),
            ..Default::default()
        });
        let plan = runner.plan(&input).unwrap();
        assert_eq!(plan.env_vars.get("NODE_ENV"), Some(&"test".to_string()));
    }
}
