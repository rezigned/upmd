//! Cha - A universal runtime engine implementing The Elm Architecture.
//!
//! This module provides core abstractions for building reactive applications
//! with clear separation between runtime, component, and rendering logic.

pub use flume::Sender;
use flume::{bounded, unbounded, Receiver, SendError};
use std::{
    collections::VecDeque,
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
/// - `Cmd::after(delay, m)` - Delivers one message after a delay
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
    /// Starts background work that emits zero, one, or many messages.
    Stream(Box<dyn FnOnce(Sender<Msg>) + Send>),
    /// Starts background work with separate low- and high-priority senders.
    PriorityStream(Box<dyn FnOnce(Sender<Msg>, Sender<Msg>) + Send>),
    /// Delivers one action after the requested delay.
    After(Duration, Msg),
    /// Runs fire-and-forget background work.
    Task(Box<dyn FnOnce() + Send>),
    /// Signals the runtime to terminate.
    Quit,
    /// Batch of commands to execute in parallel.
    Batch(Vec<Cmd<Msg>>),
}

impl<Msg: Send + 'static> Cmd<Msg> {
    /// Creates background work that emits potentially many messages.
    /// The task receives a sender to enqueue messages.
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

    /// Creates a command that delivers one action after a delay.
    pub fn after(delay: Duration, msg: Msg) -> Self {
        Cmd::After(delay, msg)
    }

    /// Creates fire-and-forget background work.
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

    /// Creates background work that computes and emits exactly one message.
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
            Cmd::After(delay, msg) => Cmd::After(delay, f(msg)),
            Cmd::Task(run) => Cmd::Task(run),
            Cmd::Batch(cmds) => {
                Cmd::Batch(cmds.into_iter().map(|c| c.map(Clone::clone(&f))).collect())
            }
        }
    }
}

/// An outward consequence of applying an action to a component.
///
/// Commands deliver future actions back to the same component. Outcomes are
/// synchronous semantic results for the component's parent.
#[must_use = "component effects may contain a command or parent outcome"]
pub enum Effect<Action, Outcome> {
    /// Runtime work that may deliver future component actions.
    Command(Cmd<Action>),
    /// A semantic result for the parent component.
    Outcome(Outcome),
    /// Runtime work and a semantic parent result produced together.
    Both {
        command: Cmd<Action>,
        outcome: Outcome,
    },
}

impl<Action, Outcome> Effect<Action, Outcome> {
    pub fn from_parts(command: Option<Cmd<Action>>, outcome: Option<Outcome>) -> Option<Self> {
        match (command, outcome) {
            (None, None) => None,
            (Some(command), None) => Some(Self::Command(command)),
            (None, Some(outcome)) => Some(Self::Outcome(outcome)),
            (Some(command), Some(outcome)) => Some(Self::Both { command, outcome }),
        }
    }
}

/// Outcome type for root and leaf components that cannot emit a result.
pub type NoOutcome = std::convert::Infallible;

/// Extension methods for optional component effects.
pub trait EffectExt {
    /// Action delivered by commands contained in the effect.
    type Action;
    /// Semantic result emitted by the effect.
    type Outcome;

    /// Splits an optional effect into its command and outcome.
    fn into_parts(self) -> (Option<Cmd<Self::Action>>, Option<Self::Outcome>);
}

impl<Action, Outcome> EffectExt for Option<Effect<Action, Outcome>> {
    type Action = Action;
    type Outcome = Outcome;

    fn into_parts(self) -> (Option<Cmd<Action>>, Option<Outcome>) {
        match self {
            None => (None, None),
            Some(Effect::Command(command)) => (Some(command), None),
            Some(Effect::Outcome(outcome)) => (None, Some(outcome)),
            Some(Effect::Both { command, outcome }) => (Some(command), Some(outcome)),
        }
    }
}

/// Command extraction for optional effects that cannot contain outcomes.
pub trait CommandEffectExt {
    /// Action delivered by the extracted command.
    type Action;

    /// Extracts the command from an optional effect.
    fn into_command(self) -> Option<Cmd<Self::Action>>;
}

impl<Action> CommandEffectExt for Option<Effect<Action, NoOutcome>> {
    type Action = Action;

    fn into_command(self) -> Option<Cmd<Action>> {
        match self {
            None => None,
            Some(Effect::Command(command)) => Some(command),
            Some(Effect::Outcome(outcome)) => match outcome {},
            Some(Effect::Both { outcome, .. }) => match outcome {},
        }
    }
}

