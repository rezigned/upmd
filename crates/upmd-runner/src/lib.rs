//! # Code Runners
//!
//! A library for defining and executing code blocks from various programming languages.
//!
//! This crate provides:
//! - Language definitions with metadata (syntax highlighting, file extensions, etc.)
//! - Execution planning for code blocks
//! - Interpreter validation and discovery
//! - Experimental state capture functionality (environment variables and working directory)
//!
//! ## Example
//!
//! ```rust,ignore
//! use upmd_runner::{find, CodeInput, Language};
//!
//! let input = CodeInput {
//!     id: 1,
//!     content: "echo 'Hello, World!'",
//!     language: &Language::default(),
//! };
//!
//! let runner = find("bash").unwrap();
//! let plan = runner.plan(&input).unwrap();
//! ```

use std::{borrow::Cow, collections::HashMap, path::PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};

pub mod languages;
pub mod quoting;
pub mod workspace;

pub type CodeId = u32;
pub type Command<'a> = Vec<Cow<'a, str>>;

pub use workspace::TempWorkspace;
pub use workspace::{WorkspaceAdapter, WorkspaceExecutionExt};

/// Path to the FIFO for state capture.
#[derive(Debug, Clone)]
pub struct FifoPaths {
    pub state_fifo: PathBuf,
}

/// PTY-level state capture config: the cleanup scope directory plus optional
/// state FIFO path.
#[derive(Debug, Clone)]
pub struct StateCapture {
    pub dir: PathBuf,
    pub fifos: Option<FifoPaths>,
}

/// State capture context for code execution
#[derive(Debug, Clone)]
pub struct StateCaptureContext {
    pub enabled: bool,
    pub fifos: Option<FifoPaths>,
    pub code_id: CodeId,
}

/// Input representation for code execution.
/// This is a simple struct that decouples the runner from any specific markdown parser.
#[derive(Debug, Clone)]
pub struct CodeInput<'a> {
    pub id: CodeId,
    pub content: &'a str,
    pub language: &'a Language,
    pub state_capture: &'a StateCaptureContext,
}

/// Language metadata for a programming language.
#[derive(Debug, PartialEq, Clone, Default)]
pub struct Language {
    pub name: String,
    pub syntax: String,
    pub kind: Kind,
    pub aliases: &'static [&'static str],
    pub binaries: &'static [&'static str],
    pub file_extension: &'static str,
    pub supports_inline: bool,
    pub supports_file: bool,
    pub package_manager: Option<&'static str>,
}

/// The kind of language (interpreted, compiled, shell, etc.)
#[derive(Debug, PartialEq, Clone, Default)]
pub enum Kind {
    #[default]
    Interpreted,
    Compiled,
    Shell,
}

/// A direct executable program (binary + args), used in place of a shell script.
#[derive(Debug, Clone)]
pub struct Program {
    pub binary: String,
    pub args: Vec<String>,
}

/// Shell quoting style used when an execution plan is assembled as a script.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ShellQuoteStyle {
    #[default]
    Posix,
    Cmd,
    PowerShell,
}

/// Execution plan containing all steps needed to run code.
#[derive(Default)]
pub struct ExecutionPlan<'a> {
    /// Commands to execute, each as a list of arguments.
    pub commands: Vec<Vec<std::borrow::Cow<'a, str>>>,
    /// Working directory for execution.
    pub working_dir: Option<PathBuf>,
    /// Environment variables to set.
    pub env_vars: HashMap<String, String>,
    /// Files to create before execution (path, content).
    pub files: Vec<(PathBuf, String)>,
    /// Files to clean up after execution.
    pub cleanup_files: Vec<PathBuf>,
    /// Whether this execution requires a file (vs inline execution).
    pub requires_file: bool,
    /// Whether the workspace should resolve relative `./` paths in commands
    /// against `target_dir`. Set by shell runners.
    pub resolve_paths: bool,
    /// Optional wrapper applied to the assembled command string to produce
    /// the final script. `None` means use the commands as-is.
    pub wrap: Option<Box<dyn Fn(String) -> String + Send + Sync>>,
    /// When set, spawn the binary directly instead of building a shell script
    /// from `commands`. Used by interpreted languages (Python, Ruby, etc.)
    /// to avoid the `sh -c` wrapper.
    pub executable: Option<Program>,
    /// Shell quoting rules for script-based execution.
    pub quote_style: ShellQuoteStyle,
}

