//! Profile-weighted prioritization: run a workload with tracing on, sum the
//! `time.busy` of every span per `wh.node`, and rank holes by their share of
//! the traced time (self time: a span's busy time minus its children's).
//!
//! Workloads:
//! - the project's scenario tests (`test "…":` blocks), built in release
//!   without LTO and with `tracing`'s release level caps disabled, so hole
//!   spans are kept; their test-writer output is parsed;
//! - runs of the app binary with the arguments listed in `wildhorse.toml`:
//!   ```toml
//!   [perf]
//!   workload = [["a@b.io", "30"], ["kid@x.io", "12"]]
//!   ```
//!   each with `WH_TRACE_FILE` set (JSON events).
//!
//! Tracing overhead is part of every span's time, so tiny holes are
//! over-weighted; the ranking is meant to order work, not to report timings.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use wh_model::Model;
use wh_project::Project;

use crate::measure::{run_with_timeout, tail};
use crate::spec::PerfSpec;

/// One closed span: its node, its parent's node, and its busy time.
#[derive(Debug, Clone, PartialEq)]
pub struct SpanClose {
    pub node: String,
    pub parent: Option<String>,
    pub busy_ns: f64,
}

/// Time spent in one node over the whole workload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodeTime {
    pub node: String,
    pub calls: u64,
    /// Sum of busy time, children included.
    pub total_ns: f64,
    /// Busy time minus the busy time of child spans.
    pub self_ns: f64,
}

