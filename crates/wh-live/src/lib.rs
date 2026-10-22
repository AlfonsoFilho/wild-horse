//! Live values (design doc, "Later milestones": Quokka-style inline values).
//!
//! [`evaluate`] computes the actual values behind a program's specs:
//! both sides of every `example` comparison (or a call's result or raised
//! error), the inputs and results the `ensures` clauses are checked on, a
//! minimal counterexample for each failing `property`, and, for every
//! `expect` of a scenario test, its values plus the value after each flow
//! step it ran through.
//!
//! How: a generated *probe* crate (`.wh/live/`) depends on the generated
//! library (`.wh/gen`) and holds one function per probe. It is built once
//! (debug, shared target dir) and rebuilt incrementally when the spec or a
//! hole changes; the probe binary runs the selected probes, each in its own
//! thread with a timeout, and prints JSON lines. A probe that hangs or
//! crashes the process is reported as such and the binary is restarted for
//! the rest. Values are `Debug` strings, truncated; `@sensitive` types redact
//! themselves, and `sensitive` parameters are shown as `<redacted>`.

pub mod probegen;
pub mod rt;

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use wh_model::Model;
use wh_project::Project;

pub use probegen::{ItemKind, ItemMeta, ProbeMeta, SlotMeta, SourceSpan};

/// Which clauses to evaluate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum LiveScope {
    /// One node's clauses; for a flow (or any def) also the scenario tests
    /// of its module that mention it.
    Node(String),
    /// Every clause and scenario test in one source file (project-relative path).
    File(String),
    All,
}

#[derive(Debug, Clone)]
pub struct LiveOptions {
    pub scope: LiveScope,
    /// Per probe; a probe running longer is reported as `Timeout`.
    pub timeout_ms: u64,
    /// For the whole evaluation (build excluded).
    pub run_timeout_secs: u64,
    /// `Debug` strings longer than this are cut (with `…`).
    pub max_value_len: usize,
    /// Inputs sampled per def for its `ensures` clauses.
    pub samples: usize,
    /// Cases per property.
    pub property_cases: u32,
    /// Cargo target dir (default: the project's shared one).
    pub target_dir: Option<PathBuf>,
}

