use crate::prelude::*;
use crate::game::*;

pub fn simulate(game: Game, input: Input, frames: u8, dt: std::time::Duration) -> Game {
    (0..frames).fold(game, |g, _| step(&g, input, dt))
}
