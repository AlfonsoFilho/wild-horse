//! The optimization tournament (design doc, "Optimization stage (M2)"):
//!
//! ```text
//! measure incumbent → agent proposes K variants → verify each → benchmark → keep fastest → lock
//! ```
//!
//! Each variant is written by a [`Driver`] in a sandboxed scratch copy of
//! `impl/` (only the hole file may change), must pass the full correctness
//! gate (clippy, the node's examples, contracts and properties) and the
//! node's performance gates, and replaces the incumbent only when the
//! improvement on the target metric is statistically significant and beyond
//! the noise threshold.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use wh_codegen::{GenOptions, HoleSpec, Manifest};
use wh_model::{Model, NodeKind};
use wh_project::Project;
use wh_synth::{Driver, KEY_PREFIX, SynthesisRequest};

use crate::baseline::{self, BaselineEntry, Fingerprint};
use crate::benchgen::{BenchCrate, BenchCrateOptions, generate_bench_crate};
use crate::gates::{self, Comparison, GateResult, GateStatus};
use crate::measure::{BenchConfig, Measurement, fmt_ns, run_benches};
use crate::mutation::{MutationOptions, MutationReport, MutationStatus, mutation_score_with};
use crate::sandbox::{copy_tree, slug, squash, strip_key, tree_digest};
use crate::spec::{PerfSpec, Target};
use crate::stats::ComplexityFit;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum OptimizeProgress {
    Measuring { node: String, label: String },
    Attempt { node: String, round: usize, variant: usize },
    Verifying { node: String, label: String },
    Benchmarking { node: String, label: String },
    Verdict { node: String, label: String, accepted: bool, reason: String },
    Done { node: String, accepted: Option<String> },
}

#[derive(Clone)]
pub struct OptimizeOptions {
    /// Variants per round (K).
    pub variants: usize,
    /// Rounds (M); stops early when a round brings no improvement.
    pub rounds: usize,
    pub bench: BenchConfig,
    /// Minimum relative improvement (and largest tolerated regression).
    pub noise_threshold: f64,
    /// Significance level of the t-tests.
    pub alpha: f64,
    /// Cargo target dir (default: the project's shared one).
    pub target_dir: Option<PathBuf>,
    /// Check `mutate` clauses on candidates (when cargo-mutants is installed).
    pub mutation: bool,
    /// Record the winner (or a missing incumbent) baseline in `impl/.bench/`.
    pub update_baseline: bool,
    pub on_progress: Option<Arc<dyn Fn(OptimizeProgress) + Send + Sync>>,
}

impl Default for OptimizeOptions {
    fn default() -> Self {
        Self {
            variants: 3,
            rounds: 2,
            bench: BenchConfig::default(),
            noise_threshold: 0.03,
            alpha: 0.05,
            target_dir: None,
            mutation: true,
            update_baseline: true,
            on_progress: None,
        }
    }
}

impl std::fmt::Debug for OptimizeOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OptimizeOptions")
            .field("variants", &self.variants)
            .field("rounds", &self.rounds)
            .field("bench", &self.bench)
            .field("noise_threshold", &self.noise_threshold)
            .field("alpha", &self.alpha)
            .field("target_dir", &self.target_dir)
            .field("mutation", &self.mutation)
            .field("update_baseline", &self.update_baseline)
            .finish_non_exhaustive()
    }
}

/// Where a variant was stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Stage {
    /// The agent failed or wrote outside the hole file.
    Sandbox,
    /// Signature, compiler or lint gate.
    Compile,
    /// Records sensitive values in traces ([`crate::privacy`]).
    Privacy,
    /// Examples, contracts or properties.
    Tests,
    /// The benchmarks failed to build or run.
    Benchmark,
    /// `complexity`, `budget` or `alloc`.
    Gates,
    Mutation,
    /// Correct, but not significantly better than the incumbent.
    NotFaster,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum VariantVerdict {
    /// Replaced the incumbent.
    Winner,
    /// Significantly better, but another variant of its round was better still.
    RunnerUp,
    Rejected { stage: Stage, reason: String },
}

