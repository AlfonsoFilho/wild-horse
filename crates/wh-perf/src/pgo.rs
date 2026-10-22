//! Profile-guided optimization with `cargo-pgo` (never installed
//! automatically; without it [`PgoStatus::NotInstalled`] is reported).
//!
//! 1. benchmark the holes normally (the "before" numbers);
//! 2. instrumented build of the app and training runs: the `[perf]
//!    workload` runs of the app, the scenario tests and the benchmarks
//!    (`cargo pgo build`, `cargo pgo test`, `cargo pgo bench`), all writing
//!    into `<target>/pgo-profiles`;
//! 3. `cargo pgo optimize bench` (the "after" numbers) and `cargo pgo
//!    optimize build` (the optimized app binaries).
//!
//! LLVM matches profiles by symbol, so the benchmarks mostly benefit from
//! the benchmark training run and the app binary from the app runs; the
//! scenario-test profiles only help code they share by symbol (dependencies).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use wh_model::Model;
use wh_project::Project;

use crate::benchgen::{BenchCrateOptions, generate_bench_crate};
use crate::gates::{Comparison, compare};
use crate::measure::{BenchConfig, BenchRunner, Measurement, build_benches, cargo, run_benches, run_benches_via, run_with_timeout, tail};
use crate::profile::ProfileOptions;
use crate::spec::Target;

pub const INSTALL_HINT: &str = "cargo install cargo-pgo && rustup component add llvm-tools-preview";
pub const LLVM_TOOLS_HINT: &str = "cargo-pgo needs `llvm-profdata`: rustup component add llvm-tools-preview";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum PgoStatus {
    NotInstalled { hint: String },
    Done,
    Failed { step: String, reason: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PgoReport {
    pub status: PgoStatus,
    /// What the profiles were gathered from.
    pub training: Vec<String>,
    pub before: Vec<Measurement>,
    pub after: Vec<Measurement>,
    /// After vs before on mean time.
    pub comparison: Option<Comparison>,
    /// PGO-optimized app binaries (in the target dir; install with `Project::install_bins`).
    pub binaries: Vec<PathBuf>,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct PgoOptions {
    pub bench: BenchConfig,
    /// Holes to benchmark (default: every benchmarkable hole).
    pub nodes: Option<Vec<String>>,
    /// App runs used for training (default: `[perf] workload`).
    pub app_runs: Vec<Vec<String>>,
    pub scenarios: bool,
    pub target_dir: Option<PathBuf>,
    pub noise_threshold: f64,
    pub alpha: f64,
}

impl PgoOptions {
    pub fn from_project(project: &Project) -> Self {
        let p = ProfileOptions::from_project(project);
        Self {
            bench: BenchConfig::default(),
            nodes: None,
            app_runs: p.app_runs,
            scenarios: true,
            target_dir: None,
            noise_threshold: 0.03,
            alpha: 0.05,
        }
    }
}

/// `cargo pgo --version`, if cargo-pgo is installed.
pub fn cargo_pgo_version() -> Option<String> {
    let out = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into())).args(["pgo", "--version"]).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// The `llvm-profdata` cargo-pgo would use: `$LLVM_PROFDATA`, the
/// toolchain's llvm-tools component, or one on `PATH`.
pub fn llvm_profdata() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("LLVM_PROFDATA").map(PathBuf::from).filter(|p| p.exists()) {
        return Some(p);
    }
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
    let sysroot = Command::new(&rustc).args(["--print", "sysroot"]).output().ok().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
    if let (Some(root), Some(host)) = (sysroot, host_triple()) {
        let p = Path::new(&root).join("lib/rustlib").join(host).join("bin/llvm-profdata");
        if p.exists() {
            return Some(p);
        }
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join("llvm-profdata")).find(|p| p.exists())
}

fn host_triple() -> Option<String> {
    let out = Command::new(std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into())).arg("-vV").output().ok()?;
    String::from_utf8_lossy(&out.stdout).lines().find_map(|l| l.strip_prefix("host: ").map(str::to_string))
}

pub fn pgo_build(project: &Project, model: &Model, app: Option<&str>) -> Result<PgoReport> {
    pgo_build_with(project, model, app, &PgoOptions::from_project(project))
}

