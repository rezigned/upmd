//! Workspace abstraction for file operations.
//!
//! This module provides a trait-based abstraction for managing temporary files
//! and directories during code execution. Users of the upmd-runner crate can
//! implement this trait to customize how files are created and managed.

use crate::quoting::quote_if_needed;
use crate::ShellQuoteStyle;
use anyhow::Result;
use std::path::{Path, PathBuf};
/// Trait for workspace file operations.
///
/// Implementors control how and where temporary files are created during
/// code execution. This allows for different strategies (temp directories,
/// in-memory, remote filesystems, etc.).
///
/// This trait is intentionally language-agnostic and does not contain
/// any shell-specific or language-specific logic. Higher-level components
/// should handle language-specific concerns.
pub trait WorkspaceAdapter {
    /// Creates a file with the given relative path and content.
    ///
    /// # Arguments
    /// * `relative_path` - Path relative to the workspace root
    /// * `content` - File content to write
    ///
    /// # Returns
    /// The absolute path to the created file
    fn create_file(&self, relative_path: &Path, content: &str) -> Result<PathBuf>;

    /// Gets the workspace's target directory (root directory for files).
    fn target_dir(&self) -> &Path;

    /// Creates a subdirectory within the workspace.
    ///
    /// # Arguments
    /// * `relative_path` - Path relative to the workspace root
    ///
    /// # Returns
    /// The absolute path to the created directory
    fn create_dir(&self, relative_path: &Path) -> Result<PathBuf>;

    /// Removes a file from the workspace.
    ///
    /// # Arguments
    /// * `relative_path` - Path relative to the workspace root
    fn remove_file(&self, relative_path: &Path) -> Result<()>;

    /// Checks if a file or directory exists in the workspace.
    ///
    /// # Arguments
    /// * `relative_path` - Path relative to the workspace root
    fn exists(&self, relative_path: &Path) -> bool;

    /// Reads the contents of a file in the workspace.
    ///
    /// # Arguments
    /// * `relative_path` - Path relative to the workspace root
    fn read_file(&self, relative_path: &Path) -> Result<String>;

    /// Creates a temporary file with the given content and returns its path.
    ///
    /// # Arguments
    /// * `content` - File content to write
    /// * `extension` - Optional file extension (e.g., "py", "rs")
    fn create_temp_file(&self, content: &str, extension: Option<&str>) -> Result<PathBuf>;

    /// Cleans up all files and directories in the workspace.
    fn cleanup(&self) -> Result<()>;
}

/// Extension trait for workspace operations related to execution plans.
///
/// This trait provides higher-level operations that build upon the basic
/// `WorkspaceAdapter` methods, specifically for working with execution plans.
/// It remains language-agnostic, only dealing with file creation and
/// directory resolution.
pub trait WorkspaceExecutionExt {
    /// Executes an execution plan by creating all required files.
    ///
    /// # Arguments
    /// * `plan` - The execution plan containing files to create
    fn execute_plan(&self, plan: &crate::ExecutionPlan) -> Result<()>;

    /// Resolves the working directory for execution.
    ///
    /// # Arguments
    /// * `plan` - The execution plan
    /// * `fallback` - Fallback directory if plan doesn't specify one
    fn resolve_working_dir(&self, plan: &crate::ExecutionPlan, fallback: PathBuf) -> PathBuf;

    /// Assembles the final shell script string from an execution plan.
    ///
    /// Single-element commands on non-file plans are emitted verbatim (inline
    /// bypass) so that raw shell content like `echo hello world` is never
    /// accidentally quoted. File-based commands go through [`format_args`] for
    /// path resolution and quoting.
    ///
    /// [`format_args`]: WorkspaceExecutionExt::format_args
    fn build_script(&self, plan: &crate::ExecutionPlan) -> Result<String>;

    /// Formats command arguments, resolving `./` paths and quoting as needed.
    ///
    /// - Arguments at index > 0 that start with `./` are resolved against
    ///   `target_dir()` when `plan.resolve_paths` is set.
    /// - All other arguments are shell-quoted if they contain whitespace or
    ///   special characters.
    fn format_args<'a>(
        &self,
        cmd: &[std::borrow::Cow<'a, str>],
        plan: &crate::ExecutionPlan,
    ) -> Vec<String>;
}

