// wh:key=7d47d94427096e71 prose=4fb19eb8
use crate::prelude::*;
use crate::game::*;
use tracing::{debug, trace};

/// Distance between each side wall and the face of its paddle.
const PADDLE_INSET: f64 = 1.0;
/// Half the ball's size: a ball grazing the end of a paddle still bounces.
const BALL_RADIUS: f64 = 0.5;
/// How much faster the ball leaves a paddle than it arrived.
const SPEEDUP: f64 = 1.05;
/// Horizontal speed cap, relative to the serve speed.
const MAX_SPEEDUP: f64 = 4.0;
/// Vertical component of a bounce off a paddle's very end, relative to its horizontal speed.
const MAX_SLOPE: f64 = 0.75;
/// Vertical component of the serve, relative to its horizontal speed.
const SERVE_SLOPE: f64 = 0.25;
/// Keeps a ball bouncing off the bottom wall strictly inside the field.
const WALL_MARGIN: f64 = 1e-6;
/// Upper bound on wall bounces within one tick; ticks are short, so this is never reached.
const MAX_WALL_BOUNCES: usize = 16;

pub fn step(game: &Game, input: Input, dt: std::time::Duration) -> Game {
    if input == Input::Quit {
        debug!(phase = ?game.phase, "quit");
        return Game { phase: Phase::Quit, ..game.clone() };
    }
    if game.phase != Phase::Playing {
        return game.clone();
    }
    let mut next = game.clone();
    move_paddle(&mut next, input);
    advance_ball(&mut next, dt.as_secs_f64());
    next
}

/// Moves the paddle named by `input` one cell, keeping it inside the field.
fn move_paddle(game: &mut Game, input: Input) {
    let (paddle, delta) = match input {
        Input::LeftUp => (&mut game.left, -1.0),
        Input::LeftDown => (&mut game.left, 1.0),
        Input::RightUp => (&mut game.right, -1.0),
        Input::RightDown => (&mut game.right, 1.0),
        Input::Idle | Input::Quit => return,
    };
    let half = game.paddle / 2.0;
    let height = f64::from(game.height);
    *paddle = (*paddle + delta).min(height - half).max(half);
}

/// Moves the ball by `dt` seconds, bouncing it off walls and paddles, and
/// scores when it gets past a paddle.
fn advance_ball(game: &mut Game, dt: f64) {
    let width = f64::from(game.width);
    let height = f64::from(game.height);
    let (x, y) = (game.ball.x, game.ball.y);
    let (vx, vy) = (game.velocity.x, game.velocity.y);
    let new_x = x + vx * dt;

    let left_face = PADDLE_INSET;
    let right_face = width - PADDLE_INSET;
    let crossing = if vx < 0.0 && new_x < left_face {
        Some((Side::Left, left_face))
    } else if vx > 0.0 && new_x > right_face {
        Some((Side::Right, right_face))
    } else {
        None
    };

    let Some((side, face)) = crossing else {
        let (new_y, new_vy) = bounce_walls(y + vy * dt, vy, height);
        game.ball = Vec2 { x: new_x, y: new_y };
        game.velocity.y = new_vy;
        return;
    };

    // Where the ball is when it reaches the paddle's column.
    let travel = new_x - x;
    let frac = if travel.abs() > f64::EPSILON {
        ((face - x) / travel).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let t_hit = dt * frac;
    let (y_hit, _) = bounce_walls(y + vy * t_hit, vy, height);

    let paddle_y = match side {
        Side::Left => game.left,
        Side::Right => game.right,
    };
    let reach = game.paddle / 2.0 + BALL_RADIUS;
    let offset = y_hit - paddle_y;

    if offset.abs() <= reach {
        let speed = (vx.abs() * SPEEDUP).min(serve_speed(width) * MAX_SPEEDUP);
        let new_vx = match side {
            Side::Left => speed,
            Side::Right => -speed,
        };
        let new_vy = speed * MAX_SLOPE * (offset / reach).clamp(-1.0, 1.0);
        let rest = dt - t_hit;
        let bounced_x = (face + new_vx * rest).max(left_face).min(right_face);
        let (bounced_y, bounced_vy) = bounce_walls(y_hit + new_vy * rest, new_vy, height);
        trace!(?side, offset, speed, "paddle hit");
        game.ball = Vec2 { x: bounced_x, y: bounced_y };
        game.velocity = Vec2 { x: new_vx, y: bounced_vy };
        return;
    }

    let scorer = opposite(side);
    let score = match scorer {
        Side::Left => &mut game.left_score,
        Side::Right => &mut game.right_score,
    };
    if *score < game.target {
        *score += 1;
    }
    let reached_target = *score >= game.target;
    debug!(?scorer, left = game.left_score, right = game.right_score, "point");
    if reached_target {
        debug!(?scorer, "game won");
        game.phase = Phase::Won(scorer);
    }
    serve(game, side);
}

/// Puts the ball back in the centre, heading towards `towards`.
fn serve(game: &mut Game, towards: Side) {
    let width = f64::from(game.width);
    let height = f64::from(game.height);
    let speed = serve_speed(width);
    let vy = speed * SERVE_SLOPE * if game.velocity.y < 0.0 { -1.0 } else { 1.0 };
    let vx = match towards {
        Side::Left => -speed,
        Side::Right => speed,
    };
    game.ball = Vec2 { x: width / 2.0, y: height / 2.0 };
    game.velocity = Vec2 { x: vx, y: vy };
}

/// The serve crosses half the field per second, as in a new game.
fn serve_speed(width: f64) -> f64 {
    width / 2.0
}

fn opposite(side: Side) -> Side {
    match side {
        Side::Left => Side::Right,
        Side::Right => Side::Left,
    }
}

/// Reflects a vertical position off the top and bottom walls, flipping the
/// velocity at each bounce, and keeps it inside `[0, height)`.
fn bounce_walls(mut y: f64, mut vy: f64, height: f64) -> (f64, f64) {
    let bottom = height - WALL_MARGIN;
    for _ in 0..MAX_WALL_BOUNCES {
        if y < 0.0 {
            y = -y;
        } else if y > bottom {
            y = 2.0 * bottom - y;
        } else {
            break;
        }
        vy = -vy;
    }
    (y.min(bottom).max(0.0), vy)
}