pub fn pgo_build_with(project: &Project, model: &Model, app: Option<&str>, opts: &PgoOptions) -> Result<PgoReport> {
    let mut report =
        PgoReport { status: PgoStatus::Done, training: vec![], before: vec![], after: vec![], comparison: None, binaries: vec![], notes: vec![] };
    if cargo_pgo_version().is_none() {
        report.status = PgoStatus::NotInstalled { hint: INSTALL_HINT.into() };
        return Ok(report);
    }
    if llvm_profdata().is_none() {
        report.status = PgoStatus::NotInstalled { hint: LLVM_TOOLS_HINT.into() };
        return Ok(report);
    }
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
    let target = opts.target_dir.clone().unwrap_or_else(|| project.target_dir());
    let runs = project.root.join(".wh/pgo-runs");
    let timeout = Duration::from_secs(opts.bench.timeout_secs);
    let fail = |mut r: PgoReport, step: &str, e: anyhow::Error| {
        r.status = PgoStatus::Failed { step: step.into(), reason: format!("{e:#}") };
        Ok(r)
    };

    // 1. before
    if !bench.targets.is_empty() {
        match run_benches(&bench, &target, None, &opts.bench, &runs.join("before")) {
            Ok(m) => report.before = m,
            Err(e) => return fail(report, "benchmarks (before)", e),
        }
    }

    // 2. training
    let _ = std::fs::remove_dir_all(target.join("pgo-profiles"));
    let gen_dir = project.gen_dir();
    let bins: Vec<String> = match app {
        Some(a) => generated.manifest.bins.iter().filter(|(id, n)| id.as_str() == a || n.as_str() == a).map(|(_, n)| n.clone()).collect(),
        None => generated.manifest.bins.values().cloned().collect(),
    };
    if !opts.app_runs.is_empty() && !bins.is_empty() {
        let mut c = cargo(&gen_dir, &target);
        c.args(["pgo", "build", "--", "--bins"]);
        if let Err(e) = run_ok(c, timeout) {
            return fail(report, "instrumented build", e);
        }
        let exe = match release_binary(&target, &bins[0]) {
            Some(e) => e,
            None => return fail(report, "instrumented build", anyhow::anyhow!("instrumented binary `{}` not found", bins[0])),
        };
        for args in &opts.app_runs {
            let mut c = Command::new(&exe);
            c.args(args).current_dir(&project.root).env("WH_LOG", "off").stdin(std::process::Stdio::null());
            let (ok, _, stderr) = run_with_timeout(c, timeout)?;
            if !ok {
                report.notes.push(format!("training run `{} {}` failed: {}", bins[0], args.join(" "), stderr.lines().last().unwrap_or("")));
            }
            report.training.push(format!("{} {}", bins[0], args.join(" ")));
        }
    }
    if opts.scenarios && generated.manifest.tests.keys().any(|t| t.contains("__test__")) {
        let mut c = cargo(&gen_dir, &target);
        c.args(["pgo", "test", "--", "--lib", "--", "__test__"]);
        match run_ok(c, timeout) {
            Ok(()) => report.training.push("scenario tests".into()),
            Err(e) => report.notes.push(format!("scenario tests were not used for training: {e:#}")),
        }
    }
    if !bench.targets.is_empty() {
        let quick = BenchConfig { timeout_secs: opts.bench.timeout_secs, ..BenchConfig::quick() };
        match run_benches_via(BenchRunner::PgoInstrumented, &bench, &target, None, &quick, &runs.join("training")) {
            Ok(_) => report.training.push("benchmarks".into()),
            Err(e) => return fail(report, "instrumented benchmarks", e),
        }
    }

    // 3. optimized builds
    if !bench.targets.is_empty() {
        match run_benches_via(BenchRunner::PgoOptimized, &bench, &target, None, &opts.bench, &runs.join("after")) {
            Ok(m) => report.after = m,
            Err(e) => return fail(report, "optimized benchmarks", e),
        }
        report.comparison = Some(compare(&report.before, &report.after, Target::Speed, opts.noise_threshold, opts.alpha));
    }
    if !bins.is_empty() {
        let mut c = cargo(&gen_dir, &target);
        c.args(["pgo", "optimize", "build", "--", "--bins"]);
        if let Err(e) = run_ok(c, timeout) {
            return fail(report, "optimized build", e);
        }
        report.binaries = bins.iter().filter_map(|b| release_binary(&target, b)).collect();
    }
    // keep the normal bench build current for later `cargo bench` runs
    let _ = build_benches(&bench, &target, timeout);
    Ok(report)
}

fn run_ok(c: Command, timeout: Duration) -> Result<()> {
    let (ok, stdout, stderr) = run_with_timeout(c, timeout)?;
    if !ok {
        anyhow::bail!("{}\n{}", tail(&stdout, 10), tail(&stderr, 30));
    }
    Ok(())
}

/// cargo-pgo builds for the explicit host target: `<target>/<triple>/release/<bin>`.
fn release_binary(target: &Path, bin: &str) -> Option<PathBuf> {
    let name = if cfg!(windows) { format!("{bin}.exe") } else { bin.to_string() };
    let triple = host_triple().context("rustc -vV").ok()?;
    [target.join(&triple).join("release").join(&name), target.join("release").join(&name)].into_iter().find(|p| p.exists())
}