/// Default implementation of `WorkspaceExecutionExt` for any type implementing `WorkspaceAdapter`.
impl<T: WorkspaceAdapter> WorkspaceExecutionExt for T {
    fn execute_plan(&self, plan: &crate::ExecutionPlan) -> Result<()> {
        for (path, content) in &plan.files {
            self.create_file(path, content)?;
        }
        Ok(())
    }

    fn resolve_working_dir(&self, plan: &crate::ExecutionPlan, fallback: PathBuf) -> PathBuf {
        match &plan.working_dir {
            Some(wd) if wd.is_absolute() => wd.clone(),
            Some(wd) => self.target_dir().join(wd),
            None => fallback,
        }
    }

    fn build_script(&self, plan: &crate::ExecutionPlan) -> Result<String> {
        use anyhow::bail;

        if plan.commands.is_empty() {
            bail!("No commands in execution plan");
        }

        let sep = command_separator(plan.quote_style);
        let commands: String = plan
            .commands
            .iter()
            .enumerate()
            .map(|(i, cmd)| {
                // Inline commands (single-line shell scripts) contain raw script
                // content that should be passed directly without quoting/formatting.
                // File-based commands use format_args for path resolution and quoting.
                let args = if !plan.requires_file && cmd.len() == 1 {
                    cmd[0].to_string()
                } else {
                    self.format_args(cmd, plan).join(" ")
                };
                if i > 0 {
                    format!("{sep}{args}")
                } else {
                    args
                }
            })
            .collect();

        Ok(plan.wrap.as_ref().map_or(commands.clone(), |w| w(commands)))
    }

    fn format_args<'a>(
        &self,
        cmd: &[std::borrow::Cow<'a, str>],
        plan: &crate::ExecutionPlan,
    ) -> Vec<String> {
        let root = self.target_dir();

        cmd.iter()
            .enumerate()
            .map(|(idx, arg)| {
                let s = arg.trim();

                if plan.resolve_paths && idx > 0 && s.starts_with("./") {
                    return root.join(&s[2..]).display().to_string();
                }

                let resolved = plan
                    .files
                    .iter()
                    .find(|(path, _)| path.to_string_lossy() == s)
                    .map(|(path, _)| root.join(path).display().to_string())
                    .unwrap_or_else(|| s.to_string());

                quote_if_needed(&resolved, plan.quote_style)
            })
            .collect()
    }
}

/// Returns the command separator appropriate for the shell quoting style.
///
/// PowerShell 5.1 does not support `&&`, so it uses `;` as a statement
/// separator. POSIX shells and cmd.exe both support `&&` for conditional
/// chaining.
fn command_separator(style: ShellQuoteStyle) -> &'static str {
    match style {
        ShellQuoteStyle::PowerShell => "; ",
        ShellQuoteStyle::Posix | ShellQuoteStyle::Cmd => " && ",
    }
}

/// Simple in-memory workspace implementation for testing.
///
/// This implementation stores files in memory rather than on disk,
/// which is useful for testing or when you don't need persistent storage.
#[cfg(feature = "memory-workspace")]
pub struct InMemoryWorkspace {
    files: std::sync::RwLock<std::collections::HashMap<PathBuf, String>>,
    root: PathBuf,
}

#[cfg(feature = "memory-workspace")]
impl InMemoryWorkspace {
    /// Creates a new in-memory workspace.
    pub fn new() -> Self {
        Self {
            files: std::sync::RwLock::new(std::collections::HashMap::new()),
            root: PathBuf::from("/in-memory"),
        }
    }
}

#[cfg(feature = "memory-workspace")]
impl WorkspaceAdapter for InMemoryWorkspace {
    fn create_file(&self, relative_path: &Path, content: &str) -> Result<PathBuf> {
        let file_path = self.root.join(relative_path);
        self.files
            .write()
            .unwrap()
            .insert(file_path.clone(), content.to_string());
        Ok(file_path)
    }

    fn target_dir(&self) -> &Path {
        &self.root
    }

    fn create_dir(&self, relative_path: &Path) -> Result<PathBuf> {
        let dir_path = self.root.join(relative_path);
        // In-memory workspace doesn't need to actually create directories
        Ok(dir_path)
    }

    fn remove_file(&self, relative_path: &Path) -> Result<()> {
        let file_path = self.root.join(relative_path);
        self.files.write().unwrap().remove(&file_path);
        Ok(())
    }

    fn exists(&self, relative_path: &Path) -> bool {
        let file_path = self.root.join(relative_path);
        self.files.read().unwrap().contains_key(&file_path)
    }

