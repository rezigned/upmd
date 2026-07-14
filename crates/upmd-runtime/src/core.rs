//! Cha - A universal runtime engine implementing The Elm Architecture.
//!
//! This module provides core abstractions for building reactive applications
//! with clear separation between runtime, component, and rendering logic.

pub use flume::Sender;
use flume::{bounded, unbounded, Receiver, SendError};
use std::{
    thread,
    time::{Duration, Instant},
};

// CMD

/// A command represents a side effect that can emit zero, one, or many messages
/// back into the runtime. Commands are the sole mechanism for initiating
/// background work or triggering state changes from within a component.
///
/// # Variants
///
/// - `Cmd::stream(f)` - Spawns a background task that emits many messages
/// - `Cmd::once(f)`  - Spawns a task that emits exactly one message
/// - `Cmd::msg(m)`   - Immediately enqueues a message
/// - `Cmd::quit()`   - Signals the runtime to terminate
///
/// # Example
///
/// ```rust
/// use upmd_runtime::prelude::*;
///
/// enum Msg {
///     Fetched(String),
/// }
///
/// fn fetch_data() -> Cmd<Msg> {
///     Cmd::once(|| Msg::Fetched("data".to_string()))
/// }
/// ```
/// use upmd_runtime::prelude::*;
///
/// fn fetch_data() -> Cmd<Msg> {
///     Cmd::once(|| async { /* ... */ })
/// }
/// ```
pub enum Cmd<Msg> {
    /// Spawns a background task emitting zero, one, or many messages.
    Stream(Box<dyn FnOnce(Sender<Msg>) + Send>),
    /// Spawns a background task with separate low- and high-priority senders.
    PriorityStream(Box<dyn FnOnce(Sender<Msg>, Sender<Msg>) + Send>),
    /// Spawns a fire-and-forget background task.
    ///
    /// The task runs on a background thread and does not send any messages
    /// back to the runtime. Useful for side effects like file I/O.
    Task(Box<dyn FnOnce() + Send>),
    /// Signals the runtime to terminate.
    Quit,
    /// Batch of commands to execute in parallel.
    Batch(Vec<Cmd<Msg>>),
}

