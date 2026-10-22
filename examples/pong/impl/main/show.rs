// wh:key=bad1fca7b1ec968b prose=e8cdacdf
use crate::prelude::*;
use crate::main::*;

pub fn show(cap_terminal: &impl Terminal, frame: &Frame) -> Result<(), TerminalError> {
    cap_terminal.draw(frame)
}
