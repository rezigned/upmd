//! Filesystem-backed workspace implementation.

use crate::workspace::WorkspaceAdapter;
use anyhow::{bail, Context, Result};
use std::path::{Component, Path, PathBuf};

/// Workspace backed by a directory on the filesystem.
///
/// Construct via `TempWorkspace::new()` (auto-managed temp dir, deleted on drop)
/// or `TempWorkspace::from_path(root)` (caller-supplied directory).
///
/// # Example
/// ```ignore
/// // Auto temp dir
/// let ws = TempWorkspace::new()?;
///
/// // App-managed dir (e.g. TARGET_DIR)
/// let ws = TempWorkspace::from_path("/var/app/workspace");
/// ```
pub struct TempWorkspace {
    root: PathBuf,
    /// Canonicalized root for path containment checks.
    real_root: PathBuf,
    /// Kept alive to defer deletion until drop (only set when using `new()`).
    _temp_dir: Option<tempfile::TempDir>,
}

impl TempWorkspace {
    /// Creates a workspace in an auto-managed temporary directory.
    pub fn new() -> Result<Self> {
        let temp_dir = tempfile::TempDir::new()?;
        let root = temp_dir.path().to_path_buf();
        let real_root =
            std::fs::canonicalize(&root).context("Failed to canonicalize workspace root")?;
        Ok(Self {
            root,
            real_root,
            _temp_dir: Some(temp_dir),
        })
    }

    /// Creates a workspace rooted at an existing caller-managed directory.
    pub fn from_path(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        let real_root = std::fs::canonicalize(&root).unwrap_or_else(|_| root.clone());
        Self {
            root,
            real_root,
            _temp_dir: None,
        }
    }

    fn resolve_workspace_path(&self, relative_path: &Path) -> Result<PathBuf> {
        if relative_path.is_absolute() {
            bail!(
                "Workspace path must be relative, got: {}",
                relative_path.display()
            );
        }

        for component in relative_path.components() {
            if matches!(component, Component::ParentDir | Component::Prefix(_)) {
                bail!(
                    "Workspace path cannot escape the workspace: {}",
                    relative_path.display()
                );
            }
        }

        Ok(self.real_root.join(relative_path))
    }

    /// Verifies that `path` resolves under `real_root`, even through symlinks.
    ///
    /// Canonicalizes `path` (or its parent if `path` doesn't exist yet) and
    /// asserts the canonical result starts with `self.real_root`.
    fn assert_contained(&self, path: &Path) -> Result<()> {
        let target = if path.exists() {
            path
        } else if let Some(parent) = path.parent() {
            parent
        } else {
            path
        };
        let real = std::fs::canonicalize(target)
            .with_context(|| format!("Failed to resolve path: {}", target.display()))?;
        if !real.starts_with(&self.real_root) {
            bail!(
                "Path escapes the workspace: {} (resolves to {})",
                path.display(),
                real.display(),
            );
        }
        Ok(())
    }
}

impl WorkspaceAdapter for TempWorkspace {
    fn create_file(&self, relative_path: &Path, content: &str) -> Result<PathBuf> {
        let file_path = self.resolve_workspace_path(relative_path)?;

        if let Some(parent) = file_path.parent() {
            std::fs::create_dir_all(parent)?;
            self.assert_contained(parent)?;
        }

        std::fs::write(&file_path, content)
            .with_context(|| format!("Failed to write: {}", file_path.display()))?;

        // Verify the written file is still inside the workspace (catches
        // symlink-based escape: the path itself is inside, but the target
        // could be outside).
        self.assert_contained(&file_path)?;

        Ok(file_path)
    }

    fn target_dir(&self) -> &Path {
        &self.root
    }

    fn create_dir(&self, relative_path: &Path) -> Result<PathBuf> {
        let dir_path = self.resolve_workspace_path(relative_path)?;
        std::fs::create_dir_all(&dir_path)?;
        self.assert_contained(&dir_path)?;
        Ok(dir_path)
    }

    fn remove_file(&self, relative_path: &Path) -> Result<()> {
        let path = self.resolve_workspace_path(relative_path)?;
        self.assert_contained(&path)?;
        std::fs::remove_file(&path)?;
        Ok(())
    }

    fn exists(&self, relative_path: &Path) -> bool {
        self.resolve_workspace_path(relative_path)
            .is_ok_and(|path| path.exists())
    }

    fn read_file(&self, relative_path: &Path) -> Result<String> {
        let path = self.resolve_workspace_path(relative_path)?;
        self.assert_contained(&path)?;
        std::fs::read_to_string(&path)
            .with_context(|| format!("Failed to read: {}", path.display()))
    }

    fn create_temp_file(&self, content: &str, extension: Option<&str>) -> Result<PathBuf> {
        use std::time::{SystemTime, UNIX_EPOCH};
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let filename = match extension {
            Some(ext) => format!("temp_{}.{}", ts, ext),
            None => format!("temp_{}", ts),
        };
        self.create_file(Path::new(&filename), content)
    }

