---
module: main
---
# The `todo` command

A todo list for the terminal:

    todo add buy milk        # added #1: buy milk
    todo list                # [ ] #1 buy milk
    todo done 1              # done #1: buy milk
    todo list --all          # [x] #1 buy milk
    todo remove 1            # removed #1: buy milk
    todo clear-done          # cleared 0 done task(s)

The list is kept in `todo.json` in the current directory, or wherever the
`TODO_FILE` environment variable points. All the rules about tasks live in the
`tasks` module (see [[tasks.TodoList]]); this document turns command lines into calls to
[[tasks.add_task]], [[tasks.complete]] and friends and prints the result.

```wh
from tasks import TodoList, Title, TaskId, TodoError, Store, JsonFileStore, MemoryStore
from tasks import load_list, commit, add_task, complete, remove, clear_done, show
```

## Commands

Each sub-command becomes one variant. Parsing happens once, at the edge, so
everything after it works with checked values: a [[tasks.Title]] that is known
to be non-empty and a [[tasks.TaskId]] that is known to be a positive number.

```wh
enum Command:
    Add(title: Title)
    Done(id: TaskId)
    Remove(id: TaskId)
    List(include_done: bool)
    ClearDone
```

Anything the parser cannot make sense of is a usage error carrying a message
for the user; failures inside the list come wrapped from [[tasks.TodoError]].

```wh
@error
enum AppError:
    Usage(str)
    Todo(from TodoError)
```

The words after `add` are joined with single spaces, so quoting the title is
optional. A missing sub-command, an unknown one, a number that is not a
positive integer or a title that is empty or too long are all usage errors.

```wh
def parse_command(args: list[str]) -> Command raises AppError:
    example parse_command(["todo", "add", "buy", "milk"]) == Command.Add(title="buy milk")
    example parse_command(["todo", "add", "  call mom "]) == Command.Add(title="call mom")
    example parse_command(["todo", "done", "3"]) == Command.Done(id=3)
    example parse_command(["todo", "list"]) == Command.List(include_done=False)
    example parse_command(["todo", "list", "--all"]) == Command.List(include_done=True)
    example parse_command(["todo", "clear-done"]) == Command.ClearDone
    example parse_command(["todo"]) raises AppError
    example parse_command(["todo", "add"]) raises AppError
    example parse_command(["todo", "done", "0"]) raises AppError
    example parse_command(["todo", "frobnicate"]) raises AppError
    note "the usage message lists the sub-commands: add <title>, done <id>, remove <id>, list [--all], clear-done"
```

## Running a command

Every command starts from the stored list. Commands that change it go
through [[tasks.commit]], which saves the new list and yields the
confirmation; `list` only reads. Because this is a flow, the routing below is
the generated program: the graph shows one branch per command.

```wh
flow execute(cmd: Command) -> str raises TodoError uses Store:
    todos = load_list()
    match cmd:
        case Add(title):
            todos |> add_task(title) |> commit
        case Done(id):
            todos |> complete(id) |> commit
        case Remove(id):
            todos |> remove(id) |> commit
        case ClearDone:
            todos |> clear_done |> commit
        case List(include_done):
            todos |> show(include_done)
```

## Output

Output goes through a capability, so tests can run commands without a
terminal and the generated code never prints on its own.

```wh
capability Console:
    def print_line(line: str) -> unit
```

The real console writes each line to standard output.

```wh
adapter StdConsole implements Console:
    note "write the line and a newline to stdout"
```

Printing the result of a command is a single step at the end of the entry
flow.

```wh

def say(text: str) -> unit uses Console:
    note "delegate to Console.print_line"
```

## The application

The binary wires the real file store and standard output to the command
flow. Logging stays at `error` so the normal output is just the answer; run
with `WH_LOG=debug` to watch every step of [[execute]].

```wh
app todo:
    target native(cli)
    log level="error" format="compact"
    provide Store = JsonFileStore(path=env("TODO_FILE"))
    provide Console = StdConsole()
    entry main(args: list[str]) -> unit raises AppError:
        args |> parse_command |> execute |> say
```

## Scenarios

End-to-end behaviour, run against [[tasks.MemoryStore]] so nothing touches
the disk. Each scenario starts with an empty list.

```wh
test "adding then listing shows the task":
    given Store = MemoryStore()
    expect execute(Command.Add(title="buy milk")) == "added #1: buy milk"
    expect execute(Command.Add(title="call mom")) == "added #2: call mom"
    expect execute(Command.List(include_done=False)) == "[ ] #1 buy milk\n[ ] #2 call mom"

test "done tasks are hidden until --all":
    given Store = MemoryStore()
    expect execute(Command.Add(title="buy milk")) == "added #1: buy milk"
    expect execute(Command.Done(id=1)) == "done #1: buy milk"
    expect execute(Command.Done(id=1)) == "done #1: buy milk"
    expect execute(Command.List(include_done=False)) == "nothing to do"
    expect execute(Command.List(include_done=True)) == "[x] #1 buy milk"

test "removed ids are not reused":
    given Store = MemoryStore()
    expect execute(Command.Add(title="a")) == "added #1: a"
    expect execute(Command.Remove(id=1)) == "removed #1: a"
    expect execute(Command.Add(title="b")) == "added #2: b"
    expect execute(Command.Remove(id=1)) raises TodoError

test "clear-done keeps open tasks":
    given Store = MemoryStore()
    expect execute(Command.Add(title="a")) == "added #1: a"
    expect execute(Command.Add(title="b")) == "added #2: b"
    expect execute(Command.Done(id=1)) == "done #1: a"
    expect execute(Command.ClearDone) == "cleared 1 done task(s)"
    expect execute(Command.List(include_done=True)) == "[ ] #2 b"
```
