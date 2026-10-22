# wh-live — live values (Quokka / Light Table style)

New crate, nothing else touched, nothing wired yet. `cargo test -p wh-live` runs 3 end-to-end tests (about 10 s warm) on scratch copies
of `examples/signup` and `examples/todo` in `target/wh-live-tests/<name>` (own target dir
`target/wh-live-tests/target`).

## API

```rust
wh_live::evaluate(&project, &model, LiveScope) -> Result<LiveReport>
wh_live::evaluate_with(&project, &model, &LiveOptions) -> Result<LiveReport>

LiveScope::{Node(id), File(project-relative path), All}
LiveOptions { scope, timeout_ms: 2000 /* per probe */, run_timeout_secs: 120, max_value_len: 160,
              samples: 3 /* per def, for ensures */, property_cases: 64, target_dir: None /* shared */ }

LiveReport {
    nodes: BTreeMap<String /* def id, or "module#test:name" */, Vec<LiveValue>>,   // source order
    steps: BTreeMap<String /* step id "m.flow#step[k]" */, Vec<StepSample>>,        // values seen on flow edges
    timings: Timings { generate_ms, probes_ms, build_ms, run_ms, total_ms, rebuilt, processes },
    build_errors: Vec<String>, notes: Vec<String>,
}
LiveReport::get(node, ItemKind, clause_index) -> Option<&LiveValue>;  LiveReport::values()

LiveValue {
    node, kind: ItemKind::{Example, Ensures, Property, Expect}, clause_index,
    file, span: SourceSpan { start, end /* byte offsets */, line, col /* 1-based */ }, source /* clause text */,
    status: Status::{Pass, Fail, Error, Timeout, Crashed, NotRun}, message: Option<String>,
    values: Vec<NamedValue { label, value, span: Option<SourceSpan> }>,
    samples: Vec<Sample { inputs: Vec<NamedValue>, result, holds: Option<bool>, error }>,   // Ensures
    steps: Vec<StepValue { step, value, error }>,                                            // flows it ran through
    cases: Option<u32> /* Property */, duration_us,
}
StepSample { value, error, node, kind, clause_index }   // which clause produced it
```

Everything is `Serialize + Deserialize`.

### What each clause gets

