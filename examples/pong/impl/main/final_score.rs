// wh:key=3bab987d3bd24c13 prose=95868b70
use crate::prelude::*;
use crate::main::*;

/// The side that won, if any. The run loop always ends in `Quit`, which
/// overwrites `Won`, so a score that reached the target also decides it.
fn winner(game: &Game) -> Option<Side> {
    match game.phase {
        Phase::Won(side) => Some(side),
        Phase::Playing | Phase::Quit if game.left_score >= game.target => Some(Side::Left),
        Phase::Playing | Phase::Quit if game.right_score >= game.target => Some(Side::Right),
        Phase::Playing | Phase::Quit => None,
    }
}

pub fn final_score(game: &Game) -> String {
    let outcome = match winner(game) {
        Some(Side::Left) => "left wins",
        Some(Side::Right) => "right wins",
        None => "abandoned",
    };
    format!("left {} : {} right - {outcome}", game.left_score, game.right_score)
}