impl<'a> ExecutionPlan<'a> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn command<I, S>(&mut self, args: I) -> &mut Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let cmd: Vec<Cow<'a, str>> = args
            .into_iter()
            .map(|s| Cow::Owned(s.as_ref().to_string()))
            .collect();
        self.commands.push(cmd);
        self
    }

    pub fn file(&mut self, path: impl Into<PathBuf>, content: impl Into<String>) -> &mut Self {
        self.files.push((path.into(), content.into()));
        self
    }

    pub fn cleanup(&mut self, path: impl Into<PathBuf>) -> &mut Self {
        self.cleanup_files.push(path.into());
        self
    }

    pub fn working_dir(&mut self, path: impl Into<PathBuf>) -> &mut Self {
        self.working_dir = Some(path.into());
        self
    }

    pub fn env(&mut self, key: impl Into<String>, value: impl Into<String>) -> &mut Self {
        self.env_vars.insert(key.into(), value.into());
        self
    }

    pub fn requires_file(&mut self) -> &mut Self {
        self.requires_file = true;
        self
    }

    pub fn resolve_paths(&mut self) -> &mut Self {
        self.resolve_paths = true;
        self
    }

    pub fn wrap<F>(&mut self, f: F) -> &mut Self
    where
        F: Fn(String) -> String + Send + Sync + 'static,
    {
        self.wrap = Some(Box::new(f));
        self
    }

    pub fn executable(
        &mut self,
        binary: impl Into<String>,
        args: Vec<impl Into<String>>,
    ) -> &mut Self {
        self.executable = Some(Program {
            binary: binary.into(),
            args: args.into_iter().map(|a| a.into()).collect(),
        });
        self
    }

    pub fn quote_style(&mut self, style: ShellQuoteStyle) -> &mut Self {
        self.quote_style = style;
        self
    }

    /// Merges runner options that apply to every runner: environment variables
    /// are added to the plan's env_vars. Extra args are intentionally handled by
    /// each runner because their position depends on the binary/executable.
    pub fn apply_options(&mut self, options: &RunnerOptions) -> &mut Self {
        for (key, value) in &options.env {
            self.env_vars.insert(key.clone(), value.clone());
        }
        self
    }
}

impl<'a> std::fmt::Debug for ExecutionPlan<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExecutionPlan")
            .field("commands", &self.commands)
            .field("working_dir", &self.working_dir)
            .field("env_vars", &self.env_vars)
            .field("files", &self.files)
            .field("cleanup_files", &self.cleanup_files)
            .field("requires_file", &self.requires_file)
            .field("resolve_paths", &self.resolve_paths)
            .field("wrap", &"<fn>")
            .field("executable", &self.executable)
            .field("quote_style", &self.quote_style)
            .finish()
    }
}

/// Execution error types
#[derive(Debug, thiserror::Error)]
pub enum ExecutionError {
    #[error("Binary not found for language: {0}")]
    BinaryNotFound(String),
    #[error("Language not supported: {0}")]
    LanguageNotSupported(String),
    #[error("Execution timeout after {0:?}")]
    Timeout(std::time::Duration),
    #[error("File creation failed: {0}")]
    FileCreationFailed(String),
}

/// Common runtime options shared by all language runners.
///
/// These options allow callers to override defaults without subclassing or
/// forking the runner. Language-specific options live in each runner's own
/// `Options` type (e.g. `JavaScriptOptions`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RunnerOptions {
    /// Override the binary (name or absolute path).
    ///
    /// When set, this takes precedence over the runner's built-in candidate
    /// list. Useful for selecting `bun` instead of `node`, or pointing at a
    /// virtualenv Python, etc.
    pub bin: Option<String>,

    /// Extra arguments inserted *before* the script/file argument.
    ///
    /// Example: `["--experimental-vm-modules"]` for Node.
    pub extra_args: Vec<String>,

    /// Additional environment variables to inject at execution time.
    pub env: HashMap<String, String>,
}