/// One implementation measured during the tournament.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Candidate {
    /// `incumbent`, or `r<round>v<variant>`.
    pub label: String,
    pub round: usize,
    pub variant: usize,
    pub code: String,
    pub impl_hash: String,
    pub measurements: Vec<Measurement>,
    pub complexity: Option<ComplexityFit>,
    pub gates: Vec<GateResult>,
    /// Against the incumbent of its round.
    pub comparison: Option<Comparison>,
    pub mutation: Option<MutationReport>,
    pub verdict: Option<VariantVerdict>,
    pub transcript: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OptimizeReport {
    pub node: String,
    pub target: Target,
    pub fingerprint: Fingerprint,
    /// The implementation the tournament started from.
    pub incumbent: Candidate,
    pub candidates: Vec<Candidate>,
    /// Label of the implementation now locked in `impl/` (None: unchanged).
    pub accepted: Option<String>,
    /// Overall improvement factor of the accepted variant (×, > 1 is better).
    pub speedup: Option<f64>,
    /// Baseline file written, if any.
    pub baseline: Option<PathBuf>,
    pub notes: Vec<String>,
}

impl OptimizeReport {
    pub fn winner(&self) -> Option<&Candidate> {
        self.accepted.as_deref().and_then(|l| self.candidates.iter().find(|c| c.label == l))
    }
}

struct Workspace {
    dir: PathBuf,
    impl_root: PathBuf,
    hole_file: PathBuf,
    gen_dir: PathBuf,
    gen_opts: GenOptions,
    manifest: Manifest,
    bench: BenchCrate,
    runs: PathBuf,
    target: PathBuf,
}

fn progress(opts: &OptimizeOptions, p: OptimizeProgress) {
    if let Some(f) = &opts.on_progress {
        f(p);
    }
}

