use crate::prelude::*;
use crate::main::*;

pub fn report(cap_console: &impl Console, id: u64) {
    cap_console.print_line(&format!("registered user {id}"));
}
