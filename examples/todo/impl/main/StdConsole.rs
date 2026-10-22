// wh:key=618f50cff7361b2e
use crate::prelude::*;
use crate::main::*;

use std::io::Write as _;
use tracing::{debug, trace};

/// Internal state of `StdConsole`, created with `Default` by `StdConsole::new`.
#[derive(Default)]
pub struct State {}

pub fn print_line(this: &StdConsole, line: &str) {
    trace!(console = ?this, len = line.len(), "writing line to stdout");
    let mut out = std::io::stdout().lock();
    if let Err(e) = writeln!(out, "{line}").and_then(|()| out.flush()) {
        debug!(error = %e, "failed to write line to stdout");
    }
}
