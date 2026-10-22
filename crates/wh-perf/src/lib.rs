//! The optimization stage (design doc, "Optimization stage (M2)").
//!
//! ```text
//! synthesize (correct) → verify → OPTIMIZE: measure → agent proposes K variants → verify each → benchmark → keep fastest → lock
//! ```
//!
//! - [`benchgen`]: a bench crate next to the generated library
//!   (`.wh/bench/`) with one criterion group per hole: the node's `bench`
//!   clauses plus an auto-bench over an input-size sweep, a counting global
//!   allocator, deterministic inputs.
//! - [`measure`]: runs it and parses criterion's estimates and samples.
//! - [`gates`]: `complexity` (growth-curve fit, [`stats`]), `budget`,
//!   `alloc`, and significance-tested comparisons.
//! - [`baseline`]: `impl/.bench/<node>.json`, keyed by machine fingerprint.
//! - [`optimize`]: the variant tournament.
//! - [`mutation`]: the `mutate` gate via `cargo-mutants`.
//! - [`profile`]: hole weights from traced workloads (what to optimize first).
//! - [`pgo`]: profile-guided optimization with `cargo-pgo`.
//! - [`check_against_baseline`]: the performance check of the synthesis gate.
//!
//! Everything a UI shows is `Serialize + Deserialize`.

pub mod baseline;
pub mod benchgen;
pub mod gates;
pub mod inputs;
pub mod measure;
pub mod mutation;
pub mod optimize;
pub mod pgo;
pub mod privacy;
pub mod profile;
pub mod sandbox;
pub mod spec;
pub mod stats;

use std::path::PathBuf;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use wh_model::Model;
use wh_project::Project;

pub use baseline::{BaselineEntry, Fingerprint, fingerprint};
pub use benchgen::{BenchCrate, BenchCrateOptions, Skipped, generate_bench_crate};
pub use gates::{Comparison, GateResult, GateStatus, compare};
pub use measure::{BenchConfig, Estimate, Measurement, run_benches};
pub use mutation::{MutationReport, MutationStatus, mutation_score, mutation_score_with};
pub use optimize::{Candidate, OptimizeOptions, OptimizeProgress, OptimizeReport, Stage, VariantVerdict, optimize};
pub use pgo::{PgoOptions, PgoReport, PgoStatus, pgo_build, pgo_build_with};
pub use profile::{HoleWeight, Profile, ProfileOptions, hot_holes, hot_holes_with, optimization_queue};
pub use spec::{Complexity, PerfSpec, Target};
pub use stats::ComplexityFit;

/// When [`bench_project`] records baselines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BaselinePolicy {
    Never,
    /// Record a baseline for nodes that have none for this machine.
    IfMissing,
    /// Replace the baselines with the new measurements.
    Always,
}

#[derive(Debug, Clone)]
pub struct PerfOptions {
    pub bench: BenchConfig,
    /// Cargo target dir (default: the project's shared one).
    pub target_dir: Option<PathBuf>,
    pub noise_threshold: f64,
    pub alpha: f64,
    /// Nodes to measure (default: every hole that can be benchmarked).
    pub nodes: Option<Vec<String>>,
    pub baselines: BaselinePolicy,
}

impl Default for PerfOptions {
    fn default() -> Self {
        Self { bench: BenchConfig::default(), target_dir: None, noise_threshold: 0.03, alpha: 0.05, nodes: None, baselines: BaselinePolicy::IfMissing }
    }
}

/// Performance results of one node.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodePerf {
    pub node: String,
    pub impl_hash: String,
    pub measurements: Vec<Measurement>,
    pub complexity: Option<ComplexityFit>,
    pub gates: Vec<GateResult>,
    /// Against the stored baseline for this machine, if there is one.
    pub baseline: Option<Comparison>,
    /// A baseline was written by this run.
    pub baseline_recorded: bool,
    /// E.g. that a first-run regression was re-measured.
    #[serde(default)]
    pub notes: Vec<String>,
}

