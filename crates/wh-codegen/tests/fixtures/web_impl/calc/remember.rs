use crate::prelude::*;
use crate::calc::*;

pub fn remember(cap_notes: &impl Notes, note: &str) -> i64 {
    cap_notes.add(note)
}
