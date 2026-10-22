//! Per-node performance baselines, committed with the lock:
//! `impl/.bench/<node>.json`, one entry per machine/target fingerprint.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::gates::{Comparison, compare};
use crate::measure::{BenchConfig, Measurement};
use crate::spec::Target;

/// Identifies the machine and toolchain measurements were taken on;
/// numbers are only compared with baselines of the same fingerprint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fingerprint {
    /// `<arch>-<os>-<hash>`: the key in baseline files.
    pub key: String,
    pub os: String,
    pub arch: String,
    pub cpu: String,
    pub cpus: usize,
    pub rustc: String,
}

fn command_output(cmd: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(cmd).args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn cpu_model() -> String {
    if cfg!(target_os = "macos")
        && let Some(s) = command_output("sysctl", &["-n", "machdep.cpu.brand_string"])
    {
        return s;
    }
    if let Ok(info) = std::fs::read_to_string("/proc/cpuinfo")
        && let Some(l) = info.lines().find(|l| l.starts_with("model name"))
    {
        return l.split(':').nth(1).unwrap_or("").trim().to_string();
    }
    "unknown".into()
}

/// The fingerprint of this machine and the active Rust toolchain.
pub fn fingerprint() -> Fingerprint {
    let rustc = command_output(&std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into()), &["-V"]).unwrap_or_else(|| "unknown".into());
    let cpus = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
    let (os, arch, cpu) = (std::env::consts::OS.to_string(), std::env::consts::ARCH.to_string(), cpu_model());
    let hash = blake3::hash(format!("{os}\0{arch}\0{cpu}\0{cpus}\0{rustc}").as_bytes()).to_hex();
    Fingerprint { key: format!("{arch}-{os}-{}", &hash[..10]), os, arch, cpu, cpus, rustc }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BaselineEntry {
    pub fingerprint: Fingerprint,
    /// Hash of the implementation measured (hole file without its key line).
    pub impl_hash: String,
    /// Unix time of the measurement.
    pub recorded: u64,
    pub config: BenchConfig,
    pub measurements: Vec<Measurement>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct BaselineFile {
    pub node: String,
    /// fingerprint key → entry
    pub entries: BTreeMap<String, BaselineEntry>,
}

pub fn baseline_dir(project_root: &Path) -> PathBuf {
    project_root.join("impl/.bench")
}

pub fn baseline_path(project_root: &Path, node: &str) -> PathBuf {
    baseline_dir(project_root).join(format!("{node}.json"))
}

/// Hash of an implementation, ignoring its `// wh:key=` header.
pub fn impl_hash(code: &str) -> String {
    let body: String = code.lines().skip_while(|l| l.starts_with(wh_synth::KEY_PREFIX)).collect::<Vec<_>>().join("\n");
    blake3::hash(body.trim().as_bytes()).to_hex()[..16].to_string()
}

pub fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

pub fn load_file(project_root: &Path, node: &str) -> Option<BaselineFile> {
    serde_json::from_str(&std::fs::read_to_string(baseline_path(project_root, node)).ok()?).ok()
}

/// The baseline of `node` for this fingerprint, if one was recorded.
pub fn load(project_root: &Path, node: &str, fp: &Fingerprint) -> Option<BaselineEntry> {
    load_file(project_root, node)?.entries.remove(&fp.key)
}

/// Record (or replace) the baseline of `node` for the entry's fingerprint.
pub fn store(project_root: &Path, node: &str, entry: BaselineEntry) -> Result<PathBuf> {
    let path = baseline_path(project_root, node);
    let mut file = load_file(project_root, node).unwrap_or_else(|| BaselineFile { node: node.to_string(), entries: BTreeMap::new() });
    file.entries.insert(entry.fingerprint.key.clone(), entry);
    std::fs::create_dir_all(baseline_dir(project_root))?;
    std::fs::write(&path, serde_json::to_string_pretty(&file)? + "\n").with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
}

/// Threshold used when either side is a quick measurement (few samples).
pub const QUICK_THRESHOLD: f64 = 0.10;

/// Few samples or a short measurement: noisy, compared with a wider threshold.
pub fn is_quick(cfg: &BenchConfig) -> bool {
    cfg.sample_size < 20 || cfg.measurement_secs < 1.0
}

/// The regression threshold for comparing measurements taken with these configs.
pub fn regression_threshold(baseline: &BenchConfig, current: &BenchConfig, threshold: f64) -> f64 {
    if is_quick(baseline) || is_quick(current) { threshold.max(QUICK_THRESHOLD) } else { threshold }
}

/// Compare current measurements (taken with `current_cfg`) with a baseline.
/// `regressed` needs the geometric mean worse by more than the threshold
/// and at least one significantly worse benchmark; the threshold widens to
/// [`QUICK_THRESHOLD`] when either side is a quick measurement.
pub fn regression(baseline: &BaselineEntry, current: &[Measurement], current_cfg: &BenchConfig, threshold: f64, alpha: f64) -> Comparison {
    let t = regression_threshold(&baseline.config, current_cfg, threshold);
    compare(&baseline.measurements, current, Target::Speed, t, alpha)
}
