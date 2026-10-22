use crate::prelude::*;
use crate::game::*;

pub fn new_game(width: u16, height: u16, target: u8) -> Game {
    let (w, h) = (f64::from(width), f64::from(height));
    let speed = w / 2.0;
    Game {
        width,
        height,
        ball: Vec2 { x: w / 2.0, y: h / 2.0 },
        velocity: Vec2 { x: speed, y: speed / 4.0 },
        left: h / 2.0,
        right: h / 2.0,
        paddle: (h / 5.0).max(3.0),
        left_score: 0,
        right_score: 0,
        target,
        phase: Phase::Playing,
    }
}
