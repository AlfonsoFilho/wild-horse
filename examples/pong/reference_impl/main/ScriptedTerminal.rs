use crate::prelude::*;
use crate::main::*;

use std::sync::Mutex;

/// Internal state of `ScriptedTerminal`, created with `Default` by `ScriptedTerminal::new`.
#[derive(Default)]
pub struct State {
    next_input: Mutex<usize>,
    frames: Mutex<usize>,
}

pub fn enter(this: &ScriptedTerminal) -> Result<(), TerminalError> {
    Ok(())
}

pub fn leave(this: &ScriptedTerminal) {}

pub fn size(this: &ScriptedTerminal) -> Result<Size, TerminalError> {
    Ok(Size { columns: this.columns, rows: this.rows })
}

pub fn poll_input(this: &ScriptedTerminal) -> Result<Input, TerminalError> {
    let mut next = this.state.next_input.lock().unwrap_or_else(|e| e.into_inner());
    let input = this.inputs.get(*next).copied().unwrap_or(Input::Quit);
    *next += 1;
    Ok(input)
}

pub fn draw(this: &ScriptedTerminal, frame: &Frame) -> Result<(), TerminalError> {
    *this.state.frames.lock().unwrap_or_else(|e| e.into_inner()) += 1;
    Ok(())
}

pub fn print_line(this: &ScriptedTerminal, line: &str) {}