/// Run the tournament for one hole and lock the winner (if any) in `impl/`.
pub fn optimize(project: &Project, model: &Model, node: &str, driver: &dyn Driver, opts: &OptimizeOptions) -> Result<OptimizeReport> {
    let spec = PerfSpec::of(model, node).with_context(|| format!("`{node}` is not a def"))?;
    let target = spec.optimize.unwrap_or(Target::Speed);
    if target == Target::Size {
        bail!("`optimize size` is not measured yet; use speed, latency, throughput or memory");
    }
    let base = project.gen_options();
    let hole = wh_codegen::generate(model, &base).manifest.holes.into_iter().find(|h| h.node == node).with_context(|| format!("`{node}` is not a hole"))?;
    let current = std::fs::read_to_string(&hole.path).with_context(|| format!("`{node}` has no implementation yet; synthesize it first"))?;
    if current.contains("todo!(\"synthesize ") {
        bail!("`{node}` has no implementation yet; synthesize it first");
    }
    if current.lines().take(3).any(|l| l.trim() == wh_synth::PINNED) {
        bail!("`{node}` is pinned; unpin it to optimize it");
    }
    let key = match wh_synth::read_key(&current) {
        Some(k) => k.to_string(),
        None => wh_model::hash::spec_key(model, node, &driver.id()),
    };
    let ws = prepare(project, model, node, &hole, &base, opts)?;
    let fp = baseline::fingerprint();
    let mut notes = Vec::new();

    // the incumbent
    progress(opts, OptimizeProgress::Measuring { node: node.into(), label: "incumbent".into() });
    let mut incumbent = Candidate {
        label: "incumbent".into(),
        round: 0,
        variant: 0,
        code: current.clone(),
        impl_hash: baseline::impl_hash(&current),
        measurements: vec![],
        complexity: None,
        gates: vec![],
        comparison: None,
        mutation: None,
        verdict: None,
        transcript: String::new(),
    };
    std::fs::write(&ws.hole_file, &current)?;
    incumbent.measurements = run_benches(&ws.bench, &ws.target, Some(&[node.to_string()]), &opts.bench, &ws.runs.join("incumbent"))
        .context("benchmarking the current implementation")?;
    let sized = ws.bench.targets.iter().any(|t| t.node == node && t.sized);
    (incumbent.complexity, incumbent.gates) = gates::evaluate(&spec, &incumbent.measurements, sized);
    for l in crate::privacy::sensitive_logging(model, node, &current) {
        notes.push(format!("the current implementation records a sensitive value: {}", l.message()));
    }
    if let Some(g) = incumbent.gates.iter().find(|g| g.failed()) {
        notes.push(format!("the current implementation fails `{}`: {}", g.gate, g.detail));
    }
    let original = incumbent.clone();

    let mut best = incumbent;
    let mut candidates: Vec<Candidate> = Vec::new();
    let mut accepted: Option<String> = None;
    for round in 1..=opts.rounds.max(1) {
        let mut round_winner: Option<usize> = None;
        for variant in 1..=opts.variants.max(1) {
            let label = format!("r{round}v{variant}");
            progress(opts, OptimizeProgress::Attempt { node: node.into(), round, variant });
            let c = run_variant(model, node, &key, &hole, &ws, &spec, target, sized, &best, &candidates, round, variant, &label, driver, project, opts)?;
            let improved = c.verdict == Some(VariantVerdict::RunnerUp);
            let reason = match &c.verdict {
                Some(VariantVerdict::Rejected { stage, reason }) => format!("{stage:?}: {}", reason.lines().next().unwrap_or("")),
                _ => c.comparison.as_ref().map(|x| x.summary.clone()).unwrap_or_default(),
            };
            progress(opts, OptimizeProgress::Verdict { node: node.into(), label: label.clone(), accepted: improved, reason });
            candidates.push(c);
            let i = candidates.len() - 1;
            if improved {
                let better = round_winner.is_none_or(|w| ratio(&candidates[i]) < ratio(&candidates[w]));
                if better {
                    round_winner = Some(i);
                }
            }
        }
        let Some(w) = round_winner else { break };
        candidates[w].verdict = Some(VariantVerdict::Winner);
        accepted = Some(candidates[w].label.clone());
        best = candidates[w].clone();
    }

    let mut speedup = None;
    let mut baseline_path = None;
    if let Some(label) = &accepted {
        let winner = candidates.iter().find(|c| &c.label == label).expect("winner is a candidate");
        let locked = format!("{KEY_PREFIX}{key}\n{}\n", strip_key(&winner.code).trim_end());
        if let Some(d) = hole.path.parent() {
            std::fs::create_dir_all(d)?;
        }
        std::fs::write(&hole.path, &locked)?;
        let overall = gates::compare(&original.measurements, &winner.measurements, target, opts.noise_threshold, opts.alpha);
        speedup = Some(1.0 / overall.geomean_ratio);
        notes.push(format!("accepted {label}: {}", overall.summary));
        if opts.update_baseline {
            baseline_path = Some(record_baseline(project, node, &fp, &locked, &winner.measurements, &opts.bench)?);
        }
    } else if opts.update_baseline && baseline::load(&project.root, node, &fp).is_none() {
        baseline_path = Some(record_baseline(project, node, &fp, &current, &original.measurements, &opts.bench)?);
    }
    progress(opts, OptimizeProgress::Done { node: node.into(), accepted: accepted.clone() });
    Ok(OptimizeReport { node: node.into(), target, fingerprint: fp, incumbent: original, candidates, accepted, speedup, baseline: baseline_path, notes })
}

fn ratio(c: &Candidate) -> f64 {
    c.comparison.as_ref().map_or(f64::INFINITY, |x| x.geomean_ratio)
}

fn record_baseline(project: &Project, node: &str, fp: &Fingerprint, code: &str, ms: &[Measurement], cfg: &BenchConfig) -> Result<PathBuf> {
    let entry = BaselineEntry { fingerprint: fp.clone(), impl_hash: baseline::impl_hash(code), recorded: baseline::now(), config: cfg.clone(), measurements: ms.to_vec() };
    baseline::store(&project.root, node, entry)
}

