// wh:key=cb8081106d0f0af1
use crate::prelude::*;
use crate::main::*;
use tracing::trace;

pub fn say(cap_console: &impl Console, text: &str) {
    trace!(len = text.len(), "say");
    cap_console.print_line(text);
}
