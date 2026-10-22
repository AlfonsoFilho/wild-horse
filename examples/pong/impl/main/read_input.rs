// wh:key=eb5ce9ee2c66a079 prose=7f26d206
use crate::prelude::*;
use crate::main::*;

pub fn read_input(cap_terminal: &impl Terminal) -> Result<Input, TerminalError> {
    cap_terminal.poll_input()
}