    fn cleanup(&self) -> Result<()> {
        // Auto temp dir: cleaned up on drop via _temp_dir.
        // Caller-managed dir: caller's responsibility.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_temp_workspace_create_file() {
        let ws = TempWorkspace::new().unwrap();
        let path = ws.create_file(Path::new("hello.sh"), "echo hi").unwrap();
        assert!(path.ends_with("hello.sh"));
        assert!(ws.exists(Path::new("hello.sh")));
        assert_eq!(ws.read_file(Path::new("hello.sh")).unwrap(), "echo hi");
    }

    #[test]
    fn test_temp_workspace_nested_file() {
        let ws = TempWorkspace::new().unwrap();
        let path = ws
            .create_file(Path::new("sub/deep/file.txt"), "data")
            .unwrap();
        assert!(path.ends_with("sub/deep/file.txt"));
        assert!(ws.exists(Path::new("sub/deep/file.txt")));
    }

    #[test]
    fn test_temp_workspace_rejects_absolute_file_path() {
        let ws = TempWorkspace::new().unwrap();
        let err = ws
            .create_file(Path::new("/tmp/escape.txt"), "data")
            .unwrap_err();
        assert!(err.to_string().contains("must be relative"));
    }

    #[test]
    fn test_temp_workspace_rejects_parent_traversal() {
        let ws = TempWorkspace::new().unwrap();
        let err = ws
            .create_file(Path::new("../escape.txt"), "data")
            .unwrap_err();
        assert!(err.to_string().contains("cannot escape"));
    }

    #[test]
    fn test_temp_workspace_read_missing() {
        let ws = TempWorkspace::new().unwrap();
        assert!(ws.read_file(Path::new("nope.txt")).is_err());
    }

    #[test]
    fn test_temp_workspace_remove_file() {
        let ws = TempWorkspace::new().unwrap();
        ws.create_file(Path::new("del.txt"), "x").unwrap();
        assert!(ws.exists(Path::new("del.txt")));
        ws.remove_file(Path::new("del.txt")).unwrap();
        assert!(!ws.exists(Path::new("del.txt")));
    }

    #[test]
    fn test_temp_workspace_create_dir() {
        let ws = TempWorkspace::new().unwrap();
        let dir = ws.create_dir(Path::new("mydir")).unwrap();
        assert!(dir.ends_with("mydir"));
        assert!(dir.exists());
    }

    #[test]
    fn test_temp_workspace_target_dir() {
        let ws = TempWorkspace::new().unwrap();
        assert!(ws.target_dir().exists());
    }

    #[test]
    fn test_temp_workspace_from_path() {
        let dir = std::env::temp_dir();
        let ws = TempWorkspace::from_path(&dir);
        assert_eq!(ws.target_dir(), dir);
    }

    #[test]
    fn test_temp_workspace_cleanup_does_not_crash() {
        // cleanup() on a caller-managed dir is a no-op. Just verify no panic
        let ws = TempWorkspace::new().unwrap();
        ws.cleanup().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn test_temp_workspace_rejects_symlink_escape() {
        let ws = TempWorkspace::new().unwrap();

        // Create a symlink inside the workspace pointing outside
        let escape_target = std::env::temp_dir().join("upmd_escape_test");
        std::fs::write(&escape_target, "should not be reachable").unwrap();
        let link = ws.target_dir().join("escape_link");
        std::os::unix::fs::symlink(&escape_target, &link).unwrap();

        // Writing through the symlink should fail
        let err = ws
            .create_file(Path::new("escape_link"), "data")
            .unwrap_err();
        assert!(
            err.to_string().contains("escapes the workspace"),
            "Expected escape error, got: {err}"
        );

        // Reading through the symlink should fail
        let err = ws.read_file(Path::new("escape_link")).unwrap_err();
        assert!(
            err.to_string().contains("escapes the workspace"),
            "Expected escape error, got: {err}"
        );

        std::fs::remove_file(&escape_target).unwrap();
        std::fs::remove_file(&link).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn test_temp_workspace_symlinked_root_contained() {
        // Create a temporary dir, symlink it, and verify TempWorkspace
        // resolves through the symlink correctly.
        let real_dir = tempfile::TempDir::new().unwrap();
        let symlink_dir = real_dir.path().join("linked");
        std::os::unix::fs::symlink(real_dir.path(), &symlink_dir).unwrap();

        let ws = TempWorkspace::from_path(&symlink_dir);
        let path = ws.create_file(Path::new("test.txt"), "safe").unwrap();
        assert_eq!(ws.read_file(Path::new("test.txt")).unwrap(), "safe");

        // The canonicalized root should be the real path, not the symlink
        assert!(path.starts_with(real_dir.path().canonicalize().unwrap()));
    }
}