/// Language runner trait.
/// Trait for language-specific execution logic.
///
/// This trait defines the interface for executing code in different programming languages.
/// It supports both traditional execution and experimental state capture features.
pub trait LanguageRunner {
    /// Creates an execution plan for the given code input.
    fn plan<'a>(&self, code: &CodeInput<'a>) -> Result<ExecutionPlan<'a>>;

    /// Returns the runner's options.
    fn options(&self) -> &RunnerOptions;

    /// Returns true if this runner supports state capture.
    fn supports_state_capture(&self) -> bool {
        false
    }

    /// Generates language-specific state capture code.
    fn generate_state_capture(&self, _code_id: CodeId) -> Option<String> {
        None
    }

    /// Checks for potential name collisions in user code.
    fn check_name_collisions(&self, _code: &str) -> bool {
        false
    }

    /// Returns additional environment variables needed for state capture.
    fn state_capture_env_vars(&self, _fifos: &FifoPaths) -> Vec<(String, String)> {
        vec![]
    }

    /// Returns the language metadata for this runner.
    fn language(&self) -> &Language;

    /// Resolves the binary and any required prefix args via `which`.
    ///
    /// Returns `(binary, args_prefix)`. The prefix is prepended to the
    /// runner's own args before the source file. When `options().bin` is
    /// set it is used directly without a `which` lookup.
    ///
    /// Runners that need version probes or wrapper subcommands override
    /// this method (e.g. TypeScript, Rust).
    fn resolve_binary(&self) -> Result<(String, Vec<String>)> {
        if let Some(bin) = &self.options().bin {
            return Ok((bin.clone(), Vec::new()));
        }
        for candidate in self.language().binaries {
            if which::which(candidate).is_ok() {
                return Ok((candidate.to_string(), Vec::new()));
            }
        }
        Err(ExecutionError::BinaryNotFound(self.language().binaries.join(", ")).into())
    }
}

/// Entry in the language registry.
///
/// Keeping this as a plain data type lets the `languages!` macro emit the
/// registry table as a normal `const` slice while still generating the runner
/// structs and lookup functions.
#[derive(Debug, Clone, Copy)]
pub struct RegistryEntry {
    /// Canonical language name.
    pub name: &'static str,
    /// Alternate identifiers accepted by [`find`].
    pub aliases: &'static [&'static str],
    /// Factory that builds a boxed runner with the given options.
    pub create: fn(RunnerOptions) -> Box<dyn LanguageRunner + Send + Sync>,
    /// Returns a trait-object reference to the shared default instance.
    pub default: fn() -> &'static (dyn LanguageRunner + Send + Sync),
}

