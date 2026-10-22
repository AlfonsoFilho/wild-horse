# wh-perf — optimization stage (M2)

This file documents the API and the exact wiring the main session should add to `wh-cli` / `wh-synth`,
plus the requests for other crates. `wh optimize` and the `wh build --release` gates are wired (main session);
the second batch (profiles, synthesis-gate check, PGO) is described under "Batch 2" below and is not wired yet.

`cargo test -p wh-perf` runs 10 unit tests and 6 end-to-end tests on scratch copies of `examples/signup` in
`target/wh-perf-tests/<name>` (own target dir `target/wh-perf-tests/target`). The first run compiles
criterion, proptest and the generated crates (about 1–2 min); warm runs take about 45 s.

## Layout on disk

| path | what |
|---|---|
| `<project>/.wh/bench/` | bench crate for `bench_project` (package `wh_bench_<crate>_<project id>`, path dep on `.wh/gen`) |
| `<project>/.wh/bench-runs/latest/` | criterion output (`CRITERION_HOME`) + `allocs.tsv` of the last `bench_project` run |
| `<project>/.wh/perf/<node-slug>/{impl,gen,bench}` | tournament sandbox: copy of `impl/`, a generated crate over it, a bench crate for the node |
| `<project>/.wh/perf-runs/<node-slug>/<label>/` | criterion output per tournament candidate (`incumbent`, `r1v1`, …) |
| `<project>/.wh/mutants/<node-slug>/` | cargo-mutants workspace (+ `mutants.out/`) |
| `<project>/.wh/perf-check/<node-slug>/{gen,bench,run}` | `check_against_baseline` workspace |
| `<project>/.wh/profile/trace-<i>.jsonl` | traces of the `[perf] workload` app runs (`hot_holes`) |
| `<project>/.wh/pgo-runs/{before,training,after}/` | criterion output of `pgo_build` |
| `<project>/impl/.bench/<node>.json` | **committed** baselines: `{ node, entries: { <fingerprint key>: BaselineEntry } }` |

All of `.wh/` is ignored by the repository's `**/.wh/*` rule; `impl/.bench/` is meant to be committed.

## API (all result types are `Serialize + Deserialize` for the IDE)

```rust
// measure every benchmarkable hole (or opts.nodes), evaluate gates, compare with / record baselines
wh_perf::bench_project(&project, &model, &PerfOptions) -> Result<PerfReport>
PerfReport { fingerprint, nodes: Vec<NodePerf>, skipped: Vec<Skipped> }   // .failures(), .passed()
NodePerf { node, impl_hash, measurements, complexity: Option<ComplexityFit>, gates: Vec<GateResult>,
           baseline: Option<Comparison>, baseline_recorded }              // .failures()
PerfOptions { bench: BenchConfig, target_dir, noise_threshold: 0.03, alpha: 0.05,
              nodes: Option<Vec<String>>, baselines: BaselinePolicy::{Never, IfMissing (default), Always} }
wh_perf::perf_nodes(&project, &model) -> Vec<String>   // holes with complexity/budget/alloc/bench/optimize or a baseline

// the tournament
wh_perf::optimize(&project, &model, node, &dyn wh_synth::Driver, &OptimizeOptions) -> Result<OptimizeReport>
OptimizeOptions { variants: 3, rounds: 2, bench: BenchConfig::default(), noise_threshold: 0.03, alpha: 0.05,
                  target_dir: None, mutation: true, update_baseline: true,
                  on_progress: Option<Arc<dyn Fn(OptimizeProgress) + Send + Sync>> }
OptimizeReport { node, target, fingerprint, incumbent: Candidate, candidates: Vec<Candidate>,
                 accepted: Option<String> /* label, e.g. "r1v3" */, speedup: Option<f64>, baseline: Option<PathBuf>, notes }
Candidate { label, round, variant, code, impl_hash, measurements, complexity, gates, comparison, mutation,
            verdict: Option<VariantVerdict::{Winner, RunnerUp, Rejected { stage: Stage, reason }}>, transcript }
Stage::{Sandbox, Compile, Privacy, Tests, Benchmark, Gates, Mutation, NotFaster}

// the mutation gate
wh_perf::mutation_score(&project, node) -> Result<MutationReport>                 // loads the model itself
wh_perf::mutation_score_with(&project, &model, node, &MutationOptions) -> Result<MutationReport>
MutationReport { node, status: MutationStatus, gate: Option<GateResult> }        // gate = the node's `mutate` clause
MutationStatus::{NotInstalled { hint }, NoMutants { detail }, Scored { caught, missed, timeout, unviable, score, missed_mutants }, Failed { reason }}

// lower-level pieces
wh_perf::generate_bench_crate(&model, &BenchCrateOptions) -> Result<BenchCrate>
wh_perf::run_benches(&BenchCrate, target_dir, nodes: Option<&[String]>, &BenchConfig, out_dir) -> Result<Vec<Measurement>>
wh_perf::gates::{evaluate(&PerfSpec, &[Measurement], sized), compare(before, after, Target, threshold, alpha)}
wh_perf::baseline::{fingerprint(), load(root, node, &fp), store(root, node, entry), regression(&entry, &current, threshold, alpha)}
wh_perf::PerfSpec::of(&model, node)    // optimize / complexity / budget / alloc / mutate / bench / no_panic clauses
BenchConfig::default()  // sizes 64..16384, warm-up 1 s, measurement 3 s, 50 samples
BenchConfig::quick()    // sizes 64..4096, 0.2 s / 0.4 s, 10 samples (tests, IDE idle loop)
```

