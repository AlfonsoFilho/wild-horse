---
module: main
---
# Signup command line

A tiny command line front end for [[signup.register]]:

    signup_cli <email> <age>

prints the id of the new user.

```wh
from signup import register, SignupForm, RegisterError, Db, MemoryDb, UserId
```

## Console output

```wh
capability Console:
    def print_line(line: str) -> unit

adapter StdConsole implements Console:
    note "write each line to stdout"
```

## Arguments

```wh
@error
enum AppError:
    Usage(str)
    Register(from RegisterError)

def parse_cli(args: list[str]) -> SignupForm raises AppError:
    note "expects exactly: <program> <email> <age>"
    example parse_cli(["prog", "a@b.io", "30"]).age == 30
    example parse_cli(["prog"]) raises AppError

def report(id: UserId) -> unit uses Console:
    note "print `registered user <id>`"
    trace info
```

## The application

```wh
app signup_cli:
    target native(cli)
    log level="info" format="pretty" release_level="info"
    provide Db = MemoryDb()
    provide Console = StdConsole()
    entry main(args: list[str]) -> unit raises AppError:
        args |> parse_cli |> register |> report
```
