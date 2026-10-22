# Wild Horse — AI-first literate dataflow IDE for Rust

## API changes
- **Breaking (unavoidable):** `wh_model::NodeKind::Machine` carries the inferred machine: `NodeKind::Machine(MachineInfo)` instead of a unit variant. Match it as `NodeKind::Machine(_)`.
- **New fields** (code building these with struct literals must add them): `ParamInfo.{written, alias}` and `FieldInfo.{written, alias}`: the type as written (`UserId | None`) and the alias it names (`signup.UserId`). `ParamInfo::synthetic(..)` builds a parameter that has no written type. `ast::Machine` also gained `{ignores, doc, trace, body_span}`.
- **New, additive:**
  - `Model` methods: `key_owner_of_alias(alias_id)`, `reference_target(alias, ty)`, `written_alias`, `machine_info`, `repository_shape`, `pattern_arg`, `type_is_hashable`, `capability_has_contracts`.
  - `NodeKind::enum_like()`. `Model::enum_info` now also returns a machine's states.
  - Modules `wh_model::eval` (literal refinement checks) and `wh_model::machine`.
  - `impl Display for ast::TypeExpr`.
  - In the runtime: `wh_rt::Transition` and `wh_rt::MEMO_CAPACITY`.
- **Second batch** (flow combinators, `requires` assumptions, `sensitive`):
  - New fields (code building these with struct literals must add them): `ast::Param.is_sensitive`, `ParamInfo.is_sensitive` (`ParamInfo::synthetic` sets it to `false`), `Node.is_sensitive` (`@sensitive` on a type), and `wh_codegen::rust::Names.qualified` (use `Names::new`).
  - New, additive: `wh_model::check::FLOW_COMBINATORS`, `wh_model::eval::eval_with`, `Model::type_is_sensitive`, and `wh_rt::join`.
  - New flow step labels: `fold:f`, `flat_map:f`, `retry:f` and `parallel`. A `parallel` step's inputs are its branch steps. `retry`/`fold`/`flat_map` steps carry the function's `raises`.
- **Third batch** (target profiles):
  - New crate `wh-targets` (`TargetProfile`, `TargetKind`, `Runner`, `EntryPoint`, `Dependency`, tool detection). It depends only on serde, so wh-model, wh-codegen and later wh-project/wh-build can share it.
  - New fields (code building these with struct literals must add them): `ast::App.{routes, statuses, listen, exports}`, `AppInfo.{target, routes, statuses, listen, exports}` (`AppInfo::profile()` defaults to `native(cli)`), `Node.available_on` (`@target(...)`), and `Manifest.targets` (app id → profile; `#[serde(default)]`).
  - New types: `ast::{Route, StatusRule}` and `RouteInfo`, `RouteSource`, `StatusInfo`.
  - New, additive: `check::{HTTP_METHODS, wire_type_problem}`, `wh_codegen::targets`.

## Context
`/Users/poncho/Projects/wild-horse` is an empty `cargo new` scaffold (edition 2024, rustc 1.99). Goal: an IDE + DSL where developers write **architecture, not implementation**: literate Markdown documents containing a DSL of types, pure function signatures, contracts, data flows and design patterns. ACP agents (Claude via `claude-agent-acp` by default) play **two roles**: they co-author the literate program itself, and they are a **stage of the compiler** that synthesizes the Rust implementation of every hole. The IDE checks, builds and tests continuously and shows results per node.

Decisions made with the user:
- **Milestone 1 = thin vertical slice**, native targets only: language, editable synchronized views (text, flow graph, data model, inspector), compiler-role synthesis with the correctness gate, author role via text edits. **Milestone 2**: optimization stage + perf gates, mutation gates, MCP semantic ops for the author agent.
- **Contracts** (`requires`/`ensures`) are debug-only by default, opt-in for release; refinements and invariants are always checked at construction.
- **Live synthesis** is automatic after an idle debounce (default 3 s), within a per-session token budget; "on demand" is a setting.
- **Author edits** are applied live, highlighted, and undoable as one step; "review before apply" is a setting.
- **Documents = Markdown + fenced `wh` DSL blocks** (`*.wh.md`), edited in a block editor (prose blocks + gpui-kit code editor for DSL blocks).
- **Live eval = check + compile + run**: instant in-process checks per keystroke; background `cargo check`/`cargo test` with results mapped back to nodes.
- **Agent = `claude-agent-acp` by default, configurable command** (any ACP agent).

## Core idea 1: one literate source, many synchronized projections
The `.wh.md` document is the **single source of truth**. The text editor, the flow graph, the data model diagram and the inspector are all *projections* of it, and all of them are *editable*:
- Every change — typing, a graph gesture, an inspector field, an agent edit, an external file change — becomes a **transaction of text edits** on one shared document buffer (`wh-doc`), tagged with its origin (`Text | Graph | Inspector | Agent | Disk`).
- After each transaction: incremental reparse → new model snapshot → every view re-renders from that snapshot. No view keeps its own copy of the program, so they can't drift.
- Graph/inspector gestures are **structured edit operations** (`AddFn`, `Connect(out_port, in_port)`, `InsertFlowStep`, `RemoveNode`, `Rename`, `EditSignature`, `AddContract`, `AddSpec`, `SetPattern`…) that `wh-edit` turns into *minimal* text edits against a **lossless syntax tree** (rowan CST), so prose, comments and formatting are preserved. E.g. connecting `validate.out → normalize_user.in` inside a pipeline flow inserts a correctly indented `|> normalize_user` line at the right spot; adding a fn node inserts a new `wh` block (with a placeholder prose paragraph) in the document section that owns that graph group.
- Rename is a single transaction updating the definition and all references.
- Undo/redo is global over the transaction log, so ⌘Z undoes a graph edit, a typed edit or an agent edit identically.
- Agent edits to the spec: ACP `fs/read_text_file` for a `.wh.md` returns the **live in-memory buffer** (including unsaved changes); `fs/write_text_file` is intercepted, diffed against the version the agent read, three-way merged (`diffy`) onto the current buffer, and applied as an `Agent` transaction — it appears live in text and graph, highlighted for review, and is undoable.
- View-only state (node positions, collapsed groups, viewport) lives in a sidecar `.wh/layout.json` keyed by `NodeId`, so the document stays clean; nodes without a stored position are placed by auto-layout.
- **Multi-file projects**: `wh-doc` is a multi-document store; one transaction may span files (e.g. a cross-module rename). The document panel shows one file per tab; the flow graph is scoped to the active module by default (cross-module calls shown as stub nodes, toggle for whole project); the data model shows the whole project.
- **Broken intermediate states**: while a `wh` block fails to parse, the diagrams keep showing that block's last valid nodes greyed out with an error marker, and structured ops on them are disabled until it parses again. Other blocks stay fully live.
- **Rename** changes the `NodeId`; the `Rename` op also re-keys `.wh/layout.json` and moves the hole file in `impl/`. Pipeline steps without a binding name get ids `flow#callee[k]` (k = occurrence of that callee), so inserting a different step does not shift other ids.

