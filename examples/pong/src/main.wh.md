---
module: main
---
# Pong in the terminal

    pong            # first to 5
    pong --to 11    # first to 11

Left player: `w` / `s`. Right player: `↑` / `↓`. Anyone: `q` to quit.

The rules are in [[game.step]] and the picture in [[view.render]]; both are
pure. This document adds the two things a real game needs and a test cannot
have: a terminal to read keys from and draw on, and a clock to keep time.
Both are capabilities, so the same loop runs against a scripted terminal and
a fake clock in the scenarios at the end.

```wh
from game import Game, Input, Phase, Side, new_game, step
from view import Frame, render
```

## Effects

The terminal. `enter` switches to raw mode and the
alternate screen and hides the cursor; `leave` undoes all of it and is safe to
call twice. Between the two, `poll_input` returns the next pending key
without waiting, and `draw` replaces the screen with a frame. `print_line`
writes to the normal screen, after `leave`.

```wh
capability Terminal:
    def enter() -> unit raises TerminalError
    def leave() -> unit
    def size() -> Size raises TerminalError
    def poll_input() -> Input raises TerminalError
    def draw(frame: Frame) -> unit raises TerminalError
    def print_line(line: str) -> unit
```

The size of the terminal in character cells.

```wh
struct Size:
    columns: u16
    rows: u16
```

Time, as a capability so tests can run a whole game in microseconds.

```wh
capability Clock:
    def now() -> instant
    def sleep(d: duration) -> unit
```

Anything the terminal reports goes into one error type; there is nothing a
game can do about it except stop.

```wh
@error
enum TerminalError:
    Io(str)
```

## The real terminal and clock

The terminal is driven with `crossterm`. Keys map to inputs as `w`/`s` for
the left paddle, the up and down arrows for the right one, and `q`, `Esc` or
`Ctrl-C` for quitting; anything else is `Idle`. The adapter must leave the
terminal usable whatever happens, so dropping it, or panicking while it is
active, also restores the screen.

```wh
adapter CrosstermTerminal implements Terminal:
    pattern Adapter
    note "enter: enable_raw_mode, EnterAlternateScreen, cursor::Hide; leave: the reverse, only if entered"
    note "poll_input: event::poll(Duration::ZERO) then event::read; only KeyEventKind::Press counts"
    note "draw: MoveTo(0, 0) and print each row, queued and flushed once per frame"
    note "restore the terminal in Drop of the state and in a panic hook installed by enter"
    trace off

depends crossterm = "0.28"
```

The system clock is `std::time`.

```wh
adapter SystemClock implements Clock:
    note "now: Instant::now(); sleep: std::thread::sleep"
    trace off
```

## Test doubles

A scripted terminal plays back a fixed list of inputs, one per poll, and
reports `Quit` once the script runs out so that every scripted game ends. It
counts the frames it was asked to draw instead of drawing them.

```wh
adapter ScriptedTerminal implements Terminal:
    columns: u16
    rows: u16
    inputs: list[Input]
    pattern InMemory
    note "keep the index of the next input and the number of drawn frames in Mutex-guarded state"
```

A fake clock never waits: `sleep` just moves its idea of "now" forward.

```wh
adapter FakeClock implements Clock:
    pattern InMemory
    note "now: a fixed starting Instant plus the total time slept so far"
```

## Options and setup

The only option is the target score, `--to N` with N from 1 to 21; the
default is 5.

```wh
struct Options:
    target: u8 = 5
```

Things that stop the game before it starts: a bad command line, a terminal
too small to play in, or a terminal that misbehaves.

```wh
@error
enum PongError:
    Usage(str)
    TooSmall(Size)
    Terminal(from TerminalError)
```

```wh
def parse_options(args: list[str]) -> Options raises PongError:
    """Read `--to N` from the command line."""
    example parse_options(["pong"]).target == 5
    example parse_options(["pong", "--to", "11"]).target == 11
    example parse_options(["pong", "--to", "0"]) raises PongError
    example parse_options(["pong", "--fast"]) raises PongError
```