impl<Msg: Send + 'static> Cmd<Msg> {
    /// Creates a command that spawns a background task emitting potentially
    /// many messages over time. The task receives a sender to enqueue messages.
    ///
    /// This is useful for long-running operations like file I/O, network requests,
    /// or timers that emit progress updates.
    ///
    /// # Example
    ///
    /// ```rust
    /// use upmd_runtime::prelude::*;
    ///
    /// enum Msg {
    ///     Progress(i32),
    ///     Done,
    /// }
    ///
    /// let _cmd: Cmd<Msg> = Cmd::stream(|tx| {
    ///     thread::spawn(move || {
    ///         for i in 0..10 {
    ///             let _ = tx.send(Msg::Progress(i));
    ///             thread::sleep(Duration::from_millis(100));
    ///         }
    ///         let _ = tx.send(Msg::Done);
    ///     });
    /// });
    /// ```
    pub fn stream<F>(f: F) -> Self
    where
        F: FnOnce(Sender<Msg>) + Send + 'static,
    {
        Cmd::Stream(Box::new(f))
    }

    /// Creates a command that can route bulk and control messages separately.
    ///
    /// The first sender targets the normal low-priority command queue. The
    /// second sender targets the high-priority message queue drained before
    /// background work on every tick. Use this when a stream contains both
    /// lossy/bulk data and lifecycle messages that must not sit behind it.
    pub fn priority_stream<F>(f: F) -> Self
    where
        F: FnOnce(Sender<Msg>, Sender<Msg>) + Send + 'static,
    {
        Cmd::PriorityStream(Box::new(f))
    }

    /// Creates a command that spawns a fire-and-forget background task.
    ///
    /// The task runs on a background thread and does not send any messages
    /// back to the runtime. This is useful for side effects like file I/O
    /// where you don't need to wait for a result.
    ///
    /// # Example
    ///
    /// ```rust
    /// use upmd_runtime::prelude::*;
    ///
    /// let _cmd: Cmd<()> = Cmd::task(|| {
    ///     std::fs::write("log.txt", "done").ok();
    /// });
    /// ```
    pub fn task<F>(f: F) -> Self
    where
        F: FnOnce() + Send + 'static,
    {
        Cmd::Task(Box::new(f))
    }

    /// Creates a command that signals the runtime to terminate.
    ///
    /// When this command is executed, `Engine::is_running` is set to `false`,
    /// causing the run loop to exit after processing pending messages.
    pub fn quit() -> Self {
        Cmd::Quit
    }

    /// Creates a command that spawns a background task which computes and emits
    /// exactly one message.
    ///
    /// The computation runs on a background thread, allowing the runtime to
    /// continue processing while work is performed.
    ///
    /// # Example
    ///
    /// ```rust
    /// use upmd_runtime::prelude::*;
    ///
    /// enum Msg {
    ///     Computed(String),
    /// }
    ///
    /// let _cmd: Cmd<Msg> = Cmd::once(|| Msg::Computed("result".to_string()));
    /// ```
    pub fn once<F>(f: F) -> Self
    where
        F: FnOnce() -> Msg + Send + 'static,
    {
        Cmd::stream(move |tx| {
            let _ = tx.send(f());
        })
    }

    /// Creates a command that immediately enqueues a message without
    /// spawning any background task.
    ///
    /// This is useful when you need to dispatch a message synchronously,
    /// such as forwarding a message from an event handler.
    pub fn msg(msg: Msg) -> Self {
        Cmd::stream(move |tx| {
            let _ = tx.send(msg);
        })
    }

    /// Transforms this command's message type into a parent message type.
    ///
    /// When the child component emits messages, they are mapped to the parent type
    /// before being sent to the parent's channel. This enables composition
    /// of nested components.
    ///
    /// # Example
    ///
    /// ```rust
    /// use upmd_runtime::prelude::*;
    ///
    /// enum ChildMsg { Updated(i32) }
    /// enum ParentMsg { ChildUpdated(i32) }
    ///
    /// let child_cmd: Cmd<ChildMsg> = Cmd::once(|| ChildMsg::Updated(42));
    /// let parent_cmd: Cmd<ParentMsg> = child_cmd.map(|child_msg| match child_msg {
    ///     ChildMsg::Updated(v) => ParentMsg::ChildUpdated(v),
    /// });
    /// ```
    pub fn map<ParentMsg, F>(self, f: F) -> Cmd<ParentMsg>
    where
        F: Fn(Msg) -> ParentMsg + Send + Clone + 'static,
        ParentMsg: Send + 'static,
    {
        match self {
            Cmd::Quit => Cmd::Quit,
            Cmd::Stream(run) => Cmd::stream(move |parent_tx| {
                let (child_tx, child_rx) = unbounded();
                thread::spawn(move || {
                    while let Ok(msg) = child_rx.recv() {
                        let _ = parent_tx.send(f(msg));
                    }
                });
                run(child_tx);
            }),
            Cmd::PriorityStream(run) => {
                Cmd::priority_stream(move |parent_low_tx, parent_high_tx| {
                    let (child_low_tx, child_low_rx) = unbounded();
                    let (child_high_tx, child_high_rx) = unbounded();
                    let map_low = Clone::clone(&f);
                    thread::spawn(move || {
                        while let Ok(msg) = child_low_rx.recv() {
                            let _ = parent_low_tx.send(map_low(msg));
                        }
                    });
                    thread::spawn(move || {
                        while let Ok(msg) = child_high_rx.recv() {
                            let _ = parent_high_tx.send(f(msg));
                        }
                    });
                    run(child_low_tx, child_high_tx);
                })
            }
            Cmd::Task(run) => Cmd::Task(run),
            Cmd::Batch(cmds) => {
                Cmd::Batch(cmds.into_iter().map(|c| c.map(Clone::clone(&f))).collect())
            }
        }
    }
}

// COMPONENT

/// The core trait for application state and logic, following The Elm Architecture.
///
/// A component owns its state and defines how that state mutates in response to messages.
/// The runtime ensures `update` is the sole place where state is modified.
///
/// # Implementers must define:
///
/// - `Msg` - The message type for this component
/// - `update()` - Handles messages and returns optional commands
///
/// # Optionally implement:
///
/// - `create()` - Performs async initialization before the run loop starts
///
/// # Example
///
/// ```rust
/// use upmd_runtime::prelude::*;
///
/// struct Counter { count: i32 }
///
/// enum Msg { Increment, Decrement }
///
/// impl Component for Counter {
///     type Msg = Msg;
///
///     fn update(&mut self, msg: Msg) -> Option<Cmd<Msg>> {
///         match msg {
///             Msg::Increment => self.count += 1,
///             Msg::Decrement => self.count -= 1,
///         }
///         None
///     }
/// }
/// ```
pub trait Component {
    /// The message type this component uses for communication.
    type Msg: Send + 'static;

    /// Called once before the run loop starts. Use for initial async work,
    /// such as loading configuration or fetching initial data.
    ///
    /// The returned command will be spawned on a background thread.
    /// Override this only if you need async initialization.
    fn create(&mut self) -> Option<Cmd<Self::Msg>> {
        None
    }

