//! Platform-specific runtime implementations for upmd-runtime.

#[cfg(feature = "cli")]
pub mod cli;

#[cfg(feature = "tui")]
pub mod tui;