## Core idea 2: agents in two roles
Both roles use the same ACP client infrastructure, but run as **separate sessions with separate permissions**, and can be configured to use different agents/models (`[agent.author]`, `[agent.compiler]` in `wildhorse.toml`; both default to `claude-agent-acp`).

**Role A — Author (edits the program).** A collaborator working at the architecture level, invoked from the agent panel or node context actions: "add a refund flow", "split this def", "suggest properties for this function", "write the prose for this section", "review these contracts", "turn this into a state machine". It reads and writes `.wh.md` documents only (never `impl/`):
- Edits arrive via ACP `fs/write_text_file` (intercepted → merged → `Agent` transaction, see Core idea 1) **or** via a `wh` MCP server attached to the session, which exposes semantic tools: `wh_outline`, `wh_get_node(id)`, `wh_apply_ops([op…])` (the same structured ops the graph uses), `wh_check()` (diagnostics), `wh_run_specs(id)`. Semantic ops are preferred in the prompt; raw text writes are the fallback.
- The IDE hosts the MCP server; `wh mcp` is a stdio shim the agent spawns, bridging to the IDE over a local Unix socket (or operating directly on files in CLI mode).
- Every author edit is applied live in all views, highlighted, and undoable as a single step; the agent panel keeps its diff history (setting: `apply live` (default) / `review before apply`).
- M1 ships the text-edit path; the MCP server with semantic ops ships in M2.

**Role B — Compiler (synthesizes implementations).** A pipeline stage, `wh-synth`, between deterministic codegen and cargo:
```
parse → check → codegen skeleton → SYNTHESIZE holes → verify → cargo build → binary
```
- **Jobs**: one per hole (def, adapter method) whose *synthesis key* changed or is missing. Key = hash(spec clauses of the node, signatures of referenced types/fns/capabilities, pattern, dependency set, agent+model id). Holes depend only on signatures, so jobs run **in parallel** (configurable concurrency and per-build token/round budget), each in a fresh ACP session. Prose is *not* in the key: a prose-only edit marks the hole *soft-stale* (shown, but never auto-resynthesized).
- **Synthesis request** (the compiler's IR handed to the agent): node prose + spec clauses, generated Rust for referenced types/traits, the wrapper it must satisfy, pattern guidance, the exact file to write, available crates, and prior failure reports.
- **Hole file template**: before the job, the compiler writes `impl/<module>/<name>.rs` with a header, `use crate::prelude::*;`, and the exact signature with a `todo!()` body. The agent replaces the body and may add private helper items in that file; the compiler rejects the result if the signature changed.
- **Sandbox** (enforced, not just requested): each job runs in a scratch directory containing a read-only copy of the generated crate plus the one writable hole file; ACP `fs/write_text_file` is allowed only for that path, every `session/request_permission` for terminal/other tools is denied, and after the job the compiler hashes the scratch tree and discards the result if anything else changed. Only the hole file is copied back into `impl/`. The compiler, not the agent, runs verification. (OS-level sandboxing — macOS `sandbox-exec`, Linux landlock — later.)
- **Dependencies**: crates are declared in the spec, never added by the compiler agent (supply-chain control). If the agent reports it needs a crate, that becomes an author-role proposal the user approves (see §11 `depends`).
- **Adapters** that talk to real systems (e.g. `PostgresDb`) are verified by compiling + the capability's shared contract tests against a test resource configured in `wildhorse.toml` (e.g. a Docker DB); if none is configured they are accepted compile-only with a warning badge.
- **Verification gate** = acceptance criteria: `cargo check` → examples → contract wrappers → properties → (if `mutate` declared) mutation score → (if `complexity`/`bench`) regression check. Failures are fed back for up to N repair rounds; still failing → node marked *unsynthesized*, build fails with node-mapped diagnostics (`--allow-holes` builds with `todo!()`).
- **Reproducibility**: accepted impls are committed in `impl/` with a header `// wh:key=<synthesis-key>`; they act as the lockfile. `wh build --frozen` never calls an agent (CI); a plain `wh build` only synthesizes stale/missing holes. A human-edited impl can be marked `// wh:pinned` → never regenerated, still verified.
- **Live**: when Role A (or the user) changes the spec, affected holes go stale and the live loop synthesizes them after an idle debounce (default 3 s, cancelled and restarted on further edits to that node), within a per-session token budget (setting: `auto` (default) / `on demand`).

**Optimization stage (M2; synthesized code must be highly optimized).** The zero-cost codegen and quality gates below are already in M1. Synthesis is two-phase: *make it correct*, then *make it fast*, and the second phase is gated by measurement, not by the agent's claims.
```
synthesize (correct) → verify → OPTIMIZE: profile → agent proposes K variants → verify each → benchmark → keep fastest → lock
```
- **Auto-benchmarks for every hole**: even without a `bench` clause, the compiler generates a criterion bench using the property strategies with an input-size sweep. `complexity O(...)` is checked by fitting the measured growth curve.
- **Optimization feedback to the agent**: criterion results, allocation counts (counting global allocator in bench builds), a sampled profile of the hot path (`samply`/`perf` where available), and optionally the generated assembly (`cargo asm`) for tiny hot functions.
- **Acceptance rule**: a variant replaces the incumbent only if *all* correctness gates still pass (examples, contracts, properties, mutation score, `alloc`/`unsafe` constraints) **and** criterion reports a statistically significant improvement beyond a noise threshold (default 3%) on the target metric. Tournament of K candidates (default 3), up to M rounds or until no improvement.
- **Baselines** are stored with the lock (`impl/.bench/<node>.json`, keyed by machine/target fingerprint); a later re-synthesis that is slower than the locked baseline is rejected (perf regression = build failure in `--release`).
- **Cost control**: the live loop runs correctness only; optimization runs when the IDE is idle, on `wh build --release`, or `wh optimize [node]`, prioritized by profile weight of the hole inside the app's entry flows (optimize what's hot first).
- **Spec-level performance contracts** (language §5): `optimize speed|latency|throughput|memory|size`, `budget <time> for <input-size>`, `alloc none|bounded(n)`, `unsafe forbid|allow`, `no_panic` — these are checked, not hints.
- **Zero-cost generated code** (deterministic codegen, no agent involved): capabilities are monomorphized generics (`impl Db`), never `dyn`; flows compile to direct calls with `?` and `#[inline]` on thin wrappers; ownership modes (`own`/`mut`/borrow) avoid clones; refined newtypes are `#[repr(transparent)]`; contract wrappers are `debug_assert!` (zero cost in release unless `contracts = "release"`); release profile defaults to `lto = "fat"`, `codegen-units = 1`, `panic = "abort"` (configurable), `opt-level = 3` (or `"s"`/`"z"` for `optimize size`/embedded), and `target-cpu=native` for local native builds. Profile-guided optimization (`cargo-pgo`, training workload = entry-flow scenario tests + benches) in M2.
- **Code-quality gates on synthesized code**: `clippy -D clippy::perf -D clippy::correctness`, `#![forbid(unsafe_code)]` unless the node declares `unsafe allow` — in which case `cargo miri test` must pass for that node.

