use crate::prelude::*;
use crate::calc::*;

pub fn shout(w: &Word) -> String {
    w.chars().map(|c| if c.is_ascii() { c.to_ascii_uppercase() } else { c }).collect()
}
