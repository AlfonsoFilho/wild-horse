use crate::prelude::*;
use crate::main::*;

pub fn show(cap_terminal: &impl Terminal, frame: &Frame) -> Result<(), TerminalError> {
    cap_terminal.draw(frame)
}