    /// The sole place where state is mutated. This method receives a message
    /// and updates the component's state accordingly.
    ///
    /// Returns a command to perform side effects, or `None` if no side effects
    /// are needed. Return `Cmd::quit()` to terminate the runtime.
    fn update(&mut self, msg: Self::Msg) -> Option<Cmd<Self::Msg>>;
}

// RUNTIME

/// A platform-specific runtime that owns the event loop and drives the engine.
///
/// Implementations handle the native event loop (e.g., terminal for TUI/CLI,
/// winit for GUI) and bridge input events into the engine via `engine.send_msg()`.
///
/// # Implementers must define:
///
/// - `run()` - Takes ownership of the engine and starts the native loop
/// - `cleanup()` - Optional teardown (e.g., restore terminal state)
///
/// # Example
///
/// ```rust
/// use upmd_runtime::prelude::*;
///
/// struct MyComponent { value: i32 }
/// enum Msg { Increment }
///
/// impl Component for MyComponent {
///     type Msg = Msg;
///     fn update(&mut self, msg: Msg) -> Option<Cmd<Msg>> {
///         match msg {
///             Msg::Increment => self.value += 1,
///         }
///         None
///     }
/// }
///
/// struct MyRuntime;
///
/// impl Runtime<MyComponent> for MyRuntime {
///     type Error = std::io::Error;
///     fn run(self, engine: Engine<MyComponent>) -> Result<(), Self::Error> {
///         // Platform-specific event loop would go here
///         Ok(())
///     }
/// }
/// ```
pub trait Runtime<C: Component> {
    /// Error type returned when the run loop fails to start.
    type Error;

    /// Takes ownership of the Engine and starts the platform's native loop.
    ///
    /// The runtime is responsible for:
    /// - Polling input events and sending them to the engine
    /// - Calling `engine.tick()` to process messages
    /// - Calling `engine.render()` when `is_dirty` is true
    fn run(self, engine: Engine<C>) -> Result<(), Self::Error>;
}

/// Renders the current state of the component to an output surface.
///
/// Implementations handle platform-specific rendering (e.g., ANSI text,
/// ratatui widgets). The runtime calls `render` after each `tick` where
/// `is_dirty` is true.
pub trait Renderer<C: Component> {
    /// Renders the current state of the component to the output surface.
    ///
    /// This is called after each `tick` when the component has changed.
    fn render(&mut self, component: &C);
}

/// Runtime configuration for channel bounds.
///
/// Use `Config::default()` for sensible defaults, or build custom:
/// ```rust
/// use upmd_runtime::prelude::*;
///
/// let config = Config::new().msg_bound(Some(2048)).cmd_bound(Some(64));
/// ```
///
/// Use `None` for unbounded channels:
/// ```rust
/// use upmd_runtime::prelude::*;
/// let config = Config::new().msg_bound(None).cmd_bound(None);
/// ```
#[derive(Debug, Clone, Copy)]
pub struct Config {
    /// Capacity of the high-priority UI message channel. `None` for unbounded.
    pub msg_bound: Option<usize>,
    /// Capacity of the low-priority background command channel. `None` for unbounded.
    pub cmd_bound: Option<usize>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            msg_bound: Some(1024),
            cmd_bound: Some(32),
        }
    }
}

impl Config {
    /// Creates a new config with default values.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the UI message channel capacity. `None` for unbounded.
    pub fn msg_bound(mut self, bound: Option<usize>) -> Self {
        self.msg_bound = bound;
        self
    }

    /// Sets the background command channel capacity. `None` for unbounded.
    pub fn cmd_bound(mut self, bound: Option<usize>) -> Self {
        self.cmd_bound = bound;
        self
    }
}

/// The runtime engine that coordinates messages, commands, and component state.
///
/// Manages two channels: one for high-priority UI messages and one for
/// background commands. The engine drains UI messages first to ensure
/// responsive input, then processes background commands within a time budget
/// to maintain frame rate stability.
pub struct Engine<C: Component> {
    /// The component instance owning all application state.
    pub component: C,
    /// Whether the runtime loop should continue.
    pub is_running: bool,
    /// Whether the component has changed since last render.
    pub is_dirty: bool,
    /// High-priority message channel (UI events).
    msg_tx: Sender<C::Msg>,
    msg_rx: Receiver<C::Msg>,
    /// Low-priority command channel (background tasks).
    cmd_tx: Sender<C::Msg>,
    cmd_rx: Receiver<C::Msg>,
}

