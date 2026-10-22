use crate::prelude::*;
use crate::main::*;

pub fn say(cap_console: &impl Console, text: &str) {
    cap_console.print_line(text);
}
