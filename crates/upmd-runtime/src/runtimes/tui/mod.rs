//! TUI Runtime - Terminal user interface runtime using ratatui.
//!
//! Provides a runtime implementation for terminal-based applications with
//! full-screen rendering and mouse support.

use std::io::{self, Stdout};
use std::time::Duration;

use ratatui::{backend::CrosstermBackend, Terminal};

use crate::core::{Component, Engine};

/// TUI Runtime that manages a full-screen terminal interface.
///
/// # Example - Default
///
/// ```rust,ignore
/// upmd_runtime::runtimes::tui::run(component)?;
/// ```
///
/// # Example - Custom
///
/// ```rust,ignore
/// upmd_runtime::runtimes::tui::Runtime::new()
///     .config(upmd_runtime::runtimes::tui::Config::new().poll(32))
///     .start(|| {
///         crossterm::terminal::enable_raw_mode()?;
///         crossterm::execute!(
///             std::io::stdout(),
///             crossterm::terminal::EnterAlternateScreen,
///         )?;
///         Ok(())
///     })?
///     .stop(|| {
///         let _ = crossterm::execute!(std::io::stdout(), crossterm::terminal::LeaveAlternateScreen);
///         let _ = crossterm::terminal::disable_raw_mode();
///     })
///     .run(engine)?;
/// ```
pub struct Runtime {
    terminal: Option<Terminal<CrosstermBackend<Stdout>>>,
    config: Config,
    start: Option<Box<dyn FnOnce() -> io::Result<()>>>,
    stop: Option<Box<dyn FnOnce()>>,
}

/// Runtime configuration for TUI behavior.
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
    /// Creates a new TUI runtime with default crossterm setup/cleanup.
    ///
    /// Default setup: raw mode, alternate screen, mouse capture
    /// Default cleanup: leave alternate screen, disable mouse, disable raw mode
    pub fn new() -> Self {
        Self {
            terminal: None,
            config: Config::default(),
            start: Some(Box::new(|| {
                crossterm::terminal::enable_raw_mode()?;
                crossterm::execute!(
                    io::stdout(),
                    crossterm::terminal::EnterAlternateScreen,
                    crossterm::event::EnableMouseCapture
                )
            })),
            stop: Some(Box::new(|| {
                let _ = crossterm::execute!(
                    io::stdout(),
                    crossterm::event::DisableMouseCapture,
                    crossterm::terminal::LeaveAlternateScreen
                );
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
    pub fn start<F>(mut self, f: F) -> Self
    where
        F: FnOnce() -> io::Result<()> + 'static,
    {
        self.start = Some(Box::new(f));
        self
    }

    /// Configures a custom cleanup function to run after the event loop.
    ///
    /// If provided, replaces the default crossterm cleanup entirely.
    pub fn stop<F>(mut self, f: F) -> Self
    where
        F: FnOnce() + 'static,
    {
        self.stop = Some(Box::new(f));
        self
    }
}

impl<C: Component + Output> crate::core::Renderer<C> for Runtime {
    fn render(&mut self, root: &C) {
        if let Some(terminal) = &mut self.terminal {
            terminal
                .draw(|f| {
                    root.render(f, f.area());
                })
                .ok();
        }
    }
}

impl<C: Component + Input + Output> crate::Runtime<C> for Runtime {
    type Error = io::Error;

    fn run(mut self, mut engine: Engine<C>) -> io::Result<()> {
        if let Some(start) = self.start.take() {
            start()?;
        }

        self.terminal =
            Some(Terminal::new(CrosstermBackend::new(io::stdout())).map_err(io::Error::other)?);

        while engine.is_running {
            if crossterm::event::poll(Duration::from_millis(self.config.poll_timeout_ms))
                .unwrap_or(false)
            {
                loop {
                    if let Ok(event) = crossterm::event::read() {
                        if let Some(msg) = engine.component.action(event) {
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

/// Trait for handling TUI input events.
pub trait Input: Component {
    fn action(&self, event: crossterm::event::Event) -> Option<Self::Msg>;
}

/// Trait for rendering the component to a ratatui frame.
pub trait Output {
    fn render(&self, frame: &mut ratatui::Frame, area: ratatui::layout::Rect);
}

/// Runs a component in the TUI runtime with default crossterm behavior.
pub fn run<C: Component + Input + Output>(component: C) -> io::Result<()> {
    use crate::Runtime as R;

    let engine = Engine::new(component);
    R::run(Runtime::new(), engine)?;
    Ok(())
}
