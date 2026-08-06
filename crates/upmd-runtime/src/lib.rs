pub mod runtimes;

pub mod core;
pub use core::{Cmd, Component, Config, Effect, Engine, NoOutcome, Renderer, Runtime};

pub mod prelude {
    pub use crate::core::{Cmd, Component, Config, Effect, Engine, NoOutcome, Renderer, Runtime};
    pub use crate::effect;
    pub use std::thread;
    pub use std::time::{Duration, Instant};
}

/// Constructs an optional component [`Effect`].
///
/// ```
/// # use upmd_runtime::{effect, Cmd, Effect};
/// # enum Action { Finished }
/// # enum Outcome { Started }
/// let command: Option<Effect<Action, Outcome>> =
///     effect!(Cmd::msg(Action::Finished));
/// let outcome: Option<Effect<Action, Outcome>> =
///     effect!(outcome: Outcome::Started);
/// let both: Option<Effect<Action, Outcome>> = effect!(
///     Cmd::msg(Action::Finished),
///     outcome: Outcome::Started,
/// );
/// ```
#[macro_export]
macro_rules! effect {
    (outcome: $outcome:expr $(,)?) => {
        Some($crate::Effect::Outcome($outcome))
    };
    ($command:expr, outcome: $outcome:expr $(,)?) => {
        Some($crate::Effect::Both {
            command: $command,
            outcome: $outcome,
        })
    };
    ($command:expr $(,)?) => {
        Some($crate::Effect::Command($command))
    };
}
