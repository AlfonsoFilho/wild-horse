# Notes from writing `examples/todo` and `examples/pong`

Both examples check clean (`wh check --lints` prints no diagnostics). They generate, build, and pass
their generated tests against the hand-written holes in `examples/<name>/reference_impl/`:

```bash
cargo run -p wh-cli -- check examples/todo --lints
cargo run -p wh-cli -- gen examples/todo --impl-root examples/todo/reference_impl --out target/wh-examples/todo
cd target/wh-examples/todo && CARGO_TARGET_DIR=../target cargo test   # 28 passed
```

The pong run is the same with `pong` in place of `todo`: 25 tests pass. Both binaries were also run by hand:

- `todo` was run against a temporary `TODO_FILE` (add, done, list --all, remove, clear-done). Usage errors and
  NotFound errors exit with code 1. A hand-edited file that breaks an invariant gives `StoreError.Corrupt`.
- `pong` was run in a pseudo-terminal with `script`, a 60x16 window and scripted keys `ww` then `q`. The
  paddle moved, the game quit, the alternate screen and cursor were restored, `stty` showed
  `icanon echo` again, and the final score was printed.
- Run without a tty, `pong` exits 1 with `error: Terminal(Io(...))`.

I mutated `step` in a scratch copy (paddles no longer clamped, `Quit` ignored). The mutants were caught
by the `Game` invariants and by the `ensures` clauses. Nothing was written to `examples/*/impl/`.

## Status (fixed in the main session)