impl Default for LiveOptions {
    fn default() -> Self {
        Self {
            scope: LiveScope::All,
            timeout_ms: 2000,
            run_timeout_secs: 120,
            max_value_len: 160,
            samples: 3,
            property_cases: 64,
            target_dir: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Status {
    /// The example / expectation / ensures / property holds.
    Pass,
    Fail,
    /// It panicked (a contract violation, `todo!()`, …): see `message`.
    Error,
    Timeout,
    /// The probe crashed the probe process (stack overflow, abort, …).
    Crashed,
    /// Not evaluated: see `message`.
    NotRun,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NamedValue {
    pub label: String,
    pub value: String,
    /// The sub-expression it is the value of (render the value next to it).
    pub span: Option<SourceSpan>,
}

/// One sampled call of a def, for its `ensures` clauses.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Sample {
    pub inputs: Vec<NamedValue>,
    pub result: Option<String>,
    /// Does this clause hold for the sample (None: could not be evaluated).
    pub holds: Option<bool>,
    pub error: Option<String>,
}

/// The value a flow step produced (or the error it raised).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StepValue {
    /// Step node id, e.g. `signup.register#validate[0]`.
    pub step: String,
    pub value: String,
    pub error: bool,
}

/// The live values of one clause.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LiveValue {
    /// Def id, or `module#test:name` for a scenario test.
    pub node: String,
    pub kind: ItemKind,
    /// Index among the def's clauses (`Expect`: among the test's expects).
    pub clause_index: usize,
    pub file: String,
    pub span: SourceSpan,
    pub source: String,
    pub status: Status,
    pub message: Option<String>,
    /// `left`/`right` of a comparison, `result` of a call, or a property's counterexample.
    pub values: Vec<NamedValue>,
    /// `Ensures`: the sampled calls.
    pub samples: Vec<Sample>,
    /// Flow steps executed while evaluating this clause, in order.
    pub steps: Vec<StepValue>,
    /// `Property`: cases run when it held.
    pub cases: Option<u32>,
    /// Time the probe took (shared by the clauses of one probe).
    pub duration_us: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Timings {
    /// Generating `.wh/gen` (skipped when unchanged).
    pub generate_ms: u64,
    /// Generating the probe crate.
    pub probes_ms: u64,
    /// `cargo build` of the probe binary (incremental).
    pub build_ms: u64,
    pub run_ms: u64,
    pub total_ms: u64,
    /// Whether cargo had anything to recompile.
    pub rebuilt: bool,
    /// Probe-process starts (1 + restarts after timeouts and crashes).
    pub processes: usize,
}

/// A step value observed while evaluating a clause (for flow edges).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StepSample {
    pub value: String,
    pub error: bool,
    /// The clause that ran it: (node, kind, clause index).
    pub node: String,
    pub kind: ItemKind,
    pub clause_index: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LiveReport {
    /// node id → its clauses' values, in source order.
    pub nodes: BTreeMap<String, Vec<LiveValue>>,
    /// step id (`module.flow#step[k]`) → values seen on that edge.
    pub steps: BTreeMap<String, Vec<StepSample>>,
    pub timings: Timings,
    /// Compiler errors of probes that were left out (their clauses are `NotRun`).
    pub build_errors: Vec<String>,
    pub notes: Vec<String>,
}

impl LiveReport {
    pub fn values(&self) -> impl Iterator<Item = &LiveValue> {
        self.nodes.values().flatten()
    }

    /// The value of one clause.
    pub fn get(&self, node: &str, kind: ItemKind, clause_index: usize) -> Option<&LiveValue> {
        self.nodes
            .get(node)?
            .iter()
            .find(|v| v.kind == kind && v.clause_index == clause_index)
    }
}

/// Evaluate the clauses in `scope` with default options.
pub fn evaluate(project: &Project, model: &Model, scope: LiveScope) -> Result<LiveReport> {
    evaluate_with(
        project,
        model,
        &LiveOptions {
            scope,
            ..Default::default()
        },
    )
}

fn ms(d: Duration) -> u64 {
    d.as_millis() as u64
}

pub fn live_dir(project: &Project) -> PathBuf {
    project.root.join(".wh/live")
}

fn write_if_changed(path: &Path, content: &str) -> std::io::Result<bool> {
    if std::fs::read_to_string(path).ok().as_deref() == Some(content) {
        return Ok(false);
    }
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    std::fs::write(path, content)?;
    Ok(true)
}

fn probe_manifest(project: &Project) -> String {
    let g = project.gen_options();
    format!(
        "# Generated by Wild Horse (wh-live). Do not edit.\n[package]\nname = \"wh_live_{}_{}\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\nautobins = false\nautotests = false\nautobenches = false\nautoexamples = false\n\n[[bin]]\nname = \"wh_live_probe\"\npath = \"src/main.rs\"\n\n[workspace]\n\n[dependencies]\n{} = {{ package = {:?}, path = {:?} }}\nproptest = \"1\"\ntracing = \"0.1\"\ntracing-subscriber = {{ version = \"0.3\", default-features = false, features = [\"registry\", \"std\"] }}\nserde_json = \"1\"\n\n# fast incremental builds: values, not debugging, are what the probe is for\n[profile.dev]\ndebug = 0\n",
        project.crate_name(),
        project.id(),
        g.crate_name,
        g.package,
        project.gen_dir().to_string_lossy()
    )
}

const MAIN: &str = "//! Generated by Wild Horse (wh-live). Do not edit.\nmod probes;\nmod rt;\n\nfn main() {\n    rt::main(probes::PROBES)\n}\n";

/// Probe ids that failed to compile, and the compiler errors.
type BuildFailure = (Vec<String>, Vec<String>);

struct Built {
    exe: PathBuf,
    rebuilt: bool,
}

/// `cargo build` the probe; on compile errors, the probes they fall in.
fn build(
    dir: &Path,
    target: &Path,
    probes: &[ProbeMeta],
) -> Result<std::result::Result<Built, BuildFailure>> {
    let out = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
        .args(["build", "--message-format=json"])
        .current_dir(dir)
        .env("CARGO_TARGET_DIR", target)
        .stdin(Stdio::null())
        .output()
        .context("running cargo")?;
    let mut exe = None;
    let mut rebuilt = false;
    let mut bad = BTreeSet::new();
    let mut errors = Vec::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        match v["reason"].as_str() {
            Some("compiler-artifact") => {
                if v["fresh"].as_bool() == Some(false) {
                    rebuilt = true;
                }
                if v["target"]["name"].as_str() == Some("wh_live_probe") {
                    exe = v["executable"].as_str().map(PathBuf::from);
                }
            }
            Some("compiler-message") if v["message"]["level"].as_str() == Some("error") => {
                errors.push(v["message"]["rendered"].as_str().unwrap_or("").to_string());
                for s in v["message"]["spans"].as_array().into_iter().flatten() {
                    if s["file_name"]
                        .as_str()
                        .is_some_and(|f| f.ends_with("probes.rs"))
                    {
                        let l = s["line_start"].as_u64().unwrap_or(0) as usize;
                        if let Some(p) = probes.iter().find(|p| p.lines.0 <= l && l <= p.lines.1) {
                            bad.insert(p.id.clone());
                        }
                    }
                }
            }
            _ => {}
        }
    }
    match (out.status.success(), exe) {
        (true, Some(exe)) => Ok(Ok(Built {
            exe: stable_executable(&exe),
            rebuilt,
        })),
        _ => {
            if errors.is_empty() {
                errors.push(
                    String::from_utf8_lossy(&out.stderr)
                        .lines()
                        .rev()
                        .take(20)
                        .collect::<Vec<_>>()
                        .into_iter()
                        .rev()
                        .collect::<Vec<_>>()
                        .join("\n"),
                );
            }
            Ok(Err((bad.into_iter().collect(), errors)))
        }
    }
}

/// The probe binary in `deps/`, which cargo rewrites only when it relinks.
///
/// Cargo re-creates the uplifted `target/debug/wh_live_probe` on every build,
/// even a no-op one, and macOS spends ~0.4 s assessing a new executable on its
/// first run; running the `deps/` file keeps unchanged evaluations fast.
fn stable_executable(exe: &Path) -> PathBuf {
    let (Some(dir), Some(name)) = (exe.parent(), exe.file_name().and_then(|n| n.to_str())) else {
        return exe.to_path_buf();
    };
    let prefix = format!("{name}-");
    let newest = std::fs::read_dir(dir.join("deps"))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_str()
                .is_some_and(|n| n.starts_with(&prefix) && !n.contains('.'))
        })
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .max();
    match newest {
        Some((_, p))
            if std::fs::metadata(&p).map(|m| m.len()).ok()
                == std::fs::metadata(exe).map(|m| m.len()).ok() =>
        {
            p
        }
        _ => exe.to_path_buf(),
    }
}

