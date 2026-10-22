use crate::prelude::*;
use crate::main::*;

use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Internal state of `FakeClock`, created with `Default` by `FakeClock::new`.
pub struct State {
    start: Instant,
    slept: Mutex<Duration>,
}

impl Default for State {
    fn default() -> Self {
        Self { start: Instant::now(), slept: Mutex::new(Duration::ZERO) }
    }
}

pub fn now(this: &FakeClock) -> Instant {
    this.state.start + *this.state.slept.lock().unwrap_or_else(|e| e.into_inner())
}

pub fn sleep(this: &FakeClock, d: Duration) {
    *this.state.slept.lock().unwrap_or_else(|e| e.into_inner()) += d;
}
