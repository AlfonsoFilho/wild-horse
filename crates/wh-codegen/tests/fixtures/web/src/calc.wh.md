---
module: calc
---
# Calculations in the browser

A word is never empty; a ratio may have a zero denominator.

```wh
type Word = str where len(self) >= 1

struct Ratio:
    num: int
    den: int

@error
enum CalcError:
    DivByZero
```

Pure functions, exported as they are.

```wh
def divide(r: Ratio) -> int raises CalcError:
    raises CalcError.DivByZero when r.den == 0

def shout(w: Word) -> str:
    ensures len(result) == len(w)
```

Notes kept in memory, which works in a browser too.

```wh
capability Notes:
    def add(note: str) -> int

adapter MemoryNotes implements Notes:
    pattern InMemory

def remember(note: str) -> int uses Notes:
    note "add the note and return how many notes there are"

flow note_twice(first: str, second: str) -> int uses Notes:
    remember(first)
    remember(second)
```

```wh
app calc_web:
    target web(wasm)
    provide Notes = MemoryNotes()
    export divide, shout
    export remember, note_twice
```
