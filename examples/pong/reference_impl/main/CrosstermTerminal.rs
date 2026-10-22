use crate::prelude::*;
use crate::main::*;

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::{cursor, execute, queue, style, terminal};

/// Internal state of `CrosstermTerminal`, created with `Default` by `CrosstermTerminal::new`.
#[derive(Default)]
pub struct State {
    entered: AtomicBool,
}

impl Drop for State {
    fn drop(&mut self) {
        if self.entered.swap(false, Ordering::SeqCst) {
            restore();
        }
    }
}

fn io(e: std::io::Error) -> TerminalError {
    TerminalError::Io(e.to_string())
}

/// Put the terminal back the way the shell expects it.
fn restore() {
    let _ = execute!(std::io::stdout(), cursor::Show, terminal::LeaveAlternateScreen);
    let _ = terminal::disable_raw_mode();
}

pub fn enter(this: &CrosstermTerminal) -> Result<(), TerminalError> {
    if this.state.entered.swap(true, Ordering::SeqCst) {
        return Ok(());
    }
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore();
        previous(info);
    }));
    terminal::enable_raw_mode().map_err(io)?;
    execute!(std::io::stdout(), terminal::EnterAlternateScreen, cursor::Hide, terminal::Clear(terminal::ClearType::All))
        .map_err(io)
}

pub fn leave(this: &CrosstermTerminal) {
    if this.state.entered.swap(false, Ordering::SeqCst) {
        restore();
    }
}

pub fn size(this: &CrosstermTerminal) -> Result<Size, TerminalError> {
    let (columns, rows) = terminal::size().map_err(io)?;
    Ok(Size { columns, rows })
}

pub fn poll_input(this: &CrosstermTerminal) -> Result<Input, TerminalError> {
    while event::poll(Duration::ZERO).map_err(io)? {
        let Event::Key(key) = event::read().map_err(io)? else { continue };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        return Ok(match key.code {
            KeyCode::Char('c') if ctrl => Input::Quit,
            KeyCode::Char('q') | KeyCode::Char('Q') | KeyCode::Esc => Input::Quit,
            KeyCode::Char('w') | KeyCode::Char('W') => Input::LeftUp,
            KeyCode::Char('s') | KeyCode::Char('S') => Input::LeftDown,
            KeyCode::Up => Input::RightUp,
            KeyCode::Down => Input::RightDown,
            _ => Input::Idle,
        });
    }
    Ok(Input::Idle)
}

pub fn draw(this: &CrosstermTerminal, frame: &Frame) -> Result<(), TerminalError> {
    let mut out = std::io::stdout().lock();
    for (i, row) in frame.rows.iter().enumerate() {
        queue!(out, cursor::MoveTo(0, i as u16), style::Print(row)).map_err(io)?;
    }
    out.flush().map_err(io)
}

pub fn print_line(this: &CrosstermTerminal, line: &str) {
    let _ = writeln!(std::io::stdout(), "{line}");
}