// Language registry macro.
//
// Generates a concrete runner struct with default options, a shared lazy
// instance, and factory helpers for each language, and emits a declarative
// `REGISTRY` table plus the lookup functions that use it.
//
// `aliases` is pulled out as a special parameter so it can be reused for both
// the Language metadata and the RegistryEntry, avoiding duplication.
#[macro_export]
macro_rules! languages {
    ($(
        $name:ident {
            aliases: $aliases:expr,
            $($field:ident: $value:expr_2021),*
        }
    ),+ $(,)?) => {
        $(
            #[derive(::std::fmt::Debug, ::std::default::Default)]
            #[allow(clippy::upper_case_acronyms)]
            pub struct $name {
                pub(crate) language: $crate::Language,
                pub options: $crate::RunnerOptions,
            }

            impl $name {
                /// Creates a new runner with default options.
                pub fn new() -> Self {
                    Self {
                        language: $crate::Language {
                            name: stringify!($name).into(),
                            aliases: $aliases,
                            $($field: $value,)*
                        },
                        options: $crate::RunnerOptions::default(),
                    }
                }

                /// Creates a new runner with custom options.
                pub fn with_options(options: $crate::RunnerOptions) -> Self {
                    Self { options, ..Self::new() }
                }

                /// Returns the shared static instance, created lazily on first call.
                pub fn get() -> &'static Self {
                    static INSTANCE: ::std::sync::OnceLock<$name> = ::std::sync::OnceLock::new();
                    INSTANCE.get_or_init(Self::new)
                }

                /// Returns the language metadata.
                pub fn language(&self) -> &$crate::Language {
                    &self.language
                }

                /// Creates a boxed runner with custom options.
                pub fn create(options: $crate::RunnerOptions) -> Box<dyn $crate::LanguageRunner + Send + Sync> {
                    Box::new(Self::with_options(options))
                }

                /// Returns a trait-object reference to the shared default instance.
                pub fn default_runner() -> &'static (dyn $crate::LanguageRunner + Send + Sync) {
                    Self::get()
                }
            }
        )+

        /// Declarative registry of all supported languages.
        pub const REGISTRY: &[$crate::RegistryEntry] = &[
            $($crate::RegistryEntry {
                name: stringify!($name),
                aliases: $aliases,
                create: $name::create,
                default: $name::default_runner,
            }),+
        ];

        /// Finds a runner for the given [`Language`] by matching its name.
        ///
        /// Returns a zero-cost static reference - no heap allocation.
        pub fn find_by_language(
            language: &$crate::Language,
        ) -> $crate::Result<&'static (dyn $crate::LanguageRunner + Send + Sync)> {
            REGISTRY
                .iter()
                .find(|entry| entry.name == language.name)
                .map(|entry| (entry.default)())
                .ok_or_else(|| $crate::ExecutionError::LanguageNotSupported(language.name.clone()).into())
        }

        /// Creates a runner with custom [`RunnerOptions`] for the given language.
        ///
        /// Use this instead of [`find_by_language`] when you need to override
        /// the binary, add extra args, or inject environment variables.
        pub fn create_runner(
            language: &$crate::Language,
            options: $crate::RunnerOptions,
        ) -> $crate::Result<Box<dyn $crate::LanguageRunner + Send + Sync>> {
            REGISTRY
                .iter()
                .find(|entry| entry.name == language.name)
                .map(|entry| (entry.create)(options))
                .ok_or_else(|| $crate::ExecutionError::LanguageNotSupported(language.name.clone()).into())
        }

        fn matches_name_or_alias(entry: &$crate::RegistryEntry, name: &str) -> bool {
            entry.name.eq_ignore_ascii_case(name)
                || entry.aliases.iter().any(|alias| alias.eq_ignore_ascii_case(name))
        }

        /// Finds a runner by string name or alias, returning a static reference.
        pub fn find(name: &str) -> $crate::Result<&'static (dyn $crate::LanguageRunner + Send + Sync)> {
            REGISTRY
                .iter()
                .find(|entry| matches_name_or_alias(entry, name))
                .map(|entry| (entry.default)())
                .ok_or_else(|| $crate::ExecutionError::LanguageNotSupported(name.to_string()).into())
        }

        /// Finds a language by name or alias, or returns a default with the given name.
        pub fn find_or_default(name: &str) -> $crate::Language {
            find(name)
                .ok()
                .map(|runner| runner.language().clone())
                .unwrap_or_else(|| $crate::Language {
                    name: name.to_string(),
                    syntax: name.to_string(),
                    kind: $crate::Kind::Interpreted,
                    aliases: &[],
                    binaries: &[],
                    file_extension: "txt",
                    supports_inline: true,
                    supports_file: true,
                    package_manager: None,
                })
        }
    };
}

// Language definitions and the lookup registry live in languages/mod.rs.
// Re-export the lookup functions and all language structs.
pub use languages::{
    create_runner, find, find_by_language, find_or_default, Bash, Cmd, Fish, Go, JavaScript,
    PowerShell, Python, Ruby, Rust, Shell, TypeScript, Zig, Zsh, C, PHP,
};

/// Declarative registry of all supported languages.
pub use languages::REGISTRY;

