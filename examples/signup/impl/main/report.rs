// wh:key=aec114f7f9c32f51
use crate::prelude::*;
use crate::main::*;

pub fn report(cap_console: &impl Console, id: u64) {
    let line = format!("registered user {id}");
    tracing::debug!(%line, "reporting registered user");
    cap_console.print_line(&line);
}
