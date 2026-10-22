use crate::prelude::*;
use crate::main::*;

pub fn read_input(cap_terminal: &impl Terminal) -> Result<Input, TerminalError> {
    cap_terminal.poll_input()
}
