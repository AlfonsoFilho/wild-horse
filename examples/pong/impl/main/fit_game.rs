// wh:key=4b48f2189c76b885 prose=97190f3c
use crate::prelude::*;
use crate::main::*;

/// Smallest terminal, in cells, that leaves room to play.
const MIN_COLUMNS: u16 = 20;
const MIN_ROWS: u16 = 8;

/// Rows taken by the top and bottom walls.
const WALL_ROWS: u16 = 2;

pub fn fit_game(size: &Size, options: &Options) -> Result<Game, PongError> {
    if size.columns < MIN_COLUMNS || size.rows < MIN_ROWS {
        debug!(columns = size.columns, rows = size.rows, "terminal too small to play");
        return Err(PongError::TooSmall(size.clone()));
    }
    let height = size.rows - WALL_ROWS;
    trace!(width = size.columns, height, target = options.target, "fitting game to terminal");
    Ok(new_game(size.columns, height, options.target))
}
