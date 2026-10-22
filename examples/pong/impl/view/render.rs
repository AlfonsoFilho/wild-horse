// wh:key=f642d5654ef01659 prose=169d5ed0
use crate::prelude::*;
use crate::view::*;
use tracing::{debug, trace};

const WALL: char = '─';
const NET: char = '┊';
const PADDLE: char = '█';
const BALL: char = '●';

/// Writes `text` centred into `row`, cutting it to fit.
fn write_centred(row: &mut [char], text: &str) {
    let len = text.chars().count();
    let start = row.len().saturating_sub(len) / 2;
    for (cell, c) in row[start..].iter_mut().zip(text.chars()) {
        *cell = c;
    }
}

/// Whether field row `i` is covered by a paddle centred at `centre`.
fn paddle_covers(centre: f64, paddle: f64, i: usize) -> bool {
    let mid = i as f64 + 0.5;
    centre - paddle / 2.0 <= mid && mid < centre + paddle / 2.0
}

pub fn render(game: &Game) -> Frame {
    let width = usize::from(game.width);
    let height = game.height.saturating_add(2);
    let field_rows = usize::from(height) - 2;

    let mut grid: Vec<Vec<char>> = Vec::with_capacity(usize::from(height));

    let mut top = vec![WALL; width];
    write_centred(&mut top, &format!("{} : {}", game.left_score, game.right_score));
    grid.push(top);

    let net_col = width / 2;
    for i in 0..field_rows {
        let mut row = vec![' '; width];
        if width > 0 {
            if i % 2 == 0 {
                row[net_col] = NET;
            }
            if paddle_covers(game.left, game.paddle, i) {
                row[0] = PADDLE;
            }
            if paddle_covers(game.right, game.paddle, i) {
                row[width - 1] = PADDLE;
            }
        }
        grid.push(row);
    }

    grid.push(vec![WALL; width]);

    let (bx, by) = (game.ball.x.floor(), game.ball.y.floor());
    if bx >= 0.0 && by >= 0.0 && bx < width as f64 && by < field_rows as f64 {
        grid[by as usize + 1][bx as usize] = BALL;
    } else {
        debug!(x = game.ball.x, y = game.ball.y, "ball outside the field; not drawn");
    }

    if let Phase::Won(side) = &game.phase {
        if field_rows > 0 {
            let message = match side {
                Side::Left => "LEFT WINS - press q",
                Side::Right => "RIGHT WINS - press q",
            };
            write_centred(&mut grid[field_rows / 2 + 1], message);
        }
    }

    let rows: Vec<String> = grid.into_iter().map(String::from_iter).collect();
    trace!(width, height, "rendered frame");
    Frame {
        width: game.width,
        height,
        rows,
    }
}
