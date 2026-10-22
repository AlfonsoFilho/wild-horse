# Wild Horse language specification (v0)

## 1. Design principles
1. **Declarative, not imperative.** A program states *what exists and how data flows*, never *how* a function computes. There are no statements, loops, or assignments in function bodies — only signatures, contracts, specs and patterns.
2. **Pure by default.** Every `def` is pure unless it declares `uses <Capability>`. Effects are explicit and typed.
3. **Python-like surface.** Indentation blocks, `def`, `:`, `#` comments, `True/False/None`, `and/or/not`, `[]` generics, `_private` names, decorators.
4. **Rust semantics underneath.** Every construct has a deterministic Rust mapping (§12). Nothing is expressible that Rust can't represent.
5. **Literate first.** Prose is part of the program: it is attached to definitions, checked for broken references, and given to the agent as intent.
6. **Order-independent.** Definitions may appear in any order and any block; the document order serves the reader, not the compiler.
7. **Every definition is a graph node.** Anything that can be written can be shown, selected and edited in the graph.

## 2. Document structure
- A source file is CommonMark with extension `.wh.md`. **One file = one module**; module name = file stem (`accounts.wh.md` → `accounts`), overridable in YAML front matter (`module: accounts.core`).
- Code lives in fenced blocks with info string `wh`. Other fenced blocks (`rust`, `text`, …) are ignored by the compiler and rendered as documentation.
- **Headings** (`##`, `###`, …) create *sections*; each section is a cluster in the graph, nested by heading level. New nodes created in a graph cluster are inserted in that section.
- **Prose attachment**: the prose paragraphs immediately preceding a `wh` block (up to the previous block or heading) attach to the block's first definition. A string docstring as the first line of a definition body is also allowed; both are concatenated.
- **References**: `` [[name]] `` or `` [[module.name]] `` in prose links to a node; unresolved references are warnings. Rendered as clickable chips.
- Multiple definitions per block are allowed; a block may also be empty (placeholder).

Example document:
````markdown
---
module: signup
---
# Signup

## Domain

An email address as typed by the user. We only check its shape; deliverability
is checked later by [[send_confirmation]].

```wh
type Email = str where matches(self, r"^[^@\s]+@[^@\s]+$")
type Age = u8 where self <= 150
```
````

