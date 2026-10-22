//! Running the bench crate and reading criterion's results
//! (`<CRITERION_HOME>/**/new/{benchmark,estimates,sample}.json`) plus the
//! allocation counts the counting allocator recorded.

use std::collections::BTreeMap;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::benchgen::BenchCrate;

/// Measurement settings (criterion command-line options plus the sweep).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BenchConfig {
    /// Input sizes of the auto-bench sweep (budget sizes are added).
    pub sizes: Vec<usize>,
    pub warm_up_secs: f64,
    pub measurement_secs: f64,
    /// criterion samples per benchmark (at least 10).
    pub sample_size: usize,
    /// Kill a benchmark run that takes longer than this.
    pub timeout_secs: u64,
}

impl Default for BenchConfig {
    fn default() -> Self {
        Self { sizes: vec![64, 256, 1024, 4096, 16384], warm_up_secs: 1.0, measurement_secs: 3.0, sample_size: 50, timeout_secs: 1800 }
    }
}

impl BenchConfig {
    /// Short runs for tests and the IDE's idle loop.
    pub fn quick() -> Self {
        Self { sizes: vec![64, 256, 1024, 4096], warm_up_secs: 0.2, measurement_secs: 0.4, sample_size: 10, timeout_secs: 600 }
    }
}

/// A criterion point estimate with its 95% confidence interval. All three
/// values are **nanoseconds per call**; use the `*_duration` helpers or
/// [`fmt_ns`] rather than interpreting them yourself.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Estimate {
    /// ns per call
    pub point: f64,
    /// ns per call (lower bound of the 95% confidence interval)
    pub lower: f64,
    /// ns per call (upper bound of the 95% confidence interval)
    pub upper: f64,
}

/// Nanoseconds (as measured) to a `Duration`.
pub fn ns_duration(ns: f64) -> Duration {
    Duration::from_secs_f64((ns / 1e9).max(0.0))
}

impl Estimate {
    pub const UNIT: &'static str = "ns";

    pub fn point_duration(&self) -> Duration {
        ns_duration(self.point)
    }

    pub fn lower_duration(&self) -> Duration {
        ns_duration(self.lower)
    }

    pub fn upper_duration(&self) -> Duration {
        ns_duration(self.upper)
    }

    /// `41.2 µs (40.1 µs – 42.0 µs)`
    pub fn display(&self) -> String {
        format!("{} ({} – {})", fmt_ns(self.point), fmt_ns(self.lower), fmt_ns(self.upper))
    }
}

/// One benchmark at one input size.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Measurement {
    pub node: String,
    /// `auto` or the name of a `bench` clause.
    pub bench: String,
    pub size: u64,
    /// Time per call (ns).
    pub mean: Estimate,
    /// Time per call (ns).
    pub median: Estimate,
    /// ns
    pub std_dev: f64,
    /// Time per call of every criterion sample (ns), for significance tests.
    pub samples: Vec<f64>,
    /// Heap allocations of one call.
    pub allocs: Option<u64>,
    pub alloc_bytes: Option<u64>,
}

impl Measurement {
    pub fn id(&self) -> String {
        format!("{}/{}/{}", self.node, self.bench, self.size)
    }
}

/// Format nanoseconds for humans.
pub fn fmt_ns(ns: f64) -> String {
    if ns >= 1e9 {
        format!("{:.3} s", ns / 1e9)
    } else if ns >= 1e6 {
        format!("{:.3} ms", ns / 1e6)
    } else if ns >= 1e3 {
        format!("{:.3} µs", ns / 1e3)
    } else {
        format!("{ns:.1} ns")
    }
}