## Core idea 4: observable by construction
Applications built with Wild Horse are debuggable without hand-written logging (language spec §15):
- Deterministic codegen instruments every flow, flow step, hole wrapper and adapter method with `tracing` spans carrying `wh.node = <NodeId>`; step errors are logged at `warn`, contract violations at `error` with the contract's source text.
- `sensitive` fields are redacted in `Debug` output and traces; `trace <level>` / `trace off` tune or remove instrumentation per node; `log level=… format=… release_level=…` in the `app` configures `tracing-subscriber` (`WH_LOG`/`RUST_LOG` override at run time).
- The IDE runs apps with `WH_TRACE_FILE` set and maps JSON trace events back to nodes: executed-path highlighting and per-node timings/errors in the flow graph, recent events in the inspector (M1: run panel with node-mapped event list; M2: timeline overlay).
- Generated tests install a test-writer subscriber so failures print their trace; the synthesis gate rejects `println!`/`eprintln!` in hole files.

## Core idea 3: the spec/impl split
The IDE owns everything derivable from the spec; the compiler-role agent owns only hole bodies.
- `.wh/gen/` — generated Cargo workspace (types, signatures, contract wrappers, tests, flows). Regenerated freely, never edited by hand or agent.
- `impl/<module>/<fn>.rs` — one file per hole, written by the agent (or a human), committed. The `// wh:key=` header marks which spec version it implements; a key mismatch marks the node **stale** in all views.
- Contracts are enforced by the generated wrapper, so an impl can't bypass them (default borrow mode → `&Email`, no clone):
  ```rust
  #[inline]
  pub fn normalize_email(e: &Email) -> Email {
      let result = imp::normalize_email(e);
      debug_assert!(result == e.to_lowercase(), "wh: ensures violated: signup.normalize_email");
      result
  }
  ```

Project layout on disk:
```
my-app/
  wildhorse.toml        # project, targets, agents, dependencies, test resources
  src/*.wh.md           # literate sources (one module per file)
  impl/<module>/*.rs    # synthesized/pinned hole bodies (committed = lockfile)
  impl/.bench/*.json    # perf baselines (committed)
  .wh/layout.json       # view state (committed)
  .wh/gen/, .wh/target/ # generated crate + build cache (gitignored)
```
- Effects (I/O) are declared and compiled to capability traits passed as parameters, so pure-by-default holds and impls stay testable/portable (later: `no_std` targets).

## Language specification — "Wild Horse" literate language (WH)
This section is the normative v0 spec. Step 1 of implementation copies it to `docs/language-spec.md` in the repo, where it evolves with the code.

### 1. Design principles
1. **Declarative, not imperative.** A program states *what exists and how data flows*, never *how* a function computes. There are no statements, loops, or assignments in function bodies — only signatures, contracts, specs and patterns.
2. **Pure by default.** Every `def` is pure unless it declares `uses <Capability>`. Effects are explicit and typed.
3. **Python-like surface.** Indentation blocks, `def`, `:`, `#` comments, `True/False/None`, `and/or/not`, `[]` generics, `_private` names, decorators.
4. **Rust semantics underneath.** Every construct has a deterministic Rust mapping (§12). Nothing is expressible that Rust can't represent.
5. **Literate first.** Prose is part of the program: it is attached to definitions, checked for broken references, and given to the agent as intent.
6. **Order-independent.** Definitions may appear in any order and any block; the document order serves the reader, not the compiler.
7. **Every definition is a graph node.** Anything that can be written can be shown, selected and edited in the graph.

### 2. Document structure
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