impl<C: Component> Engine<C> {
    /// Creates a new engine with the given component using default configuration.
    ///
    /// If the component implements `create()`, that command is spawned on a
    /// background thread before the loop starts.
    pub fn new(component: C) -> Self {
        Self::with_config(component, Config::default())
    }

    /// Creates a new engine with the given component and configuration.
    ///
    /// Use this when you need to customize channel bounds (e.g., higher throughput).
    ///
    /// # Example
    ///
    /// ```rust
    /// use upmd_runtime::prelude::*;
    ///
    /// struct MyComponent { value: i32 }
    /// enum Msg { Increment }
    ///
    /// impl Component for MyComponent {
    ///     type Msg = Msg;
    ///     fn update(&mut self, msg: Msg) -> Option<Cmd<Msg>> {
    ///         match msg {
    ///             Msg::Increment => self.value += 1,
    ///         }
    ///         None
    ///     }
    /// }
    ///
    /// let component = MyComponent { value: 0 };
    /// let config = Config::new().msg_bound(Some(2048)).cmd_bound(Some(64));
    /// let engine = Engine::with_config(component, config);
    /// ```
    pub fn with_config(mut component: C, config: Config) -> Self {
        let (msg_tx, msg_rx) = match config.msg_bound {
            Some(bound) => bounded(bound),
            None => unbounded(),
        };
        // Bounded to prevent high-volume output (like 'yes') from creating
        // a massive backlog that stalls the UI and makes ctrl-c feel unresponsive.
        // Use unbounded when cmd_bound is None.
        let (cmd_tx, cmd_rx) = match config.cmd_bound {
            Some(bound) => bounded(bound),
            None => unbounded(),
        };
        if let Some(cmd) = component.create() {
            spawn_cmd(cmd, cmd_tx.clone(), msg_tx.clone());
        }
        Self {
            component,
            msg_tx,
            msg_rx,
            cmd_tx,
            cmd_rx,
            is_running: true,
            is_dirty: true,
        }
    }

    /// Processes all pending messages and commands.
    ///
    /// This method drains messages in two phases:
    /// 1. **High-priority**: All UI messages are processed first, ensuring responsive input.
    /// 2. **Low-priority**: Background commands are processed within an 8ms time budget
    ///    to maintain stable frame rates.
    ///
    /// Processing stops early if a quit command is executed.
    pub fn tick(&mut self) {
        // High-priority: drain all UI messages before touching background cmds
        while let Ok(msg) = self.msg_rx.try_recv() {
            self.update(msg);
            if !self.is_running {
                return;
            }
        }

        // Low-priority: background cmds, time-boxed to stay within frame budget
        #[cfg(not(target_arch = "wasm32"))]
        {
            let budget = Duration::from_millis(8);
            let start = Instant::now();
            while let Ok(msg) = self.cmd_rx.try_recv() {
                self.update(msg);
                if !self.is_running {
                    return;
                }
                if start.elapsed() >= budget {
                    break;
                }
            }
        }
        #[cfg(target_arch = "wasm32")]
        {
            while let Ok(msg) = self.cmd_rx.try_recv() {
                self.update(msg);
                if !self.is_running {
                    return;
                }
            }
        }
    }

    /// Sends a high-priority UI message to the component.
    ///
    /// These messages are processed immediately in the next `tick` call,
    /// before any background commands.
    pub fn send_msg(&self, msg: C::Msg) -> Result<(), SendError<C::Msg>> {
        self.msg_tx.send(msg)
    }

    /// Renders the component using the given renderer.
    ///
    /// Typically called after `tick` when `is_dirty` is true.
    pub fn render<R: Renderer<C>>(&self, renderer: &mut R) {
        renderer.render(&self.component);
    }

    fn update(&mut self, msg: C::Msg) {
        self.is_dirty = true;
        match self.component.update(msg) {
            None => {}
            Some(Cmd::Quit) => self.is_running = false,
            Some(cmd) => spawn_cmd(cmd, self.cmd_tx.clone(), self.msg_tx.clone()),
        }
    }
}

fn spawn_cmd<Msg: Send + 'static>(cmd: Cmd<Msg>, low_tx: Sender<Msg>, high_tx: Sender<Msg>) {
    match cmd {
        Cmd::Quit => unreachable!("quit is handled before spawn_cmd is called"),
        Cmd::Stream(run) => {
            thread::spawn(move || run(low_tx));
        }
        Cmd::PriorityStream(run) => {
            thread::spawn(move || run(low_tx, high_tx));
        }
        Cmd::Task(run) => {
            thread::spawn(run);
        }
        Cmd::Batch(cmds) => {
            for cmd in cmds {
                spawn_cmd(cmd, low_tx.clone(), high_tx.clone());
            }
        }
    }
}