/// A hole and its weight in the workload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HoleWeight {
    pub node: String,
    pub calls: u64,
    pub self_ns: f64,
    pub total_ns: f64,
    /// self time / all traced time.
    pub share: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Profile {
    /// What ran (`scenario tests`, `signup_cli a@b.io 30`, …).
    pub workload: Vec<String>,
    /// Every hole that ran, heaviest first.
    pub holes: Vec<HoleWeight>,
    /// Every traced node (flows, steps, holes, adapter methods).
    pub nodes: Vec<NodeTime>,
    /// All traced time (sum of root spans).
    pub total_ns: f64,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ProfileOptions {
    /// Run the scenario tests.
    pub scenarios: bool,
    /// App runs (argument lists, program name excluded).
    pub app_runs: Vec<Vec<String>>,
    /// Tracing filter for the scenario tests (`debug` keeps hole spans and
    /// drops per-call `trace` events, whose formatting would dominate).
    pub level: String,
    pub target_dir: Option<PathBuf>,
    pub timeout_secs: u64,
}

impl Default for ProfileOptions {
    fn default() -> Self {
        Self { scenarios: true, app_runs: vec![], level: "debug".into(), target_dir: None, timeout_secs: 900 }
    }
}

impl ProfileOptions {
    /// Defaults plus the `[perf] workload` (and `level`) of `wildhorse.toml`.
    pub fn from_project(project: &Project) -> Self {
        let mut o = Self::default();
        let Ok(text) = std::fs::read_to_string(project.root.join("wildhorse.toml")) else { return o };
        let Ok(v) = text.parse::<toml::Table>() else { return o };
        if let Some(perf) = v.get("perf").and_then(|p| p.as_table()) {
            if let Some(runs) = perf.get("workload").and_then(|w| w.as_array()) {
                o.app_runs = runs
                    .iter()
                    .filter_map(|r| r.as_array())
                    .map(|r| r.iter().filter_map(|a| a.as_str().map(String::from)).collect())
                    .collect();
            }
            if let Some(l) = perf.get("level").and_then(|l| l.as_str()) {
                o.level = l.to_string();
            }
        }
        o
    }
}

pub(crate) fn parse_duration_ns(s: &str) -> Option<f64> {
    let s = s.trim();
    let (num, unit) = s.split_at(s.find(|c: char| !(c.is_ascii_digit() || c == '.'))?);
    let v: f64 = num.parse().ok()?;
    Some(
        v * match unit {
            "ns" => 1.0,
            "µs" | "us" => 1e3,
            "ms" => 1e6,
            "s" => 1e9,
            _ => return None,
        },
    )
}

/// Parse a span-close line of the test writer's text format:
/// `<time> DEBUG a{wh.node="m.f" …}:b{wh.node="m.g" …}: target: close time.busy=1.2µs time.idle=…`.
pub fn parse_text_close(line: &str) -> Option<SpanClose> {
    let at = line.find(": close time.busy=")?;
    let busy = line[at + ": close time.busy=".len()..].split_whitespace().next()?;
    let chain = &line[..at];
    let nodes: Vec<&str> = chain.split("wh.node=\"").skip(1).filter_map(|s| s.split('"').next()).collect();
    let (node, parents) = nodes.split_last()?;
    Some(SpanClose { node: node.to_string(), parent: parents.last().map(|p| p.to_string()), busy_ns: parse_duration_ns(busy)? })
}

/// Parse a span-close event of `WH_TRACE_FILE` (tracing-subscriber JSON with the span list).
pub fn parse_json_close(line: &str) -> Option<SpanClose> {
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    let busy = parse_duration_ns(v["fields"]["time.busy"].as_str()?)?;
    let node = v["span"]["wh.node"].as_str()?.to_string();
    let spans = v["spans"].as_array();
    // the span list ends with the closing span itself
    let parent = spans.and_then(|s| s.iter().rev().skip(1).find_map(|x| x["wh.node"].as_str())).map(String::from);
    Some(SpanClose { node, parent, busy_ns: busy })
}

/// Sum busy and self time per node.
pub fn aggregate(closes: &[SpanClose]) -> (Vec<NodeTime>, f64) {
    let mut by: BTreeMap<String, NodeTime> = BTreeMap::new();
    let mut total = 0.0;
    for c in closes {
        let e = by.entry(c.node.clone()).or_insert_with(|| NodeTime { node: c.node.clone(), calls: 0, total_ns: 0.0, self_ns: 0.0 });
        e.calls += 1;
        e.total_ns += c.busy_ns;
        e.self_ns += c.busy_ns;
        match &c.parent {
            Some(p) => {
                by.entry(p.clone()).or_insert_with(|| NodeTime { node: p.clone(), calls: 0, total_ns: 0.0, self_ns: 0.0 }).self_ns -= c.busy_ns;
            }
            None => total += c.busy_ns,
        }
    }
    let mut v: Vec<NodeTime> = by.into_values().collect();
    for n in &mut v {
        n.self_ns = n.self_ns.max(0.0);
    }
    v.sort_by(|a, b| b.self_ns.total_cmp(&a.self_ns));
    (v, total)
}

/// Rank the holes of `model` by their share of a profile's traced time.
/// Adapter method spans (`m.Adapter.method`) count for their adapter's hole.
pub fn rank_holes(holes: &[String], nodes: &[NodeTime], total_ns: f64) -> Vec<HoleWeight> {
    let mut by: BTreeMap<&str, HoleWeight> = BTreeMap::new();
    for n in nodes {
        let hole = holes
            .iter()
            .find(|h| **h == n.node)
            .or_else(|| n.node.rsplit_once('.').and_then(|(owner, _)| holes.iter().find(|h| *h == owner)));
        let Some(h) = hole else { continue };
        let w = by.entry(h).or_insert_with(|| HoleWeight { node: h.clone(), calls: 0, self_ns: 0.0, total_ns: 0.0, share: 0.0 });
        w.calls += n.calls;
        w.self_ns += n.self_ns;
        w.total_ns += n.total_ns;
    }
    let mut v: Vec<HoleWeight> = by.into_values().collect();
    for w in &mut v {
        w.share = if total_ns > 0.0 { w.self_ns / total_ns } else { 0.0 };
    }
    v.sort_by(|a, b| b.self_ns.total_cmp(&a.self_ns).then_with(|| a.node.cmp(&b.node)));
    v
}

/// Cargo flags for profiling builds: release speed without LTO (fast to
/// build), and without the `tracing/release_max_level_*` default feature
/// that would compile hole spans out.
const PROFILE_BUILD: &[&str] = &[
    "--release",
    "--no-default-features",
    "--config",
    "profile.release.lto=false",
    "--config",
    "profile.release.codegen-units=16",
];

/// Profile the default workload (scenario tests + `[perf] workload` runs of `app`).
pub fn hot_holes(project: &Project, model: &Model, app: Option<&str>) -> Result<Profile> {
    hot_holes_with(project, model, app, &ProfileOptions::from_project(project))
}

pub fn hot_holes_with(project: &Project, model: &Model, app: Option<&str>, opts: &ProfileOptions) -> Result<Profile> {
    let generated = project.generate(model)?;
    let missing = wh_build::unimplemented_holes(&generated.manifest);
    if !missing.is_empty() {
        bail!("{} hole(s) are not implemented yet; synthesize them before profiling", missing.len());
    }
    let target = opts.target_dir.clone().unwrap_or_else(|| project.target_dir());
    let gen_dir = project.gen_dir();
    let timeout = Duration::from_secs(opts.timeout_secs);
    let mut closes = Vec::new();
    let mut workload = Vec::new();
    let mut notes = Vec::new();
    let cargo = || {
        let mut c = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()));
        c.current_dir(&gen_dir).env("CARGO_TARGET_DIR", &target);
        c
    };

    if opts.scenarios && generated.manifest.tests.keys().any(|t| t.contains("__test__")) {
        let mut c = cargo();
        c.arg("test").args(PROFILE_BUILD).args(["--quiet", "--lib", "--", "__test__", "--nocapture", "--test-threads=1"]);
        c.env("WH_LOG", &opts.level).env("NO_COLOR", "1");
        let (ok, stdout, stderr) = run_with_timeout(c, timeout)?;
        let before = closes.len();
        closes.extend(stdout.lines().chain(stderr.lines()).filter_map(parse_text_close));
        if !ok {
            notes.push(format!("some scenario tests failed:\n{}", tail(&stdout, 15)));
        }
        if closes.len() == before && !ok {
            bail!("the scenario tests did not run:\n{}", tail(&stderr, 30));
        }
        workload.push("scenario tests".to_string());
    }

    if !opts.app_runs.is_empty() {
        let bin = match app {
            Some(a) => generated
                .manifest
                .bins
                .iter()
                .find(|(id, name)| id.as_str() == a || name.as_str() == a || id.rsplit('.').next() == Some(a))
                .map(|(_, n)| n.clone())
                .with_context(|| format!("no app `{a}`"))?,
            None => generated.manifest.bins.values().next().cloned().context("the project has no `app` to run")?,
        };
        let mut c = cargo();
        c.arg("build").args(PROFILE_BUILD).args(["--bin", &bin, "--message-format=json"]);
        let (ok, stdout, stderr) = run_with_timeout(c, timeout)?;
        if !ok {
            bail!("the app does not build:\n{}", tail(&stderr, 30));
        }
        let exe = stdout
            .lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .find_map(|v| v["executable"].as_str().filter(|e| e.ends_with(&*bin) || e.ends_with(&format!("{bin}.exe"))).map(PathBuf::from))
            .context("cargo did not report the app binary")?;
        let dir = project.root.join(".wh/profile");
        std::fs::create_dir_all(&dir)?;
        for (i, args) in opts.app_runs.iter().enumerate() {
            let trace = dir.join(format!("trace-{i}.jsonl"));
            let _ = std::fs::remove_file(&trace);
            let mut c = Command::new(&exe);
            c.args(args).current_dir(&project.root).env("WH_TRACE_FILE", &trace).env("WH_LOG", "off").stdin(std::process::Stdio::null());
            let (ok, _, stderr) = run_with_timeout(c, timeout)?;
            if !ok {
                notes.push(format!("`{bin} {}` exited with an error: {}", args.join(" "), stderr.lines().last().unwrap_or("")));
            }
            closes.extend(std::fs::read_to_string(&trace).unwrap_or_default().lines().filter_map(parse_json_close));
            workload.push(format!("{bin} {}", args.join(" ")));
        }
    }

    if workload.is_empty() {
        notes.push("no workload: the project has no scenario tests and no `[perf] workload` in wildhorse.toml".into());
    }
    let (nodes, total_ns) = aggregate(&closes);
    let holes: Vec<String> = generated.manifest.holes.iter().map(|h| h.node.clone()).collect();
    Ok(Profile { workload, holes: rank_holes(&holes, &nodes, total_ns), nodes, total_ns, notes })
}

