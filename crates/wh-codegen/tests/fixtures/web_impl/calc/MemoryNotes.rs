use crate::prelude::*;
use crate::calc::*;

/// Internal state of `MemoryNotes`, created with `Default` by `MemoryNotes::new`.
#[derive(Default)]
pub struct State {
    notes: std::sync::Mutex<Vec<String>>,
}

pub fn add(this: &MemoryNotes, note: &str) -> i64 {
    let mut n = this.state.notes.lock().unwrap();
    n.push(note.to_string());
    n.len() as i64
}
