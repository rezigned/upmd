//! Reusable pseudo-terminal process and VT100 parsing primitives.
//!
//! The crate intentionally emits raw PTY bytes through [`PtyEvent::Output`].
//! Higher layers decide whether to parse, decode, render, or persist output.

pub mod mouse;
pub mod parser;
pub mod process;
pub mod signal;

pub use process::{Process, PtyEvent, PtyExit, Size};
pub use signal::ExitSignal;