// COMPONENT

/// The core trait for application state and logic, following The Elm Architecture.
///
/// A component owns its state and defines how that state mutates in response to actions.
/// The runtime ensures `update` is the sole place where state is modified.
///
/// # Implementers must define:
///
/// - `Action` - The input type that drives this component
/// - `Outcome` - The semantic result type emitted to the parent
/// - `update()` - Handles actions and returns commands, outcomes, or both
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
/// enum Action { Increment, Decrement }
///
/// impl Component for Counter {
///     type Action = Action;
///     type Outcome = NoOutcome;
///
///     fn update(&mut self, action: Action) -> Option<Effect<Action, NoOutcome>> {
///         match action {
///             Action::Increment => self.count += 1,
///             Action::Decrement => self.count -= 1,
///         }
///         None
///     }
/// }
/// ```
pub trait Component {
    /// Actions entering this component from input or commands.
    type Action: Send + 'static;

    /// Semantic results emitted synchronously to the parent component.
    type Outcome;

    /// Called once before the run loop starts. Use for initial work such as
    /// loading configuration, fetching data, or scheduling an action.
    ///
    /// The platform runtime executes the returned command.
    fn create(&mut self) -> Option<Cmd<Self::Action>> {
        None
    }

    /// Applies an action, mutating local state and optionally producing runtime
    /// work, a semantic result for the parent, or both.
    fn update(&mut self, action: Self::Action) -> Option<Effect<Self::Action, Self::Outcome>>;
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
/// enum Action { Increment }
///
/// impl Component for MyComponent {
///     type Action = Action;
///     type Outcome = NoOutcome;
///
///     fn update(&mut self, action: Action) -> Option<Effect<Action, NoOutcome>> {
///         match action {
///             Action::Increment => self.value += 1,
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
pub trait Runtime<C: Component<Outcome = NoOutcome>> {
    /// Error type returned when the run loop fails to start.
    type Error;

    /// Takes ownership of the Engine and starts the platform's native loop.
    ///
    /// The runtime is responsible for:
    /// - Polling input events and sending them to the engine
    /// - Calling `engine.tick()` to process messages
    /// - Executing commands returned by the engine
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

struct Scheduled<Action> {
    deadline: Instant,
    action: Action,
}
/// The runtime engine that coordinates messages, commands, and component state.
///
/// Manages two channels: one for high-priority UI messages and one for
/// background commands. The engine drains UI messages first to ensure
/// responsive input, then processes background commands within a time budget
/// to maintain frame rate stability.
pub struct Engine<C: Component<Outcome = NoOutcome>> {
    /// The component instance owning all application state.
    pub component: C,
    /// Whether the runtime loop should continue.
    pub is_running: bool,
    /// Whether the component has changed since last render.
    pub is_dirty: bool,
    /// High-priority message channel (UI events).
    msg_tx: Sender<C::Action>,
    msg_rx: Receiver<C::Action>,
    /// Low-priority command channel (background tasks).
    cmd_tx: Sender<C::Action>,
    cmd_rx: Receiver<C::Action>,
    /// Delayed actions ordered by deadline.
    scheduled: VecDeque<Scheduled<C::Action>>,
}

impl<C: Component<Outcome = NoOutcome>> Engine<C> {
    /// Creates a new engine with the given component using default configuration.
    ///
    /// Commands returned by `create()` are executed before the run loop starts.
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
    /// enum Action { Increment }
    ///
    /// impl Component for MyComponent {
    ///     type Action = Action;
    ///     type Outcome = NoOutcome;
    ///
    ///     fn update(&mut self, action: Action) -> Option<Effect<Action, NoOutcome>> {
    ///         match action {
    ///             Action::Increment => self.value += 1,
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
        let initial_command = component.create();
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
        let mut engine = Self {
            component,
            msg_tx,
            msg_rx,
            cmd_tx,
            cmd_rx,
            scheduled: VecDeque::new(),
            is_running: true,
            is_dirty: true,
        };
        if let Some(command) = initial_command {
            engine.execute(command);
        }
        engine
    }