| clause | `values` | other |
|---|---|---|
| `example a == b` (also `!=`, `<`, `<=`, `>`, `>=`, `in`, `not in`) | `left`, `right`, each with the span of its sub-expression; a raising call on either side shows `raised <Err>` and fails | `steps` |
| `example call(...) raises E` | `result` (`raised Variant(..)` or the Ok value) | `steps` |
| other `example <bool>` | none (just the status) | `steps` |
| `ensures …` | none | `samples`: 3 generated inputs per def (named by parameter), the result, `holds` per clause; inputs violating `requires` are discarded |
| `property …` | on failure: the shrunk counterexample (one value per parameter, with the parameter's span), plus `left`/`right` at that counterexample when the body is a comparison | `cases` when it held |
| scenario `expect …` | as for examples (`Expect`, `clause_index` = index of the expect in the test); expects run in order against the same `given` capabilities | `steps` |

Status: `Error` = it panicked (contract violation, `todo!()` of an unsynthesized hole, …), with the panic message.
`Timeout` = the probe exceeded `timeout_ms`; `Crashed` = it took the probe process down (stack overflow, abort);
`NotRun` = not evaluated, with the reason (unsupported for sampling, probe failed to compile, an earlier expect of
the same test panicked, …).

### Flow steps

Step values come from the generated instrumentation: inside each `wh.step` span (`wh.node = "m.flow#step[k]"`), the
callee's hole wrapper emits `trace!(result = ?result)`; a failing step emits `debug!(wh.node = step, error = ?e)`.
So a step's value is recorded when its callee is a hole (its `Ok`/`Err` result), and step errors always. Steps whose
callee is a flow, a constructor or a combinator (`map`, `filter`, …) show the hole results inside them (for `map`,
one value per element) or nothing. See request 1 below for the precise fix.

### Sensitive values

Values are `Debug` strings: `@sensitive` types and `sensitive` struct fields redact themselves (generated `Debug`);
generated inputs of `sensitive` parameters (ensures samples, property counterexamples) are shown as `<redacted>`.
Literal values written in the spec are not secret, but they are shown through `Debug` too, so a literal of a
`@sensitive` type also shows `<redacted>`.

## How it works

- `.wh/live/` is a generated crate (package `wh_live_<crate>_<project id>`, `[workspace]`, path dependency on
  `.wh/gen`, `proptest`, `tracing`, `tracing-subscriber` (registry only), `serde_json`, `[profile.dev] debug = 0`) with
  one function per probe in `src/probes.rs` (all probes of the project, so changing the scope never rebuilds),
  `src/rt.rs` (runtime) and `src/main.rs`. Files are written only when their content changes.
- `cargo build` (debug: contracts are checked, as in the generated tests) in the shared target dir. If a probe does
  not compile, the probes named by the error spans are left out (`NotRun`, error text in `build_errors`) and the
  crate is rebuilt, up to twice; errors elsewhere (the library itself) end the evaluation with `build_errors`.
- The binary runs `--only <ids> --timeout-ms N --max-len N`. Each probe runs in its own thread (64 MiB stack) under
  `catch_unwind`, with a panic hook that records the message silently. Output: one JSON line per probe start/result.
  A probe that exceeds its timeout ends the process (a thread cannot be killed); a probe that crashes the process
  (stack overflow) is identified by its unfinished `start` line. The host restarts the binary for the remaining
  probes; `timings.processes` counts the starts. `run_timeout_secs` bounds the whole run.
- Ensures samples and property inputs use `wh_perf::inputs::InputGen` (the same strategies as the benches, small
  sizes); `given x from gen()` uses the given generator. Properties use a deterministic proptest runner with
  failure persistence off; panics with `requires violated` reject the case instead of failing it.

## Timings (signup scratch copy, Apple Silicon, this machine)

| situation | build | run | total (edit → values) |
|---|---|---|---|
| first evaluation (probe crate + `.wh/gen` lib compiled from a warm dependency cache) | 1.6–2.3 s | 0.4 s | 2.0–2.9 s |
| nothing changed, scope = one node | 0.06 s | 0.013 s | **0.09 s** |
| one hole edited, scope = that node | 0.9 s | 0.42 s | **1.4 s** |
| 3 probes hang + 3 crash (timeout 500 ms) | — | 1.2 s, 7 processes | — |

The probes themselves take 1–3 ms each; the whole signup suite runs in about 10 ms.

**The 0.4 s after a rebuild is macOS, not the probe.** The first execution of a newly written executable is
assessed by the system (about 0.4–0.65 s; later runs of the same file take 10 ms). Cargo re-creates the uplifted
`target/debug/wh_live_probe` on every build, even a no-op one, so wh-live runs the `deps/wh_live_probe-<hash>` file
instead. That file only changes when the probe is really relinked, which is what makes the unchanged case 0.09 s
instead of 0.5 s. After a real change the cost is unavoidable. On developer machines it disappears when the IDE
(or the terminal running `wh`) is listed under System Settings → Privacy & Security → Developer Tools.

### Hot reload (`cdylib` + `libloading`): measured, not adopted

I built the same probe crate as a `cdylib` in a scratch copy and timed four hole edits, alternating the order:

| | incremental build after a hole edit | loading the new code |
|---|---|---|
| probe binary (kept) | 0.52–1.23 s | exec of the new binary: 0.48–0.50 s |
| `cdylib` | 0.45–1.06 s | `dlopen` of the new dylib: 0.40–0.41 s |

Whichever crate built first after the edit was slower, so the builds are the same work: both recompile the generated
library and the probe crate, and linking is a small part of it (rebuilding only the probe crate takes 0.8 s). The load
cost is the same macOS first-load assessment for a new dylib as for a new executable. A long-running host would also
have to unload Rust dylibs, which is unreliable (TLS destructors, `dlclose` usually being a no-op on macOS), and would
lose process isolation for hangs and stack overflows. So wh-live keeps the incremental binary. The real costs are the
recompile of the generated library and its dependents (the lever is in codegen: smaller crates or more codegen
units per module) and the OS assessment of new executables.

## IDE integration (for the IDE sessions)

Call it from the live pipeline, off the UI thread:

```rust
// after a successful check of the current model (debounced, like `live::check`)
let opts = wh_live::LiveOptions { scope: wh_live::LiveScope::File(doc_path.clone()), ..Default::default() };
let report = wh_live::evaluate_with(&project, &model, &opts)?;   // blocking: run on a worker thread
```

- **Scope**: `File(<path of the open document>)` while editing; `Node(id)` for the inspector's selected node (a flow
  also gets the scenario tests of its module that mention it); `All` for a project-wide view. Changing the scope never
  rebuilds (all probes are compiled in), so switching documents costs ~0.1 s.