### What is measured and how

- **Benches**: one criterion group per hole (`group = node id`, `function = auto | <bench clause name>`,
  `parameter = n`). The auto-bench draws inputs with proptest strategies of exactly size `n` (collections get `n`
  elements, `str` gets `n` chars; integers get the value `n` only when no parameter has a length) from a
  deterministic RNG seeded by `n`. A def without any sized parameter runs at one size (`n = 1`) and gets no
  complexity fit. `own`/`mut` parameters use `iter_batched` with a clone per iteration. `bench` clauses:
  `bench big(xs: list[Email]): dedupe_emails(xs)` sweeps `xs`; an `int` parameter without default is the size `n`;
  an `int` parameter with a literal default (`n: int = 10_000`) fixes the size. Budget sizes are always added to the
  sweep. Skipped (reported in `skipped`): defs with `uses` (capabilities), generic defs, recursive or function-typed
  parameters.
- **Allocations**: the bench crate's `#[global_allocator]` counts allocations and bytes (realloc growth counted)
  for one call per size, outside the timed loop.
- **Complexity**: weighted least squares (relative error) of `t ≈ a + b·f(n)` for O(1), O(log n), O(n),
  O(n log n), O(n²), O(n³); the simplest class within 5 % RMS of the best fit wins. The gate fails only if the class
  exceeds the declared one *and* the log-log slope over the larger sizes exceeds the declared class's slope + 0.3
  (O(n) vs O(n log n) can't be told apart reliably over a few sizes; O(n²) vs O(n) can).
- **Significance**: Welch's t-test on criterion's per-sample times. A benchmark is improved or regressed only if
  the change exceeds the noise threshold (3 %) *and* p < α (0.05). A variant wins only if the geometric mean of the
  ratios improves by more than the threshold, at least one benchmark is significantly better and none is
  significantly worse (so "faster on small inputs, slower on big ones" loses). `optimize memory` compares allocated
  bytes; `latency` uses medians; `size` is rejected as not measured yet.
- **Tournament**: incumbent measured first; each variant starts from the incumbent's code in the sandbox; the
  sandbox tree digest (everything except the hole file, `target/` and `Cargo.lock`) must not change; then signature
  check → clippy (errors anywhere, warnings in the hole file) → `Cargo::test(Some(node))` → benches → gates
  (`complexity`, `budget`, `alloc`) → significance vs incumbent → `mutate` gate (if declared and installed). Best
  variant of a round becomes the incumbent of the next; stops after a round without a winner. The winner is
  written to `impl/` as `// wh:key=<incumbent's key>\n<code>` and its measurements become the baseline. Without a
  winner a missing baseline is recorded for the incumbent.
- **Mutation**: `cargo mutants --in-place --dir <ws>/gen --file wh_impl/<module>/<hole>.rs --output <ws>`, where
  the workspace crate is generated with its impl root *inside* the crate (`gen/wh_impl/`) so cargo-mutants can see
  the hole file. Score = (caught + timeout) / (caught + missed + timeout). cargo-mutants is not installed on this
  machine, so only the "not installed" path and the `outcomes.json` parser are tested; the real invocation is
  untested (in particular whether cargo-mutants follows codegen's absolute `#[path = "…"]` module attributes — if
  it reports no mutants, codegen would need relative `#[path]`s for this workspace, see requests below).

## Wiring for the main session

### `wh optimize [node]` (wh-cli)

```rust
let project = wh_project::Project::load(dir)?;
let model = project.model()?;            // refuse on model errors, as `wh build` does
let driver = /* the compiler-role AcpDriver, exactly as `wh synth` builds it */;
let nodes = match node {
    Some(n) => vec![resolve_node_arg(&model, n)?],   // accept `dedupe_emails` or `signup.dedupe_emails`
    // no node: every hole with `optimize`, hottest first once profile weights exist (design doc); for now
    None => model.nodes.values().filter(|n| n.is_hole() && wh_perf::PerfSpec::of(&model, &n.id).is_some_and(|s| s.optimize.is_some())).map(|n| n.id.clone()).collect(),
};
let opts = wh_perf::OptimizeOptions {
    on_progress: Some(std::sync::Arc::new(|p| eprintln!("{p:?}"))),   // or a nicer renderer
    ..Default::default()
};
for n in nodes {
    let r = wh_perf::optimize(&project, &model, &n, &driver, &opts)?;
    // print: incumbent table (r.incumbent.measurements: size, mean ± CI, allocs), one line per candidate
    // (label, verdict/stage + first line of reason, comparison.summary), then accepted/speedup/baseline/notes
}
```

After an accepted variant, also refresh the synthesis cache so a later restore gets the optimized code:
`cache.put(&SynthCache::key(&key, &hole), &locked_text, &meta)` — or simply call `wh_synth::seed_cache` after
`optimize` (it only adds missing keys, so the main session may want an overwrite variant there).

### `wh build --release` (wh-cli)

After synthesis and a successful release build:

```rust
let nodes = wh_perf::perf_nodes(&project, &model);
if !nodes.is_empty() {
    let report = wh_perf::bench_project(&project, &model, &wh_perf::PerfOptions {
        nodes: Some(nodes),
        baselines: wh_perf::BaselinePolicy::IfMissing,
        ..Default::default()
    })?;
    for s in &report.skipped { /* warn: node, reason */ }
    for (node, msg) in report.failures() { /* node-mapped error */ }
    if !report.passed() { bail!("performance gates failed") }   // perf regression = build failure in --release
}
```

Consider `--quick` (→ `BenchConfig::quick()`) and `--update-baselines` (→ `BaselinePolicy::Always`) flags.
For `mutate` clauses, `wh build --release` can call `wh_perf::mutation_score_with(&project, &model, node, &Default::default())`
for every hole whose `PerfSpec` has `mutate`, failing on `gate.status == Fail` and warning on `NotInstalled`
(print `wh_perf::mutation::INSTALL_HINT`; never install it automatically).

### wh-synth (verification gate, design doc step "(if complexity/bench) regression check")

Use `wh_perf::check_against_baseline` (Batch 2 below) from the callback: call it with the job's scratch `impl/`
after the correctness gate accepts a round, and reject the round with the returned messages when they are
non-empty. `wh-perf` depends on `wh-synth`, so the callback has to be installed by `wh-cli`.

### IDE

`PerfReport`, `NodePerf`, `OptimizeReport`, `MutationReport`, `OptimizeProgress` serialize to JSON; the inspector
can show `measurements` (mean + CI, allocs) vs `baseline`, `gates`, `complexity.class`/`exponent`, and per-candidate
verdicts.

## Regression rule (revised after the first `wh build --release --quick` runs)

A false regression (unchanged `dedupe_emails`, ×1.024 geomean, one size ×1.07 with 10 samples) failed a build.
Now:
- `Comparison::regressed` needs **both** the geometric mean worse than `1 + threshold` **and** at least one
  benchmark significantly worse. A single significantly worse benchmark only sets the new
  `Comparison::worse_somewhere` (summary: "slower at some sizes, not overall"). The tournament still vetoes a
  variant that is `worse_somewhere`.
- `baseline::regression(baseline, current, current_cfg, threshold, alpha)` (signature changed: the current
  `BenchConfig` is a parameter) widens the threshold to `baseline::QUICK_THRESHOLD` (10%) when either side
  `baseline::is_quick` (fewer than 20 samples or under 1 s of measurement). `BaselineEntry::config` was
  already stored.
- `bench_project` and `check_against_baseline` re-measure once when the first run regresses, and report the
  second run (gates are evaluated on it too). `NodePerf::notes` (new, `#[serde(default)]`) says whether the
  regression was confirmed, and also when the baseline was measured with different settings.
  `check_against_baseline` measures with the baseline's own `BenchConfig`, so it always compares like with like.

## PGO prerequisites

`pgo_build` reports `PgoStatus::NotInstalled { hint }` with `pgo::INSTALL_HINT` when cargo-pgo is missing, and with
`pgo::LLVM_TOOLS_HINT` ("cargo-pgo needs `llvm-profdata`: rustup component add llvm-tools-preview") when cargo-pgo
is installed but `llvm-profdata` is not found (`$LLVM_PROFDATA`, the toolchain's llvm-tools component, or `PATH`;
see `pgo::llvm_profdata()`). That second case is the current state of this machine.

## Mutation workspace

The main session changed `mutation.rs` to use `GenOptions { impl_root: PathBuf::from("wh_impl"), .. }` (relative,
resolved by codegen against the generated crate), because cargo-mutants rejects absolute `#[path]`s; keep it.
Request 3 below is done. The main session also made `--file` the glob `**/wh_impl/<module>/<hole>.rs`, because
cargo-mutants names files by their unnormalized module path (`src/modules/../../wh_impl/…`); keep it. A run whose
filter matches nothing is now `MutationStatus::NoMutants` ("cargo-mutants found no mutants matching `--file …`"),
not `Failed`. The end-to-end test `mutation_gate_scores_a_hole` adds `mutate score >= 0.5` to `signup.validate` in a
scratch copy, runs the real gate when cargo-mutants is installed (about 10 s), and fails if no mutant is found. I
checked that it fails with the old `wh_impl/<rel>` filter.

## Units

Every time value in results is **nanoseconds per call** (`f64`): `Estimate { point, lower, upper }` (mean and median
of a `Measurement`, with the 95% CI), `Measurement::std_dev`/`samples`, `Delta::{before, after}` for time targets
(allocated bytes + 1 for `optimize memory`), `Budget::time_ns`, `HoleWeight::{self_ns, total_ns}`. Helpers:
`Estimate::{point_duration, lower_duration, upper_duration, display}`, `measure::ns_duration`, `measure::fmt_ns`.

## Sensitive values (`src/privacy.rs`)

`wh_perf::privacy::sensitive_logging(&model, node, code) -> Vec<Leak>` — heuristic: `tracing` macros
(`trace!`…`error!`, `*_span!`, `event!`, with or without `tracing::`) must not record
- parameters marked `sensitive`, or whose type contains a sensitive type (`@sensitive` types and the types of
  `sensitive` fields — but not structs/enums, which redact their own sensitive fields in `Debug`);
- values derived from them through `for … in`, closures over them, `let x = …` and collections they are pushed or
  inserted into;
- `sensitive` fields read as `.field`;
- including inline format args (`"{email}"`). Lengths/counts (`.len()`, `.is_empty()`, `.count()`, `.capacity()`)
  and comparisons/predicates (`a != b`, `.contains(…)`, `.is_…()`) are fine.

On the signup impls it flags exactly the `trace!(email = %email, …)` in `dedupe_emails` (the example still has it).
In the tournament a leaking variant is rejected at `Stage::Privacy` before any build; a leaking incumbent is listed in
`OptimizeReport::notes` and in the prompt; the prompt carries the synthesis rule verbatim. It is text-level (no type
inference): a value laundered through a helper function or a struct field it was moved into is not tracked.
**Request (wh-synth)**: the synthesis gate could use the same check, but wh-synth cannot depend on wh-perf (wh-perf
depends on wh-synth). Either move `privacy.rs` into wh-synth (it only needs `wh_model`; wh-perf would re-export it)
or call it from wh-cli through the verification callback.

## Batch 2: profiles, the synthesis-gate check, PGO

### Profile-weighted prioritization (`src/profile.rs`)

```rust
wh_perf::hot_holes(&project, &model, app: Option<&str>) -> Result<Profile>       // workload from wildhorse.toml
wh_perf::hot_holes_with(&project, &model, app, &ProfileOptions) -> Result<Profile>
Profile { workload: Vec<String>, holes: Vec<HoleWeight> /* heaviest first */, nodes: Vec<NodeTime>, total_ns, notes }
HoleWeight { node, calls, self_ns, total_ns, share /* self time / all traced time */ }
ProfileOptions { scenarios: true, app_runs: Vec<Vec<String>>, level: "debug", target_dir, timeout_secs: 900 }
ProfileOptions::from_project(&project)                // reads the optional [perf] table:
//   [perf]
//   workload = [["a@b.io", "30"], ["kid@x.io", "12"]]   # app runs (args without the program name)
//   level = "debug"                                       # tracing filter for the scenario tests
wh_perf::optimization_queue(&model, Option<&Profile>) -> Vec<String>   // holes with `optimize`, hottest first
```

- Workloads: the scenario tests (`cargo test --release --no-default-features --config profile.release.lto=false
  … -- __test__ --nocapture`, `WH_LOG=debug`, test-writer text parsed for `close time.busy=` lines) and, if
  `[perf] workload` is set, runs of the app binary (same build flags) with `WH_TRACE_FILE` (JSON). Both builds drop
  the `tracing/release_max_level_*` default feature, so hole spans exist in release. Results go to `.wh/profile/`.
- Weight = self time (span busy time minus the busy time of its child spans), summed per node; adapter method spans
  (`m.Adapter.method`) count for the adapter hole. Tracing overhead is included, so very small holes are
  over-weighted: the ranking orders work, it does not report timings.
- On `examples/signup` with the two runs above: validate 34 % (its regex compiles on first use), parse_cli 5 %,
  normalize_form 3 %, … dedupe_emails 0 % (nothing calls it), so the queue is just `[signup.dedupe_emails]`.

Wiring (`wh optimize` without a node, in wh-cli):

```rust
let profile = match wh_perf::hot_holes(&project, &model, None) {
    Ok(p) => Some(p),
    Err(e) => { eprintln!("warning: no profile ({e:#}); optimizing in id order"); None }
};
let nodes = wh_perf::optimization_queue(&model, profile.as_ref());
```

A `wh profile [app]` command printing `profile.holes` (share, calls, node) and `profile.notes` would be useful too.
The IDE can show `share` as a heat overlay on the flow graph.

### Performance check for the synthesis gate

```rust
wh_perf::check_against_baseline(&project, &model, node, impl_root: &Path, &PerfOptions) -> Result<Option<Vec<String>>>
```

- `Ok(None)`: no committed baseline for this machine's fingerprint (or the node cannot be benchmarked): skip.
- `Ok(Some(vec![]))`: passed. `Ok(Some(msgs))`: failed `complexity`/`budget`/`alloc` gates and/or a significant
  regression beyond `opts.noise_threshold` against the baseline — usable verbatim as repair feedback.
- Generates a crate over `impl_root` (e.g. the synthesis job's scratch `impl/` copy) in
  `.wh/perf-check/<node>/{gen,bench}` with unique package names, and measures with the **baseline's own
  `BenchConfig`** (only the timeout comes from `opts.bench`), so numbers are comparable. From `opts` it uses
  `target_dir`, `noise_threshold`, `alpha`, `bench.timeout_secs`.
- Cost: one release build of the scratch crate + bench crate and one benchmark run (a few seconds per node with
  `BenchConfig::quick()` baselines, ~20 s with the default config). Only call it for nodes that have a baseline
  (`wh_perf::baseline::load(&root, node, &wh_perf::fingerprint()).is_some()` is cheap).

### PGO (`src/pgo.rs`)

```rust
wh_perf::pgo_build(&project, &model, app: Option<&str>) -> Result<PgoReport>
wh_perf::pgo_build_with(&project, &model, app, &PgoOptions) -> Result<PgoReport>
PgoReport { status: PgoStatus::{NotInstalled { hint }, Done, Failed { step, reason }}, training: Vec<String>,
            before: Vec<Measurement>, after: Vec<Measurement>, comparison: Option<Comparison>,
            binaries: Vec<PathBuf>, notes }
PgoOptions::from_project(&project)   // bench: BenchConfig::default(), app_runs from [perf] workload, scenarios: true
wh_perf::pgo::INSTALL_HINT          // "cargo install cargo-pgo && rustup component add llvm-tools-preview"
```

Steps: benches normally ("before") → clear `<target>/pgo-profiles` → `cargo pgo build -- --bins` + the
`[perf] workload` runs of the instrumented app → `cargo pgo test -- --lib -- __test__` → `cargo pgo bench`
(quick config) → `cargo pgo optimize bench` ("after"; `comparison` = after vs before) → `cargo pgo optimize build
-- --bins` (`binaries`, in `<target>/<host triple>/release/`; install them with `Project::install_bins`).
cargo-pgo and llvm-tools are **not installed here**, so only the `NotInstalled` path is tested; the cargo-pgo argument
forwarding (`cargo pgo bench -- --bench wh_bench -- <criterion args>`) follows its README and is unverified.
Profiles match by symbol, so the bench numbers mostly reflect the bench training run and the app binary the app
runs (see the module docs).

Wiring (`wh build --release --pgo` in wh-cli): after the normal release build and gates,

```rust
let r = wh_perf::pgo_build(&project, &model, app)?;
match &r.status {
    wh_perf::PgoStatus::NotInstalled { hint } => eprintln!("PGO skipped: cargo-pgo is not installed ({hint})"),
    wh_perf::PgoStatus::Failed { step, reason } => bail!("PGO failed at {step}: {reason}"),
    wh_perf::PgoStatus::Done => {
        project.install_bins(&r.binaries)?;
        if let Some(c) = &r.comparison { println!("PGO: {}", c.summary); }
    }
}
```

Never install cargo-pgo automatically; print the hint and let the user decide.

## Requests for other crates (not done; worked around inside wh-perf)

1. **wh-synth**: make the sandbox helpers and the verification gate public, so the tournament reuses them instead of
   copies — `copy_tree`, `tree_digest`, `slug`, `squash`, and `Synthesizer::verify` as a free function, e.g.
   `pub fn verify_hole(hole: &HoleSpec, file: &Path, crate_dir: &Path, target: &Path, manifest: &Manifest) -> Result<Verdict>`.
   wh-perf has copies in `src/sandbox.rs` and `optimize::verify`.
2. **wh-codegen**: expose the strategy helper code of `TestGen` (the private `strategy_code`, e.g.
   `pub fn helpers(&self) -> &str`) so benches can reuse the property strategies verbatim; wh-perf has its own
   size-aware generator (`src/inputs.rs`) mirroring `TestGen::strategy` (regex refinements, `parse` filters,
   structs with `wh_check`, enums), because properties draw at most 8 elements and a sweep needs exactly `n`.
   (Numeric range refinements use filtering in wh-perf rather than codegen's range strategies.)
3. **Done** (relative impl roots in codegen). Was: **wh-codegen** (only if cargo-mutants turns out not to follow absolute `#[path]`s): an option in `GenOptions`
   to emit hole `#[path]`s relative to the generated crate.
4. **wh-codegen**: let `wh_rt::init_test_tracing` also write JSON events to `WH_TRACE_FILE` when it is set (same
   layer as `init_tracing`'s file layer). The profiler then reads scenario-test traces as JSON instead of parsing the
   test writer's text format (`profile::parse_text_close`, which relies on `wh.node="…"` appearing in span fields).
5. **wh-project**: make `live::parse_duration` public (wh-perf has a copy, `profile::parse_duration_ns`).

## Not done (M2 follow-ups)

- Sampled profiles of a hole's hot path (`samply`/`perf`) and `cargo asm` in the optimization prompt (the
  span-level weights of `hot_holes` exist; nothing below hole granularity).
- `optimize size` (code size); idle-time optimization in the IDE (it can call `optimization_queue` + `optimize`).
- Benchmarks for defs that use capabilities (would need the `InMemory` adapters as inputs) and generic defs.
- `no_panic` is parsed into `PerfSpec::no_panic` but not checked here (property tests + a link check belong in the
  correctness gate).
