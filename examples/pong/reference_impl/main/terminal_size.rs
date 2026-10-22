use crate::prelude::*;
use crate::main::*;

pub fn terminal_size(cap_terminal: &impl Terminal) -> Result<Size, TerminalError> {
    cap_terminal.size()
}
