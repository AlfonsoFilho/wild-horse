// wh:key=ba7584afa6157f13
use crate::prelude::*;
use crate::main::*;

use std::io::Write as _;

use tracing::{debug, trace};

/// Internal state of `StdConsole`, created with `Default` by `StdConsole::new`.
#[derive(Default)]
pub struct State {}

pub fn print_line(this: &StdConsole, line: &str) {
    trace!(len = line.len(), "writing line to stdout");
    let mut out = std::io::stdout().lock();
    // The capability returns unit, so a failed write (e.g. a closed pipe) is logged, not propagated.
    if let Err(e) = writeln!(out, "{line}").and_then(|()| out.flush()) {
        debug!(error = %e, "failed to write line to stdout");
    }
}
