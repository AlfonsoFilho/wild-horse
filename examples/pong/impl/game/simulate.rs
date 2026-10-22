// wh:key=1bc5db047829e100 prose=d4d1ac9b
use crate::prelude::*;
use crate::game::*;

pub fn simulate(game: Game, input: Input, frames: u8, dt: std::time::Duration) -> Game {
    (0..frames).fold(game, |game, _| step(&game, input, dt))
}
