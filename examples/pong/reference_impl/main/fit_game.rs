use crate::prelude::*;
use crate::main::*;

pub fn fit_game(size: &Size, options: &Options) -> Result<Game, PongError> {
    if size.columns < 20 || size.rows < 8 {
        return Err(PongError::TooSmall(size.clone()));
    }
    Ok(new_game(size.columns, size.rows - 2, options.target))
}
