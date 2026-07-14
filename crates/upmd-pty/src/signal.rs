//! Thread synchronization for child-process exit notification.
//!
//! [`ExitSignal`] pairs a flag with a condition variable so waiters can
//! block without polling.

use std::sync::{Arc, Condvar, Mutex};

/// A signal that becomes true once and notifies all waiters.
///
/// Cloning shares the same underlying state.
#[derive(Clone)]
pub struct ExitSignal {
    inner: Arc<ExitInner>,
}

struct ExitInner {
    done: Mutex<bool>,
    condvar: Condvar,
}

impl ExitSignal {
    /// Creates a new signal in the unsignalled state.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(ExitInner {
                done: Mutex::new(false),
                condvar: Condvar::new(),
            }),
        }
    }

    /// Sets the signal to true and wakes all waiters.
    pub fn notify(&self) {
        *self.inner.done.lock().unwrap() = true;
        self.inner.condvar.notify_all();
    }

    /// Returns `true` if [`notify`](Self::notify) has been called.
    pub fn is_done(&self) -> bool {
        *self.inner.done.lock().unwrap()
    }

    /// Blocks until [`notify`](Self::notify) is called.
    ///
    /// Returns immediately if the signal is already set.
    pub fn wait(&self) {
        let mut done = self.inner.done.lock().unwrap();
        while !*done {
            done = self.inner.condvar.wait(done).unwrap();
        }
    }
}

impl Default for ExitSignal {
    fn default() -> Self {
        Self::new()
    }
}
