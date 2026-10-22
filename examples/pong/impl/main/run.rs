// wh:key=ae7d7d8c01a850b4 prose=8c81ffc6
use crate::prelude::*;
use crate::main::*;

use std::time::Duration;
use tracing::{debug, trace};

/// The fixed time step: 1/60 s.
const TICK: Duration = Duration::from_nanos(1_000_000_000 / 60);

/// The most ticks run in one iteration of the loop, so a stall does not make the ball teleport.
const MAX_TICKS_PER_ITERATION: u32 = 5;

pub fn run(cap_terminal: &impl Terminal, cap_clock: &impl Clock, game: Game) -> Result<Game, TerminalError> {
    cap_terminal.enter()?;
    let result = game_loop(cap_terminal, cap_clock, game);
    cap_terminal.leave();
    if let Err(e) = &result {
        debug!(error = ?e, "game loop failed; terminal left");
    }
    result
}

/// Run fixed-step ticks until the phase is `Quit`. The terminal must already be entered.
fn game_loop(cap_terminal: &impl Terminal, cap_clock: &impl Clock, mut game: Game) -> Result<Game, TerminalError> {
    let mut last = cap_clock.now();
    let mut lag = Duration::ZERO;
    while game.phase != Phase::Quit {
        let now = cap_clock.now();
        lag += now.saturating_duration_since(last);
        last = now;

        let mut ticks = 0;
        while lag >= TICK && ticks < MAX_TICKS_PER_ITERATION && game.phase != Phase::Quit {
            game = tick(cap_terminal, &game, TICK)?;
            lag -= TICK;
            ticks += 1;
        }
        if lag >= TICK {
            // Too far behind: drop the backlog instead of catching up all at once.
            trace!(?lag, "dropping backlog");
            lag = Duration::ZERO;
        }

        if game.phase != Phase::Quit {
            cap_clock.sleep(TICK - lag);
        }
    }
    debug!("phase is Quit; leaving the game loop");
    Ok(game)
}