/// Resolves a fence identifier to a full Language struct.
pub fn find_language(fence: &str) -> Language {
    find_or_default(fence)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve() {
        [("sh", "Shell"), ("zsh", "Zsh")]
            .iter()
            .for_each(|(bin, lang)| {
                let found = find_or_default(bin);
                assert_eq!(lang, &found.name.as_str());
            });

        let unknown = find_or_default("xyz");
        assert_eq!(unknown.file_extension, "txt");
    }

    #[test]
    fn test_find_or_default() {
        let r = find_or_default("");
        assert_eq!(r.name, "");
        assert_eq!(r.file_extension, "txt");
    }

    #[test]
    fn test_state_capture_support() {
        // Experimental languages should support state capture
        let rust = Rust::new();
        assert!(rust.supports_state_capture());

        let python = Python::new();
        assert!(python.supports_state_capture());

        let go = Go::new();
        assert!(go.supports_state_capture());

        let typescript = TypeScript::new();
        assert!(typescript.supports_state_capture());

        // Shell languages use different mechanism (workspace-level)
        // They don't need runner-level state capture support
        let bash = Bash::new();
        assert!(!bash.supports_state_capture());
    }

    #[test]
    fn test_state_capture_generation() {
        let rust = Rust::new();
        assert!(rust.generate_state_capture(1).is_some());

        let python = Python::new();
        assert!(python.generate_state_capture(1).is_some());

        let go = Go::new();
        assert!(go.generate_state_capture(1).is_some());

        let typescript = TypeScript::new();
        assert!(typescript.generate_state_capture(1).is_some());
    }

    #[test]
    fn test_collision_detection() {
        let rust = Rust::new();

        // Should detect collisions
        assert!(rust.check_name_collisions("upmd_capture_state"));
        assert!(rust.check_name_collisions("upmd_write_state"));
        assert!(rust.check_name_collisions("upmd_state_escape"));

        // Should not detect collisions in normal code
        assert!(!rust.check_name_collisions("fn main() { println!(\"hello\"); }"));
    }

    #[test]
    fn test_state_capture_env_vars() {
        use std::path::PathBuf;

        let fifos = FifoPaths {
            state_fifo: PathBuf::from("/tmp/state.fifo"),
        };

        let rust = Rust::new();
        let env_vars = rust.state_capture_env_vars(&fifos);

        assert_eq!(env_vars.len(), 1);
        assert!(env_vars
            .iter()
            .any(|(k, v)| k == "UPMD_STATE_FIFO" && v == "/tmp/state.fifo"));
    }

    #[test]
    fn test_shell_backward_compatibility() {
        // Test that shell languages work the same regardless of state capture setting
        let bash = Bash::new();
        let bash_lang = Bash::get().language().clone();
        let input_enabled = CodeInput {
            id: 1,
            content: "echo 'hello'",
            language: &bash_lang,
            state_capture: &StateCaptureContext {
                enabled: true,
                fifos: None,
                code_id: 1,
            },
        };

        let input_disabled = CodeInput {
            state_capture: &StateCaptureContext {
                enabled: false,
                fifos: None,
                code_id: 1,
            },
            ..input_enabled
        };

        // Both should succeed
        let plan_enabled = bash.plan(&input_enabled);
        let plan_disabled = bash.plan(&input_disabled);

        assert!(plan_enabled.is_ok());
        assert!(plan_disabled.is_ok());
    }

    #[test]
    fn test_experimental_error_handling() {
        // Test that experimental features fail gracefully
        let rust = Rust::new();
        let rust_lang = Rust::get().language().clone();
        let input = CodeInput {
            id: 1,
            content: "fn main() { println!(\"hello\"); }",
            language: &rust_lang,
            state_capture: &StateCaptureContext {
                enabled: true,
                fifos: None,
                code_id: 1,
            },
        };

        // Should succeed even with experimental features
        let plan = rust.plan(&input);
        assert!(plan.is_ok());

        let plan = plan.unwrap();
        assert!(plan.requires_file);
        assert!(!plan.files.is_empty());
    }
}
