//! Workspace abstraction and implementations.

pub mod adapter;

#[cfg(feature = "temp-workspace")]
pub mod temp;

pub use adapter::WorkspaceAdapter;
pub use adapter::WorkspaceExecutionExt;

#[cfg(feature = "temp-workspace")]
pub use temp::TempWorkspace;