- **Concurrency**: one evaluation per project at a time (they share `.wh/live/` and cargo's lock). Keep a generation
  counter and drop reports older than the newest model; there is no cancellation. It competes with `wh build` and
  `cargo check` for cargo's build lock, so start it after the check of the same model finishes.
- **Inline values next to clauses**: for each `LiveValue` in `report.nodes`, anchor at `file` + `span` (byte offsets
  into the source text as the model read it, plus 1-based line/col). Status badge: Pass ✓ / Fail ✗ / Error ⚠ (with
  `message`) / Timeout ⏱ / Crashed / NotRun (greyed, `message` as tooltip).
  - `values`: each `NamedValue` with a `span` belongs right after that sub-expression. Typical: render `left` after the
    left operand and `right` after the right one, e.g. ``example normalize_email("A@B.com") ⟨Email("a@b.com")⟩ ==
    "a@b.com"``. A failing comparison should show both prominently.
  - `Property`: on failure, `values` holds the counterexample (labels = parameter names, spans = the parameters in
    the property header) and possibly `left`/`right` without spans (render them after the body); on success
    `cases` = 64.
  - `Ensures`: `samples` (inputs, result, holds) fit a hover table; the badge summarizes.
  - `Expect` (scenario tests): node id `module#test:<name>` (the same id the test manifest uses); `clause_index` = the
    expect's position in the test. Spans point into the test block.
- **Flow edges**: `report.steps` maps a step id (`signup.register#validate[0]`, the same ids as
  `wh_model::FlowInfo.steps[].id` and the graph's step nodes) to the values seen there. Show the last value on the
  edge leaving that step and list all of them (with the clause that produced each, `StepSample::{node, kind,
  clause_index}`) in a tooltip; `error: true` → draw the edge as failed. `LiveValue::steps` has the same values in
  execution order per clause, for highlighting the path one expectation took.
- **Staleness**: values describe the model that was evaluated. Spans become stale as the user types; hide or dim
  values whose clause text (`LiveValue::source`) no longer matches the text at the span.
- **Sensitive data**: values come from generated `Debug` impls, so `@sensitive` types and `sensitive` fields are
  already `<redacted>`; do not try to recover values from elsewhere.
- **Timeouts**: defaults are 2 s per probe and 120 s per evaluation; for a typing-speed loop consider
  `timeout_ms: 500`. A hung probe costs at most its timeout plus a process restart (~10 ms, plus the first-exec cost if
  the binary was just rebuilt).
- **Build errors**: `report.build_errors` (rendered rustc messages). A probe that fails to compile is left out and its
  clauses are `NotRun`; if the generated library itself does not compile, `nodes` is empty and the IDE should show
  its regular check diagnostics instead.

## Requests for other crates

1. **wh-codegen — step values**: emit the value of every flow step, e.g. right after `let wh_sN = …;`:
   `tracing::trace!(wh.node = "<step id>", value = ?wh_sN, "step value");` (generated `Debug` already redacts). The
   probe's tracing layer already records `value` events whose `wh.node` is a step id, so this needs no change in
   wh-live. Today a step's value is known only when its callee is a hole (the wrapper's `trace!(result = ?result)`),
   so steps calling flows, constructors or combinators (`map`, `filter`, …) show nothing, or one value per element.
   Async flows open no `wh.step` spans at all.
2. **wh-codegen — sensitive value in `InvalidValue::new`**: the runtime logs
   `tracing::warn!(wh.node = node, rule, value = %value, "invalid value")` with the raw value of a refinement that
   failed. For an `@sensitive` refined type that writes the secret into traces; it should use the type's redacted
   `Debug` (or omit the value).
3. **wh-codegen — strategies**: same as wh-perf's request 2. wh-live samples `ensures` inputs and property inputs with
   `wh_perf::inputs::InputGen`, which mirrors `TestGen::strategy`; reusing TestGen's helpers would keep generated
   inputs identical to the tests'.
