//! Execution context for a single code block run.
//!
//! `TARGET_DIR` is the shared file root for all code blocks in a document.
//! Each execution gets its own isolated `state_dir` for cleanup scope.
//!
//! ```text
//! TARGET_DIR/               ← shared, all executions write files here
//!   script_1.sh
//!   script_2.py
//!   state_1_<ts>/           ← cleanup scope (empty dir on Windows)
//!   state_2_<ts>/           ← cleanup scope (empty dir on Windows)
//! ```
//!
//! # Platform notes
//!
//! - Unix: a single FIFO via `mkfifo` at `state_dir/state.fifo`. The reader
//!   blocks until the writer connects, writes, and closes.
//! - Windows: a single regular file written to `state_dir/state`. The parent
//!   polls for content after the child exits. Regular files work with all
//!   Windows shells (cmd, powershell, Git Bash, WSL).

use anyhow::Result;
use std::ops::Deref;
use std::path::{Path, PathBuf};

use upmd_runner::{FifoPaths, TempWorkspace as BaseWorkspace};

/// Per-execution context: shared file root + isolated state directory.
///
/// The file root (`TARGET_DIR`) is shared across all code blocks - creating
/// this struct is cheap (just a `PathBuf` clone + one `mkdir`).
/// The `state_dir` is unique per execution and is used for cleanup scope.
/// It is intentionally NOT cleaned up on drop - the reader thread may still
/// be active after the process exits.
pub struct ExecutionContext {
    /// Shared file root - all script files land here.
    files: BaseWorkspace,
    /// Isolated per-execution directory (cleanup scope).
    state_dir: PathBuf,
}

impl Deref for ExecutionContext {
    type Target = BaseWorkspace;

    fn deref(&self) -> &Self::Target {
        &self.files
    }
}

impl ExecutionContext {
    /// Creates a new execution context for the given code block.
    ///
    /// Allocates a unique `state_dir` under `TARGET_DIR` (timestamp-scoped,
    /// safe for concurrent runs). Script files in `TARGET_DIR` are named by
    /// `code_id` and are overwritten on each run - safe for sequential re-runs,
    /// but concurrent re-runs of the same block could collide.
    pub fn new(code_id: crate::runner::CodeId) -> Result<Self> {
        let target_dir = crate::apps::target_dir()?.to_path_buf();

        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();

        let state_dir = target_dir.join(format!("state_{}_{}", code_id, timestamp));
        create_state_dir(&state_dir)?;

        Ok(Self {
            files: BaseWorkspace::from_path(target_dir),
            state_dir,
        })
    }

    /// Returns the isolated state directory for this execution.
    pub fn state_dir(&self) -> &Path {
        &self.state_dir
    }

    /// Creates FIFO paths for state capture in the state directory.
    ///
    /// Delegates to [`crate::pty::state::create_state_fifos`].
    pub fn create_state_fifos(&self) -> Result<FifoPaths> {
        crate::pty::state::create_state_fifos(&self.state_dir)
    }
}

/// Creates the isolated state directory with restrictive permissions.
#[cfg(unix)]
fn create_state_dir(path: &Path) -> Result<()> {
    use std::os::unix::fs::DirBuilderExt;

    let mut builder = std::fs::DirBuilder::new();
    builder.mode(0o700);
    builder.recursive(true).create(path)?;
    Ok(())
}

#[cfg(not(unix))]
fn create_state_dir(path: &Path) -> Result<()> {
    std::fs::create_dir_all(path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_execution_context_state_dir_naming() {
        let _ = crate::apps::target_dir().unwrap();

        let ctx = ExecutionContext::new(42).unwrap();
        let dir_name = ctx.state_dir().file_name().unwrap().to_str().unwrap();
        assert!(
            dir_name.starts_with("state_42_"),
            "unexpected dir name: {}",
            dir_name
        );
    }

    #[test]
    fn test_execution_context_state_dir_under_target() {
        let target = crate::apps::target_dir().unwrap();
        let ctx = ExecutionContext::new(7).unwrap();
        assert!(ctx.state_dir().starts_with(target));
    }

    #[test]
    fn test_execution_context_state_dir_contains_code_id() {
        let _ = crate::apps::target_dir().unwrap();
        let ctx = ExecutionContext::new(7).unwrap();
        let dir_name = ctx.state_dir().file_name().unwrap().to_str().unwrap();
        assert!(
            dir_name.starts_with("state_7_"),
            "unexpected dir name: {}",
            dir_name
        );
    }

    #[test]
    fn test_execution_context_different_code_ids_different_dirs() {
        let _ = crate::apps::target_dir().unwrap();
        let ctx1 = ExecutionContext::new(1).unwrap();
        let ctx2 = ExecutionContext::new(2).unwrap();
        // Different code IDs must produce different dirs regardless of timing
        assert_ne!(ctx1.state_dir(), ctx2.state_dir());
    }

    #[test]
    #[cfg(unix)]
    fn test_create_state_fifos_creates_pipes() {
        let _ = crate::apps::target_dir().unwrap();
        let ctx = ExecutionContext::new(99).unwrap();
        let fifos = ctx.create_state_fifos().unwrap();
        assert!(fifos.state_fifo.ends_with("state.fifo"));
        // Verify they exist on the filesystem
        assert!(fifos.state_fifo.exists());
    }
}