/// What the probe process printed for one probe.
#[derive(Debug, Clone)]
enum Outcome {
    Done {
        entries: Vec<serde_json::Value>,
        us: Option<u64>,
    },
    Timeout,
    Crashed(String),
}

fn run_probes(
    exe: &Path,
    ids: &[String],
    opts: &LiveOptions,
    timings: &mut Timings,
) -> BTreeMap<String, Outcome> {
    let mut results: BTreeMap<String, Outcome> = BTreeMap::new();
    let deadline = Instant::now() + Duration::from_secs(opts.run_timeout_secs);
    loop {
        let remaining: Vec<&String> = ids.iter().filter(|i| !results.contains_key(*i)).collect();
        if remaining.is_empty() || Instant::now() >= deadline {
            break;
        }
        timings.processes += 1;
        let only = remaining
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join(",");
        let child = Command::new(exe)
            .args([
                "--only",
                &only,
                "--timeout-ms",
                &opts.timeout_ms.to_string(),
                "--max-len",
                &opts.max_value_len.to_string(),
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("RUST_BACKTRACE", "0")
            .spawn();
        let Ok(mut child) = child else { break };
        let mut so = child.stdout.take().expect("piped");
        let mut se = child.stderr.take().expect("piped");
        let out_t = std::thread::spawn(move || {
            let mut s = String::new();
            let _ = so.read_to_string(&mut s);
            s
        });
        let err_t = std::thread::spawn(move || {
            let mut s = String::new();
            let _ = se.read_to_string(&mut s);
            s
        });
        let status = loop {
            if let Ok(Some(st)) = child.try_wait() {
                break Some(st);
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        let stdout = out_t.join().unwrap_or_default();
        let stderr = err_t.join().unwrap_or_default();
        let before = results.len();
        let mut started: Option<String> = None;
        for line in stdout.lines() {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            if let Some(id) = v["start"].as_str() {
                started = Some(id.to_string());
            } else if let Some(id) = v["timeout"].as_str() {
                results.insert(id.to_string(), Outcome::Timeout);
                started = None;
            } else if let Some(id) = v["id"].as_str() {
                let entries = v["entries"].as_array().cloned().unwrap_or_default();
                results.insert(
                    id.to_string(),
                    Outcome::Done {
                        entries,
                        us: v["us"].as_u64(),
                    },
                );
                started = None;
            }
        }
        if let Some(id) = started {
            let why = match status {
                None => Outcome::Timeout,
                Some(st) => {
                    let tail: Vec<&str> = stderr.lines().rev().take(5).collect();
                    Outcome::Crashed(format!(
                        "the probe process exited ({st}): {}",
                        tail.into_iter().rev().collect::<Vec<_>>().join("\n")
                    ))
                }
            };
            results.insert(id, why);
        }
        if results.len() == before {
            break; // no progress
        }
    }
    results
}

fn in_scope(model: &Model, p: &ProbeMeta, scope: &LiveScope) -> bool {
    match scope {
        LiveScope::All => true,
        LiveScope::File(f) => p.items.iter().any(|i| &i.file == f),
        LiveScope::Node(id) => {
            if &p.node == id {
                return true;
            }
            // scenario tests of the node's module that mention it
            let Some(n) = model.nodes.get(id) else {
                return false;
            };
            p.node
                .split_once("#test:")
                .is_some_and(|(m, _)| m == n.module)
                && p.items.iter().any(|i| i.source.contains(&n.name))
        }
    }
}

fn status_of(pass: Option<bool>, error: Option<&str>) -> (Status, Option<String>) {
    match (pass, error) {
        (Some(true), _) => (Status::Pass, None),
        (Some(false), e) => (Status::Fail, e.map(String::from)),
        (None, Some(e)) => (Status::Error, Some(e.to_string())),
        (None, None) => (Status::NotRun, Some("not reached".into())),
    }
}

fn base_value(item: &ItemMeta) -> LiveValue {
    LiveValue {
        node: item.node.clone(),
        kind: item.kind,
        clause_index: item.clause_index,
        file: item.file.clone(),
        span: item.span,
        source: item.source.clone(),
        status: Status::NotRun,
        message: None,
        values: vec![],
        samples: vec![],
        steps: vec![],
        cases: None,
        duration_us: None,
    }
}

fn named(item: &ItemMeta, values: &serde_json::Value) -> Vec<NamedValue> {
    values
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| {
            let slot = v[0].as_u64()? as usize;
            let meta = item.slots.get(slot);
            Some(NamedValue {
                label: meta.map_or_else(|| format!("#{slot}"), |m| m.label.clone()),
                value: v[1].as_str()?.to_string(),
                span: meta.and_then(|m| m.span),
            })
        })
        .collect()
}

fn steps(e: &serde_json::Value) -> Vec<StepValue> {
    e["steps"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|s| {
            Some(StepValue {
                step: s[0].as_str()?.to_string(),
                value: s[1].as_str()?.to_string(),
                error: s[2].as_bool().unwrap_or(false),
            })
        })
        .collect()
}

/// Turn one probe's outcome into live values.
fn merge(p: &ProbeMeta, outcome: Option<&Outcome>) -> Vec<LiveValue> {
    let mut out: Vec<LiveValue> = p.items.iter().map(base_value).collect();
    let (entries, us) = match outcome {
        None => {
            for v in &mut out {
                v.message = Some("not run (time limit of the evaluation reached)".into());
            }
            return out;
        }
        Some(Outcome::Timeout) => {
            for v in &mut out {
                v.status = Status::Timeout;
                v.message = Some("did not finish in time".into());
            }
            return out;
        }
        Some(Outcome::Crashed(why)) => {
            for v in &mut out {
                v.status = Status::Crashed;
                v.message = Some(why.clone());
            }
            return out;
        }
        Some(Outcome::Done { entries, us }) => (entries, *us),
    };
    for v in &mut out {
        v.duration_us = us;
    }
    let kind = p.items.first().map(|i| i.kind);
    if kind == Some(ItemKind::Ensures) {
        for (j, v) in out.iter_mut().enumerate() {
            let item = &p.items[j];
            let n_inputs = item.slots.len().saturating_sub(1);
            for e in entries {
                let vals = named(item, &e["values"]);
                let holds = e["checks"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .find(|c| c[0].as_u64() == Some(j as u64))
                    .and_then(|c| c[1].as_bool());
                let result = e["values"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .find(|x| x[0].as_u64() == Some(n_inputs as u64))
                    .and_then(|x| x[1].as_str())
                    .map(String::from);
                v.samples.push(Sample {
                    inputs: vals.into_iter().filter(|nv| nv.label != "result").collect(),
                    result,
                    holds,
                    error: e["error"].as_str().map(String::from),
                });
            }
            let (status, message) = if v.samples.is_empty() {
                (
                    Status::NotRun,
                    Some("no sampled input satisfied the `requires` clauses".into()),
                )
            } else if v.samples.iter().any(|s| s.holds == Some(false)) {
                (Status::Fail, None)
            } else if let Some(e) = v.samples.iter().find_map(|s| s.error.clone()) {
                // a failed contract names its clause: attribute it
                if e.contains(&item.source.trim_start_matches("ensures ").to_string()) {
                    (Status::Fail, Some(e))
                } else {
                    (Status::Error, Some(e))
                }
            } else {
                (Status::Pass, None)
            };
            v.status = status;
            v.message = message;
        }
        return out;
    }
    for (j, v) in out.iter_mut().enumerate() {
        let item = &p.items[j];
        let key = if kind == Some(ItemKind::Expect) { j } else { 0 };
        let Some(e) = entries
            .iter()
            .find(|e| e["key"].as_u64() == Some(key as u64))
        else {
            v.message = Some("not reached: an earlier expectation panicked".into());
            continue;
        };
        v.values = named(item, &e["values"]);
        v.steps = steps(e);
        v.cases = e["cases"].as_u64().map(|c| c as u32);
        let (status, message) = status_of(e["pass"].as_bool(), e["error"].as_str());
        v.status = status;
        v.message = message;
    }
    out
}

/// Evaluate the clauses selected by `opts.scope`.
pub fn evaluate_with(project: &Project, model: &Model, opts: &LiveOptions) -> Result<LiveReport> {
    let t0 = Instant::now();
    let mut report = LiveReport::default();
    project.generate(model)?;
    report.timings.generate_ms = ms(t0.elapsed());

    let dir = live_dir(project);
    let target = opts
        .target_dir
        .clone()
        .unwrap_or_else(|| project.target_dir());
    let lib = project.gen_options().crate_name;
    let mut settings = probegen::GenSettings {
        samples: opts.samples,
        cases: opts.property_cases,
        exclude: vec![],
    };
    let mut attempt = 0;
    let (generated, built) = loop {
        let t = Instant::now();
        let g = probegen::generate(model, &lib, &settings);
        write_if_changed(&dir.join("Cargo.toml"), &probe_manifest(project))?;
        write_if_changed(&dir.join("src/main.rs"), MAIN)?;
        write_if_changed(&dir.join("src/rt.rs"), rt::PROBE_RT)?;
        write_if_changed(&dir.join("src/probes.rs"), &g.code)?;
        write_if_changed(&dir.join(".gitignore"), "*\n")?;
        report.timings.probes_ms += ms(t.elapsed());
        let t = Instant::now();
        let b = build(&dir, &target, &g.probes)?;
        report.timings.build_ms += ms(t.elapsed());
        match b {
            Ok(built) => break (g, built),
            Err((bad, errors)) => {
                attempt += 1;
                if bad.is_empty() || attempt > 2 {
                    // not attributable to probes: the generated library itself does not build
                    report.build_errors = errors;
                    report.notes.push("the probe does not build; fix the errors (or run `wh check`) to see live values".into());
                    report.timings.total_ms = ms(t0.elapsed());
                    return Ok(report);
                }
                report.build_errors.extend(errors);
                settings.exclude.extend(bad);
            }
        }
    };
    report.timings.rebuilt = built.rebuilt;

    let selected: Vec<&ProbeMeta> = generated
        .probes
        .iter()
        .filter(|p| in_scope(model, p, &opts.scope))
        .collect();
    let ids: Vec<String> = selected.iter().map(|p| p.id.clone()).collect();
    let t = Instant::now();
    let outcomes = if ids.is_empty() {
        BTreeMap::new()
    } else {
        run_probes(&built.exe, &ids, opts, &mut report.timings)
    };
    report.timings.run_ms = ms(t.elapsed());

    let mut values: Vec<LiveValue> = Vec::new();
    for p in &selected {
        values.extend(merge(p, outcomes.get(&p.id)));
    }
    // probes left out because they did not compile
    let excluded: BTreeSet<&String> = settings.exclude.iter().collect();
    let all = probegen::generate(
        model,
        &lib,
        &probegen::GenSettings {
            exclude: vec![],
            ..settings.clone()
        },
    );
    for p in all
        .probes
        .iter()
        .filter(|p| excluded.contains(&p.id) && in_scope(model, p, &opts.scope))
    {
        for item in &p.items {
            let mut v = base_value(item);
            v.message =
                Some("the probe for this clause does not compile (see build_errors)".into());
            values.push(v);
        }
    }
    for u in generated.unprobed.iter().filter(|u| match &opts.scope {
        LiveScope::All => true,
        LiveScope::File(f) => &u.item.file == f,
        LiveScope::Node(id) => &u.item.node == id,
    }) {
        let mut v = base_value(&u.item);
        v.message = Some(u.reason.clone());
        values.push(v);
    }
    values.sort_by(|a, b| {
        (&a.file, a.span.start, a.kind as u8).cmp(&(&b.file, b.span.start, b.kind as u8))
    });
    for v in values {
        for s in &v.steps {
            report
                .steps
                .entry(s.step.clone())
                .or_default()
                .push(StepSample {
                    value: s.value.clone(),
                    error: s.error,
                    node: v.node.clone(),
                    kind: v.kind,
                    clause_index: v.clause_index,
                });
        }
        report.nodes.entry(v.node.clone()).or_default().push(v);
    }
    report.timings.total_ms = ms(t0.elapsed());
    Ok(report)
}