    fn read_file(&self, relative_path: &Path) -> Result<String> {
        let file_path = self.root.join(relative_path);
        self.files
            .read()
            .unwrap()
            .get(&file_path)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("File not found: {}", file_path.display()))
    }

    fn create_temp_file(&self, content: &str, extension: Option<&str>) -> Result<PathBuf> {
        use std::time::{SystemTime, UNIX_EPOCH};

        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();

        let filename = match extension {
            Some(ext) => format!("temp_{}.{}", timestamp, ext),
            None => format!("temp_{}", timestamp),
        };

        self.create_file(Path::new(&filename), content)
    }

    fn cleanup(&self) -> Result<()> {
        self.files.write().unwrap().clear();
        Ok(())
    }
}

#[cfg(feature = "memory-workspace")]
impl Default for InMemoryWorkspace {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_quote_if_needed_plain() {
        assert_eq!(quote_if_needed("hello", ShellQuoteStyle::Posix), "hello");
        assert_eq!(quote_if_needed("", ShellQuoteStyle::Posix), "''");
    }

    #[test]
    fn test_quote_if_needed_with_space() {
        assert_eq!(
            quote_if_needed("hello world", ShellQuoteStyle::Posix),
            "'hello world'"
        );
    }

    #[test]
    fn test_quote_if_needed_with_single_quote() {
        assert_eq!(
            quote_if_needed("it's", ShellQuoteStyle::Posix),
            "'it'\\''s'"
        );
    }

    #[test]
    fn test_quote_if_needed_with_double_quote() {
        assert_eq!(
            quote_if_needed(r#"say "hi""#, ShellQuoteStyle::Posix),
            r#"'say "hi"'"#
        );
    }

    #[test]
    fn test_quote_if_needed_with_exclamation() {
        assert_eq!(quote_if_needed("run!", ShellQuoteStyle::Posix), "'run!'");
    }

    #[test]
    fn test_quote_if_needed_with_dollar() {
        assert_eq!(quote_if_needed("$HOME", ShellQuoteStyle::Posix), "'$HOME'");
    }

    #[test]
    fn test_quote_if_needed_with_pipe() {
        assert_eq!(quote_if_needed("a|b", ShellQuoteStyle::Posix), "'a|b'");
    }

    #[test]
    fn test_quote_if_needed_with_semicolon() {
        assert_eq!(quote_if_needed("a;b", ShellQuoteStyle::Posix), "'a;b'");
    }

    #[test]
    fn test_quote_if_needed_backtick() {
        assert_eq!(
            quote_if_needed("`rm -rf /`", ShellQuoteStyle::Posix),
            "'`rm -rf /`'"
        );
    }

    #[test]
    fn test_quote_if_needed_cmd() {
        assert_eq!(
            quote_if_needed(r#"C:\Program Files\upmd\script.bat"#, ShellQuoteStyle::Cmd),
            r#""C:\Program Files\upmd\script.bat""#
        );
        assert_eq!(quote_if_needed("", ShellQuoteStyle::Cmd), r#""""#);
    }

    #[test]
    fn test_quote_if_needed_powershell() {
        assert_eq!(
            quote_if_needed(
                r#"C:\Users\O'Brien\script.ps1"#,
                ShellQuoteStyle::PowerShell
            ),
            r#"'C:\Users\O''Brien\script.ps1'"#
        );
    }

    #[test]
    fn test_build_script_uses_conditional_separator_for_cmd() {
        use crate::ExecutionPlan;

        let mut plan = ExecutionPlan::new();
        plan.quote_style = ShellQuoteStyle::Cmd;
        plan.command(["echo a"]);
        plan.command(["echo b"]);

        let ws = crate::workspace::TempWorkspace::from_path("/tmp/ws");
        let script = ws.build_script(&plan).unwrap();
        assert_eq!(script, "echo a && echo b");
    }

    #[test]
    fn test_build_script_uses_statement_separator_for_powershell() {
        use crate::ExecutionPlan;

        let mut plan = ExecutionPlan::new();
        plan.quote_style = ShellQuoteStyle::PowerShell;
        plan.command(["Write-Host a"]);
        plan.command(["Write-Host b"]);

        let ws = crate::workspace::TempWorkspace::from_path("/tmp/ws");
        let script = ws.build_script(&plan).unwrap();
        assert_eq!(script, "Write-Host a; Write-Host b");
    }
}
