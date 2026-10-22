---
module: tasks
---
# Tasks

This module is the heart of the todo list: what a task *is*, the handful of
things you can do to a list of tasks, and where the list is kept between runs.
Everything here except the [[Store]] is pure, so every rule about the list is
stated once, as a contract, and checked by the generated tests.

The command line in [[main.execute]] only decides *which* operation to run;
the operations themselves live here.

## The domain

A task title is what the user typed after `todo add`. It must contain
something visible, must not start or end with whitespace (the parser trims it),
and is kept short enough to fit on one line of a terminal.

```wh
@derive(Serialize, Deserialize)
type Title = str where matches(self, r"^\S(.*\S)?$") and len(self) <= 80
```

Tasks are numbered from 1, in the order they were added. Numbers are never
reused, even after a task is removed, so `todo done 3` always means the task
that was shown as `#3`.

```wh
@derive(Serialize, Deserialize)
type TaskId = u32 where self >= 1
```

A task is either still open or done. There is deliberately no third state:
removing a task deletes it rather than marking it.

```wh
@derive(Serialize, Deserialize)
enum Status:
    Open
    Done
```

A single task. Its [[TaskId]] is its identity (`key`): two tasks with the same
title are still different tasks.

```wh
@derive(Serialize, Deserialize)
struct Task:
    key id: TaskId
    title: Title
    status: Status = Status.Open
```

The whole list, exactly as it is stored. `next_id` is the number the next
added task will get; keeping it in the list (instead of computing
`max(id) + 1`) is what stops ids from being reused after [[remove]].
The invariants are checked whenever a list is built or loaded.

```wh
@derive(Serialize, Deserialize)
struct TodoList:
    tasks: list[Task]
    next_id: TaskId
    invariant all(t.id < self.next_id for t in self.tasks)
    invariant len(set([t.id for t in self.tasks])) == len(self.tasks)
```

Every operation that changes the list also says what it did, so the command
line can confirm it ("added #3: buy milk"). Pairing the two keeps the message
and the change from drifting apart.

```wh
struct Change:
    todos: TodoList
    message: str
```

## Errors

Asking for a task that does not exist is the only thing that can go wrong
with the list itself.

```wh
@error
enum TodoError:
    NotFound(TaskId)
    Storage(from StoreError)
```

Storage can fail because the file cannot be read or written, or because it
does not contain a valid list (for example after a hand edit).

```wh
@error
enum StoreError:
    Io(str)
    Corrupt(str)
```

## Operations

All operations take the list by value (`own`) and hand back a [[Change]]:
nothing is mutated in place, which keeps them trivially testable.

A fresh list has no tasks and hands out `#1` next.

```wh
def new_list() -> TodoList:
    ensures len(result.tasks) == 0
    ensures result.next_id == 1
```

### Adding

Adding appends an open task with the next free number. Whatever was in the
list before is still there afterwards, and the new task can be found by its
title.

```wh
def add_task(own todos: TodoList, title: Title) -> Change:
    ensures len(result.todos.tasks) == len(todos.tasks) + 1
    ensures result.todos.next_id == todos.next_id + 1
    ensures all(t in result.todos.tasks for t in todos.tasks)
    example add_task(new_list(), "buy milk").message == "added #1: buy milk"
    example add_task(add_task(new_list(), "a").todos, "b").todos.next_id == 3
    property listed_after_add(title: Title):
        any(t.title == title for t in add_task(new_list(), title).todos.tasks)
    property ids_are_unique(a: Title, b: Title):
        len(set([t.id for t in add_task(add_task(new_list(), a).todos, b).todos.tasks])) == 2
```

### Completing

Completing marks one task done. Completing a task that is already done is
not an error: the command is *idempotent*, so running `todo done 3` twice is
harmless.

```wh
def complete(own todos: TodoList, id: TaskId) -> Change raises TodoError:
    raises TodoError.NotFound when not any(t.id == id for t in todos.tasks)
    ensures len(result.todos.tasks) == len(todos.tasks)
    ensures all(t.status == Status.Done for t in result.todos.tasks if t.id == id)
    example complete(add_task(new_list(), "buy milk").todos, 1).message == "done #1: buy milk"
    example complete(new_list(), 1) raises TodoError
    property idempotent(title: Title):
        complete(complete(add_task(new_list(), title).todos, 1).todos, 1).todos == complete(add_task(new_list(), title).todos, 1).todos
```

### Removing

Removing deletes one task for good. Its number is not handed out again.

```wh
def remove(own todos: TodoList, id: TaskId) -> Change raises TodoError:
    raises TodoError.NotFound when not any(t.id == id for t in todos.tasks)
    ensures not any(t.id == id for t in result.todos.tasks)
    ensures len(result.todos.tasks) == len(todos.tasks) - 1
    ensures result.todos.next_id == todos.next_id
    example remove(add_task(new_list(), "x").todos, 1).message == "removed #1: x"
    example remove(add_task(new_list(), "x").todos, 2) raises TodoError
```

Clearing drops every finished task at once and keeps the open ones in their
original order.

```wh
def clear_done(own todos: TodoList) -> Change:
    ensures all(t.status == Status.Open for t in result.todos.tasks)
    ensures len(result.todos.tasks) == len([t for t in todos.tasks if t.status == Status.Open])
    ensures result.todos.next_id == todos.next_id
    example clear_done(new_list()).message == "cleared 0 done task(s)"
    pattern Transform
```

### Showing

The list as the user sees it: one line per task, `[ ]` for open and `[x]` for
done, followed by the number and the title. Done tasks are hidden unless
`include_done` is set (`todo list --all`). An empty listing says so instead of
printing nothing.

```wh
def show(todos: TodoList, include_done: bool) -> str:
    example show(new_list(), False) == "nothing to do"
    example show(add_task(new_list(), "buy milk").todos, False) == "[ ] #1 buy milk"
    property done_tasks_are_hidden(title: Title):
        show(complete(add_task(new_list(), title).todos, 1).todos, False) == "nothing to do"
    property done_tasks_shown_with_all(title: Title):
        show(complete(add_task(new_list(), title).todos, 1).todos, True) == "[x] #1 " + title
    note "join the lines with a single newline, no trailing newline"
```

## Storage

The list is loaded at the start of every command and saved after every
change. The store holds one whole [[TodoList]]; there is no partial update.

```wh
capability Store:
    def load() -> TodoList raises StoreError
    def save(todos: TodoList) -> unit raises StoreError
```

On disk the list is a small JSON document, which makes it easy to inspect,
back up or edit by hand. A missing file is an empty list, so the first
`todo add` just works.

```wh
@pattern(Adapter)
adapter JsonFileStore implements Store:
    path: str
    note "an empty `path` means `todo.json` in the current directory"
    note "load: a missing file is new_list(); unreadable JSON or a list that breaks its invariants is StoreError.Corrupt"
    note "save: write to `<path>.tmp` and rename it over `path`, so a crash never leaves half a file"

depends serde_json = "1"
```

For tests the list lives in memory and starts out empty.

```wh
adapter MemoryStore implements Store:
    pattern InMemory
    note "keep the list in a Mutex<TodoList> that starts as new_list()"
```

Loading and committing are the only two steps that touch the store. A
commit saves the changed list and passes its message on.

```wh
def load_list() -> TodoList raises StoreError uses Store:
    note "delegate to Store.load"

def commit(own change: Change) -> str raises StoreError uses Store:
    """Save `change.todos` and return `change.message`."""
    ensures result == change.message
```
