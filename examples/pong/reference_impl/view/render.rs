use crate::prelude::*;
use crate::view::*;

pub fn render(game: &Game) -> Frame {
    let (w, h) = (usize::from(game.width), usize::from(game.height));
    let mut grid = vec![vec![' '; w]; h + 2];
    grid[0].fill('─');
    grid[h + 1].fill('─');
    put_centred(&mut grid[0], &format!("{} : {}", game.left_score, game.right_score));
    for i in (0..h).step_by(2) {
        grid[i + 1][w / 2] = '┊';
    }
    let half = game.paddle / 2.0;
    for i in 0..h {
        let centre = i as f64 + 0.5;
        if game.left - half <= centre && centre < game.left + half {
            grid[i + 1][0] = '█';
        }
        if game.right - half <= centre && centre < game.right + half {
            grid[i + 1][w - 1] = '█';
        }
    }
    let bx = (game.ball.x.floor().max(0.0) as usize).min(w - 1);
    let by = (game.ball.y.floor().max(0.0) as usize).min(h - 1);
    grid[by + 1][bx] = '●';
    if let Phase::Won(side) = &game.phase {
        let who = match side {
            Side::Left => "LEFT",
            Side::Right => "RIGHT",
        };
        put_centred(&mut grid[h / 2 + 1], &format!(" {who} WINS - press q "));
    }
    Frame {
        width: game.width,
        height: game.height + 2,
        rows: grid.into_iter().map(|r| r.into_iter().collect()).collect(),
    }
}

/// Overwrite the middle of `row` with `text`, cut to fit.
fn put_centred(row: &mut [char], text: &str) {
    let chars: Vec<char> = text.chars().take(row.len()).collect();
    let start = (row.len() - chars.len()) / 2;
    row[start..start + chars.len()].copy_from_slice(&chars);
}
