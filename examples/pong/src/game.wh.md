---
module: game
---
# Pong: the rules

Two paddles, one ball, first to N points wins. This module is the whole game
as pure data and pure functions: no terminal, no clock, no randomness. Time
comes in as an argument to [[step]], keys come in as an [[Input]], and what
the players see is computed separately by [[view.render]]. Because of that,
every rule below is checked by ordinary tests, and the loop that drives the
real game in [[main.run]] is the only code that ever waits or draws.

## Geometry

The field is measured in terminal cells: `x` grows to the right, `y` grows
downwards, and `(0, 0)` is the top-left corner of the playing area (the walls
are drawn outside it). Positions are continuous so the ball can move at any
angle and speed; they are rounded to cells only when drawing.

```wh
struct Vec2:
    x: float
    y: float
```

## The players

There are two sides. The left player uses `w`/`s`, the right player the arrow
keys.

```wh
enum Side:
    Left
    Right
```

One input per tick: at most one paddle moves one cell, or a player quits.
Terminals report key presses but not releases, so a held key simply arrives
again and again through auto-repeat; `Idle` means nothing was pressed.

```wh
enum Input:
    Idle
    LeftUp
    LeftDown
    RightUp
    RightDown
    Quit
```

A game is being played until somebody reaches the target score, and it can
be abandoned at any time with `q`. A finished game stays on screen, frozen,
until the players quit.

```wh
enum Phase:
    Playing
    Won(Side)
    Quit
```

## The game state

Everything needed to continue a game. Each paddle is described by the `y` of
its centre; `paddle` is the height of both paddles. The ball moves by
`velocity` cells per second.

The invariants are the rules of the field: the ball is always inside it,
paddles never stick out of it, and nobody scores past the target. They are
checked every time a game is produced, which in practice means after every
single [[step]].

```wh
struct Game:
    width: u16
    height: u16
    ball: Vec2
    velocity: Vec2
    left: float
    right: float
    paddle: float
    left_score: u8
    right_score: u8
    target: u8
    phase: Phase = Phase.Playing
    invariant 0.0 <= self.ball.x < self.width and 0.0 <= self.ball.y < self.height
    invariant self.paddle / 2.0 <= self.left <= self.height - self.paddle / 2.0
    invariant self.paddle / 2.0 <= self.right <= self.height - self.paddle / 2.0
    invariant self.left_score <= self.target and self.right_score <= self.target
```

## Starting a game

A new game puts the ball in the middle, centres both paddles and serves
towards the right player. Paddles are a fifth of the field high, but never
shorter than three cells. The field must be big enough to play on.

```wh
def new_game(width: u16, height: u16, target: u8) -> Game:
    requires width >= 10 and height >= 5 and target >= 1
    ensures result.width == width and result.height == height and result.target == target
    ensures result.left_score == 0 and result.right_score == 0
    ensures result.phase == Phase.Playing
    ensures result.left == result.right
    ensures result.velocity.x > 0.0
    example new_game(40, 12, 3).left == 6.0
    example new_game(40, 12, 3).paddle == 3.0
    note "ball at (width / 2, height / 2); speed about width / 2 cells per second, moving right and slightly down"
```

## One tick

Advancing the game by `dt` applies the input and then moves the ball:

1. `LeftUp`/`LeftDown`/`RightUp`/`RightDown` move that paddle one cell,
   stopping at the walls; `Quit` ends the game whatever its phase.
2. The ball travels `velocity * dt`. It bounces off the top and bottom walls.
   When it reaches a paddle's column while the paddle covers it, it bounces
   back, a little faster, and steeper the further from the paddle's centre it
   hit.
3. A ball that gets past a paddle scores a point for the other side and is
   served again from the centre, towards the player who lost the point.
4. Reaching the target score wins.

Ticks are short: the game loop runs at 60 per second and never asks for more
than a tenth of a second at once, which keeps the ball from jumping across a
paddle.

```wh
def step(game: Game, input: Input, dt: duration) -> Game:
    requires dt <= 100ms
    ensures result.width == game.width and result.height == game.height
    ensures result.left_score >= game.left_score and result.right_score >= game.right_score
    ensures result.left_score + result.right_score <= game.left_score + game.right_score + 1
    ensures input != Input.Quit or result.phase == Phase.Quit
    ensures game.phase == Phase.Playing or input == Input.Quit or result == game
    example step(new_game(40, 12, 3), Input.LeftUp, 0ms).left == 5.0
    example step(new_game(40, 12, 3), Input.RightDown, 0ms).right == 7.0
    example step(step(new_game(40, 12, 3), Input.LeftUp, 0ms), Input.LeftDown, 0ms) == new_game(40, 12, 3)
    example step(new_game(40, 12, 3), Input.Quit, 16ms).phase == Phase.Quit
    property quitting_always_ends(input: Input, frames: u8):
        step(simulate(new_game(40, 12, 3), input, frames, 50ms), Input.Quit, 16ms).phase == Phase.Quit
    property ball_keeps_moving(input: Input, frames: u8):
        step(simulate(new_game(40, 12, 3), input, frames, 50ms), Input.Idle, 16ms).ball != simulate(new_game(40, 12, 3), input, frames, 50ms).ball or simulate(new_game(40, 12, 3), input, frames, 50ms).phase != Phase.Playing
    trace off
    no_panic
```

Playing many identical ticks in a row is how the laws above are exercised
over long rallies: every intermediate [[step]] is checked against its
contract and every intermediate [[Game]] against its invariants. It is also
what an attract mode would use.

```wh
def simulate(own game: Game, input: Input, frames: u8, dt: duration) -> Game:
    requires dt <= 100ms
    ensures frames > 0 or result == game
    example simulate(new_game(20, 6, 1), Input.Idle, 255, 100ms).phase == Phase.Won(Side.Left)
    property held_paddle_stops_at_the_wall(frames: u8):
        frames < 5 or simulate(new_game(40, 12, 3), Input.LeftUp, frames, 16ms).left == 1.5
    note "apply `step(game, input, dt)` `frames` times"
    trace off
```
