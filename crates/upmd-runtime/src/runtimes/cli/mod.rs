//! CLI Runtime - Command-line interface runtime with simple text output.
//!
//! Provides a runtime implementation for terminal applications that render
//! plain text (ANSI-escaped) output without full-screen terminal libraries.

use std::io;
use std::time::Duration;

use crate::core::{Component, Engine};

/// CLI Runtime with minimal terminal setup.
///
/// # Example - Default
///
/// ```rust,ignore
/// upmd_runtime::runtimes::cli::run(component)?;
/// ```
///
/// # Example - Custom
///
/// ```rust,ignore
/// upmd_runtime::runtimes::cli::Runtime::new()
///     .config(upmd_runtime::runtimes::cli::Config::new().poll(32))
///     .start(|| {
///         crossterm::terminal::enable_raw_mode()?;
///         let _ = crossterm::execute!(std::io::stdout(), crossterm::cursor::Hide);
///         Ok(())
///     })?
///     .stop(|| {
///         let _ = crossterm::execute!(std::io::stdout(), crossterm::cursor::Show);
///         let _ = crossterm::terminal::disable_raw_mode();
///     })
///     .run(engine)?;
/// ```
pub struct Runtime {
    config: Config,
    start: Option<Box<dyn FnOnce() -> io::Result<()>>>,
    stop: Option<Box<dyn FnOnce()>>,
}

/// Runtime configuration for CLI behavior.
#[derive(Debug, Clone, Copy)]
pub struct Config {
    poll_timeout_ms: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self::new()
    }
}

impl Config {
    /// Creates a new config with default values (poll timeout = 16ms).
    pub fn new() -> Self {
        Self {
            poll_timeout_ms: 16,
        }
    }
    /// Sets the poll timeout duration in milliseconds.
    ///
    /// This controls how long `crossterm::event::poll()` waits for input.
    /// Lower values = more CPU usage, higher responsiveness.
    /// Higher values = less CPU usage, lower responsiveness.
    pub fn poll(mut self, ms: u64) -> Self {
        self.poll_timeout_ms = ms;
        self
    }
}

impl Default for Runtime {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            stop();
        }
    }
}

impl Runtime {
    /// Creates a new CLI runtime with default setup/cleanup.
    ///
    /// Default setup: raw mode, hide cursor
    /// Default cleanup: show cursor, disable raw mode
    pub fn new() -> Self {
        Self {
            config: Config::default(),
            start: Some(Box::new(|| {
                crossterm::terminal::enable_raw_mode()?;
                let _ = crossterm::execute!(io::stdout(), crossterm::cursor::Hide);
                Ok(())
            })),
            stop: Some(Box::new(|| {
                let _ = crossterm::execute!(io::stdout(), crossterm::cursor::Show);
                let _ = crossterm::terminal::disable_raw_mode();
            })),
        }
    }

    /// Configures the runtime with custom settings.
    pub fn config(mut self, config: Config) -> Self {
        self.config = config;
        self
    }

    /// Configures a custom setup function to run before the event loop.
    ///
    /// If provided, replaces the default crossterm setup entirely.
    pub fn start<F: FnOnce() -> io::Result<()> + 'static>(mut self, f: F) -> Self {
        self.start = Some(Box::new(f));
        self
    }

    /// Configures a custom cleanup function to run after the event loop.
    ///
    /// If provided, replaces the default crossterm cleanup entirely.
    pub fn stop<F: FnOnce() + 'static>(mut self, f: F) -> Self {
        self.stop = Some(Box::new(f));
        self
    }
}

impl<C: Component + Output> crate::core::Renderer<C> for Runtime {
    fn render(&mut self, root: &C) {
        use std::io::{stdout, Write};

        // Full-screen programs (vim, less, htop) manage their own
        // terminal output via the alternate screen buffer. Skip our
        // replace-in-place rendering to avoid stomping on their output.
        if root.is_alternate_screen() {
            let _ = stdout().flush();
            return;
        }

        // Buffer the entire render output so the escape sequence,
        // clear, and component content arrive at the terminal in one
        // atomic write (eliminates the visible blank frame).
        let mut buf = Vec::new();
        let _ = root.render(&mut buf);
        let mut out = stdout();
        let _ = out.write_all(&buf);
        let _ = out.flush();
    }
}

impl<C: Component + Input + Output> crate::Runtime<C> for Runtime {
    type Error = io::Error;

    fn run(mut self, mut engine: Engine<C>) -> io::Result<()> {
        if let Some(start) = self.start.take() {
            start()?;
        }

        let frame_duration = Duration::from_millis(self.config.poll_timeout_ms);
        while engine.is_running {
            if crossterm::event::poll(frame_duration).unwrap_or(false) {
                loop {
                    if let Ok(evt) = crossterm::event::read() {
                        if let Some(msg) = engine.component.action(evt) {
                            engine.send_msg(msg).ok();
                        }
                    }
                    // Stop draining once the OS buffer is empty
                    if !crossterm::event::poll(Duration::ZERO).unwrap_or(false) {
                        break;
                    }
                }
            }
            // Process queued messages and background commands
            engine.tick();
            // Only redraw if something changed
            if engine.is_dirty {
                engine.render(&mut self);
                engine.is_dirty = false;
            }
        }

        if let Some(stop) = self.stop.take() {
            stop();
        }
        Ok(())
    }
}

/// Trait for handling CLI input events.
pub trait Input: Component {
    fn action(&self, event: crossterm::event::Event) -> Option<Self::Msg>;
}

/// Trait for rendering the component to a terminal output handle.
pub trait Output {
    /// Render the component directly to the given output handle.
    fn render<W: io::Write>(&self, out: &mut W) -> io::Result<()>;

    /// Return `true` when the running program has switched to an alternate
    /// screen buffer (e.g. vim, less, htop). The runtime skips its
    /// replace-in-place dance while this is active, letting the full-screen
    /// app own the terminal.
    fn is_alternate_screen(&self) -> bool {
        false
    }
}

/// Runs a component in the CLI runtime with default setup/cleanup.
pub fn run<C: Component + Input + Output>(component: C) -> io::Result<()> {
    use crate::Runtime as R;
    R::run(Runtime::new(), Engine::new(component))?;
    Ok(())
}