The field fills the whole terminal except the two wall rows. Below 20 by 8
cells there is no room to play.

```wh
def fit_game(size: Size, options: Options) -> Game raises PongError:
    raises PongError.TooSmall when size.columns < 20 or size.rows < 8
    ensures result.width == size.columns and result.height + 2 == size.rows
    ensures result.target == options.target
    example fit_game(Size(columns=80, rows=24), Options()).height == 22
    example fit_game(Size(columns=10, rows=24), Options()) raises PongError
    note "new_game(size.columns, size.rows - 2, options.target)"

def terminal_size() -> Size raises TerminalError uses Terminal:
    """Ask the terminal how big it is."""
    note "delegate to Terminal.size"
```

## One frame

A frame of the game is a small generated pipeline: read at most one key,
advance the rules by one fixed tick, draw the result. The loop below calls it
sixty times a second; it is a flow, so the graph shows exactly this.

```wh
flow tick(game: Game, dt: duration) -> Game raises TerminalError uses Terminal:
    input = read_input()
    next = step(game, input, dt)
    show(render(next))
    return next

def read_input() -> Input raises TerminalError uses Terminal:
    """The next pending key, or `Idle`."""
    note "delegate to Terminal.poll_input"
    trace off

def show(frame: Frame) -> unit raises TerminalError uses Terminal:
    """Put a frame on the screen."""
    note "delegate to Terminal.draw"
    trace off
```

## The game loop

The loop runs [[tick]] with a fixed time step of 1/60 s, whatever the speed
of the machine: it measures real time with the clock, runs as many ticks as
fit into the time that has passed (at most five, so a stalled terminal does
not make the ball teleport), and sleeps until the next tick is due. It stops
when the game's phase is `Quit` and always leaves the terminal, also when a
tick fails.

```wh
def run(own game: Game) -> Game raises TerminalError uses Terminal, Clock:
    pattern GameLoop
    ensures result.phase == Phase.Quit
    note "Terminal.enter first; Terminal.leave on every exit path, before returning the error"
    note "fixed step TICK = 1/60 s; accumulate Clock.now() deltas; at most 5 ticks per iteration"
```

After the game the normal screen shows the final score, for example
`left 5 : 3 right - left wins`, or `left 2 : 1 right - abandoned` if the
players quit early.

```wh
def final_score(game: Game) -> str:
    example final_score(new_game(40, 12, 3)) == "left 0 : 0 right - abandoned"
    pattern Transform

def announce(text: str) -> unit uses Terminal:
    """Print a line on the normal screen."""
    note "delegate to Terminal.print_line"
```

## The application

Tracing would scribble over the playing field, so console logging is off.
To see what happened, set `WH_TRACE_FILE=pong.trace.json`; the per-frame
functions are marked `trace off`, so the trace stays readable.

```wh
app pong:
    target native(cli)
    log level="off" format="compact"
    provide Terminal = CrosstermTerminal()
    provide Clock = SystemClock()
    entry main(args: list[str]) -> unit raises PongError:
        options = parse_options(args)
        size = terminal_size()
        game = fit_game(size, options)
        final = run(game)
        announce(final_score(final))
```

## Scenarios

Whole games against the scripted terminal and the fake clock.

```wh
test "q quits straight away":
    given Terminal = ScriptedTerminal(columns=40, rows=14, inputs=[Input.Quit])
    given Clock = FakeClock()
    expect run(new_game(40, 12, 3)).phase == Phase.Quit
    expect run(new_game(40, 12, 3)).left_score == 0

test "paddles follow the keys":
    given Terminal = ScriptedTerminal(columns=40, rows=14, inputs=[Input.LeftUp, Input.LeftUp, Input.RightDown])
    given Clock = FakeClock()
    expect run(new_game(40, 12, 3)).left == 4.0
```