### 3. Lexical structure
- UTF-8. **Indentation is significant** (Python rules): 4 spaces per level, tabs are an error; the lexer emits `INDENT`/`DEDENT`/`NEWLINE`. Newlines inside `()`, `[]`, `{}` are ignored (implicit line joining). A line starting with `|>` continues the previous expression.
- Comments: `#` to end of line (inside `wh` blocks only — Markdown headings live outside blocks, so there is no clash).
- Identifiers: `snake_case` for values, functions, flows, fields; `PascalCase` for types, variants, capabilities, patterns. Violations are errors (they drive Rust mapping). Leading `_` = module-private.
- Literals: integers `1_000`, `0xFF`, `0b1010` (optional suffix `1u8`); floats `3.14`, `1e-3`; strings `"…"` with escapes, raw `r"…"`, triple-quoted `"""…"""` (docstrings); bytes `b"…"`; durations `10ns`, `50us`, `16ms`, `5s`; `True`, `False`, `None`.
- **Hard keywords** (reserved everywhere): `import from as type alias struct enum capability adapter implements def async flow machine app test extern pure uses raises requires ensures invariant example property where forall exists in is and or not if else lambda return own mut True False None`.
- **Soft keywords** (keywords only in their clause/statement position, usable as identifiers elsewhere — like Python's `match`): `match case on initial emit ignore target provide entry depends given expect bench mutate pattern note complexity optimize budget alloc unsafe no_panic key when result old self`.

### 4. Types
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
- `key` marks the identifying field; its type (`UserId`) then denotes a *reference to User* wherever it appears (used by the data model view and by `Repository`).
- Refinements and struct invariants are **always** checked at construction (they define valid data, at the parse boundary); `requires`/`ensures` contracts are debug-only by default. A struct with an `invariant` gets private fields, a validating `new(...) -> Result<Self, InvalidValue>` and `#[inline]` getters; without invariants fields are public.
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

### 5. Functions (`def`)
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
- Parameters: `name: Type`, optional default `= expr`. Passing mode: default is shared borrow (`&T`; by value for `Copy` types), `own x: T` takes ownership, `mut x: T` is a mutable borrow (the only way to model in-place updates; `old(x)` refers to the pre-call value in `ensures`).
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

**Extern** functions bind to existing Rust instead of an agent hole: `extern def sha256(data: bytes) -> array[u8, 32] from "sha2::Sha256::digest"` (the crate must be declared with `depends`). Contracts and specs still apply to them.

### 6. Capabilities and adapters (effects)
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

### 7. Flows (dataflow composition)
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
- Allowed in flow bodies: bindings, `|>`, calls to `def`/`flow`/constructors, field access, tuple destructuring, `match/case` on enums, `return`, and combinators `map(f)`, `filter(p)`, `fold(init, f)`, `flat_map(f)`, `parallel(a, b, …)`, `retry(f, times=n)`, `each(f)`. No arithmetic, loops or literals beyond constants — anything computed belongs in a `def`.
- `x |> f` ≡ `f(x)`; `x |> f(a)` ≡ `f(x, a)` (piped value is the first argument).
- Errors propagate automatically (`?`): each step's `raises E` must convert into the flow's error via a `from` variant (or be the same type), else a type error at that edge.
- `parallel(...)`: async steps → `futures::join!`/`try_join!`; sync steps → sequential in v0 (`rayon::join` opt-in via `optimize throughput` later). Steps may only share capabilities by `&` (always true, see §6).
- `match` arms must produce the same type; `return` with no value only in `unit` flows.
- Step types must compose; a mismatch is reported *on the edge* (and in the text at the step).

### 8. State machines
```wh
@pattern(StateMachine)
machine Door(event: DoorEvent) -> DoorEffect:
    Open           on Close                  -> Closed
    Closed         on Open                   -> Open
    Closed         on Lock(code)             -> Locked(code)
    Locked(code)   on Unlock(c) if c == code -> Closed      emit Unlocked
    initial Closed
```
States are inferred as an enum `Door`; generated: `fn step(state: Door, event: DoorEvent) -> (Door, Option<DoorEffect>)`, a transition table, and properties (no unreachable states, no unhandled `(state, event)` unless marked `ignore`). Rendered in the graph as a state diagram.

### 9. Design patterns (lowest abstraction level)
`pattern` attaches an implementation strategy to a node. Each pattern in the catalog defines: (a) **shape rules** checked statically, (b) **codegen contribution**, (c) **agent guidance** injected into the prompt, (d) **auto specs** it adds. v0 catalog:

| pattern | applies to | shape rule | adds |
|---|---|---|---|
| `Transform` | def | pure, 1 input → 1 output | totality property (no panic on any valid input) |
| `Pipeline` | flow | linear `\|>` chain | per-stage tracing hook |
| `StateMachine` | machine/enum | transitions total | reachability properties |
| `Repository[T]` | capability/adapter | has insert/find style methods | round-trip property `find(insert(x)) == x` |
| `Adapter` | adapter | implements a capability | contract tests shared across adapters of a capability |
| `InMemory` | adapter | — | used as default test double |
| `Builder[T]` | struct | — | generated builder (no agent) |
| `Strategy` | capability | single-method | — |
| `Memoize` | def | pure, `Hash` inputs | cache wrapper (no agent) |
| `GameLoop` | def | uses a clock/input capability | fixed-timestep loop guidance; frame-time bench |

Patterns are user-extensible later via `pattern` definition files (rules + prompt template + codegen template).

### 10. Specification expressions (contracts, examples, properties)
A pure, typed, Python-like expression language compiled to Rust predicates:
- Arithmetic `+ - * / // % **`, comparison with chaining `0 <= x < n`, `and or not`, `in`, `not in`, `is None`, `is not None`, conditional `a if c else b`.
- Field access, indexing `xs[i]`, slicing `xs[a:b]`, tuple/struct/variant construction `User(id=1, email=e, age=30)`, enum tests `ev is Created`.
- Comprehensions and quantifiers: `[f(x) for x in xs if p(x)]`, `all(p(x) for x in xs)`, `any(...)`, `forall x in xs: p(x)`, `exists x in xs: p(x)`, `sum`, `len`, `min`, `max`, `sorted`, `lambda x: …`.
- Special names: `result` (return value, in `ensures`), `old(e)` (pre-state, for `mut` params), `self` (in refinements/invariants).
- String/collection functions from a fixed std-spec library (`lower` → `to_lowercase`, `upper`, `strip`, `startswith`, `contains`, `matches` → `regex`, `keys`, `values`, …), each with a defined Rust mapping (Unicode semantics, as in Python).
- May call any pure `def` (calls to holes are allowed — that is how laws like idempotence are stated). Must type-check as `bool` where a condition is expected.

### 11. Modules, apps and targets
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
    target native(cli)              # M1. Later: server(axum), web(wasm), game(bevy), embedded(thumbv7em-none-eabihf)
    provide Db = PostgresDb(url=env("DATABASE_URL"))
    provide Clock = SystemClock
    entry main(args: list[str]) -> unit raises AppError:
        args |> parse_cli |> register |> report
```
- `provide` binds capabilities to adapters for this app; tests use `test … given Db = MemoryDb()`.
- `entry` is a flow that becomes the binary's `main` (or the target's entry point). Its error type must implement `Display`: an `Err` prints the error and exits with code 1.

**Scenario tests** (module level, multi-step, with capability substitution):
```wh
test "minors need a guardian":
    given Db = MemoryDb()
    expect register(form(age=12)) raises RegisterError.TooYoung
    expect register(form(age=12, guardian=7)) is not None
```

### 12. Node identity and Rust mapping
- Node ids: `module.Name` (types, capabilities), `module.fn_name`, `module.flow_name`, `module.flow_name#binding` (flow step), `module.fn_name.example[i]` / `.property.name`. Ids are stable across edits; renames go through the `Rename` op.
- Mapping summary: module → Rust module `mod signup`; struct/enum → `pub struct/enum` with derives; refined type → newtype with private field + `parse`/`TryFrom`; capability → trait; adapter → struct + `impl Trait` with hole methods; def → public wrapper (contracts) + `imp::fn` hole in `impl/signup/normalize_email.rs`; flow → generated fn using `?` and `From`; machine → enum + `step` fn; app → `main.rs` wiring adapters; specs → `#[cfg(test)]` modules, `proptest!`, `benches/`.

### 13. Static semantics (checker rules, M1)
1. All names resolve; no duplicate definitions per module; naming conventions hold.
2. Types well-formed; generic bounds satisfied; directly self-containing structs without `list`/`| None`/collection indirection are an error (infinite size); `| None` recursion is boxed automatically.
3. Purity: no transitive path from a non-`uses` def to a `uses` def.
4. Flows: step types compose; error conversions exist; flow capabilities ⊆ declared `uses` (if declared) or inferred.
5. Spec expressions are pure, well-typed, use `result`/`old` only where valid.
6. Examples/properties reference only in-scope names; property parameter types have strategies (refined types, structs of strategies, or `given`).
7. Pattern shape rules (§9).
8. Literate: broken `[[refs]]` (warning), definitions without any prose or docstring (lint, configurable).
9. Async coloring (§6), `depends` declared for every crate used by `extern`/derives/adapters, at most one `key` field per struct, literals in specs satisfy refinements.

### 14. Grammar sketch (EBNF, indentation via INDENT/DEDENT)
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
field       = "key"? NAME ":" type ("=" expr)?
invariant   = "invariant" expr
enum        = "enum" TYPE_NAME generics? ":" block(variant)
variant     = TYPE_NAME ("(" (variant_item ("," variant_item)*)? ")")?
variant_item= "from"? (NAME ":")? type
capability  = "capability" TYPE_NAME generics? ":" block("async"? "def" sig)
adapter     = "adapter" TYPE_NAME "implements" type ("," type)* ":" block(field | clause)
def         = "async"? "def" sig (":" block(docstring? clause*))?
            | "extern" "async"? "def" sig "from" STRING (":" block(clause*))?
sig         = NAME generics? "(" params? ")" ("->" type ("raises" type)?)? ("uses" names)?
param       = ("own" | "mut")? NAME ":" type ("=" expr)?
clause      = "pure" | "requires" expr | "ensures" expr
            | "raises" type "when" expr | "example" expr ("raises" type)?
            | "property" NAME "(" params ")" ":" (expr | block(given | expr))
            | "bench" NAME "(" params ")" ":" expr | "mutate" expr
            | "complexity" expr | "pattern" type | "note" STRING
            | "optimize" NAME | "budget" DURATION "for" expr
            | "alloc" ("none" | call) | "unsafe" ("forbid" | "allow") | "no_panic"
flow        = "flow" sig ":" block(docstring? flow_stmt*)
flow_stmt   = (pattern_lhs "=")? flow_expr | "return" flow_expr? | match
flow_expr   = expr ("|>" expr)*
machine     = "machine" TYPE_NAME "(" param ")" ("->" type)? ":" block(transition | "initial" state)
transition  = state "on" pattern_expr ("if" expr)? "->" state ("emit" expr)?
app         = "app" NAME ":" block("target" call | depends | "provide" TYPE_NAME "=" expr | "entry" sig ":" block(flow_stmt))
test        = "test" STRING ":" block("given" TYPE_NAME "=" expr | "expect" expr ("raises" type)?)
type        = TYPE_NAME ("[" type ("," type)* "]")? | type "|" "None"
            | "(" types? ")" "->" type
generics    = "[" NAME (":" bounds)? ("," NAME (":" bounds)?)* "]"
```
The full expression grammar follows Python's precedence table restricted to §10.

## Workspace layout (convert root to a Cargo workspace; remove `src/main.rs`)
| crate | responsibility |
|---|---|
| `crates/wh-syntax` | Whole-document **lossless CST** (`rowan`): Markdown prose blocks (`pulldown-cmark` offsets) + `wh` blocks parsed by a `logos` lexer with an indentation pass (INDENT/DEDENT/NEWLINE, bracket-aware line joining, `|>` continuation) and a hand-written recursive-descent/Pratt parser with error recovery, implementing the grammar in §14; typed AST layer over the CST; diagnostics with spans. Round-trip test: `print(parse(src)) == src` |
| `crates/wh-doc` | Document store: rope buffer (`ropey`), transaction log with origins, global undo/redo, versions, change broadcast; three-way merge for agent/disk writes; file save/load |
| `crates/wh-edit` | Structured edit ops (graph/inspector/agent-level) → minimal `TextEdit`s on the CST; formatting-preserving insertion rules; rename-with-references |
| `crates/wh-model` | Semantic model + checker: name resolution, type checking of signatures/flows (port type compatibility), purity (pure fn can't reach a `uses` fn), contract expression typing, spec hashing per node. Stable `NodeId`s |
| `crates/wh-graph` | Two graph projections from the model + layered (Sugiyama-style) layout: **flow graph** (nodes: defs, flows, flow steps, capabilities, adapters, machines; edges: data flow, capability use) and **data model graph** (nodes: structs, enums, refined types, aliases, errors; edges: relationships with cardinality, see IDE §Data model) |
| `crates/wh-codegen` | Deterministic Rust emission into `.wh/gen/`: types with derives + refinement constructors, contract wrappers, `#[path]` links to `impl/`, hole file templates, `todo!()` stubs for missing impls, flows, `Cargo.toml` from `depends`, tests (examples → `#[test]`, properties → `proptest`, benches → `criterion`). Test names encode node ids: `wh__<node>__example_0`. Writes a file only if its content changed (keeps cargo's incremental cache warm) |
| `crates/wh-build` | Runs `cargo check/test --message-format=json` in `.wh/gen` (shared warm target dir), maps rustc spans and test names back to `NodeId`s |
| `crates/wh-agent` | ACP client infrastructure (`agent-client-protocol` 2.x, `AcpAgent`/`Stdio` transport) on a dedicated tokio thread, bridged to UI via channels: spawn/config of agents per role, `initialize` + `authenticate` (surface the agent's auth methods, e.g. Claude login vs API key, in the UI/CLI), sessions with attached MCP servers, streaming updates, cancellation, permission requests, `fs/*` handlers backed by a per-session **policy** (author: `.wh.md` via doc store; compiler: one hole file in its scratch dir) |
| `crates/wh-author` | Role A: author session management, prompt context (outline, selection), `Agent` transactions (live apply / review); M2: the `wh` MCP server (tools above) and its Unix-socket bridge |
| `crates/wh-synth` | Role B: synthesis keys, job scheduler (parallel, cancellable), synthesis-request builder, verification gate (uses `wh-build`), repair loop, impl cache/lock headers |
| `crates/wh-perf` (M2) | Optimization stage: auto-bench generation, criterion JSON parsing + significance test, allocation counting, profiler integration, complexity curve fitting, baselines, variant tournament |
| `crates/wh-project` | `wildhorse.toml` (name, targets, agent command/args/env), file watching (`notify`), live pipeline orchestration: debounce → parse/check (sync, <10ms) → codegen → background check → node-scoped tests; results store keyed by `NodeId` |
| `crates/wh-cli` (`wh`) | Headless: `wh new`, `wh check`, `wh gen`, `wh synth [node] [--force]`, `wh optimize [node]`, `wh build [--release] [--frozen\|--allow-holes]`, `wh test [node]`, `wh ask "<instruction>"` (author role on files), `wh mcp` (stdio shim) — used for CI and to verify every layer before the GUI |
| `crates/wh-ide` | `gpui` + `gpui-kit` 0.8 app (see below) |

## IDE (wh-ide)
gpui-kit `DockArea` with these panels, all subscribed to the same `wh-doc` document store. Default layout: Document (left) | Flow graph over Data model (center) | Inspector + Agent tabs (right) | log (bottom); the layout is serialized per project.
1. **Document** — block editor over the `.wh.md` buffer: prose blocks rendered as styled text (editable as Markdown source on focus), DSL blocks use gpui-kit's code editor (rope + highlighting from `logos` tokens in v0). Typing emits `Text` transactions. Inline gutter badges per node: ✓ pass / ✗ fail / ◌ stale / ⋯ agent working. Regions changed by graph/agent transactions flash-highlight.
2. **Graph / flow editor** — custom GPUI canvas element (gpui-kit has no node graph): nodes with typed ports, bezier edges, pan/zoom, status colors. **Editable**: create nodes from a palette, drag edges between type-compatible ports (incompatible ports greyed out using the checker), reorder/insert flow steps, delete, rename inline — each emits a structured op → `Graph` transaction. Invalid-but-parseable states (e.g. a dangling edge) show as diagnostics rather than being blocked.
3. **Data model** — a second custom canvas (default: split below the flow graph in the center column; dockable/tabbable anywhere) showing **all data types of the project and their relationships**, ER/class-diagram style:
   - Nodes: `struct` (fields with types, defaults, `invariant`s), `enum` (variants and payloads), refined `type` (base type + `where` predicate), `alias`, `@error` enums, generic types (type parameters + bounds). Badges for derives and patterns (`Builder`, `StateMachine`).
   - Relationship edges, derived from field/variant types: **has** (`T` = 1, `T | None` = 0..1, `list[T]`/`set[T]` = 0..*, `NonEmpty[T]` = 1..*, `dict[K, V]` = keyed 0..* with the key type on the edge, `array[T, N]` = N), **variant-of** (enum → payload types), **refines / alias-of**, **converts-from** (error `from` conversions), **instantiates** (generic → concrete args), **references** (a field typed with another struct's `key` type, e.g. `Order.buyer: UserId` → `User`, drawn as a foreign-key edge; ownership is declared explicitly by `key id: UserId` in `User`).
   - Optional overlay: which defs/flows/capabilities **produce** or **consume** each type (toggle), linking the data model to the flow graph.
   - Filters: by module/section, by relationship kind; focus mode (selected type + N hops); search.
   - **Editable** through the same structured ops: add type, add/rename/retype/remove field, add variant, draw an edge between two types (creates a field with a chosen cardinality), change cardinality on an edge (rewrites the field type, e.g. `T` → `list[T]`), add invariant — each emits a `Graph` transaction on the document.
4. **Inspector** — selected node (from any view), editable form fields (signature, contracts, pattern, specs, performance clauses) that also emit structured ops; plus read-only results (examples, property counterexamples, mutation score, bench numbers vs baseline, allocation counts), generated Rust, impl source + key/staleness, actions (Synthesize / Re-synthesize / Optimize / Pin / Run specs / Ask agent to revise spec), per-node agent transcript.
5. **Agent panel** (tab in the right dock): author-role chat with the current selection as context; pending author edits shown as reviewable diffs (accept / reject / undo); synthesis queue with per-hole status (queued, synthesizing, verifying round k/N, accepted, failed).
6. Bottom: build diagnostics, agent logs, permission prompts.
- Selection and cursor are shared across all views: clicking a node in the flow graph or data model scrolls the document to its definition and highlights it in the other diagram; moving the cursor into a definition selects its node everywhere.

## Milestone 1 build order
1. Workspace scaffold (root `.gitignore` for `/target`, `.wh/gen`, `.wh/target`); write `docs/language-spec.md` from the spec section above; `cargo add gpui-kit` and `agent-client-protocol` and **read their sources/examples in `~/.cargo/registry`** (gpui version pinned by gpui-kit and its macOS Metal/Xcode build requirements; ACP `Client.builder().connect_with`, `yolo_one_shot_client.rs`; confirm `claude-agent-acp` routes file edits through client `fs/*` and honours denied permissions) before writing UI/agent code.
2. `wh-syntax` (lossless CST, round-trip tests, one parser test per spec section/example) + `wh-model` implementing §13 rules, with `insta` snapshot tests over example docs (valid + one failing doc per rule). M1 covers §2–7, 10–13; `machine` (§8) and the full pattern catalog (§9: M1 ships `Transform`, `Pipeline`, `Adapter`, `InMemory`) parse in M1 but generate code in M2. Performance clauses (`optimize`, `budget`, `alloc`, `no_panic`, `mutate`) parse and type-check in M1; `unsafe forbid/allow` and `complexity` as an agent hint are enforced in M1, the rest are enforced in M2.
3. `wh-doc` + `wh-edit`: transaction log, undo, structured ops with snapshot tests of resulting text (`op applied to doc → expected .wh.md`).
4. `wh-codegen` + `wh-build` + `wh check/gen/test` CLI; golden tests that generated crate compiles with `todo!()` stubs.
5. `wh-agent` (ACP infra + policies) → `wh-synth` + `wh synth`/`wh build` against `claude-agent-acp` (correctness gate: check, examples, contracts, properties, clippy, unsafe policy) → `wh-author` + `wh ask` (spec-edit interception, merge, live apply).
6. `wh-project` live pipeline.
7. `wh-ide`: dock shell → inspector → document editor → shared canvas widget (nodes, ports, edges, pan/zoom, selection) → flow graph and data model on top of it (read-only, then editable ops) → wiring to live pipeline + agents.
8. `examples/signup/` — literate example exercised end to end.

## Milestone 2
1. `wh-perf` + `wh optimize` / `wh build --release`: auto-benches, allocation counting, `alloc`/`budget`/`no_panic`/`complexity` gates, variant tournament, baselines in `impl/.bench/`, idle-time optimization prioritized by profile weight.
2. Per-node `mutate` gates via `cargo-mutants --file`.
3. `wh mcp` + IDE-hosted MCP server with semantic ops for the author agent.
4. Code generation for `machine` (§8) and the rest of the pattern catalog (§9).

## Later milestones (designed for, not built in M1/M2)
- Quokka-style inline values: hot-reloaded dylib of the generated crate, evaluating `example` expressions on every change.
- Project-wide mutation and benchmark dashboards (M2 has per-node gates and results in the inspector).
- Target profiles (`TargetProfile` trait: crate template, deps, entry point, cargo args, runner): web (wasm32 + trunk), server (axum), games (bevy), embedded (`no_std`, probe-rs flashing), desktop bundling.
- Cross-hole synthesis (agent may propose helper `def`s back to the spec via Role A), synthesis cost/latency budgets, model routing per pattern.
- `salsa` incremental model for large projects; more patterns (Observer, Command, ECS system, Actor).

## Verification
- `cargo test --workspace` (snapshot + golden tests; codegen output compiles).
- `cargo run -p wh-cli -- check examples/signup` → no diagnostics; introduce an error → span-accurate diagnostic.
- Compiler role: `wh build --allow-holes examples/signup` → builds with `todo!()`, specs report holes per node; `wh build examples/signup` with `claude-agent-acp` installed → holes synthesized in parallel, files written only under `impl/`, all specs pass, binary runs; second `wh build` makes zero agent calls; `wh build --frozen` works offline; editing one contract re-synthesizes only that hole; a deliberately impossible contract ends as *unsynthesized* with a clear diagnostic after N rounds; a synthesis job that tries to write outside its hole file has its result discarded.
- Optimization (M2): `examples/signup` includes a deliberately hot pure def (e.g. `dedupe_emails(list[Email]) -> list[Email]` with `complexity O(n)`, `alloc bounded(1)`, `optimize speed`); `wh optimize dedupe_emails` → report shows baseline vs accepted variant with criterion confidence, allocation counts, all specs still green; a variant that is faster but violates a property is rejected; re-running with an O(n²) impl pinned fails the `complexity` gate.
- Author role: `wh ask "add an age check step to register" examples/signup` → `.wh.md` updated with prose preserved, `wh check` clean, then synthesis picks up the new hole.
- `wh-edit` tests: every structured op leaves prose/comments byte-identical outside the edited range, and the reparsed model reflects the op.
- `cargo run -p wh-ide -- examples/signup` → screenshot shows Document, Flow graph, Data model, Inspector/Agent and log panels, then sync checks in each direction: (a) type a new `|> step` in a flow block → graph gains the edge; (b) drag an edge / add a node in the graph → the document text updates in place; (c) edit a contract in the inspector → text + graph update; (c2) in the data model, change the `User → Email` edge cardinality from 1 to 0..* → the field becomes `email: list[Email]` in the text and dependent defs show type errors in the flow graph; ⌘Z restores it; (d) ask the author agent to revise a flow → change appears in both views highlighted, ⌘Z reverts it; (e) the revision makes a hole stale → synthesis queue picks it up automatically and the node flips ◌ → ⋯ → ✓.

## Implementation notes (deviations from the plan, M1)
- **No rowan CST.** The source text itself is the lossless representation; the AST carries exact byte spans for every node, and `wh-edit` computes minimal text edits from them (re-parsing every plan to guarantee it never introduces parse errors). Same guarantees, far less machinery.
- **`wh-doc` buffers are `String`s**, not ropes: documents are small, and the GPUI editor keeps its own rope.
- **Struct invariants**: fields stay `pub` (decided; spec §4 gives the reasons). Codegen adds a validating `new(...)` and `wh_check()`. Hole wrappers check the invariants of returned values and of `mut` arguments after the call, in debug builds. A per-project private-fields option was rejected: hole modules are Rust children of the generated module, so privacy would not constrain them, and getters would break cross-module construction in specs and flows.
- **Synthesis sandbox package names**: cargo derives artifact metadata from a root package's path *relative to its workspace root*, so two `[workspace]`-root packages with the same name in one target dir reuse each other's artifacts. Each synthesis job's scratch crate therefore gets a unique package name (`wh_synth_<node>`), and each worker slot has its own target dir (`.wh/target-synth-<slot>`).
- **Redaction** applies to `sensitive` struct fields; a bare parameter of a refined type (e.g. `e: Email`) is still recorded in traces. M2: a `sensitive` modifier on types/params.

- **Machines, patterns and literals** (spec §8, §9, §13 rule 9):
  - Machine states are inferred by the checker (`wh_model::machine`), not by `resolve_kinds`, because payload types need expression typing. Machines are analyzed before every other node, so defs can use their states.
  - The adapter contract tests (patterns `Adapter`/`InMemory`) are generated tests only; adapter wrappers do not check capability contracts at run time. The contracts are resolved in the capability's module and emitted with fully qualified paths (`Names::qualified`) into the adapter module's tests.
  - Pattern and machine tests draw small values (±1000), so totality tests stay fast on inputs whose cost grows with their size (e.g. pong's `render(game)`).
  - Checked `Deserialize` (shadow struct + `TryFrom` + `wh_check`) is only generated for non-generic structs with invariants. Generic ones keep the plain derive.

- **Target profiles** (spec §11):
  - `wh-targets` describes each target as data. The checker uses it for adapter availability and per-target rules, codegen for dependencies, crate type and entry point, and the manifest records each app's profile.
  - `wh-project::run_app` still runs every binary as `Runner::Exec`. A server app runs until it is stopped, so the IDE's Run should read `Manifest.targets` and use `Runner::Serve` (start it, show the address, stop it with SIGTERM). wh-build should likewise use `cargo_build_args` for web apps. Both are left to those crates' owners.
  - Server flows run on `spawn_blocking`, so their capabilities need `Send + Sync + 'static` (the state is shared in an `Arc`).
  - Web exports use JSON strings rather than `serde-wasm-bindgen`, so the only wasm dependency is `wasm-bindgen`.
  - The wasm32 target was not installed on this machine, so the wasm build itself is only checked when it is (`tests/compile.rs` skips with a message). The generated code is compiled and tested on the host every time.

## Caching, budgets and staleness (implemented)
- **Synthesis cache** (per user: `WH_CACHE_DIR`, else `$XDG_CACHE_HOME/wild-horse`, else `~/.cache/wild-horse`; entries in `synth/<aa>/<key>.rs|json`):
  - Keys are the synthesis key plus the exact Rust signature.
  - Every accepted implementation is stored, and so is every up-to-date locked file seen by `wh status`/`plan`. A branch's committed lockfile thus becomes reusable from other branches.
  - Before calling the agent, cache hits are installed and verified together in one clippy + test run. Entries that fail are evicted and go to the agent.
  - `--no-cache` disables reuse.
  - **Verified:** switching `examples/signup` between two spec versions restored the 3 changed holes with zero agent calls, byte-identical to Claude's versions.
- **Codegen output cache**:
  - `wh_codegen::generate_cached` skips regeneration when the fingerprint is unchanged. The fingerprint covers the sources, the options, and `CODEGEN_VERSION`, which includes a build-time hash of the generator's own source.
  - Missing hole stubs are still recreated when generation is skipped.
  - The IDE's inspector memoizes in-memory generation per fingerprint.
- **Shared cargo target dir**:
  - One per user (`<cache>/target`, `WH_TARGET_DIR` overrides), shared by every project, synthesis job and the restore check, so dependencies compile once. Cargo serializes builds; agents still run in parallel.
  - Every generated package name is unique per project location (`wh_<name>_<hash>`, scratch crates `…_synth_<node>`); the Rust crate name stays the project name.
  - Binaries are copied to `<project>/.wh/bin/`.
- **Budgets**:
  - `[synthesis] budget_usd` / `max_attempts` limit a run. `wh_synth::Budgeted` wraps any `Driver`; cost is read from ACP `usage_update` messages (`cost`, USD) when the agent reports it.
  - `wh synth`/`wh build` print attempts, cache reuse and reported cost.
- **Soft-stale**:
  - Lock headers record a prose hash (`// wh:key=<k> prose=<p>`).
  - A prose-only change shows the hole as `SoftStale` in `wh status`; it is never re-synthesized automatically.
- **Compile-only adapters**:
  - An accepted hole with no specs of its own gets a note ("only compile- and lint-checked").
  - Adapter contract tests generated with the adapter's node prefix are run by the gate automatically. Test resources (databases, …) come from the environment, which `cargo test` inherits.
- **`wh new <dir>`** scaffolds a literate starter project (`wh_project::scaffold::new_project`, also used by the IDE's New Project).
- **Author agent through MCP (M2, CLI done)**:
  - `wh mcp [--project <dir> | --bridge <socket>]` serves the `wh` MCP server (`crates/wh-mcp`, 7 tools).
  - `wh ask` attaches it by default (`--raw-edits` for the old Write/Edit mirroring), and disables the agent's own file-edit, shell and sub-agent tools.
  - **Verified with real Claude:** a property plus its prose were added purely via `wh_apply_ops` / `wh_edit_prose`, and checked with `wh_check`.
  - **Not done:** IDE bridge mode, which needs a `wh_mcp::Backend` over the IDE's `DocStore`; see `crates/wh-mcp/NOTES.md`.
- **Synthesis scheduling**:
  - Jobs run in spec-dependency order (`wh_synth::spec_dependencies`).
  - A failure caused only by other holes' stubs is `Verdict::Blocked`: no agent round is spent. The agent's code is re-verified once those holes land.
  - Run-time cycles (a hole whose implementation calls a hole whose specs call it back) are resolved by overlaying blocked candidates into each other's sandboxes.
  - The signature check ignores `mut` bindings and module paths; the compiler still enforces the types.
- **Optimization stage wiring (M2, CLI)**:
  - `wh optimize [--node X] [--quick] [--variants K] [--rounds M] [--no-mutation]` runs `wh_perf::optimize`. Without `--node`, it takes every `optimize` hole, hottest first (`wh_perf::profile::hot_holes` over the scenario tests). Winners refresh the synthesis cache.
  - `wh build --release [--quick] [--update-baselines] [--pgo]` enforces the performance gates (`bench_project`) and the mutation gates, and optionally runs PGO (`cargo-pgo`; skipped with a note when not installed).
  - Synthesis rejects a re-synthesized hole that regresses against its committed baseline. The check is `wh_perf::check_against_baseline`, injected through `Synthesizer::run_with(jobs, hook)`, and the regression is fed back to the agent.
  - **Verified with real Claude:** a tournament made `signup.dedupe_emails` 1.23× faster for $0.61.
  - **Found:** the winning variant traced a `sensitive` email. Synthesis and optimize prompts now forbid it, and wh-perf rejects such variants (`Stage::Privacy`).
- **Mutation gate (M2, verified)**: cargo-mutants 27.1 runs per hole in `.wh/mutants/<node>/`.
  - The impl root is relative to the generated crate (`wh_impl`). Codegen then declares hole modules at the top level, because rustc and cargo-mutants resolve `#[path]` inside inline modules differently.
  - The `--file` filter is the glob `**/wh_impl/<file>`, because cargo-mutants keeps the unnormalized module path.
  - **Real run on signup**: `validate` 100% (6/6). `check_age` failed at 75%; the surviving mutant `<` → `<=` exposed an untested boundary (age 13), and a boundary example brought it to 100%. `normalize_email` had a single unviable mutant, so it gets no score.
  - Failing gates list the surviving mutants.