- Fixed: 1 and 11 (integer spec arithmetic is now mathematical, in `i128`), 2 (unit-only enums derive `Copy`), 3, 4, 5 for refined types (`serde(try_from/into)` through `parse`), 6 (`wh_check` recurses into nested structs, lists and options), 7 (readable `Display` for error enums), 8 (step and app failures log at `debug`; the `error:` line is printed once), 9 (docstrings on adapters and capabilities), 10, 12 (`[[module]]` and `[[Adapter.method]]` resolve), 13 (`trace <level>` at the top of a flow body, `trace off` drops all step spans), 14 (range refinements use range strategies, `duration` has a real strategy), 16 (nested calls in flow arguments are steps), 17 (spec §4 now describes the generated API), 18.
- The workarounds were reverted: todo's `next_id == todos.next_id + 1`, pong's sum-of-scores and `result.height + 2 == size.rows`, and pong's `@derive(Copy)` lines. Both examples still pass.
- Open: 5 for structs with invariants (deserialization doesn't run `wh_check`); 14 for structs whose invariants relate fields (strategies still generate fields independently); 15 is by design for M1 (only `map`, `filter` and `each` exist, and the checker reports anything else as an unknown function).

---

Below, every bug or gap I hit, most important first. Every item marked "still reproduces" was re-checked on
2026-10-22 against the crates as they were then, using the minimal project shown in the first item.

---

## Codegen bugs (generated crate does not compile)

### 1. Sized-int (or refined-int) `+ literal` in specs is cast to `i128` on one side only

Still reproduces. Every comparison where one side is `<sized int field> + <literal>` fails to compile.

```wh
type Id = u32 where self >= 1
struct Box:
    n: Id
    k: u16
def bump(b: Box, d: Dir) -> Box:
    ensures result.n == b.n + 1
    ensures result.k == b.k + 2
```

Generated:

```rust
if !((result.clone().n.into_inner() == (((b.clone().n.into_inner()) as i128) + ((1i64) as i128)))) {
//                                     ^ expected `u32`, found `i128`  /  can't compare `u32` with `i128`
if !((result.clone().k == (((b.clone().k) as i128) + ((2i64) as i128)))) {
```

Hit in:
- todo `add_task`: `ensures result.todos.next_id == todos.next_id + 1`
- pong `step`: `... <= game.left_score + game.right_score + 1` (u8)
- pong `fit_game`: `result.height + 2 == size.rows` (u16)
- pong `render`: `result.height == game.height + 2` (u16)

Expected: either cast both sides of the comparison to the common type (`i128`), or keep the
arithmetic in the operand type. Spec arithmetic should really be mathematical (see 11).

Workarounds in the examples:
- todo: `ensures result.todos.next_id > todos.next_id` (a weaker contract).
- pong: rewritten with subtraction, e.g. `size.rows - result.height == 2`,
  `result.left_score - game.left_score + result.right_score - game.right_score <= 1`.

`len(x) + 1` works, because both sides are `i64`.

### 2. Unit-only enums are passed by value as `Copy` but do not derive `Copy`

Still reproduces. `Model::type_derives_copy` returns true for an enum whose variants are all unit.
`Names::param_ty` therefore passes it by value, and flows dereference it with `*d`. But
`ItemGen::enum_` / `derive_list` never emit `#[derive(Copy)]` for such enums.

```wh
enum Dir:
    Up
    Down
def bump(b: Box, d: Dir) -> Box:
    ensures d != Dir.Up or result.k >= b.k
```

This produces three errors:
- `error[E0382]: borrow of moved value: d`, because the wrapper passes `d` into the hole and then
  uses it in `ensures`.
- `error[E0382]: use of moved value` in property tests that use the value twice.
- `error[E0507]: cannot move out of *d which is behind a shared reference`, in a flow `match` arm
  binding (`Cmd::Go { d: d } => ... bump(b, *d)`).

Expected: emit `#[derive(Copy)]` whenever `type_derives_copy` is true, for enums and also for
refined types. Refined types already get it.

Workaround: `@derive(Copy)` on `Input` and `Side` in `pong/src/game.wh.md`. The todo example's
`Status` is never passed as a parameter, so it did not hit this.

## Codegen warnings in generated code

### 3. Non-shorthand field patterns in flow `match`

Still reproduces. For record variants, the flow `match` emits `Command::Add { title: title } => {`. That
triggers `warning: the title: in this pattern is redundant` (`non_shorthand_field_patterns`), four times
in todo. Either emit the shorthand when the binding has the same name, or add
`non_shorthand_field_patterns` to the module's `#![allow(...)]`.

### 4. A flow ending in `return x` emits `Ok(unreachable!())` after it

Still reproduces. Pong `tick` ends with `return next`. The generated code is:

```rust
    return Ok(next.clone());
    Ok(unreachable!())
}
```

This gives `warning: unreachable expression` and `warning: unreachable call`. Don't emit the trailing
expression when the last statement is a `return`.

## Semantics / correctness gaps

### 5. `@derive(Deserialize)` on a refined type skips the refinement

The refined newtype derives plain `serde::Deserialize`, so `{"next_id": 0}` deserializes into
`TaskId(0)` although `TaskId = u32 where self >= 1`. Expected:
`#[serde(try_from = "<base>", into = "<base>")]`, or a hand-written `Deserialize` that calls `parse`.
Struct invariants are also not checked after deserializing.

Workaround: the reference `JsonFileStore.load` re-checks every `TaskId`/`Title` with `check()` and calls
`TodoList::wh_check()`.

### 6. Invariants of structs nested inside a returned value are not checked

The hole wrapper checks `wh_check()` only when the return type itself has invariants. `add_task`
returns `Change { todos: TodoList, message }`. `TodoList` has invariants (unique ids, ids below
`next_id`), but `Change` has none, so a broken `TodoList` inside a `Change` passes unnoticed.
Expected: check invariants of struct-typed fields recursively (or of every invariant-carrying value
reachable from the result).

### 7. Error `Display` is just `Debug`, so CLI errors are unreadable

`impl Display for E { write!(f, "{self:?}") }`. A usage error therefore prints as
`error: Usage("unknown command: frob\nusage: todo add <title> | ...")`, with a literal `\n` and quotes.

Suggestions:
- For variants whose single payload is `str`, display the payload. Wrapped errors (`from`) would display
  their inner error.
- Or allow a message per variant, e.g. `Usage(str) "usage error: {0}"`, or a `note`.

### 8. Duplicate error output in apps

`main` does `tracing::error!(..., "app failed")` and then `eprintln!("error: {e}")`. At any log
level ≥ `error`, every failure is printed twice.

With `level="warn"` it gets worse: each failing step also logs `step failed` at `warn`, once per flow level.
So a simple `todo done 9` printed three log lines plus the `error:` line.

I set `log level="error"` in todo, which still leaves the duplicated `app failed` line. Suggestions:
- Log step failures at `debug`; the error is reported at the top anyway.
- Skip the `error!` event when the console layer would print it next to the `eprintln!`.

## Checker / language gaps

### 9. Adapter bodies don't accept a docstring

```wh
adapter StdConsole implements Console:
    """Writes each line to standard output."""
```

gives `error: unknown clause """Writes each line to standard output."""`. Spec §2 says a docstring as
the first line of a definition body is allowed. The grammar in §14 lists
`adapter ... block(field | clause)`, without a docstring. `def` accepts docstrings.

Workaround: put the adapter in its own block with a prose paragraph before it.

### 10. Prose before a block attaches to `depends`, not to the first definition

```wh
depends serde_json = "1"

@pattern(Adapter)
adapter JsonFileStore implements Store:
```

With the prose paragraph above this block, `JsonFileStore` gets the lint
`JsonFileStore has no prose or docstring`. `depends` is not a definition, so §2 says the prose should
attach to `JsonFileStore`.

Workaround: put `depends` after the adapter, in the same block.

### 11. Spec arithmetic on sized ints can overflow or underflow

Spec arithmetic stays in the operand type, so `a - b` on `u8` underflows and panics in debug when
`b > a`. My workaround for item 1 (`result.left_score - game.left_score + ...`) relies on the earlier
`ensures result.left_score >= game.left_score` panicking first.

§10 says spec expressions are "Python-like". Python arithmetic doesn't overflow, so I would expect
`i128` or checked arithmetic for all integer spec arithmetic.

### 12. `[[module]]` and `[[Adapter.method]]` references don't resolve

- `[[tasks]]`, meant as a link to the module itself, gives `warning: reference to unknown definition tasks`.
- `[[CrosstermTerminal.enter]]` gives the same warning, although `main.CrosstermTerminal.enter` is a node
  id in tracing. Capability methods are presumably the same.

Expected: modules and capability/adapter methods are linkable. I rephrased the prose around them.

### 13. No `trace` level for flows

`trace off` works on `def`s and adapters, which is useful for pong's per-frame defs. But the per-frame
flow `tick` always gets an `info` span plus a `debug` `wh.step` span per step, 60 times a second. The
flow grammar has no clause position for `trace`.

Suggestion: allow clauses (at least `trace`) at the top of a flow body, like a docstring.

### 14. Property strategies that are useless in practice (avoided, not hit)

- A property parameter of type `duration` gets `Just(Default::default())`, so it is always `0s`.
- A numeric refinement such as `type T = u32 where 1 <= self <= 1000` gets `any::<u32>()` filtered by
  `parse`, which rejects about 99.99998% of cases and would fail with "too many rejects".
- A struct with invariants that depend on each other (pong's `Game`: ball inside the field size) gets
  its fields generated independently and then filtered by `wh_check`. That essentially never passes.
- Generated lists are 0–8 long.

Because of these, pong's laws are stated over `simulate(new_game(...), input, frames: u8, dt)`
instead of over arbitrary `Game` values. That is fine, and arguably more readable. But range
refinements should map to range strategies (`1u32..=1000`), and `duration` should get e.g.
`(0u64..1000).prop_map(Duration::from_millis)`.

The `given x from <expr>` form emits the expression as the strategy itself, and WH has no type for
strategies, so I did not try it.

### 15. Flow combinators other than `map`/`filter` look unimplemented (from reading, not hit)

`FlowGen::step` (items.rs ~870) handles combinator steps `map` and `filter`. Every other label
(`fold(init, f)`, `flat_map`, `retry`, ...) falls into the `_` arm, which emits a `for x in ... { f(x)?; }`
loop. That is right for `each` and wrong for `fold`.

I designed pong's input handling to need no `fold`: one `Input` per tick. A real `fold` would let
`tick` apply all pending key events per frame.

### 16. Nested calls inside flow arguments are not steps

`show(render(next))` in pong's `tick` produces `show(cap_terminal, &render(&next.clone()))`. Only `show`
gets a `wh.step` span (`main.tick#show[0]`); `render` has no step id. I'm not sure whether the graph shows
it as a node. If nested calls are meant to be steps, they need ids and spans. Otherwise the checker
could suggest binding them.

### 17. Spec deviation: structs with invariants keep `pub` fields

§4 says a struct with an `invariant` gets private fields, a validating `new` and getters. The generated
structs (`TodoList`, `Game`, and signup's `User`) have `pub` fields plus `new` and `wh_check`. Holes can
therefore build invalid values without going through `new`. The return-value check catches this for
direct returns only (see 6).

This is convenient for holes; either update the spec or the codegen.

## CLI

### 18. `wh gen --impl-root <dir>` fails when `<dir>` does not exist, without naming the path

`opts.impl_root = r.canonicalize()?` gives `Error: No such file or directory (os error 2)`. Expected:
create the directory (gen writes stub files there anyway), or name the missing path in the error.

### 19. (transient) the workspace failed to load while `crates/wh-synth` had no target

`cargo run -p wh-cli` failed for everyone with "no targets specified in the manifest" for
`crates/wh-synth/Cargo.toml` while that crate was being created. This was transient; I used a copy of
the previously built `target/debug/wh` until it was fixed. Creating `src/lib.rs` together with the
manifest avoids this.