/// Run a command with a timeout, returning (status success, stdout, stderr).
pub(crate) fn run_with_timeout(mut cmd: Command, timeout: Duration) -> Result<(bool, String, String)> {
    let mut child = cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().context("spawning cargo")?;
    let mut so = child.stdout.take().context("stdout")?;
    let mut se = child.stderr.take().context("stderr")?;
    let out = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = so.read_to_string(&mut s);
        s
    });
    let err = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = se.read_to_string(&mut s);
        s
    });
    let start = Instant::now();
    let status = loop {
        if let Some(st) = child.try_wait()? {
            break Some(st);
        }
        if start.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let (stdout, stderr) = (out.join().unwrap_or_default(), err.join().unwrap_or_default());
    match status {
        Some(st) => Ok((st.success(), stdout, stderr)),
        None => bail!("timed out after {} s\n{}", timeout.as_secs(), tail(&stderr, 20)),
    }
}

pub(crate) fn tail(s: &str, lines: usize) -> String {
    let v: Vec<&str> = s.lines().collect();
    v[v.len().saturating_sub(lines)..].join("\n")
}

pub(crate) fn cargo(dir: &Path, target_dir: &Path) -> Command {
    let mut c = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()));
    c.current_dir(dir).env("CARGO_TARGET_DIR", target_dir);
    c
}

/// Compile the benchmarks without running them; `Err` carries the compiler output.
pub fn build_benches(bench: &BenchCrate, target_dir: &Path, timeout: Duration) -> Result<()> {
    let mut c = cargo(&bench.dir, target_dir);
    c.args(["bench", "--bench", "wh_bench", "--no-run", "--quiet"]);
    let (ok, _, stderr) = run_with_timeout(c, timeout)?;
    if !ok {
        let errors: Vec<&str> = stderr.lines().filter(|l| !l.trim_start().starts_with("Compiling")).collect();
        bail!("the benchmarks do not compile:\n{}", tail(&errors.join("\n"), 60));
    }
    Ok(())
}

/// How `cargo` is asked to run the benchmarks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BenchRunner {
    /// `cargo bench`
    Cargo,
    /// `cargo pgo bench`: an instrumented run that gathers PGO profiles.
    PgoInstrumented,
    /// `cargo pgo optimize bench`: built with the gathered profiles.
    PgoOptimized,
}

/// Build and run the benchmarks of `nodes` (all when `None`) and collect
/// the results. Each run writes into a fresh `out_dir`, so criterion never
/// compares against an unrelated earlier run.
pub fn run_benches(bench: &BenchCrate, target_dir: &Path, nodes: Option<&[String]>, cfg: &BenchConfig, out_dir: &Path) -> Result<Vec<Measurement>> {
    build_benches(bench, target_dir, Duration::from_secs(cfg.timeout_secs))?;
    run_benches_via(BenchRunner::Cargo, bench, target_dir, nodes, cfg, out_dir)
}

/// [`run_benches`] without the separate build step, through any runner.
pub fn run_benches_via(
    runner: BenchRunner,
    bench: &BenchCrate,
    target_dir: &Path,
    nodes: Option<&[String]>,
    cfg: &BenchConfig,
    out_dir: &Path,
) -> Result<Vec<Measurement>> {
    let timeout = Duration::from_secs(cfg.timeout_secs);
    let _ = std::fs::remove_dir_all(out_dir);
    std::fs::create_dir_all(out_dir)?;
    let home = out_dir.join("criterion");
    let allocs = out_dir.join("allocs.tsv");
    let sizes: Vec<String> = cfg.sizes.iter().map(usize::to_string).collect();
    let mut c = cargo(&bench.dir, target_dir);
    match runner {
        BenchRunner::Cargo => c.args(["bench", "--bench", "wh_bench", "--quiet", "--"]),
        // cargo-pgo forwards the arguments after `--` to `cargo bench`
        BenchRunner::PgoInstrumented => c.args(["pgo", "bench", "--", "--bench", "wh_bench", "--"]),
        BenchRunner::PgoOptimized => c.args(["pgo", "optimize", "bench", "--", "--bench", "wh_bench", "--"]),
    };
    c.args(["--warm-up-time", &cfg.warm_up_secs.to_string()])
        .args(["--measurement-time", &cfg.measurement_secs.to_string()])
        .args(["--sample-size", &cfg.sample_size.max(10).to_string()])
        .arg("--noplot")
        .env("CRITERION_HOME", &home)
        .env("WH_PERF_ALLOC_OUT", &allocs)
        .env("WH_PERF_SIZES", sizes.join(","))
        .env("WH_PERF_ONLY", nodes.map(|n| n.join(",")).unwrap_or_default());
    let (ok, stdout, stderr) = run_with_timeout(c, timeout)?;
    if !ok {
        bail!("the benchmark run failed:\n{}\n{}", tail(&stdout, 10), tail(&stderr, 30));
    }
    let mut ms = read_criterion(&home)?;
    let counts = read_allocs(&allocs);
    for m in &mut ms {
        if let Some((n, b)) = counts.get(&(m.node.clone(), m.bench.clone(), m.size)) {
            m.allocs = Some(*n);
            m.alloc_bytes = Some(*b);
        }
    }
    ms.sort_by(|a, b| (&a.node, &a.bench, a.size).cmp(&(&b.node, &b.bench, b.size)));
    Ok(ms)
}

