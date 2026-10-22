use crate::prelude::*;
use crate::main::*;

#[derive(Default)]
pub struct State {}

pub fn print_line(this: &StdConsole, line: &str) {
    use std::io::Write;
    let _ = writeln!(std::io::stdout(), "{line}");
}