    /// Processes all pending messages and commands.
    ///
    /// This method drains messages in two phases:
    /// 1. **High-priority**: All UI messages are processed first, ensuring responsive input.
    /// 2. **Low-priority**: Background commands are processed within an 8ms time budget
    ///    to maintain stable frame rates.
    ///
    /// If a high-priority message stops the engine, drain queued low-priority
    /// messages once before returning so accepted PTY output reaches the final render.
    pub fn tick(&mut self) {
        self.deliver_due();
        if !self.is_running {
            return;
        }

        // High-priority: drain all UI messages before touching background cmds
        while let Ok(msg) = self.msg_rx.try_recv() {
            self.update(msg);
            if !self.is_running {
                // Preserve accepted PTY output before the final render.
                while let Ok(msg) = self.cmd_rx.try_recv() {
                    self.update(msg);
                }
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
    pub fn send_msg(&self, msg: C::Action) -> Result<(), SendError<C::Action>> {
        self.msg_tx.send(msg)
    }

    /// Returns the maximum time the platform can wait before the next action.
    pub fn poll_timeout(&self, maximum: Duration) -> Duration {
        self.scheduled
            .front()
            .map(|scheduled| {
                maximum.min(scheduled.deadline.saturating_duration_since(Instant::now()))
            })
            .unwrap_or(maximum)
    }

    /// Renders the component using the given renderer.
    ///
    /// Typically called after `tick` when `is_dirty` is true.
    pub fn render<R: Renderer<C>>(&self, renderer: &mut R) {
        renderer.render(&self.component);
    }

    fn update(&mut self, action: C::Action) {
        self.is_dirty = true;
        if let Some(command) = self.component.update(action).into_command() {
            self.execute(command);
        }
    }

    fn execute(&mut self, command: Cmd<C::Action>) {
        match command {
            Cmd::Stream(run) => {
                let tx = self.cmd_tx.clone();
                thread::spawn(move || run(tx));
            }
            Cmd::PriorityStream(run) => {
                let low_tx = self.cmd_tx.clone();
                let high_tx = self.msg_tx.clone();
                thread::spawn(move || run(low_tx, high_tx));
            }
            Cmd::After(delay, action) => {
                let deadline = Instant::now() + delay;
                let index = self
                    .scheduled
                    .iter()
                    .position(|scheduled| scheduled.deadline > deadline)
                    .unwrap_or(self.scheduled.len());
                self.scheduled.insert(index, Scheduled { deadline, action });
            }
            Cmd::Task(run) => {
                thread::spawn(run);
            }
            Cmd::Quit => self.is_running = false,
            Cmd::Batch(commands) => {
                for command in commands {
                    if !self.is_running {
                        break;
                    }
                    self.execute(command);
                }
            }
        }
    }

    fn deliver_due(&mut self) {
        let now = Instant::now();
        let count = self
            .scheduled
            .iter()
            .take_while(|scheduled| scheduled.deadline <= now)
            .count();

        for _ in 0..count {
            if let Some(scheduled) = self.scheduled.pop_front() {
                self.update(scheduled.action);
                if !self.is_running {
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct CreateCommand(Option<Cmd<()>>);

    impl Component for CreateCommand {
        type Action = ();
        type Outcome = NoOutcome;

        fn create(&mut self) -> Option<Cmd<Self::Action>> {
            self.0.take()
        }

        fn update(&mut self, (): ()) -> Option<Effect<Self::Action, Self::Outcome>> {
            None
        }
    }

    struct Delayed {
        delivered: bool,
    }

    impl Component for Delayed {
        type Action = ();
        type Outcome = NoOutcome;

        fn create(&mut self) -> Option<Cmd<Self::Action>> {
            Some(Cmd::after(Duration::ZERO, ()))
        }

        fn update(&mut self, (): ()) -> Option<Effect<Self::Action, Self::Outcome>> {
            self.delivered = true;
            None
        }
    }

    #[test]
    fn quit_from_create_stops_the_engine() {
        let engine = Engine::new(CreateCommand(Some(Cmd::quit())));
        assert!(!engine.is_running);
    }

    #[test]
    fn quit_inside_a_batch_stops_the_engine() {
        let command = Cmd::Batch(vec![Cmd::task(|| {}), Cmd::quit()]);
        let engine = Engine::new(CreateCommand(Some(command)));
        assert!(!engine.is_running);
    }

    #[test]
    fn delayed_command_delivers_action() {
        let mut engine = Engine::new(Delayed { delivered: false });

        engine.tick();

        assert!(engine.component.delivered);
    }

    #[test]
    fn effect_can_contain_command_and_outcome() {
        let effect: Option<Effect<(), i32>> = crate::effect!(Cmd::task(|| {}), outcome: 7);
        let (command, outcome) = effect.into_parts();

        assert!(command.is_some());
        assert_eq!(outcome, Some(7));
    }
}
