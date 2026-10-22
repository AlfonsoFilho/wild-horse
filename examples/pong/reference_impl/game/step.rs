use crate::prelude::*;
use crate::game::*;

use std::time::Duration;

/// Each paddle hit speeds the ball up by this factor ...
const SPEED_UP: f64 = 1.05;
/// ... up to this many field widths per second.
const MAX_SPEED_WIDTHS: f64 = 1.5;
/// Keeps positions strictly inside the half-open field.
const EPS: f64 = 1e-6;

pub fn step(game: &Game, input: Input, dt: Duration) -> Game {
    let mut g = game.clone();
    if input == Input::Quit {
        g.phase = Phase::Quit;
        return g;
    }
    if g.phase != Phase::Playing {
        return g;
    }
    let (w, h) = (f64::from(g.width), f64::from(g.height));
    let half = g.paddle / 2.0;
    let clamp = |y: f64| y.clamp(half, h - half);
    match input {
        Input::LeftUp => g.left = clamp(g.left - 1.0),
        Input::LeftDown => g.left = clamp(g.left + 1.0),
        Input::RightUp => g.right = clamp(g.right - 1.0),
        Input::RightDown => g.right = clamp(g.right + 1.0),
        Input::Idle | Input::Quit => {}
    }

    let t = dt.as_secs_f64();
    let (old_x, mut vx, mut vy) = (g.ball.x, g.velocity.x, g.velocity.y);
    let mut x = old_x + vx * t;
    let mut y = g.ball.y + vy * t;
    // bounce off the top and bottom walls (several times if the ball is fast)
    for _ in 0..8 {
        if y < 0.0 {
            y = -y;
            vy = -vy;
        } else if y >= h {
            y = 2.0 * h - y;
            vy = -vy;
        } else {
            break;
        }
    }
    y = y.clamp(0.0, h - EPS);

    // paddle faces: the left paddle fills column 0, the right one column w - 1
    let hits = |paddle: f64| (y - paddle).abs() <= half + 0.5;
    let deflect = |paddle: f64, vx: f64| ((y - paddle) / half).clamp(-1.0, 1.0) * vx.abs() * 0.75;
    let max_speed = MAX_SPEED_WIDTHS * w;
    if vx < 0.0 && old_x >= 1.0 && x < 1.0 && hits(g.left) {
        x = 2.0 - x;
        vy = deflect(g.left, vx);
        vx = (-vx * SPEED_UP).min(max_speed);
    } else if vx > 0.0 && old_x < w - 1.0 && x >= w - 1.0 && hits(g.right) {
        x = 2.0 * (w - 1.0) - x;
        vy = deflect(g.right, vx);
        vx = (-vx * SPEED_UP).max(-max_speed);
    }

    if x < 0.0 || x >= w {
        let scorer = if x < 0.0 { Side::Right } else { Side::Left };
        let score = match scorer {
            Side::Left => &mut g.left_score,
            Side::Right => &mut g.right_score,
        };
        *score += 1;
        if *score >= g.target {
            g.phase = Phase::Won(scorer);
        }
        // serve from the centre towards the player who lost the point
        let speed = w / 2.0;
        let towards_left = scorer == Side::Right;
        let down = (g.left_score + g.right_score) % 2 == 0;
        g.ball = Vec2 { x: w / 2.0, y: h / 2.0 };
        g.velocity = Vec2 {
            x: if towards_left { -speed } else { speed },
            y: if down { speed / 4.0 } else { -speed / 4.0 },
        };
        return g;
    }
    g.ball = Vec2 { x: x.clamp(0.0, w - EPS), y };
    g.velocity = Vec2 { x: vx, y: vy };
    g
}