/// Scratch copy of `impl/`, a generated crate over it and a bench crate.
fn prepare(project: &Project, model: &Model, node: &str, hole: &HoleSpec, base: &GenOptions, opts: &OptimizeOptions) -> Result<Workspace> {
    let s = slug(node);
    let dir = project.root.join(".wh/perf").join(&s);
    let _ = std::fs::remove_dir_all(&dir);
    let impl_root = dir.join("impl");
    copy_tree(&project.impl_dir(), &impl_root).context("preparing the sandbox")?;
    let rel = hole.path.strip_prefix(project.impl_dir()).context("hole outside impl/")?;
    let hole_file = impl_root.join(rel);
    let gen_opts = GenOptions {
        // unique per project and node: packages share a target dir
        package: format!("{}_perf_{s}", base.package),
        crate_name: base.crate_name.clone(),
        impl_root: impl_root.clone(),
        contracts_in_release: false,
    };
    let generated = wh_codegen::generate(model, &gen_opts);
    let gen_dir = dir.join("gen");
    wh_codegen::write(&generated, &gen_dir)?;
    let bench = generate_bench_crate(
        model,
        &BenchCrateOptions {
            dir: dir.join("bench"),
            package: format!("{}_perfbench_{s}", base.package),
            lib_dir: gen_dir.clone(),
            lib_package: gen_opts.package.clone(),
            lib: gen_opts.crate_name.clone(),
            nodes: Some(vec![node.to_string()]),
        },
    )?;
    if let Some(sk) = bench.skipped.iter().find(|s| s.node == node) {
        bail!("`{node}` cannot be benchmarked: {}", sk.reason);
    }
    Ok(Workspace {
        runs: project.root.join(".wh/perf-runs").join(&s),
        target: opts.target_dir.clone().unwrap_or_else(|| project.target_dir()),
        dir,
        impl_root,
        hole_file,
        gen_dir,
        gen_opts,
        manifest: generated.manifest,
        bench,
    })
}

fn reject(mut c: Candidate, stage: Stage, reason: impl Into<String>) -> Candidate {
    c.verdict = Some(VariantVerdict::Rejected { stage, reason: reason.into() });
    c
}

