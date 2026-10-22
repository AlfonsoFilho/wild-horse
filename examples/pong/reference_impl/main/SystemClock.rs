use crate::prelude::*;
use crate::main::*;

/// Internal state of `SystemClock`, created with `Default` by `SystemClock::new`.
#[derive(Default)]
pub struct State {}

pub fn now(this: &SystemClock) -> std::time::Instant {
    std::time::Instant::now()
}

pub fn sleep(this: &SystemClock, d: std::time::Duration) {
    std::thread::sleep(d);
}
