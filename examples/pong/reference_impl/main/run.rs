use crate::prelude::*;
use crate::main::*;

use std::time::Duration;

/// The fixed time step: 60 ticks per second.
const TICK: Duration = Duration::from_nanos(16_666_667);
/// Most ticks run to catch up after a stall; the rest of the backlog is dropped.
const MAX_CATCH_UP: u32 = 5;

pub fn run(cap_terminal: &impl Terminal, cap_clock: &impl Clock, game: Game) -> Result<Game, TerminalError> {
    cap_terminal.enter()?;
    let result = play(cap_terminal, cap_clock, game);
    cap_terminal.leave();
    result
}

fn play(cap_terminal: &impl Terminal, cap_clock: &impl Clock, mut game: Game) -> Result<Game, TerminalError> {
    show(cap_terminal, &render(&game))?;
    let mut last = cap_clock.now();
    let mut lag = Duration::ZERO;
    while game.phase != Phase::Quit {
        let now = cap_clock.now();
        lag += now.saturating_duration_since(last);
        last = now;
        let mut ticks = 0;
        while lag >= TICK && game.phase != Phase::Quit {
            if ticks == MAX_CATCH_UP {
                lag = Duration::ZERO;
                break;
            }
            game = tick(cap_terminal, &game, TICK)?;
            lag -= TICK;
            ticks += 1;
        }
        if game.phase != Phase::Quit {
            cap_clock.sleep(TICK.saturating_sub(lag));
        }
    }
    Ok(game)
}
