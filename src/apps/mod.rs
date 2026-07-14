use std::sync::LazyLock;
use tempfile::TempDir;

pub mod cli;
pub mod config;
pub mod exec;
pub mod navigation;
pub mod picker;
pub mod task;
mod theme;
pub mod tui;

/// Target directory for writing files.
///
/// Initialized lazily on first access. If temp directory creation fails,
/// `target_dir()` returns an error so the caller can report it gracefully
/// instead of panicking.
static TARGET_DIR: LazyLock<Option<TempDir>> =
    LazyLock::new(
        || match tempfile::Builder::new().prefix(config::APP_NAME).tempdir() {
            Ok(dir) => Some(dir),
            Err(e) => {
                tracing::error!("Failed to create temporary directory: {e}");
                None
            }
        },
    );

/// Returns the shared target directory path for writing files.
///
/// Returns an error if the temp directory could not be created.
pub fn target_dir() -> anyhow::Result<&'static std::path::Path> {
    TARGET_DIR
        .as_ref()
        .map(|d| d.path())
        .ok_or_else(|| anyhow::anyhow!("Failed to create temporary directory"))
}
