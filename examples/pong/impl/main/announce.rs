// wh:key=dce9860dc32049cc prose=70220ef9
use crate::prelude::*;
use crate::main::*;

pub fn announce(cap_terminal: &impl Terminal, text: &str) {
    tracing::trace!(len = text.len(), "announce");
    cap_terminal.print_line(text);
}