## 3. Lexical structure
- UTF-8. **Indentation is significant** (Python rules): 4 spaces per level, tabs are an error; the lexer emits `INDENT`/`DEDENT`/`NEWLINE`. Newlines inside `()`, `[]`, `{}` are ignored (implicit line joining). A line starting with `|>` continues the previous expression.
- Comments: `#` to end of line (inside `wh` blocks only — Markdown headings live outside blocks, so there is no clash).
- Identifiers: `snake_case` for values, functions, flows, fields; `PascalCase` for types, variants, capabilities, patterns. Violations are errors (they drive Rust mapping). Leading `_` = module-private.
- Literals: integers `1_000`, `0xFF`, `0b1010` (optional suffix `1u8`); floats `3.14`, `1e-3`; strings `"…"` with escapes, raw `r"…"`, triple-quoted `"""…"""` (docstrings); bytes `b"…"`; durations `10ns`, `50us`, `16ms`, `5s`; `True`, `False`, `None`.
- **Hard keywords** (reserved everywhere): `import from as type alias struct enum capability adapter implements def async flow machine app test extern pure uses raises requires ensures invariant example property where forall exists in is and or not if else lambda return own mut True False None`.
- **Soft keywords** (keywords only in their clause/statement position, usable as identifiers elsewhere — like Python's `match`): `match case on initial emit ignore target provide entry depends given expect bench mutate pattern note complexity optimize budget alloc unsafe no_panic key sensitive trace log when result old self`.

## 4. Types
**Built-ins** (Python names map to sensible Rust defaults; sized forms available):

| WH | Rust | WH | Rust |
|---|---|---|---|
| `bool` | `bool` | `list[T]` | `Vec<T>` |
| `int` | `i64` | `dict[K, V]` | `HashMap<K, V>` |
| `i8…i128`, `u8…u128`, `usize` | same | `sorted_dict[K, V]` | `BTreeMap<K, V>` |
| `float` | `f64` | `set[T]` / `sorted_set[T]` | `HashSet` / `BTreeSet` |
| `f32` | `f32` | `tuple[A, B]` | `(A, B)` |
| `str` | `String` | `array[T, N]` | `[T; N]` |
| `char` | `char` | `T \| None` | `Option<T>` |
| `bytes` | `Vec<u8>` | `(A, B) -> R` | `impl Fn(A, B) -> R` |
| `unit` | `()` | `Stream[T]` | `impl Stream<Item = T>` (async) |
| `duration`, `instant` | `std::time::{Duration, Instant}` | | |

- In **borrowed parameter position** the mapping uses the slice form: `str` → `&str`, `list[T]` → `&[T]`, `bytes` → `&[u8]`; other types → `&T` (by value if `Copy`).
- Function types and `Stream[T]` are allowed only in parameter and return positions in v0 (not as struct fields).
- Unions are only `T | None`; anything else must be an `enum`.

**Alias** (no new type): `alias UserId = u64`

**Refined newtype** (new type, checked at construction; `self` = the value):
```wh
type Email = str where matches(self, r"^[^@\s]+@[^@\s]+$")
type Percent = float where 0.0 <= self <= 100.0
type NonEmpty[T] = list[T] where len(self) > 0
```
Construction returns `T raises InvalidValue` (`Email.parse(s)`); refinements also generate proptest strategies. A refined type exposes the read-only operations of its base type (Rust `Deref` to the base) and compares equal with base values (`PartialEq<base>`); operations return the **base** type (`e.lower()` is a `str`, not an `Email`). In spec expressions a literal is accepted where a refined type is expected and is validated when the spec is compiled (`normalize_email("A@B.com")` → error at check time if the literal violates `Email`'s predicate).

**Struct** (product type):
```wh
@derive(Hash, Serialize)
struct User:
    key id: UserId                        # `key`: this field identifies a User
    email: Email
    age: Age
    guardian: UserId | None = None        # default value
    invariant self.age >= 13 or self.guardian is not None
```
- Arithmetic on integers inside spec expressions is mathematical (evaluated in `i128`), so contracts never overflow.
- `sensitive` marks personal or secret data (`sensitive email: Email`): its `Debug` output and every trace show `<redacted>` instead of the value. The same marker exists for parameters (`def login(user: str, sensitive password: str)`: the hole wrapper's span records `password = <redacted>`) and, as the decorator `@sensitive`, for refined types, structs and enums (`@sensitive type Token = str`): every value of the type prints as `<redacted>` in `Debug`, so in traces, logs, contract and invariant failures, test failures and error messages (`Display` is kept for deliberate output, but generated error messages show a sensitive payload through its redacted `Debug`).
- `key` marks the identifying field; its type (`UserId`) then denotes a *reference to User* wherever it appears (used by the data model view and by `Repository`).
- Refinements and struct invariants are **always** checked at construction (they define valid data, at the parse boundary); `requires`/`ensures` contracts are debug-only by default. Every struct gets `wh_check(&self) -> Result<(), InvalidValue>`, which checks its invariants and, recursively, those of the structs it contains; a struct with an `invariant` also gets a validating `new(...) -> Result<Self, InvalidValue>`. Fields stay public; contract wrappers check the invariants of every returned value, and of every `mut` argument after the call, in debug builds (and with the `wh-contracts` feature in release). Fields are public by design, not private behind getters: hole implementations are Rust child modules of their WH module, so private fields would not stop them from building invalid values anyway, while getters would make spec expressions, flows and holes in other modules unable to build or destructure values. Invalid values are caught instead at every boundary the generated code controls: `new`, deserialization, hole results and `mut` arguments.
- Deserialization is a parse boundary too: a refined type with `@derive(Deserialize)` deserializes its base value and goes through `parse`; a (non-generic) struct with invariants deserializes into a hidden shadow struct with the same fields and is converted with `TryFrom`, which runs `wh_check` (`#[serde(try_from = "WhDeUser")]`), so invalid data is a deserialization error, never a value.
**Enum** (sum type; variants may be unit, tuple or record):
```wh
enum Event:
    Created(user: User)
    Deleted(UserId)
    Cleared
```
**Generics** use `[]` with optional bounds from the known trait set (`Eq Ord Hash Clone Copy Default Display Debug Serialize Deserialize Send Sync`): `struct Page[T: Clone]:`. Default derives on all data types: `Debug, Clone, PartialEq` (+ `Eq, Hash` when all fields allow it); `@derive(...)` adds more (codegen adds the needed crate, e.g. `serde`, automatically).

**Recursive types** (`struct Node: children: list[Node]`, `next: Node | None`) are allowed; codegen inserts `Box` where Rust needs indirection.

**Errors** are ordinary enums tagged `@error`; `from` declares automatic conversion used by `raises` propagation:
```wh
@error
enum RegisterError:
    Invalid(from ValidationError)
    TooYoung
    Storage(from DbError)
```

## 5. Functions (`def`)
A `def` declares a signature plus *clauses*. The body contains **no code** — it is a *hole* filled by the agent (or `extern`).
```wh
def normalize_email(e: Email) -> Email:
    """Canonical form used for uniqueness checks."""
    ensures result == e.lower()
    example normalize_email("A@B.com") == "a@b.com"
    property idempotent(x: Email):
        normalize_email(normalize_email(x)) == normalize_email(x)
    complexity O(n)
```
**Signature**
- Parameters: `name: Type`, optional default `= expr`, optionally `sensitive` (redacted in traces, §4). Passing mode: default is shared borrow (`&T`; by value for `Copy` types), `own x: T` takes ownership, `mut x: T` is a mutable borrow (the only way to model in-place updates; `old(x)` refers to the pre-call value in `ensures`).
- Return: `-> T`; `-> T raises E` ≡ Rust `Result<T, E>`; omitted ≡ `unit`.
- Effects: `uses Cap1, Cap2` is part of the **header** (same position for `def` and `flow`): `def save_user(u: User) -> UserId raises DbError uses Db:`.
- `async def` → `async fn`. Generic functions: `def max_by[T, K: Ord](items: list[T], key: (T) -> K) -> T | None:`.

**Clauses** (any order, each optional, repeatable where sensible):

| clause | meaning | enforced by |
|---|---|---|
| `pure` | explicit purity (default; documentation only) | checker |
| `requires <expr>` | precondition on params | wrapper `debug_assert!` + proptest filter |
| `ensures <expr>` | postcondition; may use `result`, `old(p)` | wrapper `debug_assert!` + every test |
| `raises E when <expr>` | error contract: must return `Err(E)` (or the named variant) whenever expr holds | wrapper check + every test |
| `example <bool-expr>` / `example <call> raises E` | concrete case | `#[test]` |
| `property name(x: T, …): <bool-expr>` (optionally `given x from gen_fn`) | universally quantified law | `proptest` |
| `bench name(n: 1_000..100_000): <call>` | benchmark with input sweep | `criterion` |
| `mutate score >= 0.8` | mutation score threshold for this impl | `cargo-mutants` |
| `complexity O(...)` | expected complexity | growth-curve fit on auto-bench |
| `optimize speed\|latency\|throughput\|memory\|size` | target metric for the optimization stage | optimization tournament |
| `budget 50us for n=10_000` | hard time budget at an input size | bench gate (release builds) |
| `alloc none` / `alloc bounded(n)` | heap-allocation limit per call | counting allocator in tests |
| `unsafe forbid` (default) / `unsafe allow` | permits `unsafe` in the impl | `forbid(unsafe_code)` / `miri` |
| `no_panic` | impl must not panic on valid inputs | property tests + `no-panic` link check |
| `pattern P[args]` (or `@pattern(P)`) | implementation pattern | pattern rules (§9) |
| `note "…"` | free-form guidance to the agent | prompt only |
| `trace off\|error\|warn\|info\|debug\|trace` | level of this node's tracing span (§15) | generated instrumentation |

**Extern** functions bind to existing Rust instead of an agent hole: `extern def sha256(data: bytes) -> array[u8, 32] from "sha2::Sha256::digest"` (the crate must be declared with `depends`). Contracts and specs still apply to them.

## 6. Capabilities and adapters (effects)
```wh
capability Db:
    def insert(own u: User) -> UserId raises DbError
    def find(id: UserId) -> User | None raises DbError

capability Clock:
    def now() -> instant

capability Mailer:
    async def send(to: Email, body: str) -> unit raises MailError

@pattern(Adapter)
adapter PostgresDb implements Db:
    url: str                                  # adapter configuration fields
    note "use sqlx with a connection pool"

adapter MemoryDb implements Db:               # test double, also agent-implemented
    pattern InMemory
```
- A capability → Rust trait with `&self` methods (adapters use interior mutability / pools, which also keeps capabilities shareable across `parallel` steps). `uses Db` → the generated fn takes `db: &impl Db` (monomorphized), so pure code never sees I/O and tests can substitute adapters.
- **Async coloring**: capability methods may be `async def`. Any def/flow that calls an async step is async (inferred for flows; must be declared `async def` for defs, else an error). An app whose entry is async gets a `tokio` runtime.
- An `adapter` → a struct + trait impl whose method bodies are agent holes; each method inherits the capability's contracts.
- Purity rule: a function without `uses` may only reference functions without `uses` (transitively). Capabilities required by a flow = union of its steps'.

## 7. Flows (dataflow composition)
Flows are **fully generated** (no agent): they are the program's wiring and the primary graph edges.

Pipeline form:
```wh
flow register(form: SignupForm) -> UserId raises RegisterError:
    form
    |> validate                 # SignupForm -> ValidForm raises ValidationError
    |> normalize_user           # ValidForm -> User
    |> check_age                # User -> User raises RegisterError
    |> save_user                # User -> UserId raises DbError, uses Db
```
DAG form (named bindings; each binding is a node instance, each argument an edge):
```wh
flow checkout(cart: Cart, user: User) -> Receipt raises CheckoutError:
    priced = price(cart)
    taxed  = apply_tax(priced, user.region)
    paid, shipped = parallel(charge(user.card, taxed.total), reserve(cart.items))
    return receipt(taxed, paid, shipped)
```
Routing:
```wh
flow handle(ev: Event) -> unit uses Mailer, Db:
    match ev:
        case Created(user): send_welcome(user)
        case Deleted(id): purge(id)
        case Cleared: return
```
Rules:
- Allowed in flow bodies: bindings, `|>`, calls to `def`/`flow`/constructors, field access, tuple destructuring, `match/case` on enums, `return`, and the combinators below. No arithmetic, loops or literals beyond constants — anything computed belongs in a `def`.
- **Combinators** (built in; a definition with the same name takes precedence). Each is one flow step (`module.flow#fold:add_line[0]`), and errors of the function propagate like any step's:
  - `xs |> map(f)`, `xs |> filter(p)`, `xs |> each(f)`: apply `f` to every element (collect the results / keep the elements where `p` holds / for effect only).
  - `xs |> fold(init, f)`: `f(acc, x)` for every element, starting from `init` (a constant, a name or a call); `f` takes `(A, T)` and returns `A`, the result is `A`.
  - `xs |> flat_map(f)`: `f` returns a list for each element; the result is their concatenation.
  - `x |> retry(f, times=n)` or `retry(f, times=n)`: call `f(x)` up to `n` times (an integer literal ≥ 1) until it returns `Ok`; `f` must raise, and the last error propagates. With `backoff=10ms` the attempts are separated by a doubling delay (10ms, 20ms, 40ms, …); without it they follow each other immediately. Each retried failure is logged at `debug` with its attempt number.
  - `a, b = parallel(f(x), g(y), …)`: two or more branches, each a call (with its nested calls), produce a tuple. In an async flow the branches run concurrently on the same task (joined); in a sync flow each branch runs on a scoped thread (`std::thread::scope`), so the capabilities a sync `parallel` shares — in that flow or in flows calling it — are taken as `&(impl Cap + Sync)`. All branches finish before the step completes; then the first failed branch's error propagates (in branch order). Branch spans are children of the `parallel` step's span.
- `x |> f` ≡ `f(x)`; `x |> f(a)` ≡ `f(x, a)` (piped value is the first argument).
- Errors propagate automatically (`?`): each step's `raises E` must convert into the flow's error via a `from` variant (or be the same type), else a type error at that edge.
- `match` arms must produce the same type; `return` with no value only in `unit` flows.
- Step types must compose; a mismatch is reported *on the edge* (and in the text at the step).

## 8. State machines
A `machine` declares states and transitions; it is **fully generated** (no agent).
```wh
enum DoorEvent:
    Open
    Close
    Lock(str)
    Unlock(str)

enum DoorEffect:
    Unlocked
    Alarm

@pattern(StateMachine)
machine Door(event: DoorEvent) -> DoorEffect:
    """A door with a code lock; a wrong code raises the alarm."""
    Open           on Close                  -> Closed
    Closed         on Open                   -> Open
    Closed         on Lock(code)             -> Locked(code)
    Locked(code)   on Unlock(c) if c == code -> Closed       emit Unlocked
    Locked(code)   on Unlock(c)              -> Locked(code) emit Alarm
    ignore Open on Open
    ignore Open on Lock
    ignore Open on Unlock
    ignore Closed on Close
    ignore Closed on Unlock
    ignore Locked on _
    initial Closed
```
- **Header**: `machine Name(event: E) -> Effect`. The event type must be an enum; the effect type is optional (without it, transitions cannot `emit`).
- **Transitions**: `State on Event [if guard] -> Target [emit effect]`. A source state or an event may bind its payload by position (`Locked(code)`, `Unlock(c)`) or be `_` (any state, any event). The guard is a pure `bool` spec expression over the bound names; the target may build a payload from them (`Locked(code)`); `emit` gives a value of the effect type. Transitions are tried **in order**: the first whose state and event match and whose guard holds fires. If none fires (an `ignore`d pair, or every matching guard is false), the state stays and nothing is emitted.
- **States are inferred** as an enum named after the machine: every name used as a source, target or `initial` is a state; a state's payload types are inferred from the values the transitions (and `initial`) build it from (`Locked(code)` with `code` bound by `Lock(code)` → `Locked(str)`).
- `ignore State on Event` (either side may be `_`) marks a pair as deliberately left alone; `initial State` is required; `trace <level>` sets the level of the per-transition events (default `debug`; `off` removes them); a docstring is allowed.
- **Checker rules**: unknown events, unknown states in `ignore`, payload arities that disagree, payload types that cannot be inferred, non-`bool` or impure guards, effects of the wrong type, a missing `initial`, **unhandled `(state, event)` pairs** (every reachable state must handle every event by a transition or an `ignore`; a guarded transition counts as handling its pair) and **unreachable states** (no path of transitions from `initial`) are errors.
- **Generated** (`machine Door`): `enum Door { Open, Closed, Locked(String) }` with the default derives, and on it `STATES` (state names), `TRANSITIONS` (the transition table: id, from, event, to, guard and emit source, `_` for wildcards), `IGNORED`, `initial()`, `name(&self)`, `event_name(&DoorEvent)` and the total `Door::step(state: Door, event: DoorEvent) -> (Door, Option<DoorEffect>)`. Each transition logs a tracing event with `wh.node = "door.Door#Locked.Unlock[0]"` (`module.Machine#From.Event[k]`, `k` counting transitions of the same pair), `from`, `event` and `to`; unmatched pairs log at `trace`.
- **Generated tests**: *totality* — a property over every state and event (small values, see §9) checks that the pair is handled, and that `step` lands in a declared state the table allows from there (or stays put, emitting nothing); *reachability* — a deterministic test walks a shortest witness path from `initial` to every state (unguarded transitions preferred), sampling events of the needed variants until each transition fires.
- **In specs** a machine is a type: `Door.Locked("1")`, `d is Locked`, `Door.initial()` and `Door.step(state, event)` (a `tuple[Door, DoorEffect | None]`) are available, e.g. `example Door.step(Door.Closed, DoorEvent.Lock("12")) == (Door.Locked("12"), None)`.
- Rendered in the graph as a state diagram.

## 9. Design patterns (lowest abstraction level)
`pattern` attaches an implementation strategy to a node. Each pattern in the catalog defines: (a) **shape rules** checked statically, (b) **codegen contribution**, (c) **agent guidance** injected into the prompt, (d) **auto specs** it adds. v0 catalog:

| pattern | applies to | shape rule (checked) | generated (no agent) |
|---|---|---|---|
| `Transform` | def | pure, exactly one parameter | totality property `<def>__transform_total`: for every generated input satisfying `requires`, the call (through the contract-checking wrapper) returns a value or a declared error without panicking or breaking a contract |
| `Pipeline` | flow | a single `\|>` chain | per-step `wh.step` spans (as for every flow) |
| `StateMachine` | machine | the machine rules of §8 | the code and the totality/reachability tests of §8 (generated for every machine; the pattern documents intent) |
| `Repository[T]` | capability or adapter | an insert-style method taking one `T` (`insert`, `add`, `save`, `put`, `create`, `store`, or `insert_…`) and a find-style method (`find`, `get`, `load`, `fetch`, `lookup`, `read`, or `find_…`) taking the key and returning `T \| None`; the key is what insert returns, or `T`'s `key` field. `T` defaults to the insert parameter's type | round-trip property `<adapter>__repository_roundtrip` for every adapter of the capability (or the tagged adapter): on a fresh adapter, inserting `x` succeeds and finding its key returns `x` unchanged — `find(insert(x)) == x` |
| `Adapter` | adapter | implements a capability | contract tests `<adapter>__contract__<method>` for every capability method with contracts: on a fresh adapter, `requires` are assumptions, then `raises E when c` and `ensures` are checked on the result. The tests come from the capability, so every adapter tagged `Adapter` or `InMemory` is held to the same ones |
| `InMemory` | adapter | — | used as default test double; gets the same contract tests as `Adapter` |
| `Builder[T]` | struct | `T`, if written, is the struct itself | `T::builder() -> TBuilder`: one setter per field (`with_build` for a field named `build`), and `build() -> Result<T, InvalidValue>` filling in field defaults, reporting a missing field (`field \`x\` is required`) and checking the invariants |
| `Strategy` | capability | exactly one method | — |
| `Memoize` | def | pure, not generic, not async, no `mut` parameter, every parameter hashable | the wrapper caches results (including errors) per argument tuple in a process-wide map, checked after `requires`; a hit skips the hole. The cache holds at most `wh_rt::MEMO_CAPACITY` (4096) entries and is cleared when full |
| `GameLoop` | def | — | fixed-timestep loop guidance (prompt only) |

Tests that patterns and machines add, and adapter contract tests, need values the test can build: an adapter is constructed with `new` from its fields' defaults (an adapter with a field without default gets no generated tests; when its capability has contracts, the checker warns). An adapter that needs an external system reads its configuration from the environment through such a default, e.g. `url: str = env("DATABASE_URL")`; `cargo test` inherits the environment. These tests are named with the adapter's node-id prefix (`wh__signup__MemoryDb__contract__insert`, `wh__signup__MemoryDb__repository_roundtrip`) and mapped to the adapter's node, so the synthesis gate runs them when it verifies that adapter, and parameters whose types have no strategy (functions, streams, capabilities) are skipped. Their inputs use **small values** — integers and floats within ±1000 (intersected with the type's range and refinement bounds) — because they check behaviour, not performance; user-written properties use the full ranges.

**Strategies for structs with invariants.** A struct's fields are generated from their types and the invariants then filter the value. Where an invariant bounds a numeric field (or a numeric field of a struct field) by literals or by other fields — `0.0 <= self.ball.x < self.width`, `self.left_score <= self.target`, `self.n == len(self.items)` — that field is instead *computed* from the fields it depends on (in dependency order, from a uniform seed within the bounds), so related fields are generated together instead of being rejected almost always. Invariants outside this shape (quantifiers, uniqueness, non-numeric relations) still filter. There is deliberately no fallback to building values through a constructor `def`: constructors are holes, so strategies built on them would panic before synthesis and would hide a constructor's bugs from the properties meant to find them.

Patterns are user-extensible later via `pattern` definition files (rules + prompt template + codegen template).

## 10. Specification expressions (contracts, examples, properties)
A pure, typed, Python-like expression language compiled to Rust predicates:
- Arithmetic `+ - * / // % **`, comparison with chaining `0 <= x < n`, `and or not`, `in`, `not in`, `is None`, `is not None`, conditional `a if c else b`.
- Field access, indexing `xs[i]`, slicing `xs[a:b]`, tuple/struct/variant construction `User(id=1, email=e, age=30)`, enum tests `ev is Created`.
- Comprehensions and quantifiers: `[f(x) for x in xs if p(x)]`, `all(p(x) for x in xs)`, `any(...)`, `forall x in xs: p(x)`, `exists x in xs: p(x)`, `sum`, `len`, `min`, `max`, `sorted`, `lambda x: …`.
- Special names: `result` (return value, in `ensures`), `old(e)` (pre-state, for `mut` params), `self` (in refinements/invariants).
- String/collection functions from a fixed std-spec library (`lower` → `to_lowercase`, `upper`, `strip`, `startswith`, `contains`, `matches` → `regex`, `keys`, `values`, …), each with a defined Rust mapping (Unicode semantics, as in Python).
- May call any pure `def` (calls to holes are allowed — that is how laws like idempotence are stated). Must type-check as `bool` where a condition is expected.
- Machines (§8) offer `M.State(...)`, `M.initial()` and `M.step(state, event)`.

## 11. Modules, apps and targets
```wh
from accounts import User, UserId
import billing                      # use as billing.charge
```
- Visibility: names starting with `_` are module-private; everything else is public.
- **Dependencies** on Rust crates are declared, never implicit (front matter applies to the module, `app` to the binary):
```wh
depends sqlx = "0.8" features ["postgres", "runtime-tokio"]
depends sha2 = "0.10"
```
  The compiler agent sees only declared crates; a request for a new one becomes an author proposal needing user approval.
- **App** (one per binary; usually in `main.wh.md`):
```wh
app signup_service:
    target native(cli)              # or server(axum), web(wasm) (below). Later: game(bevy), embedded(thumbv7em-none-eabihf)
    log level="info" format="pretty"   # §15
    provide Db = PostgresDb(url=env("DATABASE_URL"))
    provide Clock = SystemClock
    entry main(args: list[str]) -> unit raises AppError:
        args |> parse_cli |> register |> report
```
- `provide` binds capabilities to adapters for this app; tests use `test … given Db = MemoryDb()`.
- `entry` is a flow that becomes the binary's `main` (or the target's entry point). Its error type must implement `Display`: an `Err` prints the error and exits with code 1.

**Target profiles.** An app has exactly one `target`; one project may hold several apps (one per target), except that a `web` app must be its project's only app (it turns the generated library into a wasm module). Each target is a *profile* (`wh_targets::TargetProfile`) that fixes the shape of the generated crate, the dependencies it adds, the entry point, the cargo invocations that build and test it, and how it is run:

| target | crate and entry point | dependencies added | build | tests | run |
|---|---|---|---|---|---|
| `native(cli)` | a `[[bin]]` whose `main` calls the `entry` flow with the command line | — | `cargo build` | `cargo test` | the binary, with the user's arguments |
| `server(axum)` | a `[[bin]]` with `#[tokio::main]` serving the app's router | `axum`, `tokio`, `serde`, `serde_json` (tests: `tower`, `serde_urlencoded`) | `cargo build` | `cargo test` (routes are requested in-process) | the binary; it listens on `$WH_ADDR` (default: `listen`, else `127.0.0.1:3000`), prints `listening on http://<addr>` on stderr, and stops gracefully on Ctrl-C/SIGTERM |
| `web(wasm)` | the library as `crate-type = ["cdylib", "rlib"]` with `#[wasm_bindgen]` exports; no binary; `web/index.html` | `wasm-bindgen`, `serde`, `serde_json` | `cargo build --lib --target wasm32-unknown-unknown`, then `wasm-bindgen --target web --out-dir web/pkg` when that CLI is installed (it is detected, never installed) | `cargo test` on the host, through the exports' Rust entry points | open `web/index.html` (exports are on `window.wh`) |

Adapters say where they run with `@target(native, server, web)`. Without it, an in-memory adapter (`pattern InMemory`) runs on every target and any other adapter only on `native` and `server` (it may use files, sockets, threads or a terminal). Providing an adapter that is not available on the app's target is an error.

**`server(axum)`**:
```wh
app signup_api:
    target server(axum)
    listen "127.0.0.1:3000"
    provide Db = MemoryDb()
    route POST "/signup" -> register
    route GET "/users/{id}" -> find_user
    status ValidationError = 400
    status RegisterError.TooYoung = 422
    status DbError.Duplicate = 409
```
- `route METHOD "/path" -> target` maps `GET`, `POST`, `PUT`, `PATCH` or `DELETE` on a path to a flow or def. Its parameters come from the request: a `{name}` path segment fills the parameter of that name; for `GET`/`DELETE` the others come from the query string (`?prefix=A&limit=3`); for the other methods they come from the JSON body — the value itself when one parameter remains, otherwise an object keyed by parameter name. Path and query parameters must be scalars (numbers, strings, booleans, refined scalars, unit-only enums, or `| None` of these). The response is the result as JSON with `200`, or `204 No Content` for `unit`.
- Every type that crosses a route (parameters and result, transitively) gets `Serialize`/`Deserialize` without `@derive`; reading a request runs refinements and invariants, so an invalid value is rejected by deserialization (`422`, with the violated rule) before the flow runs.
- `status E.Variant = code` and `status E = code` map errors to HTTP statuses. A variant takes its own rule, else its type's rule, else (for a `from` variant) the status of the error it wraps, else `500`. The error body is `{"error": "<Display of the error>", "kind": "<variant>"}`.
- Flows run on tokio's blocking pool (async flows driven by `block_on`), inside the request's span. A panic (e.g. a violated contract) answers `500 {"error": "internal error"}` and is logged.
- **Tracing**: a `wh.request` span per request at `info` carries `wh.node = "<app id>"`, the method, the path and the response status; the flow's own spans nest inside it.
- **Generated state**: `<App>Server` holds one field per provided capability (generic, so tests substitute adapters) and `<App>Server::new()` builds the provided adapters; `<app>_router(state)` returns the `axum::Router`.
- **Generated tests** `<app>__route__<METHOD>_<path>`, one per route: 16 deterministic sample inputs are sent through the router (in-process, no network) using the capabilities' in-memory adapters (or the provided ones) and must answer exactly what the flow returns when called directly on its own copy of those adapters — the same JSON, or the status of the same error. Routes with a body must also reject a malformed body with a `4xx`.

**`web(wasm)`**:
```wh
app calc_web:
    target web(wasm)
    provide Notes = MemoryNotes()
    export divide, shout
    export remember
```
- `export f, g` exports flows or defs to JavaScript. Each export takes JSON (the argument, or an object keyed by parameter name; no input for a function without parameters) and returns JSON, or throws a JS `Error` with the error's text. The generated `wh_export_<f>(input: &str) -> Result<String, String>` holds the logic (and is what the tests call); `#[wasm_bindgen(js_name = f)] wh_wasm_<f>` wraps it. Exported types get serde derives as for routes.
- The provided adapters live in a thread-local state (wasm is single-threaded), created on first use.
- **Rules**: every capability an export uses must be provided by an adapter available on `web`; exports cannot be `async` yet; an exported flow must not (directly or through the flows it calls) run a sync `parallel` or `retry … backoff` (both need threads).
- **Generated tests** `<app>__export__<f>`: 16 deterministic sample inputs through the JSON entry point must give what calling the function directly gives, on its own copy of the adapters, and malformed JSON is rejected.
- Checking the wasm build needs `rustup target add wasm32-unknown-unknown`; the toolchain never installs targets or tools itself.

**Scenario tests** (module level, multi-step, with capability substitution):
```wh
test "minors need a guardian":
    given Db = MemoryDb()
    expect register(form(age=12)) raises RegisterError.TooYoung
    expect register(form(age=12, guardian=7)) is not None
```

## 12. Node identity and Rust mapping
- Node ids: `module.Name` (types, capabilities), `module.fn_name`, `module.flow_name`, `module.flow_name#binding` (flow step), `module.fn_name.example[i]` / `.property.name`. Ids are stable across edits; renames go through the `Rename` op.
- Mapping summary: module → Rust module `mod signup`; struct/enum → `pub struct/enum` with derives; refined type → newtype with private field + `parse`/`TryFrom`; capability → trait; adapter → struct + `impl Trait` with hole methods; def → public wrapper (contracts) + `imp::fn` hole in `impl/signup/normalize_email.rs`; flow → generated fn using `?` and `From`; machine → state enum + `STATES`/`TRANSITIONS`/`IGNORED` tables + `initial`, `name`, `event_name` and a total `step` (§8); app → the target profile's entry point: `src/bin/<app>.rs` wiring adapters for `native` (`main`) and `server` (router + `#[tokio::main]`), `#[wasm_bindgen]` exports in the library for `web` (§11); specs → `#[cfg(test)]` modules, `proptest!`, `benches/`.

## 13. Static semantics (checker rules, M1)
1. All names resolve; no duplicate definitions per module; naming conventions hold.
2. Types well-formed; generic bounds satisfied; directly self-containing structs without `list`/`| None`/collection indirection are an error (infinite size); `| None` recursion is boxed automatically.
3. Purity: no transitive path from a non-`uses` def to a `uses` def.
4. Flows: step types compose; error conversions exist; flow capabilities ⊆ declared `uses` (if declared) or inferred.
5. Spec expressions are pure, well-typed, use `result`/`old` only where valid.
6. Examples/properties reference only in-scope names; property parameter types have strategies (refined types, structs of strategies, or `given`).
7. Pattern shape rules (§9).
8. Literate: broken `[[refs]]` (warning), definitions without any prose or docstring (lint, configurable).
9. A call with literal arguments in a spec expression (example, property, scenario test, contract) must satisfy the callee's `requires`, decided with the same evaluator as refinements: `fib(91)` against `requires n <= 90` is an error at the call. In generated properties, each `requires` of the def is assumed (`prop_assume!`) for every call of the def in the property body, with the call's arguments: generated inputs outside the precondition are not test cases.
10. Async coloring (§6), `depends` declared for every crate used by `extern`/derives/adapters, at most one `key` field per struct, literals in specs satisfy refinements: wherever a literal (number, string, list of literals) is passed where a refined type is expected — arguments in examples, properties and scenario tests, constructor calls such as `Email("x")`, flow arguments, field and parameter defaults — the refinement predicate is evaluated on the literal at check time and a violation is an error at the literal (`"nope" does not satisfy \`Email\``); an integer literal must also fit its integer type. The evaluated subset is: literals and `self`, arithmetic and (chained) comparisons on numbers and strings, `and`/`or`/`not`, `in`, `if … else`, `len`, `abs`, `min`, `max`, `matches` (with the same `regex` semantics as generated code; an invalid regular expression is itself an error) and the pure string methods (`lower`, `upper`, `strip`, `startswith`, `endswith`, `contains`, `isdigit`, `isalpha`, `is_empty`). A predicate outside the subset (e.g. calling a `def`) is left to the run-time check.
11. State machines: the rules of §8.

## 14. Grammar sketch (EBNF, indentation via INDENT/DEDENT)
```
file        = front_matter? (markdown | wh_block)*
wh_block    = "```wh" NEWLINE item* "```"
item        = decorator* (import | depends | alias | type_def | struct | enum | capability
              | adapter | def | flow | machine | app | test) NEWLINE*
decorator   = "@" NAME ("(" args ")")? NEWLINE
import      = "from" dotted "import" NAME ("," NAME)* | "import" dotted ("as" NAME)?
depends     = "depends" NAME "=" STRING ("features" "[" STRING ("," STRING)* "]")?
docstring   = TRIPLE_STRING
alias       = "alias" TYPE_NAME generics? "=" type
type_def    = "type" TYPE_NAME generics? "=" type ("where" expr)?
struct      = "struct" TYPE_NAME generics? ":" block(field | invariant | docstring)
field       = ("key" | "sensitive")* NAME ":" type ("=" expr)?
invariant   = "invariant" expr
enum        = "enum" TYPE_NAME generics? ":" block(variant)
variant     = TYPE_NAME ("(" (variant_item ("," variant_item)*)? ")")?
variant_item= "from"? (NAME ":")? type
capability  = "capability" TYPE_NAME generics? ":" block("async"? "def" sig)
adapter     = "adapter" TYPE_NAME "implements" type ("," type)* ":" block(field | clause)
def         = "async"? "def" sig (":" block(docstring? clause*))?
            | "extern" "async"? "def" sig "from" STRING (":" block(clause*))?
sig         = NAME generics? "(" params? ")" ("->" type ("raises" type)?)? ("uses" names)?
param       = "sensitive"? ("own" | "mut")? "sensitive"? NAME ":" type ("=" expr)?
clause      = "pure" | "requires" expr | "ensures" expr
            | "raises" type "when" expr | "example" expr ("raises" type)?
            | "property" NAME "(" params ")" ":" (expr | block(given | expr))
            | "bench" NAME "(" params ")" ":" expr | "mutate" expr
            | "complexity" expr | "pattern" type | "note" STRING
            | "optimize" NAME | "budget" DURATION "for" expr
            | "alloc" ("none" | call) | "unsafe" ("forbid" | "allow") | "no_panic"
            | "trace" NAME
flow        = "flow" sig ":" block(docstring? flow_stmt*)
flow_stmt   = (pattern_lhs "=")? flow_expr | "return" flow_expr? | match
flow_expr   = expr ("|>" expr)*
machine     = "machine" TYPE_NAME "(" param ")" ("->" type)? ":"
              block(docstring | transition | "ignore" state_pat "on" event_pat | "initial" target | "trace" NAME)
transition  = state_pat "on" event_pat ("if" expr)? "->" target ("emit" expr)?
state_pat   = "_" | TYPE_NAME ("(" NAME ("," NAME)* ")")?
event_pat   = "_" | TYPE_NAME ("(" NAME ("," NAME)* ")")?
target      = TYPE_NAME ("(" expr ("," expr)* ")")?
app         = "app" NAME ":" block("target" call | "log" (NAME "=" STRING)+ | depends | "provide" TYPE_NAME "=" expr
              | "entry" sig ":" block(flow_stmt) | "route" NAME STRING "->" dotted | "status" type "=" INT
              | "listen" STRING | "export" dotted ("," dotted)*)
test        = "test" STRING ":" block("given" TYPE_NAME "=" expr | "expect" expr ("raises" type)?)
type        = TYPE_NAME ("[" type ("," type)* "]")? | type "|" "None"
            | "(" types? ")" "->" type
generics    = "[" NAME (":" bounds)? ("," NAME (":" bounds)?)* "]"
```
The full expression grammar follows Python's precedence table restricted to §10.

## 15. Observability: tracing and logging

Every application built from WH is traceable without anyone writing logging code, and every trace event can be mapped back to the node that produced it.

- **Generated instrumentation** (`tracing` crate, no agent involved):
  - every `flow` and app `entry` opens a span named after its node id (`signup.register`) at `info`;
  - every flow step opens a child span `wh.step` with `wh.node = "signup.register#validate[0]"` at `debug`, and an `Err` leaving a step is logged at `warn` with the error value;
  - every hole wrapper opens a span at `debug` recording its arguments and, at `trace`, its result (`Debug` formatting, with `sensitive` fields, parameters and types redacted);
  - every adapter method opens a span at `debug` (`wh.node = "signup.MemoryDb.insert"`);
  - a violated contract (`requires`, `ensures`, invariant, refinement) is logged at `error` with the contract's source text before the panic or `Err`.
- **Levels per node**: `trace <level>` raises or lowers a node's span level; `trace off` removes its instrumentation entirely (for very hot code).
- **App configuration**: `log level="info" format="pretty"` sets the default filter and output format (`pretty`, `compact`, `json`); `release_level="info"` compiles out more verbose events in release builds (`tracing` `release_max_level_*`). At run time `WH_LOG` (or `RUST_LOG`) overrides the filter with the usual `EnvFilter` syntax, e.g. `WH_LOG=signup=debug`.
- **IDE trace capture**: when the IDE runs an app it sets `WH_TRACE_FILE=<path>`; the app then additionally writes JSON trace events there. The IDE maps each event's `wh.node` to graph nodes, so it can highlight the executed path, show per-node timings and errors, and list a node's recent events in the inspector.
- **Tests**: generated tests install a test-writer subscriber, so a failing example, property or scenario test prints the trace that led to the failure.
- **Synthesized code** uses `tracing::{trace, debug, info, warn, error}` for any extra diagnostics; `println!`/`eprintln!` are rejected by the verification gate (output goes through a capability such as `Console`).
