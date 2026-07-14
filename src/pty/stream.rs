use crate::apps::config::Envs;

/// Messages flowing from the PTY process to the UI.
/// The order is always: `Out*` (zero or more) → `Env` (optional) → `Cwd` (optional) → `Exit` → `End`.
#[derive(Debug, Clone)]
pub enum Stream {
    /// Raw bytes from PTY stdout/stderr, decoded as UTF-8.
    Out(String),
    /// Captured environment variables sent through the state FIFO.
    Env(Envs),
    /// Working directory captured at state-read time.
    Cwd(String),
    /// Process exit code.
    Exit(i32),
    /// Signals that the stream is complete and no more messages will follow.
    End,
}
