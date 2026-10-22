---
module: view
---
# Pong: what the players see

Drawing is a pure function from a [[game.Game]] to a [[Frame]]: a grid of
characters that the terminal adapter in [[main.CrosstermTerminal]] copies to
the screen. Keeping it pure means the picture can be tested character by
character, without a terminal.

```wh
from game import Game, Phase, Side, Input, new_game, step
```

## Frames

A frame is the playing field plus a wall row above and below it, one string
per row, every row exactly `width` characters wide.

```wh
struct Frame:
    width: u16
    height: u16
    rows: list[str]
    invariant len(self.rows) == self.height
    invariant all(len(r) == self.width for r in self.rows)
```

## Rendering

The picture, from back to front:

- the walls are rows of `─`, and the score is written into the middle of the
  top wall as `left : right`;
- a dotted net `┊` runs down the middle column on every other row;
- the paddles are columns of `█` at the far left and far right;
- the ball is a `●` in the cell under its position;
- once somebody has won, the middle row says who, and how to leave.

```wh
def render(game: Game) -> Frame:
    ensures result.width == game.width
    ensures result.height - game.height == 2
    example render(new_game(10, 5, 3)).rows[0] == "──0 : 0───"
    example render(new_game(10, 5, 3)).rows[3] == "█    ●   █"
    example render(new_game(10, 5, 3)).rows[6] == "──────────"
    example render(new_game(10, 5, 3)).rows[1] == "     ┊    "
    example render(new_game(10, 6, 3)).rows[5] == "     ┊    "
    property rows_fit(input: Input):
        len(render(step(new_game(30, 10, 3), input, 50ms)).rows) == 12
    note "paddle cells: rows i with centre - paddle / 2 <= i + 0.5 < centre + paddle / 2; ball cell: (floor(x), floor(y))"
    note "the win message is `LEFT WINS - press q` or `RIGHT WINS - press q`, centred, cut to fit"
    trace off
    pattern Transform
```
