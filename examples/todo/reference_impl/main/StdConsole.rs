use crate::prelude::*;
use crate::main::*;

/// Internal state of `StdConsole`, created with `Default` by `StdConsole::new`.
#[derive(Default)]
pub struct State {}

pub fn print_line(this: &StdConsole, line: &str) {
    use std::io::Write;
    let _ = writeln!(std::io::stdout(), "{line}");
}