/// Holes with an `optimize` clause, hottest first (by `profile`), then the
/// ones the workload never reached, by id: the order for `wh optimize`
/// without a node.
pub fn optimization_queue(model: &Model, profile: Option<&Profile>) -> Vec<String> {
    let mut wanted: Vec<String> = model
        .nodes
        .values()
        .filter(|n| n.is_hole() && PerfSpec::of(model, &n.id).is_some_and(|s| s.optimize.is_some()))
        .map(|n| n.id.clone())
        .collect();
    let weight = |id: &str| profile.and_then(|p| p.holes.iter().find(|h| h.node == id)).map_or(0.0, |h| h.self_ns);
    wanted.sort_by(|a, b| weight(b).total_cmp(&weight(a)).then_with(|| a.cmp(b)));
    wanted
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_span_closes() {
        let text = r#"2026-10-22T21:47:31.566892Z DEBUG signup.register{wh.node="signup.register"}:wh.step{wh.node="signup.register#normalize_form[0]"}:signup.normalize_form{wh.node="signup.normalize_form" form=ValidForm { email: <redacted>, age: Age(30), guardian: None }}:signup.normalize_email{wh.node="signup.normalize_email" e=Email("a@x.io")}: signup::signup: close time.busy=6.08µs time.idle=167ns"#;
        let c = parse_text_close(text).unwrap();
        assert_eq!(c.node, "signup.normalize_email");
        assert_eq!(c.parent.as_deref(), Some("signup.normalize_form"));
        assert!((c.busy_ns - 6080.0).abs() < 1e-6);
        assert!(parse_text_close("2026 TRACE a{wh.node=\"m.f\"}: m: result=1").is_none());
        let json = r#"{"timestamp":"t","level":"DEBUG","fields":{"message":"close","time.busy":"1.62ms","time.idle":"3µs"},"target":"signup::main","span":{"name":"main.parse_cli","wh.node":"main.parse_cli"},"spans":[{"name":"main.signup_cli","wh.node":"main.signup_cli"},{"name":"main.parse_cli","wh.node":"main.parse_cli"}]}"#;
        let c = parse_json_close(json).unwrap();
        assert_eq!((c.node.as_str(), c.parent.as_deref()), ("main.parse_cli", Some("main.signup_cli")));
        assert!((c.busy_ns - 1.62e6).abs() < 1e-3);
    }

    #[test]
    fn self_time_and_ranking() {
        let c = |n: &str, p: Option<&str>, t: f64| SpanClose { node: n.into(), parent: p.map(String::from), busy_ns: t };
        let closes = vec![
            c("m.a", Some("m.flow#a[0]"), 30.0),
            c("m.Db.insert", Some("m.save"), 40.0),
            c("m.save", Some("m.flow#save[0]"), 50.0),
            c("m.flow#a[0]", Some("m.flow"), 35.0),
            c("m.flow#save[0]", Some("m.flow"), 55.0),
            c("m.flow", None, 100.0),
        ];
        let (nodes, total) = aggregate(&closes);
        assert_eq!(total, 100.0);
        let holes = vec!["m.a".to_string(), "m.save".to_string(), "m.Db".to_string()];
        let ranked = rank_holes(&holes, &nodes, total);
        let names: Vec<&str> = ranked.iter().map(|h| h.node.as_str()).collect();
        assert_eq!(names, ["m.Db", "m.a", "m.save"]);
        assert!((ranked[0].share - 0.4).abs() < 1e-9 && (ranked[2].self_ns - 10.0).abs() < 1e-9);
    }
}