#[allow(clippy::too_many_arguments)]
fn run_variant(
    model: &Model,
    node: &str,
    key: &str,
    hole: &HoleSpec,
    ws: &Workspace,
    spec: &PerfSpec,
    target: Target,
    sized: bool,
    best: &Candidate,
    history: &[Candidate],
    round: usize,
    variant: usize,
    label: &str,
    driver: &dyn Driver,
    project: &Project,
    opts: &OptimizeOptions,
) -> Result<Candidate> {
    // every variant starts from the incumbent's code
    std::fs::write(&ws.hole_file, &best.code)?;
    let before = tree_digest(&ws.dir, &ws.hole_file);
    let prompt = build_optimize_prompt(model, hole, &ws.gen_dir, &ws.hole_file, spec, target, best, history, opts, variant);
    let req = SynthesisRequest {
        node: node.to_string(),
        key: key.to_string(),
        round: (round - 1) * opts.variants.max(1) + variant,
        workdir: ws.dir.clone(),
        hole_file: ws.hole_file.clone(),
        prompt,
    };
    let mut c = Candidate {
        label: label.to_string(),
        round,
        variant,
        code: String::new(),
        impl_hash: String::new(),
        measurements: vec![],
        complexity: None,
        gates: vec![],
        comparison: None,
        mutation: None,
        verdict: None,
        transcript: String::new(),
    };
    let outcome = driver.attempt(&req);
    c.code = std::fs::read_to_string(&ws.hole_file).unwrap_or_default();
    c.impl_hash = baseline::impl_hash(&c.code);
    let outcome = match outcome {
        Ok(o) => o,
        Err(e) => return Ok(reject(c, Stage::Sandbox, format!("the agent failed: {e:#}"))),
    };
    c.transcript = outcome.transcript;
    if tree_digest(&ws.dir, &ws.hole_file) != before {
        // restore the sandbox: fresh copy of impl/ and of the generated crate
        let _ = std::fs::remove_dir_all(&ws.impl_root);
        copy_tree(&project.impl_dir(), &ws.impl_root)?;
        let _ = std::fs::remove_dir_all(ws.gen_dir.join("src"));
        wh_codegen::write(&wh_codegen::generate(model, &ws.gen_opts), &ws.gen_dir)?;
        let _ = generate_bench_crate(
            model,
            &BenchCrateOptions {
                dir: ws.bench.dir.clone(),
                package: ws.bench.package.clone(),
                lib_dir: ws.gen_dir.clone(),
                lib_package: ws.gen_opts.package.clone(),
                lib: ws.gen_opts.crate_name.clone(),
                nodes: Some(vec![node.to_string()]),
            },
        )?;
        return Ok(reject(c, Stage::Sandbox, "the agent modified files other than the hole file; only the hole file may be written"));
    }
    if squash(&strip_key(&c.code)) == squash(&strip_key(&best.code)) {
        return Ok(reject(c, Stage::NotFaster, "the variant is identical to the current implementation"));
    }
    let leaks = crate::privacy::sensitive_logging(model, node, &c.code);
    if !leaks.is_empty() {
        let why = format!(
            "the variant records sensitive values in traces; log counts, lengths or ids instead:\n{}",
            leaks.iter().map(|l| l.message()).collect::<Vec<_>>().join("\n")
        );
        return Ok(reject(c, Stage::Privacy, why));
    }
    progress(opts, OptimizeProgress::Verifying { node: node.into(), label: label.into() });
    if let Some((stage, why)) = verify(hole, &ws.hole_file, &ws.gen_dir, &ws.target, &ws.manifest)? {
        return Ok(reject(c, stage, why));
    }
    progress(opts, OptimizeProgress::Benchmarking { node: node.into(), label: label.into() });
    c.measurements = match run_benches(&ws.bench, &ws.target, Some(&[node.to_string()]), &opts.bench, &ws.runs.join(label)) {
        Ok(m) => m,
        Err(e) => return Ok(reject(c, Stage::Benchmark, format!("{e:#}"))),
    };
    (c.complexity, c.gates) = gates::evaluate(spec, &c.measurements, sized);
    c.comparison = Some(gates::compare(&best.measurements, &c.measurements, target, opts.noise_threshold, opts.alpha));
    if let Some(g) = c.gates.iter().find(|g| g.failed()) {
        let why = format!("`{}` failed: {}", g.gate, g.detail);
        return Ok(reject(c, Stage::Gates, why));
    }
    let cmp = c.comparison.clone().expect("set above");
    if !cmp.improved {
        return Ok(reject(c, Stage::NotFaster, cmp.summary));
    }
    if opts.mutation && spec.mutate.is_some() {
        let mopts = MutationOptions { target_dir: Some(ws.target.clone()), code: Some(c.code.clone()), ..Default::default() };
        let m = mutation_score_with(project, model, node, &mopts)?;
        let failed = m.gate.as_ref().filter(|g| g.status == GateStatus::Fail).map(|g| format!("`{}` failed: {}", g.gate, g.detail));
        let skipped = matches!(m.status, MutationStatus::NotInstalled { .. });
        c.mutation = Some(m);
        if let Some(why) = failed {
            return Ok(reject(c, Stage::Mutation, why));
        }
        if skipped {
            tracing::warn!(node, "cargo-mutants is not installed; the mutate gate was not checked");
        }
    }
    c.verdict = Some(VariantVerdict::RunnerUp);
    Ok(c)
}