fn read_allocs(path: &Path) -> BTreeMap<(String, String, u64), (u64, u64)> {
    let mut out = BTreeMap::new();
    for line in std::fs::read_to_string(path).unwrap_or_default().lines() {
        let f: Vec<&str> = line.split('\t').collect();
        if let [node, bench, size, n, b] = f.as_slice()
            && let (Ok(size), Ok(n), Ok(b)) = (size.parse(), n.parse(), b.parse())
        {
            out.insert((node.to_string(), bench.to_string(), size), (n, b));
        }
    }
    out
}

fn estimate(v: &serde_json::Value) -> Option<Estimate> {
    Some(Estimate {
        point: v["point_estimate"].as_f64()?,
        lower: v["confidence_interval"]["lower_bound"].as_f64()?,
        upper: v["confidence_interval"]["upper_bound"].as_f64()?,
    })
}

fn find_new_dirs(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if !p.is_dir() {
            continue;
        }
        if p.file_name().is_some_and(|n| n == "new") && p.join("benchmark.json").exists() {
            out.push(p);
        } else {
            find_new_dirs(&p, out);
        }
    }
}

/// Parse every benchmark criterion stored under `home`.
pub fn read_criterion(home: &Path) -> Result<Vec<Measurement>> {
    let mut dirs = Vec::new();
    find_new_dirs(home, &mut dirs);
    let mut out = Vec::new();
    for d in dirs {
        let read = |f: &str| -> Result<serde_json::Value> {
            let p = d.join(f);
            Ok(serde_json::from_str(&std::fs::read_to_string(&p).with_context(|| format!("reading {}", p.display()))?)?)
        };
        let id = read("benchmark.json")?;
        let est = read("estimates.json")?;
        let sample = read("sample.json")?;
        let (Some(node), Some(bench)) = (id["group_id"].as_str(), id["function_id"].as_str()) else { continue };
        let size = id["value_str"].as_str().and_then(|s| s.parse().ok()).unwrap_or(0);
        let iters: Vec<f64> = sample["iters"].as_array().map(|a| a.iter().filter_map(|v| v.as_f64()).collect()).unwrap_or_default();
        let times: Vec<f64> = sample["times"].as_array().map(|a| a.iter().filter_map(|v| v.as_f64()).collect()).unwrap_or_default();
        let samples = iters.iter().zip(&times).filter(|(i, _)| **i > 0.0).map(|(i, t)| t / i).collect();
        let (Some(mean), Some(median)) = (estimate(&est["mean"]), estimate(&est["median"])) else { continue };
        out.push(Measurement {
            node: node.to_string(),
            bench: bench.to_string(),
            size,
            mean,
            median,
            std_dev: est["std_dev"]["point_estimate"].as_f64().unwrap_or(0.0),
            samples,
            allocs: None,
            alloc_bytes: None,
        });
    }
    Ok(out)
}
