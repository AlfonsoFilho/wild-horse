// wh:key=66442b778183ab2f prose=1856205f
use crate::prelude::*;
use crate::main::*;

use std::io::{self, Write};
use std::sync::Once;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crossterm::cursor::{Hide, MoveTo, MoveToNextLine, Show};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::style::Print;
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::{execute, queue};

/// Whether some adapter currently holds the terminal; read by the panic hook.
static ACTIVE: AtomicBool = AtomicBool::new(false);
static PANIC_HOOK: Once = Once::new();

/// Internal state of `CrosstermTerminal`, created with `Default` by `CrosstermTerminal::new`.
#[derive(Default)]
pub struct State {
    entered: AtomicBool,
}

impl Drop for State {
    fn drop(&mut self) {
        if *self.entered.get_mut() {
            restore();
        }
    }
}

fn io_error(e: io::Error) -> TerminalError {
    TerminalError::Io(e.to_string())
}

/// Undo `enter` as far as possible; errors are logged, since there is nothing else to do.
fn restore() {
    ACTIVE.store(false, Ordering::SeqCst);
    if let Err(e) = execute!(io::stdout(), Show, LeaveAlternateScreen) {
        debug!(error = %e, "failed to leave the alternate screen");
    }
    if let Err(e) = terminal::disable_raw_mode() {
        debug!(error = %e, "failed to disable raw mode");
    }
}

/// Restore the screen before the default hook prints the panic message.
fn install_panic_hook() {
    PANIC_HOOK.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if ACTIVE.load(Ordering::SeqCst) {
                restore();
            }
            previous(info);
        }));
    });
}

fn map_key(key: KeyEvent) -> Input {
    match key.code {
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => Input::Quit,
        KeyCode::Char('q' | 'Q') | KeyCode::Esc => Input::Quit,
        KeyCode::Char('w' | 'W') => Input::LeftUp,
        KeyCode::Char('s' | 'S') => Input::LeftDown,
        KeyCode::Up => Input::RightUp,
        KeyCode::Down => Input::RightDown,
        _ => Input::Idle,
    }
}

pub fn enter(this: &CrosstermTerminal) -> Result<(), TerminalError> {
    if this.state.entered.load(Ordering::SeqCst) {
        return Ok(());
    }
    install_panic_hook();
    terminal::enable_raw_mode().map_err(io_error)?;
    this.state.entered.store(true, Ordering::SeqCst);
    ACTIVE.store(true, Ordering::SeqCst);
    if let Err(e) = execute!(io::stdout(), EnterAlternateScreen, Hide) {
        leave(this);
        return Err(io_error(e));
    }
    Ok(())
}

pub fn leave(this: &CrosstermTerminal) {
    if this.state.entered.swap(false, Ordering::SeqCst) {
        restore();
    }
}

pub fn size(this: &CrosstermTerminal) -> Result<Size, TerminalError> {
    let (columns, rows) = terminal::size().map_err(io_error)?;
    Ok(Size { columns, rows })
}

pub fn poll_input(this: &CrosstermTerminal) -> Result<Input, TerminalError> {
    if !event::poll(Duration::ZERO).map_err(io_error)? {
        return Ok(Input::Idle);
    }
    Ok(match event::read().map_err(io_error)? {
        Event::Key(key) if key.kind == KeyEventKind::Press => map_key(key),
        _ => Input::Idle,
    })
}

pub fn draw(this: &CrosstermTerminal, frame: &Frame) -> Result<(), TerminalError> {
    let mut out = io::stdout().lock();
    queue!(out, MoveTo(0, 0)).map_err(io_error)?;
    for (i, row) in frame.rows.iter().enumerate() {
        if i > 0 {
            queue!(out, MoveToNextLine(1)).map_err(io_error)?;
        }
        queue!(out, Print(row)).map_err(io_error)?;
    }
    out.flush().map_err(io_error)
}

pub fn print_line(this: &CrosstermTerminal, line: &str) {
    let mut out = io::stdout().lock();
    if let Err(e) = writeln!(out, "{line}").and_then(|()| out.flush()) {
        debug!(error = %e, "failed to print a line");
    }
}