/// The correctness gate (as in synthesis): signature, compiler, lints, then
/// the node's examples, contracts and properties. `None` = passed.
fn verify(hole: &HoleSpec, file: &Path, crate_dir: &Path, target: &Path, manifest: &Manifest) -> Result<Option<(Stage, String)>> {
    let text = std::fs::read_to_string(file).unwrap_or_default();
    if text.contains("todo!(") || text.contains("unimplemented!(") {
        return Ok(Some((Stage::Compile, "the hole file contains `todo!()`/`unimplemented!()`".into())));
    }
    if !hole.signature.starts_with("adapter ") && !squash(&text).contains(&squash(&hole.signature)) {
        return Ok(Some((Stage::Compile, format!("the signature changed; it must stay exactly:\n{}", hole.signature))));
    }
    let cargo = wh_build::Cargo { crate_dir, target_dir: target, manifest };
    let lint = cargo.clippy()?;
    let file_s = file.to_string_lossy();
    let mine: Vec<&wh_build::NodeDiagnostic> = lint
        .diagnostics
        .iter()
        .filter(|d| d.level == "error" || (d.level == "warning" && d.file.as_deref().is_some_and(|f| file_s.ends_with(f) || f.ends_with(&*file_s))))
        .collect();
    if !mine.is_empty() || !lint.success {
        let mut s = String::from("the code does not compile or violates a lint gate:\n");
        for d in mine.iter().take(8) {
            s.push_str(d.rendered.trim_end());
            s.push('\n');
        }
        return Ok(Some((Stage::Compile, s)));
    }
    let tested = cargo.test(Some(&hole.node))?;
    let failed: Vec<&wh_build::TestOutcome> = tested.failed_tests().collect();
    if !failed.is_empty() {
        let mut s = String::from("specs failed:\n");
        for t in failed.iter().take(6) {
            let _ = writeln!(s, "--- {}\n{}", t.name, t.output.lines().take(12).collect::<Vec<_>>().join("\n"));
        }
        return Ok(Some((Stage::Tests, s)));
    }
    if !tested.success {
        return Ok(Some((Stage::Tests, "the test run failed to build".into())));
    }
    Ok(None)
}

fn measurement_table(c: &Candidate) -> String {
    let mut s = String::from("| benchmark | n | mean (95% CI) | median | allocations/call | bytes/call |\n|---|---|---|---|---|---|\n");
    for m in &c.measurements {
        let _ = writeln!(
            s,
            "| {} | {} | {} ({} – {}) | {} | {} | {} |",
            m.bench,
            m.size,
            fmt_ns(m.mean.point),
            fmt_ns(m.mean.lower),
            fmt_ns(m.mean.upper),
            fmt_ns(m.median.point),
            m.allocs.map_or("?".into(), |a| a.to_string()),
            m.alloc_bytes.map_or("?".into(), |a| a.to_string()),
        );
    }
    s
}

