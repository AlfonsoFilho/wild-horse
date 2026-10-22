// wh:key=5e5942d344fb8dfe prose=88b8eebe
use crate::prelude::*;
use crate::game::*;

/// Paddles span a fifth of the field height.
const PADDLE_FRACTION: f64 = 0.2;
/// Paddles are never shorter than this many cells.
const MIN_PADDLE: f64 = 3.0;
/// Vertical component of the serve, relative to its horizontal speed.
const SERVE_SLOPE: f64 = 0.25;

pub fn new_game(width: u16, height: u16, target: u8) -> Game {
    let w = f64::from(width);
    let h = f64::from(height);

    let paddle = (h * PADDLE_FRACTION).max(MIN_PADDLE);
    let centre = h / 2.0;
    let speed = w / 2.0;

    let game = Game {
        width,
        height,
        ball: Vec2 { x: w / 2.0, y: centre },
        velocity: Vec2 { x: speed, y: speed * SERVE_SLOPE },
        left: centre,
        right: centre,
        paddle,
        left_score: 0,
        right_score: 0,
        target,
        phase: Phase::Playing,
    };
    tracing::debug!(width, height, target, paddle, "new game");
    game
}
