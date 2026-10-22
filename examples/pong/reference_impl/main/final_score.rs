use crate::prelude::*;
use crate::main::*;

pub fn final_score(game: &Game) -> String {
    let outcome = if game.left_score >= game.target {
        "left wins"
    } else if game.right_score >= game.target {
        "right wins"
    } else {
        "abandoned"
    };
    format!("left {} : {} right - {outcome}", game.left_score, game.right_score)
}
