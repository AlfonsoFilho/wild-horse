// wh:key=b615e2c0dd9e2740 prose=f4316c12
use crate::prelude::*;
use crate::main::*;

use std::sync::Mutex;
use std::time::{Duration, Instant};

use tracing::trace;

/// Internal state of `FakeClock`, created with `Default` by `FakeClock::new`.
///
/// "Now" is the fixed `start` plus the total time slept so far.
pub struct State {
    start: Instant,
    slept: Mutex<Duration>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            start: Instant::now(),
            slept: Mutex::new(Duration::ZERO),
        }
    }
}

impl State {
    fn slept(&self) -> Duration {
        *self.slept.lock().unwrap_or_else(|e| e.into_inner())
    }
}

pub fn now(this: &FakeClock) -> std::time::Instant {
    let state = &this.state;
    let slept = state.slept();
    trace!(?slept, "fake clock now");
    state.start + slept
}

pub fn sleep(this: &FakeClock, d: std::time::Duration) {
    let mut slept = this.state.slept.lock().unwrap_or_else(|e| e.into_inner());
    *slept = slept.saturating_add(d);
    trace!(?d, total = ?*slept, "fake clock advanced");
}
