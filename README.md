# Wild Horse

An AI-first development environment for Rust where you write **architecture, not implementation**.

Programs are literate Markdown documents (`*.wh.md`). Prose explains the intent. Fenced `wh` blocks, in a
Python-like language, state types, pure function signatures, contracts, examples, properties, effects (capabilities),
data flows and design patterns, but never function bodies. ACP agents (Claude via `claude-agent-acp`) play two
roles:

- **Compiler agent:** a stage of the compiler that writes the Rust body of every function "hole". Its output must
  pass the compiler, lints, contracts and specs before it is accepted, and an optional optimization tournament can
  make it faster.
- **Author agent:** a collaborator that edits the program itself, through structured operations.

Deterministic code generation produces everything else: types, contract-checking wrappers, flows, tests,
benchmarks, tracing and the app's `main`.

```wh
def normalize_email(e: Email) -> Email:
    """Canonical form used for uniqueness checks."""
    ensures result == e.lower()
    example normalize_email("A@B.com") == "a@b.com"
    property idempotent(x: Email):
        normalize_email(normalize_email(x)) == normalize_email(x)
    pattern Transform

flow register(form: SignupForm) -> UserId raises RegisterError uses Db:
    form
    |> validate
    |> normalize_form
    |> check_age
    |> save_user
```

## Quick start

Requirements:
- Rust (edition 2024 toolchain).
- Node.js (for `npx`).
- A Claude sign-in for the agents: run `claude auth login`, or put `ANTHROPIC_API_KEY` in `[agent.*] env` in `wildhorse.toml`.
- Optional: `cargo-mutants` (mutation gates) and `cargo-pgo` plus `rustup component add llvm-tools-preview` (profile-guided optimization).

```bash
cargo build --release -p wh-cli -p wh-ide        # binaries: target/release/wh, target/release/wh-ide
wh new hello && cd hello                          # a literate starter project
wh check                                          # parse + type/contract/flow checks
wh build                                          # synthesize holes (cache first), build, install to .wh/bin
wh test                                           # examples, properties and scenario tests, per node
wh-ide .                                          # the IDE
```

The IDE shows synchronized, editable views of the same document:
- the document, with prose and code blocks;
- the flow graph;
- the data model;
- the inspector;
- the agent panel;
- the log and run-trace panels.

Typing, graph gestures, inspector fields and agent edits are all transactions on one document store, so ⌘Z undoes
any of them.

## Commands

| Command | What it does |
|---|---|
| `wh new <dir>` | Create a starter project |
| `wh check [--lints]` | Diagnostics for the literate sources |
| `wh gen [--impl-root DIR] [--out DIR]` | Generate the Rust crate (skipped when nothing changed) |
| `wh status` | Each hole: missing / up to date / stale / soft-stale (prose changed) / pinned / manual |
| `wh synth [--node X] [--force] [--no-cache]` | Synthesize holes: dependency-ordered and parallel, verified, locked in `impl/` |
| `wh build [--release] [--frozen] [--allow-holes] [--quick] [--update-baselines] [--pgo]` | Synthesize what is stale, build and install. `--release` adds performance and mutation gates; `--frozen` never calls an agent (CI) |
| `wh test [--node X]` | Run the specs, results per node |
| `wh optimize [--node X] [--quick] [--variants K] [--rounds M]` | The variant tournament: faster code that still passes every spec |
| `wh live [--node X] [--steps] [--json]` | Live values: every example, ensures sample and property evaluated with the current code, plus the value on each flow step |
| `wh ask "<instruction>" [--node X]` | Ask the author agent to change the program (through the `wh` MCP tools) |
| `wh mcp [--project DIR \| --bridge SOCKET]` | The MCP server that author agents use |

## How it fits together

| Crate | Role |
|---|---|
| `wh-syntax` | Lexer, indentation-aware parser and AST with exact spans for `.wh.md` documents |
| `wh-model` | Semantic model and checker: names, types, purity, flows, contracts, machines, patterns, literate lints |
| `wh-doc` | The document store: transactions with origins, global undo/redo, three-way merge |
| `wh-edit` | Structured edit operations (graph, inspector and agent gestures) → minimal text edits |
| `wh-codegen` | Deterministic Rust generation: types, contract wrappers, flows, tests, tracing, apps |
| `wh-build` | Runs cargo and maps compiler diagnostics and test results back to nodes |
| `wh-agent` | ACP client: agent processes, sessions, sandbox policies, permissions, streaming |
| `wh-synth` | The compiler-agent stage: synthesis keys, sandboxed jobs, verification gate, repair rounds, cache, budgets |
| `wh-perf` | Benchmarks, complexity/budget/alloc gates, baselines, the optimization tournament, mutation scores, PGO |
| `wh-author` | The author agent |
| `wh-mcp` | The `wh` MCP server: semantic tools for the author agent |
| `wh-project` | Projects on disk, the live pipeline, the shared cache and target dirs, `wh new` |
| `wh-targets` | Target profiles beyond native CLI (server, web), work in progress |
| `wh-live` | Live, Quokka-style values for examples and flow steps, work in progress |
| `wh-graph` | Flow-graph and data-model projections with layered layout |
| `wh-cli` | The `wh` command |
| `wh-ide` | The GPUI IDE (gpui-kit) |

On disk, a project is:
- `wildhorse.toml`;
- `src/*.wh.md`, the program;
- `impl/`: the synthesized hole bodies, committed. Each carries a `// wh:key=…` lock header, so `impl/` is the
  lockfile;
- `impl/.bench/`: performance baselines, committed;
- `.wh/`: generated and derived state, ignored except `layout.json`.

Synthesized implementations are also kept in a per-user, content-addressed cache (`~/.cache/wild-horse`), and one
shared cargo target dir is used for all projects.

## Examples

| Example | What it shows |
|---|---|
| `examples/signup` | Refinements, contracts, error conversion, capabilities and adapters, a pipeline flow, scenario tests, tracing with `sensitive` redaction. Fully synthesized by Claude |
| `examples/todo` | A CLI todo list with a JSON file store, `match` routing and properties. Fully synthesized by Claude ($1.07 for the last 5 holes) |
| `examples/pong` | A terminal Pong: a pure `step`/`render` core, a crossterm terminal adapter, a fixed-timestep game loop. Fully synthesized by Claude ($4.01) |

## Documentation

- [`docs/language-spec.md`](docs/language-spec.md): the language specification.
- [`docs/design.md`](docs/design.md): the design, milestones, implementation notes and API changes.
- [`docs/ux-review.md`](docs/ux-review.md): UX review of the IDE, with a style guide and a prioritized roadmap.
- `crates/*/NOTES.md`: per-crate notes (APIs, findings, open items).

## Status

Milestone 1 (the literate language, synchronized IDE views, synthesis with verification, and the author agent) and
milestone 2 (the optimization stage, mutation gates, and MCP-based authoring) are implemented. They are exercised
end to end with real Claude on the three examples.

In progress:
- running builds from the IDE;
- the UX review's roadmap;
- server and web targets;
- live values.

See `docs/design.md` for details and known gaps.
