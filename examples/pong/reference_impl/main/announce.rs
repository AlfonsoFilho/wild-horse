use crate::prelude::*;
use crate::main::*;

pub fn announce(cap_terminal: &impl Terminal, text: &str) {
    cap_terminal.print_line(text);
}
