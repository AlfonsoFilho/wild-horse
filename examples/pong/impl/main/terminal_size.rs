// wh:key=a3ad6876ea65bf0b prose=87c600f2
use crate::prelude::*;
use crate::main::*;

use tracing::trace;

pub fn terminal_size(cap_terminal: &impl Terminal) -> Result<Size, TerminalError> {
    let size = cap_terminal.size()?;
    trace!(columns = size.columns, rows = size.rows, "terminal size");
    Ok(size)
}