impl NodePerf {
    /// Failed gates and regressions, as messages.
    pub fn failures(&self) -> Vec<String> {
        let mut out: Vec<String> = self.gates.iter().filter(|g| g.failed()).map(|g| format!("`{}` failed: {}", g.gate, g.detail)).collect();
        if let Some(b) = self.baseline.as_ref().filter(|b| b.regressed) {
            out.push(format!("performance regression against the baseline: {}", b.summary));
        }
        out
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PerfReport {
    pub fingerprint: Fingerprint,
    pub nodes: Vec<NodePerf>,
    /// Holes that could not be benchmarked, and why.
    pub skipped: Vec<Skipped>,
}

impl PerfReport {
    /// (node, message) for every failed gate and regression.
    pub fn failures(&self) -> Vec<(String, String)> {
        self.nodes.iter().flat_map(|n| n.failures().into_iter().map(move |f| (n.node.clone(), f))).collect()
    }

    pub fn passed(&self) -> bool {
        self.failures().is_empty()
    }
}

/// Holes with performance clauses (`complexity`, `budget`, `alloc`,
/// `bench`, `optimize`) or a recorded baseline: what `wh build --release`
/// measures.
pub fn perf_nodes(project: &Project, model: &Model) -> Vec<String> {
    model
        .nodes
        .values()
        .filter(|n| n.is_hole())
        .filter(|n| {
            let s = PerfSpec::of(model, &n.id).unwrap_or_default();
            s.complexity.is_some()
                || !s.budgets.is_empty()
                || s.alloc.is_some()
                || !s.benches.is_empty()
                || s.optimize.is_some()
                || baseline::baseline_path(&project.root, &n.id).exists()
        })
        .map(|n| n.id.clone())
        .collect()
}

/// Generate `.wh/gen` and `.wh/bench`, benchmark the holes, evaluate their
/// performance gates and compare with (and maybe record) baselines.
pub fn bench_project(project: &Project, model: &Model, opts: &PerfOptions) -> Result<PerfReport> {
    let generated = project.generate(model)?;
    let gen_opts = project.gen_options();
    let bench = generate_bench_crate(
        model,
        &BenchCrateOptions {
            dir: project.root.join(".wh/bench"),
            package: format!("wh_bench_{}_{}", project.crate_name(), project.id()),
            lib_dir: project.gen_dir(),
            lib_package: gen_opts.package.clone(),
            lib: gen_opts.crate_name.clone(),
            nodes: opts.nodes.clone(),
        },
    )?;
    let fp = fingerprint();
    let mut report = PerfReport { fingerprint: fp.clone(), nodes: vec![], skipped: bench.skipped.clone() };
    if bench.targets.is_empty() {
        return Ok(report);
    }
    let target = opts.target_dir.clone().unwrap_or_else(|| project.target_dir());
    let all = run_benches(&bench, &target, None, &opts.bench, &project.root.join(".wh/bench-runs/latest"))?;
    for t in &bench.targets {
        let spec = PerfSpec::of(model, &t.node).unwrap_or_default();
        let first: Vec<Measurement> = all.iter().filter(|m| m.node == t.node).cloned().collect();
        let code = generated
            .manifest
            .holes
            .iter()
            .find(|h| h.node == t.node)
            .and_then(|h| std::fs::read_to_string(&h.path).ok())
            .unwrap_or_default();
        let impl_hash = baseline::impl_hash(&code);
        let stored = baseline::load(&project.root, &t.node, &fp);
        let mut notes = Vec::new();
        let (measurements, comparison) = match &stored {
            None => (first, None),
            Some(b) => {
                let retry = project.root.join(".wh/bench-runs/retry");
                let (m, c, note) = confirm(b, first, &opts.bench, opts, || run_benches(&bench, &target, Some(std::slice::from_ref(&t.node)), &opts.bench, &retry))?;
                notes.extend(note);
                (m, Some(c))
            }
        };
        let (complexity, gates) = gates::evaluate(&spec, &measurements, t.sized);
        let record = match opts.baselines {
            BaselinePolicy::Never => false,
            BaselinePolicy::IfMissing => stored.is_none(),
            BaselinePolicy::Always => true,
        } && !measurements.is_empty();
        if record {
            let entry = BaselineEntry { fingerprint: fp.clone(), impl_hash: impl_hash.clone(), recorded: baseline::now(), config: opts.bench.clone(), measurements: measurements.clone() };
            baseline::store(&project.root, &t.node, entry)?;
        }
        report.nodes.push(NodePerf { node: t.node.clone(), impl_hash, measurements, complexity, gates, baseline: comparison, baseline_recorded: record, notes });
    }
    Ok(report)
}

/// The performance check of the synthesis gate: benchmark `node` as
/// implemented in `impl_root` (e.g. a synthesis scratch copy of `impl/`)
/// with the settings of its committed baseline, and compare.
///
/// `Ok(None)`: no baseline for this machine, nothing to check.
/// `Ok(Some(failures))`: empty when the candidate passes; otherwise failed
/// performance gates and regressions beyond the noise threshold, as
/// messages suitable as repair feedback.
pub fn check_against_baseline(project: &Project, model: &Model, node: &str, impl_root: &std::path::Path, opts: &PerfOptions) -> Result<Option<Vec<String>>> {
    let fp = fingerprint();
    let Some(base) = baseline::load(&project.root, node, &fp) else { return Ok(None) };
    let gen_base = project.gen_options();
    let s = sandbox::slug(node);
    let dir = project.root.join(".wh/perf-check").join(&s);
    let gen_opts = wh_codegen::GenOptions {
        package: format!("{}_check_{s}", gen_base.package),
        crate_name: gen_base.crate_name.clone(),
        impl_root: impl_root.to_path_buf(),
        contracts_in_release: false,
    };
    let gen_dir = dir.join("gen");
    wh_codegen::generate_cached(model, &gen_opts, &gen_dir)?;
    let bench = generate_bench_crate(
        model,
        &BenchCrateOptions {
            dir: dir.join("bench"),
            package: format!("{}_checkbench_{s}", gen_base.package),
            lib_dir: gen_dir,
            lib_package: gen_opts.package.clone(),
            lib: gen_opts.crate_name.clone(),
            nodes: Some(vec![node.to_string()]),
        },
    )?;
    let Some(t) = bench.targets.iter().find(|t| t.node == node) else { return Ok(None) };
    // measure exactly as the baseline was measured
    let cfg = BenchConfig { timeout_secs: opts.bench.timeout_secs, ..base.config.clone() };
    let target = opts.target_dir.clone().unwrap_or_else(|| project.target_dir());
    let first = run_benches(&bench, &target, Some(&[node.to_string()]), &cfg, &dir.join("run"))?;
    let (measurements, comparison, note) = confirm(&base, first, &cfg, opts, || run_benches(&bench, &target, Some(&[node.to_string()]), &cfg, &dir.join("retry")))?;
    let spec = PerfSpec::of(model, node).unwrap_or_default();
    let (complexity, gates) = gates::evaluate(&spec, &measurements, t.sized);
    let perf = NodePerf {
        node: node.to_string(),
        impl_hash: String::new(),
        measurements,
        complexity,
        gates,
        baseline: Some(comparison),
        baseline_recorded: false,
        notes: note.into_iter().collect(),
    };
    Ok(Some(perf.failures()))
}

/// Compare with a baseline; a regression on the first run is only reported
/// if a second measurement confirms it (the second run is then used).
fn confirm(
    base: &BaselineEntry,
    first: Vec<Measurement>,
    cfg: &BenchConfig,
    opts: &PerfOptions,
    remeasure: impl FnOnce() -> Result<Vec<Measurement>>,
) -> Result<(Vec<Measurement>, Comparison, Option<String>)> {
    let c1 = baseline::regression(base, &first, cfg, opts.noise_threshold, opts.alpha);
    if !c1.regressed {
        let note = (base.config != *cfg).then(|| "the baseline was measured with different benchmark settings".to_string());
        return Ok((first, c1, note));
    }
    let second = remeasure()?;
    let c2 = baseline::regression(base, &second, cfg, opts.noise_threshold, opts.alpha);
    let note = if c2.regressed {
        format!("regression confirmed by a second measurement (first run: {})", c1.summary)
    } else {
        format!("a regression on the first run was not confirmed by a second measurement (first run: {})", c1.summary)
    };
    Ok((second, c2, Some(note)))
}