/// The prompt asking the compiler agent for one faster variant.
#[allow(clippy::too_many_arguments)]
pub fn build_optimize_prompt(
    model: &Model,
    hole: &HoleSpec,
    crate_dir: &Path,
    hole_file: &Path,
    spec: &PerfSpec,
    target: Target,
    incumbent: &Candidate,
    history: &[Candidate],
    opts: &OptimizeOptions,
    variant: usize,
) -> String {
    let node = model.nodes.get(&hole.node);
    let mut p = String::new();
    p.push_str("You are the optimization stage of the Wild Horse compiler.\n\n");
    let _ = write!(
        p,
        "`{}` already has a correct implementation. Write a faster variant of it by editing exactly one file:\n    {}\n\n",
        hole.node,
        hole_file.display()
    );
    p.push_str("Rules:\n");
    p.push_str("- Write only that file. Do not create, edit or delete any other file. Do not run commands; the compiler verifies and measures your work.\n");
    p.push_str("- Keep every signature exactly as it is. You may add private helper items in the file.\n");
    p.push_str("- The behaviour must not change: every example, contract and property of the specification must still hold.\n");
    p.push_str("- Use only the Rust standard library and the crates declared in the generated Cargo.toml.\n");
    p.push_str("- Use `tracing::{debug, trace}` for diagnostics; never `println!`, `eprintln!` or `dbg!`.\n");
    p.push_str("- Never log, trace or format into errors the values of `sensitive` fields, or of types/parameters holding personal or secret data (emails, names, tokens, passwords); log counts, lengths or ids instead.\n");
    p.push_str("- No `unsafe` unless the spec says `unsafe allow`. The code must pass clippy.\n\n");
    if let Some(node) = node {
        if let Some(prose) = &node.prose {
            p.push_str("## Intent (from the literate source)\n\n");
            p.push_str(prose.trim());
            p.push_str("\n\n");
        }
        let text = &model.files[node.file].text[node.span.range()];
        let _ = write!(p, "## Specification (Wild Horse source)\n\n```wh\n{}\n```\n\n", text.trim_end());
        for dep in wh_model::hash::dependencies(model, &node.id) {
            if let Some(d) = model.nodes.get(&dep)
                && matches!(d.kind, NodeKind::Struct(_) | NodeKind::Enum(_) | NodeKind::Refined { .. } | NodeKind::Capability(_) | NodeKind::Alias(_))
            {
                let t = &model.files[d.file].text[d.span.range()];
                let _ = writeln!(p, "Referenced definition `{dep}`:\n```wh\n{}\n```", t.trim_end());
            }
        }
    }
    let _ = write!(
        p,
        "\n## Rust context\n\nThe generated crate is at {} (read-only); `src/modules/` holds the types and the contract-checking wrapper that calls your function.\nRequired signature:\n```rust\n{}\n```\n\n",
        crate_dir.display(),
        hole.signature
    );
    let _ = write!(p, "## Current implementation\n\n```rust\n{}\n```\n\n", strip_key(&incumbent.code).trim());
    let leaks = crate::privacy::sensitive_logging(model, &hole.node, &incumbent.code);
    if !leaks.is_empty() {
        p.push_str("The current implementation records sensitive values in traces; your variant must not:\n");
        for l in &leaks {
            let _ = writeln!(p, "- {}", l.message());
        }
        p.push('\n');
    }
    p.push_str("## Measurements of the current implementation\n\n");
    p.push_str("Release build, criterion; `auto` benchmarks call the function on generated inputs of size n. Allocations are counted for one call.\n\n");
    p.push_str(&measurement_table(incumbent));
    if let Some(f) = &incumbent.complexity {
        let _ = writeln!(p, "\nMeasured growth: {} (log-log slope {:.2}).", f.class.label(), f.exponent);
    }
    for g in &incumbent.gates {
        let _ = writeln!(p, "- `{}`: {:?} — {}", g.gate, g.status, g.detail);
    }
    p.push_str("\n## Goal\n\n");
    let _ = writeln!(
        p,
        "Optimize for {}. Your variant replaces the current implementation only if it passes every correctness gate and every performance clause of the specification, is at least {:.0}% better overall with statistical significance, and is not significantly worse at any input size.",
        target.label(),
        opts.noise_threshold * 100.0
    );
    let clauses: Vec<&str> = spec
        .complexity
        .iter()
        .map(|c| c.1.as_str())
        .chain(spec.alloc.iter().map(|a| a.1.as_str()))
        .chain(spec.budgets.iter().map(|b| b.text.as_str()))
        .collect();
    if !clauses.is_empty() {
        let _ = writeln!(p, "The performance clauses `{}` are checked on every variant.", clauses.join("`, `"));
    }
    p.push_str("Look for algorithmic improvements first, then fewer allocations (reuse buffers, reserve capacity, borrow instead of clone), then constant factors.\n");
    if !history.is_empty() {
        p.push_str("\n## Earlier variants\n\n");
        for h in history {
            let outcome = match &h.verdict {
                Some(VariantVerdict::Winner) => "accepted".to_string(),
                Some(VariantVerdict::RunnerUp) => format!("faster, but not the best ({})", h.comparison.as_ref().map(|c| c.summary.as_str()).unwrap_or("")),
                Some(VariantVerdict::Rejected { stage, reason }) => format!("rejected at {stage:?}: {}", reason.lines().next().unwrap_or("")),
                None => "pending".into(),
            };
            let _ = writeln!(p, "- {}: {outcome}", h.label);
        }
    }
    let _ = writeln!(p, "\nThis is variant {variant} of {}; try an approach different from the earlier variants.", opts.variants.max(1));
    p
}
