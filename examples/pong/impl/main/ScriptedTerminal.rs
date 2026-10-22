// wh:key=0a43bd36c5ac9085 prose=31d9de8e
use crate::prelude::*;
use crate::main::*;

use std::sync::{Mutex, MutexGuard};

use tracing::{debug, trace};

/// Internal state of `ScriptedTerminal`, created with `Default` by `ScriptedTerminal::new`.
#[derive(Default)]
pub struct State {
    inner: Mutex<Playback>,
}

/// Progress through the script and the number of frames drawn so far.
#[derive(Debug, Default)]
struct Playback {
    next: usize,
    frames: u64,
}

/// Lock the playback state; a poisoned lock still holds consistent counters.
fn playback(this: &ScriptedTerminal) -> MutexGuard<'_, Playback> {
    this.state
        .inner
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

pub fn enter(this: &ScriptedTerminal) -> Result<(), TerminalError> {
    debug!(columns = this.columns, rows = this.rows, inputs = this.inputs.len(), "entering scripted terminal");
    Ok(())
}

pub fn leave(this: &ScriptedTerminal) {
    let state = playback(this);
    debug!(polled = state.next, frames = state.frames, "leaving scripted terminal");
}

pub fn size(this: &ScriptedTerminal) -> Result<Size, TerminalError> {
    Ok(Size { columns: this.columns, rows: this.rows })
}

pub fn poll_input(this: &ScriptedTerminal) -> Result<Input, TerminalError> {
    let mut state = playback(this);
    let input = match this.inputs.get(state.next) {
        Some(&input) => {
            state.next += 1;
            input
        }
        None => Input::Quit,
    };
    trace!(index = state.next, ?input, "scripted input");
    Ok(input)
}

pub fn draw(this: &ScriptedTerminal, frame: &Frame) -> Result<(), TerminalError> {
    let mut state = playback(this);
    state.frames += 1;
    trace!(frames = state.frames, width = frame.width, height = frame.height, "frame counted");
    Ok(())
}

pub fn print_line(this: &ScriptedTerminal, line: &str) {
    debug!(line, "scripted terminal line");
}
