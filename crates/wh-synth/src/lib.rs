//! Role B — the compiler agent as a pipeline stage (design doc, Core idea 2):
//!
//! ```text
//! parse → check → codegen skeleton → SYNTHESIZE holes → verify → cargo build
//! ```
//!
//! For every hole whose synthesis key is missing or stale, a job prepares a
//! sandboxed scratch crate, asks a [`Driver`] (an ACP agent in production) to
//! write exactly one file, verifies the result with the compiler, clippy and
//! the node's specs, feeds failures back for a bounded number of repair
//! rounds, and on success copies the file into `impl/` with a key header —
//! the committed lockfile of synthesized code.

pub mod acp;
pub mod cache;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result};
use serde::Serialize;
use wh_codegen::{GenOptions, HoleSpec};
use wh_model::{Model, NodeKind};

pub const KEY_PREFIX: &str = "// wh:key=";
pub const PINNED: &str = "// wh:pinned";

/// State of a hole implementation on disk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum HoleState {
    /// Still the generated `todo!()` stub (or missing).
    Missing,
    /// Synthesized for the current spec.
    UpToDate,
    /// Synthesized for an older spec.
    Stale { recorded: String },
    /// Up to date for the spec, but the prose explaining the node changed
    /// since: never re-synthesized automatically, shown so a human can judge.
    SoftStale,
    /// Hand-written and pinned: verified but never regenerated.
    Pinned,
    /// Implemented by hand without a key (treated like pinned, but reported).
    Manual,
}

/// The synthesis key recorded in a hole file's header (`// wh:key=<key> prose=<hash>`).
pub fn read_key(text: &str) -> Option<&str> {
    text.lines().next()?.strip_prefix(KEY_PREFIX)?.split_whitespace().next()
}

/// The prose hash recorded in a hole file's header, if any.
pub fn read_prose_key(text: &str) -> Option<&str> {
    text.lines().next()?.strip_prefix(KEY_PREFIX)?.split_whitespace().find_map(|t| t.strip_prefix("prose="))
}

/// Short hash of the prose attached to a node.
pub fn prose_key(model: &Model, node: &str) -> String {
    let prose = model.nodes.get(node).and_then(|n| n.prose.clone()).unwrap_or_default();
    let normalized = prose.split_whitespace().collect::<Vec<_>>().join(" ");
    blake3::hash(normalized.as_bytes()).to_hex()[..8].to_string()
}

/// `hole_state`, also reporting [`HoleState::SoftStale`] when the recorded prose hash differs.
pub fn hole_state_with_prose(hole: &HoleSpec, key: &str, prose: &str) -> HoleState {
    let st = hole_state(hole, key);
    if st != HoleState::UpToDate {
        return st;
    }
    let text = std::fs::read_to_string(&hole.path).unwrap_or_default();
    match read_prose_key(&text) {
        Some(p) if p != prose => HoleState::SoftStale,
        _ => HoleState::UpToDate,
    }
}

pub fn hole_state(hole: &HoleSpec, key: &str) -> HoleState {
    let Ok(text) = std::fs::read_to_string(&hole.path) else { return HoleState::Missing };
    if text.lines().take(3).any(|l| l.trim() == PINNED) {
        return HoleState::Pinned;
    }
    if text.contains("todo!(\"synthesize ") {
        return HoleState::Missing;
    }
    match read_key(&text) {
        Some(k) if k == key => HoleState::UpToDate,
        Some(k) => HoleState::Stale { recorded: k.to_string() },
        None => HoleState::Manual,
    }
}

/// Everything an agent needs to implement one hole (the compiler's IR).
#[derive(Debug, Clone, Serialize)]
pub struct SynthesisRequest {
    pub node: String,
    pub key: String,
    pub round: usize,
    /// Scratch directory the agent works in (its session cwd).
    pub workdir: PathBuf,
    /// The one file the agent may write (absolute, inside `workdir`).
    pub hole_file: PathBuf,
    /// The full prompt text.
    pub prompt: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct AttemptOutcome {
    /// Agent transcript (for the inspector).
    pub transcript: String,
}

/// Something that can carry out a synthesis attempt — an ACP agent session
/// in production, a scripted fake in tests.
pub trait Driver: Send + Sync {
    /// Identifies agent and model; part of the synthesis key.
    fn id(&self) -> String;
    fn attempt(&self, req: &SynthesisRequest) -> Result<AttemptOutcome>;
    /// Total cost reported by the agent so far, in USD, if it reports cost.
    fn spent_usd(&self) -> Option<f64> {
        None
    }
}

/// Limits on what one synthesis run may spend.
#[derive(Debug, Clone, Default)]
pub struct Budget {
    /// Maximum agent cost in USD (needs an agent that reports cost via ACP `usage_update`).
    pub max_usd: Option<f64>,
    /// Maximum number of agent attempts (rounds across all jobs).
    pub max_attempts: Option<usize>,
}

/// A driver that stops calling the agent once a [`Budget`] is used up.
pub struct Budgeted<D: Driver> {
    pub inner: D,
    pub budget: Budget,
    attempts: Mutex<usize>,
}

impl<D: Driver> Budgeted<D> {
    pub fn new(inner: D, budget: Budget) -> Self {
        Self { inner, budget, attempts: Mutex::new(0) }
    }

    pub fn attempts(&self) -> usize {
        *self.attempts.lock().unwrap()
    }
}

impl<D: Driver> Driver for Budgeted<D> {
    fn id(&self) -> String {
        self.inner.id()
    }

    fn attempt(&self, req: &SynthesisRequest) -> Result<AttemptOutcome> {
        {
            let mut n = self.attempts.lock().unwrap();
            if let Some(max) = self.budget.max_attempts {
                if *n >= max {
                    anyhow::bail!("synthesis budget exhausted: {max} agent attempt(s) used");
                }
            }
            if let (Some(max), Some(spent)) = (self.budget.max_usd, self.inner.spent_usd()) {
                if spent >= max {
                    anyhow::bail!("synthesis budget exhausted: ${spent:.2} of ${max:.2} spent");
                }
            }
            *n += 1;
        }
        self.inner.attempt(req)
    }

    fn spent_usd(&self) -> Option<f64> {
        self.inner.spent_usd()
    }
}

#[derive(Debug, Clone, Serialize)]
pub enum Verdict {
    Accepted,
    Rejected(String),
    /// The specs could not run because other holes they call are still stubs
    /// (not the agent's fault; re-verified once those holes are implemented).
    Blocked(Vec<String>),
}

/// Holes a node's specs depend on: every hole called (directly, or through a
/// flow) from its contracts, examples, properties and benches. A hole must be
/// synthesized after these, or its specs cannot run.
pub fn spec_dependencies(model: &Model, node: &str) -> std::collections::BTreeSet<String> {
    use wh_syntax::ast::{ClauseKind, Expr, ExprKind};
    fn walk(e: &Expr, out: &mut Vec<String>) {
        match &e.kind {
            ExprKind::Call { func, args } => {
                match &func.kind {
                    ExprKind::Name(n) => out.push(n.clone()),
                    ExprKind::Attr { base, attr } => {
                        if let ExprKind::Name(b) = &base.kind {
                            out.push(format!("{b}.{}", attr.name));
                        }
                        walk(base, out);
                    }
                    _ => walk(func, out),
                }
                args.iter().for_each(|a| walk(&a.value, out));
            }
            ExprKind::Attr { base, .. } => walk(base, out),
            ExprKind::Unary { expr, .. } => walk(expr, out),
            ExprKind::Index { base, index } => {
                walk(base, out);
                walk(index, out);
            }
            ExprKind::Slice { base, lo, hi } => {
                walk(base, out);
                lo.iter().chain(hi.iter()).for_each(|x| walk(x, out));
            }
            ExprKind::Binary { lhs, rhs, .. } => {
                walk(lhs, out);
                walk(rhs, out);
            }
            ExprKind::Compare { first, rest } => {
                walk(first, out);
                rest.iter().for_each(|(_, x)| walk(x, out));
            }
            ExprKind::IfElse { then, cond, otherwise } => {
                walk(then, out);
                walk(cond, out);
                walk(otherwise, out);
            }
            ExprKind::Lambda { body, .. } => walk(body, out),
            ExprKind::List(xs) | ExprKind::Tuple(xs) => xs.iter().for_each(|x| walk(x, out)),
            ExprKind::Comprehension { elt, iter, cond, .. } => {
                walk(elt, out);
                walk(iter, out);
                cond.iter().for_each(|c| walk(c, out));
            }
            ExprKind::Quantified { iter, body, .. } => {
                walk(iter, out);
                walk(body, out);
            }
            _ => {}
        }
    }
    fn holes_of(model: &Model, id: &str, seen: &mut std::collections::BTreeSet<String>, out: &mut std::collections::BTreeSet<String>) {
        if !seen.insert(id.to_string()) {
            return;
        }
        let Some(n) = model.nodes.get(id) else { return };
        match &n.kind {
            NodeKind::Def(d) if d.extern_path.is_none() => {
                out.insert(id.to_string());
            }
            NodeKind::Adapter(_) => {
                out.insert(id.to_string());
            }
            NodeKind::Flow(f) => {
                for st in &f.steps {
                    if let Some(c) = &st.callee {
                        holes_of(model, c, seen, out);
                    }
                }
            }
            _ => {}
        }
    }
    let mut out = std::collections::BTreeSet::new();
    let Some(n) = model.nodes.get(node) else { return out };
    let Some(def) = n.def() else { return out };
    let mut names = Vec::new();
    for c in &def.clauses {
        match &c.kind {
            ClauseKind::Requires(e) | ClauseKind::Ensures(e) | ClauseKind::Mutate(e) => walk(e, &mut names),
            ClauseKind::RaisesWhen { when, .. } => walk(when, &mut names),
            ClauseKind::Example { expr, .. } => walk(expr, &mut names),
            ClauseKind::Property { body, given, .. } => {
                walk(body, &mut names);
                given.iter().for_each(|(_, g)| walk(g, &mut names));
            }
            ClauseKind::Bench { body, .. } => walk(body, &mut names),
            _ => {}
        }
    }
    let mut seen = std::collections::BTreeSet::new();
    for name in names {
        if let Some(id) = model.resolve_path(&n.module, &name) {
            holes_of(model, &id, &mut seen, &mut out);
        }
    }
    out.remove(node);
    out
}

/// Is the hole file at `path` still the generated stub (so a candidate may replace it)?
fn hole_state_text(path: &Path) -> bool {
    std::fs::read_to_string(path).map_or(true, |t| t.contains("todo!(\"synthesize "))
}

/// Holes named by `todo!("synthesize …")` panics in test output (adapter
/// methods map to their adapter).
fn stub_panics(output: &str, holes: &[HoleSpec]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for part in output.split("not yet implemented: synthesize ").skip(1) {
        let name: String = part.chars().take_while(|c| !c.is_whitespace()).collect();
        let owner = holes
            .iter()
            .map(|h| h.node.as_str())
            .filter(|h| name == *h || name.starts_with(&format!("{h}.")))
            .max_by_key(|h| h.len());
        if let Some(o) = owner {
            if !out.iter().any(|x| x == o) {
                out.push(o.to_string());
            }
        }
    }
    out
}

#[derive(Debug, Clone, Serialize)]
pub struct RoundReport {
    pub round: usize,
    pub verdict: Verdict,
    pub transcript: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct JobReport {
    pub node: String,
    pub key: String,
    pub accepted: bool,
    /// Reused from the synthesis cache (no agent call).
    pub from_cache: bool,
    /// Caveats about the verification, e.g. an adapter that could only be compile-checked.
    pub notes: Vec<String>,
    pub rounds: Vec<RoundReport>,
}

/// Progress notifications for UIs.
#[derive(Debug, Clone, Serialize)]
pub enum Progress {
    Queued { node: String },
    Attempt { node: String, round: usize },
    Verifying { node: String, round: usize },
    Accepted { node: String, rounds: usize },
    /// Reused from the synthesis cache and verified.
    Restored { node: String },
    Failed { node: String, reason: String },
}

/// Scheduler state shared by the worker threads of one run.
struct Sched {
    pending: std::collections::VecDeque<(HoleSpec, Option<Vec<RoundReport>>)>,
    running: std::collections::BTreeSet<String>,
    /// node → accepted?
    finished: BTreeMap<String, bool>,
    /// dependencies discovered at verification time (stub panics)
    extra_deps: BTreeMap<String, std::collections::BTreeSet<String>>,
    requeues: BTreeMap<String, usize>,
    /// Latest code of blocked jobs: overlaid into other jobs' sandboxes so
    /// holes whose specs call each other (a cycle) can be verified together.
    candidates: BTreeMap<String, String>,
}

enum JobOutcome {
    Done(JobReport),
    /// Specs called holes that are still stubs; the agent's file is kept.
    Blocked { on: Vec<String>, rounds: Vec<RoundReport>, candidate: String },
}

/// An extra acceptance check run after the verification gate accepts a hole,
/// e.g. the performance-baseline check of `wh-perf` (which depends on this
/// crate, so it is injected). Arguments: the hole and the scratch `impl/` root
/// holding the candidate. Returns failure messages (empty = pass); they are
/// fed back to the agent like any other rejection.
pub type AcceptHook<'h> = dyn Fn(&HoleSpec, &Path) -> Result<Vec<String>> + Sync + 'h;

pub struct Synthesizer<'a> {
    pub model: &'a Model,
    /// Project root (holds `impl/`).
    pub root: &'a Path,
    /// Package-name prefix unique to the project (scratch crates share a target dir).
    pub package: &'a str,
    /// Cargo target dir shared by all jobs (cargo serializes builds; agents run in parallel).
    pub target_dir: &'a Path,
    /// Store of accepted implementations to reuse before calling the agent.
    pub cache: Option<&'a cache::SynthCache>,
    pub max_rounds: usize,
    pub concurrency: usize,
    pub driver: &'a dyn Driver,
    pub on_progress: &'a (dyn Fn(Progress) + Sync),
}

fn slug(node: &str) -> String {
    node.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '_' }).collect()
}

fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    let Ok(rd) = std::fs::read_dir(from) else { return Ok(()) };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            copy_tree(&p, &to.join(e.file_name()))?;
        } else {
            std::fs::copy(&p, to.join(e.file_name()))?;
        }
    }
    Ok(())
}

/// Digest of every file under `dir` except `skip` (sandbox integrity check).
fn tree_digest(dir: &Path, skip: &Path) -> String {
    fn walk(d: &Path, skip: &Path, out: &mut Vec<(PathBuf, Vec<u8>)>) {
        let Ok(rd) = std::fs::read_dir(d) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if p == skip || p.file_name().is_some_and(|n| n == "target") {
                continue;
            }
            if p.is_dir() {
                walk(&p, skip, out);
            } else {
                out.push((p.clone(), std::fs::read(&p).unwrap_or_default()));
            }
        }
    }
    let mut files = Vec::new();
    walk(dir, skip, &mut files);
    files.sort();
    let mut h = blake3::Hasher::new();
    for (p, c) in files {
        h.update(p.to_string_lossy().as_bytes());
        h.update(&c);
    }
    h.finalize().to_hex().to_string()
}

impl<'a> Synthesizer<'a> {
    pub fn impl_root(&self) -> PathBuf {
        self.root.join("impl")
    }

    pub fn key(&self, node: &str) -> String {
        wh_model::hash::spec_key(self.model, node, &self.driver.id())
    }

    /// Holes needing synthesis (missing or stale), or the given ones when forced.
    /// Up-to-date implementations are added to the cache on the way.
    pub fn plan(&self, holes: &[HoleSpec], only: Option<&str>, force: bool) -> Vec<HoleSpec> {
        if let Some(cache) = self.cache {
            seed_cache(self.model, holes, &self.driver.id(), cache);
        }
        holes
            .iter()
            .filter(|h| only.is_none_or(|o| h.node == o))
            .filter(|h| {
                let st = hole_state(h, &self.key(&h.node));
                force && only.is_some() || matches!(st, HoleState::Missing | HoleState::Stale { .. })
            })
            .cloned()
            .collect()
    }

    /// Run jobs with bounded parallelism. Each worker slot has its own cargo
    /// target dir (dependencies compile once per slot, builds run in parallel).
    pub fn run(&self, jobs: Vec<HoleSpec>) -> Vec<JobReport> {
        self.run_with(jobs, None)
    }

    /// [`run`](Self::run) with an extra acceptance check (see [`AcceptHook`]).
    pub fn run_with(&self, jobs: Vec<HoleSpec>, hook: Option<&AcceptHook>) -> Vec<JobReport> {
        for j in &jobs {
            (self.on_progress)(Progress::Queued { node: j.node.clone() });
        }
        let (mut restored, jobs) = self.restore(jobs);
        // dependency-aware scheduling: a hole whose specs call other holes in
        // this run starts only after those are accepted (or have failed)
        let in_run: std::collections::BTreeSet<String> = jobs.iter().map(|j| j.node.clone()).collect();
        let deps: BTreeMap<String, std::collections::BTreeSet<String>> = jobs
            .iter()
            .map(|j| (j.node.clone(), spec_dependencies(self.model, &j.node).into_iter().filter(|d| in_run.contains(d)).collect()))
            .collect();
        let state = Mutex::new(Sched {
            pending: jobs.into_iter().map(|j| (j, None)).collect(),
            running: std::collections::BTreeSet::new(),
            finished: BTreeMap::new(),
            extra_deps: BTreeMap::new(),
            requeues: BTreeMap::new(),
            candidates: BTreeMap::new(),
        });
        let paths: BTreeMap<String, PathBuf> = state.lock().unwrap().pending.iter().map(|(j, _)| (j.node.clone(), j.path.clone())).collect();
        let wake = std::sync::Condvar::new();
        let reports = Mutex::new(Vec::new());
        let slots = self.concurrency.max(1);
        let fail = |node: &str, reason: String, rounds: Vec<RoundReport>| {
            (self.on_progress)(Progress::Failed { node: node.to_string(), reason: reason.clone() });
            JobReport { node: node.to_string(), key: self.key(node), accepted: false, from_cache: false, notes: vec![reason], rounds }
        };
        std::thread::scope(|s| {
            for _ in 0..slots {
                let (state, wake, reports, deps, fail, in_run, paths) = (&state, &wake, &reports, &deps, &fail, &in_run, &paths);
                s.spawn(move || {
                    loop {
                        // pick a job whose dependencies are settled
                        let (job, resume, overlay) = {
                            let mut st = state.lock().unwrap();
                            loop {
                                if st.pending.is_empty() {
                                    return;
                                }
                                let deps_of = |st: &Sched, n: &str| -> Vec<String> {
                                    let mut d: Vec<String> = deps.get(n).into_iter().flatten().cloned().collect();
                                    d.extend(st.extra_deps.get(n).into_iter().flatten().cloned());
                                    d
                                };
                                // a dependency failed: this job cannot be verified
                                if let Some(i) = st.pending.iter().position(|(j, _)| deps_of(&st, &j.node).iter().any(|d| st.finished.get(d) == Some(&false))) {
                                    let (j, rounds) = st.pending.remove(i).unwrap();
                                    let mut bad: Vec<String> = deps_of(&st, &j.node).into_iter().filter(|d| st.finished.get(d) == Some(&false)).collect();
                                    bad.sort();
                                    bad.dedup();
                                    st.finished.insert(j.node.clone(), false);
                                    let reason = format!("blocked: its specs call {}, which could not be synthesized", bad.join(", "));
                                    reports.lock().unwrap().push(fail(&j.node, reason, rounds.unwrap_or_default()));
                                    wake.notify_all();
                                    continue;
                                }
                                let ready = st.pending.iter().position(|(j, _)| deps_of(&st, &j.node).iter().all(|d| st.finished.contains_key(d)));
                                // nothing ready and nothing running: a cycle — break it
                                let pick = ready.or_else(|| st.running.is_empty().then_some(0));
                                if let Some(i) = pick {
                                    let (j, resume) = st.pending.remove(i).unwrap();
                                    st.running.insert(j.node.clone());
                                    let overlay: Vec<(PathBuf, String)> = st
                                        .candidates
                                        .iter()
                                        .filter(|(n, _)| *n != &j.node)
                                        .filter_map(|(n, c)| Some((paths.get(n)?.clone(), c.clone())))
                                        .collect();
                                    break (j, resume, overlay);
                                }
                                st = wake.wait(st).unwrap();
                            }
                        };
                        let outcome = self.job(&job, resume, &overlay, hook).unwrap_or_else(|e| {
                            JobOutcome::Done(fail(&job.node, format!("{e:#}"), vec![]))
                        });
                        let mut st = state.lock().unwrap();
                        st.running.remove(&job.node);
                        match outcome {
                            JobOutcome::Done(report) => {
                                st.finished.insert(job.node.clone(), report.accepted);
                                st.candidates.remove(&job.node);
                                reports.lock().unwrap().push(report);
                            }
                            JobOutcome::Blocked { on, rounds, candidate } => {
                                st.candidates.insert(job.node.clone(), candidate);
                                let n = st.requeues.entry(job.node.clone()).or_insert(0);
                                *n += 1;
                                let waitable = on.iter().all(|d| in_run.contains(d) && d != &job.node);
                                if waitable && *n <= 3 {
                                    // wait for those holes, then re-verify without a new agent round
                                    (self.on_progress)(Progress::Queued { node: job.node.clone() });
                                    st.extra_deps.entry(job.node.clone()).or_default().extend(on);
                                    st.pending.push_back((job, Some(rounds)));
                                } else {
                                    st.finished.insert(job.node.clone(), false);
                                    let reason = format!(
                                        "blocked: its specs call {}, which {} not implemented; synthesize {} first",
                                        on.join(", "),
                                        if on.len() == 1 { "is" } else { "are" },
                                        if on.len() == 1 { "it" } else { "them" }
                                    );
                                    reports.lock().unwrap().push(fail(&job.node, reason, rounds));
                                }
                            }
                        }
                        wake.notify_all();
                    }
                });
            }
        });
        let mut r = reports.into_inner().unwrap();
        r.append(&mut restored);
        r.sort_by(|a, b| a.node.cmp(&b.node));
        r
    }

    /// Caveats for an accepted hole: no specs at all means it was only compile-checked.
    fn notes_for(&self, hole: &HoleSpec, ran: usize) -> Vec<String> {
        if ran > 0 {
            return vec![];
        }
        let what = if hole.signature.starts_with("adapter ") { "adapter" } else { "hole" };
        let note = format!(
            "{what} `{}` has no specs of its own, so it was only compile- and lint-checked; add examples/properties{}",
            hole.node,
            if what == "adapter" { " or capability contract tests (test resources come from the environment)" } else { "" }
        );
        tracing::warn!("{note}");
        vec![note]
    }

    fn cache_key(&self, hole: &HoleSpec) -> String {
        cache::SynthCache::key(&self.key(&hole.node), hole)
    }

    /// Install cached implementations for the jobs that have one, verify them
    /// all in one build + test run, and return (reports of the restored ones,
    /// jobs still needing the agent).
    fn restore(&self, jobs: Vec<HoleSpec>) -> (Vec<JobReport>, Vec<HoleSpec>) {
        let Some(cache) = self.cache else { return (vec![], jobs) };
        let (hits, mut misses): (Vec<HoleSpec>, Vec<HoleSpec>) =
            jobs.into_iter().partition(|h| cache.get(&self.cache_key(h)).is_some());
        if hits.is_empty() {
            return (vec![], misses);
        }
        let mut previous = Vec::new();
        for h in &hits {
            previous.push(std::fs::read_to_string(&h.path).ok());
            if let Some(d) = h.path.parent() {
                let _ = std::fs::create_dir_all(d);
            }
            let _ = std::fs::write(&h.path, cache.get(&self.cache_key(h)).unwrap_or_default());
        }
        let failed = self.verify_installed(&hits).unwrap_or_else(|e| {
            tracing::warn!(error = %e, "could not verify cached implementations; synthesizing them instead");
            hits.iter().map(|h| h.node.clone()).collect()
        });
        let mut reports = Vec::new();
        for (h, prev) in hits.into_iter().zip(previous) {
            if failed.contains(&h.node) {
                cache.evict(&self.cache_key(&h));
                let _ = std::fs::write(&h.path, prev.unwrap_or_else(|| h.template.clone()));
                misses.push(h);
            } else {
                (self.on_progress)(Progress::Restored { node: h.node.clone() });
                reports.push(JobReport { node: h.node.clone(), key: self.key(&h.node), accepted: true, from_cache: true, notes: vec![], rounds: vec![] });
            }
        }
        (reports, misses)
    }

    /// Build the project with the installed implementations and run its specs;
    /// returns the nodes among `holes` that fail to compile or fail a spec.
    fn verify_installed(&self, holes: &[HoleSpec]) -> Result<std::collections::BTreeSet<String>> {
        let nodes: std::collections::BTreeSet<String> = holes.iter().map(|h| h.node.clone()).collect();
        let opts = GenOptions {
            package: format!("{}_restore", self.package),
            crate_name: "wh_restore".into(),
            impl_root: self.impl_root(),
            contracts_in_release: false,
        };
        let crate_dir = self.root.join(".wh/synth/_restore");
        let (generated, _) = wh_codegen::generate_cached(self.model, &opts, &crate_dir)?;
        let cargo = wh_build::Cargo { crate_dir: &crate_dir, target_dir: self.target_dir, manifest: &generated.manifest };
        let mut failed = std::collections::BTreeSet::new();
        let lint = cargo.clippy()?;
        for d in lint.errors() {
            match &d.node {
                Some(n) if nodes.contains(n) => {
                    failed.insert(n.clone());
                }
                Some(_) => {}
                // an error we cannot attribute: trust none of them
                None => return Ok(nodes),
            }
        }
        if !failed.is_empty() || !lint.success {
            return Ok(if failed.is_empty() { nodes } else { failed });
        }
        let tested = cargo.test(None)?;
        for t in tested.failed_tests() {
            if let Some(n) = t.node.as_ref().filter(|n| nodes.contains(*n)) {
                failed.insert(n.clone());
            }
        }
        Ok(failed)
    }

    /// One synthesis job. With `resume`, the scratch sandbox of an earlier
    /// attempt that was blocked on other holes is refreshed with their (now
    /// available) implementations and re-verified before asking the agent again.
    fn job(&self, hole: &HoleSpec, resume: Option<Vec<RoundReport>>, overlay: &[(PathBuf, String)], hook: Option<&AcceptHook>) -> Result<JobOutcome> {
        let key = self.key(&hole.node);
        let work = self.root.join(".wh/synth").join(slug(&hole.node));
        let scratch_impl = work.join("impl");
        let rel = hole.path.strip_prefix(self.impl_root()).context("hole outside impl/")?;
        let hole_file = scratch_impl.join(rel);
        let resuming = resume.is_some();
        if resuming {
            // keep the agent's file, refresh everything else from impl/
            let ours = std::fs::read_to_string(&hole_file).unwrap_or_else(|_| hole.template.clone());
            let _ = std::fs::remove_dir_all(&scratch_impl);
            copy_tree(&self.impl_root(), &scratch_impl).context("refreshing the sandbox")?;
            std::fs::write(&hole_file, ours)?;
        } else {
            let _ = std::fs::remove_dir_all(&work);
            copy_tree(&self.impl_root(), &scratch_impl).context("preparing the sandbox")?;
            // start every job from the clean template
            if let Some(d) = hole_file.parent() {
                std::fs::create_dir_all(d)?;
            }
            std::fs::write(&hole_file, &hole.template)?;
        }
        // candidate code of blocked holes this one may call
        for (path, code) in overlay {
            if let Ok(rel) = path.strip_prefix(self.impl_root()) {
                let f = scratch_impl.join(rel);
                if f != hole_file && hole_state_text(&f) {
                    std::fs::write(&f, code)?;
                }
            }
        }
        let opts = GenOptions {
            // unique package per project and job: all jobs share one target dir
            package: format!("{}_synth_{}", self.package, slug(&hole.node)),
            crate_name: format!("wh_synth_{}", slug(&hole.node)),
            impl_root: scratch_impl.clone(),
            contracts_in_release: false,
        };
        let generated = wh_codegen::generate(self.model, &opts);
        let crate_dir = work.join("gen");
        wh_codegen::write(&generated, &crate_dir)?;
        let target = self.target_dir.to_path_buf();

        let max = self.max_rounds.max(1);
        let mut rounds = resume.unwrap_or_default();
        let mut feedback: Option<String> = None;
        let mut verify_first = resuming;
        loop {
            let (verdict, specs_ran, transcript, round) = if verify_first {
                verify_first = false;
                let round = rounds.len();
                (self.on_progress)(Progress::Verifying { node: hole.node.clone(), round });
                let (v, n) = self.verify(hole, &hole_file, &crate_dir, &target, &generated.manifest)?;
                (v, n, None, round)
            } else {
                if rounds.len() >= max {
                    break;
                }
                let round = rounds.len() + 1;
                (self.on_progress)(Progress::Attempt { node: hole.node.clone(), round });
                let before = tree_digest(&work, &hole_file);
                let prompt = build_prompt(self.model, hole, &crate_dir, &hole_file, feedback.as_deref());
                let req = SynthesisRequest { node: hole.node.clone(), key: key.clone(), round, workdir: work.clone(), hole_file: hole_file.clone(), prompt };
                let outcome = self.driver.attempt(&req)?;
                (self.on_progress)(Progress::Verifying { node: hole.node.clone(), round });
                let (v, n) = if tree_digest(&work, &hole_file) != before {
                    // the sandbox was modified outside the hole file: restore it and reject
                    let hole_text = std::fs::read_to_string(&hole_file).unwrap_or_default();
                    let _ = std::fs::remove_dir_all(&scratch_impl);
                    let _ = std::fs::remove_dir_all(crate_dir.join("src"));
                    copy_tree(&self.impl_root(), &scratch_impl)?;
                    wh_codegen::write(&generated, &crate_dir)?;
                    std::fs::write(&hole_file, hole_text)?;
                    (Verdict::Rejected("the agent modified files other than the hole file; only the hole file may be written".into()), 0)
                } else {
                    self.verify(hole, &hole_file, &crate_dir, &target, &generated.manifest)?
                };
                (v, n, Some(outcome.transcript), round)
            };
            // the injected check (performance baseline) only runs on otherwise accepted code
            let verdict = match (verdict, hook) {
                (Verdict::Accepted, Some(h)) => match h(hole, &scratch_impl) {
                    Ok(f) if f.is_empty() => Verdict::Accepted,
                    Ok(f) => Verdict::Rejected(format!("the code is correct but fails the performance gate:\n{}", f.join("\n"))),
                    Err(e) => {
                        tracing::warn!(error = %e, node = %hole.node, "acceptance hook failed; accepting without it");
                        Verdict::Accepted
                    }
                },
                (v, _) => v,
            };
            match &verdict {
                Verdict::Rejected(why) => feedback = Some(why.clone()),
                Verdict::Blocked(on) => {
                    let on = on.clone();
                    if let Some(t) = transcript {
                        rounds.push(RoundReport { round, verdict, transcript: t });
                    }
                    let candidate = std::fs::read_to_string(&hole_file).unwrap_or_default();
                    return Ok(JobOutcome::Blocked { on, rounds, candidate });
                }
                Verdict::Accepted => {}
            }
            let accepted = matches!(verdict, Verdict::Accepted);
            match transcript {
                Some(t) => rounds.push(RoundReport { round, verdict, transcript: t }),
                None => {
                    // a re-verification after dependencies landed: amend the last round
                    if let Some(last) = rounds.last_mut() {
                        last.verdict = verdict;
                    }
                }
            }
            if accepted {
                let body = std::fs::read_to_string(&hole_file)?;
                let body = body.lines().skip_while(|l| l.starts_with(KEY_PREFIX)).collect::<Vec<_>>().join("\n");
                if let Some(d) = hole.path.parent() {
                    std::fs::create_dir_all(d)?;
                }
                let locked = format!("{KEY_PREFIX}{key} prose={}\n{}\n", prose_key(self.model, &hole.node), body.trim_end());
                std::fs::write(&hole.path, &locked)?;
                if let Some(cache) = self.cache {
                    let meta = cache::EntryMeta {
                        node: hole.node.clone(),
                        spec_key: key.clone(),
                        signature: hole.signature.clone(),
                        agent: self.driver.id(),
                        rounds: round,
                        created: cache::now(),
                    };
                    if let Err(e) = cache.put(&self.cache_key(hole), &locked, &meta) {
                        tracing::warn!(error = %e, "could not store the implementation in the synthesis cache");
                    }
                }
                (self.on_progress)(Progress::Accepted { node: hole.node.clone(), rounds: round });
                let report = JobReport { node: hole.node.clone(), key, accepted: true, from_cache: false, notes: self.notes_for(hole, specs_ran), rounds };
                return Ok(JobOutcome::Done(report));
            }
        }
        let reason = match rounds.last().map(|r| &r.verdict) {
            Some(Verdict::Rejected(w)) => w.lines().next().unwrap_or("").to_string(),
            _ => "no attempts".into(),
        };
        (self.on_progress)(Progress::Failed { node: hole.node.clone(), reason });
        Ok(JobOutcome::Done(JobReport { node: hole.node.clone(), key, accepted: false, from_cache: false, notes: vec![], rounds }))
    }

    /// The verification gate: signature, compiler, lints, then specs.
    /// The verification gate; on acceptance it also returns how many of the node's own specs ran.
    fn verify(&self, hole: &HoleSpec, file: &Path, crate_dir: &Path, target: &Path, manifest: &wh_codegen::Manifest) -> Result<(Verdict, usize)> {
        let text = std::fs::read_to_string(file).unwrap_or_default();
        if text.contains("todo!(") || text.contains("unimplemented!(") {
            return Ok((Verdict::Rejected("the hole file still contains `todo!()`/`unimplemented!()`".into()), 0));
        }
        if !hole.signature.starts_with("adapter ") && !squash(&text).contains(&squash(&hole.signature)) {
            return Ok((Verdict::Rejected(format!("the signature changed; it must stay exactly:\n{}", hole.signature)), 0));
        }
        let cargo = wh_build::Cargo { crate_dir, target_dir: target, manifest };
        let lint = cargo.clippy()?;
        let file_s = file.to_string_lossy();
        let mine: Vec<&wh_build::NodeDiagnostic> = lint
            .diagnostics
            .iter()
            .filter(|d| d.level == "error" || (d.level == "warning" && d.file.as_deref().is_some_and(|f| file_s.ends_with(f) || f.ends_with(&*file_s))))
            .collect();
        let errors: Vec<&&wh_build::NodeDiagnostic> = mine.iter().filter(|d| d.level == "error").collect();
        if !errors.is_empty() || !lint.success {
            let mut s = String::from("the code does not compile or violates a lint gate:\n");
            for d in errors.iter().take(8) {
                s.push_str(d.rendered.trim_end());
                s.push('\n');
            }
            if errors.is_empty() {
                for d in lint.diagnostics.iter().take(4) {
                    s.push_str(d.rendered.trim_end());
                    s.push('\n');
                }
            }
            return Ok((Verdict::Rejected(s), 0));
        }
        let tested = cargo.test(Some(&hole.node))?;
        let ran = tested.tests.iter().filter(|t| t.node.as_deref() == Some(hole.node.as_str())).count();
        let failed: Vec<&wh_build::TestOutcome> = tested.failed_tests().collect();
        if !failed.is_empty() {
            // every failure is another hole's stub panicking: blocked, not wrong
            let per_test: Vec<Vec<String>> = failed
                .iter()
                .map(|t| stub_panics(&t.output, &manifest.holes).into_iter().filter(|n| n != &hole.node).collect())
                .collect();
            if per_test.iter().all(|b| !b.is_empty()) {
                let mut on: Vec<String> = per_test.into_iter().flatten().collect();
                on.sort();
                on.dedup();
                return Ok((Verdict::Blocked(on), 0));
            }
            let mut s = String::from("specs failed:\n");
            for t in failed.iter().take(6) {
                s.push_str(&format!("--- {}\n{}\n", t.name, t.output.lines().filter(|l| !l.contains("\u{1b}[")).take(12).collect::<Vec<_>>().join("\n")));
            }
            return Ok((Verdict::Rejected(s), 0));
        }
        if !tested.success {
            return Ok((Verdict::Rejected("the test run failed to build".into()), 0));
        }
        Ok((Verdict::Accepted, ran))
    }
}

/// Whitespace-normalized source, with `mut` on parameter bindings dropped
/// (`fn f(mut x: T)` has the same signature as `fn f(x: T)`).
/// Module paths are dropped too (`std::time::Duration` = `Duration` after a
/// `use`); the compiler still rejects real changes, because the generated
/// wrapper calls the hole with fixed types.
fn squash(s: &str) -> String {
    let joined = s.split_whitespace().collect::<Vec<_>>().join(" ");
    let joined = joined.replace("(mut ", "(").replace(", mut ", ", ");
    // drop `path::` qualifiers (lower-case path segments followed by `::`)
    let mut out = String::with_capacity(joined.len());
    let chars: Vec<char> = joined.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let start_of_word = i == 0 || !(chars[i - 1].is_alphanumeric() || chars[i - 1] == '_');
        if start_of_word && (chars[i].is_ascii_lowercase() || chars[i] == '_') {
            let mut j = i;
            while j < chars.len() && (chars[j].is_alphanumeric() || chars[j] == '_') {
                j += 1;
            }
            if j + 1 < chars.len() && chars[j] == ':' && chars[j + 1] == ':' {
                i = j + 2;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// The prompt handed to the compiler agent.
pub fn build_prompt(model: &Model, hole: &HoleSpec, crate_dir: &Path, hole_file: &Path, feedback: Option<&str>) -> String {
    let node_id = hole.node.rsplit_once('.').filter(|(m, _)| model.nodes.contains_key(*m)).map_or(hole.node.as_str(), |(m, _)| m);
    let node = model.nodes.get(node_id);
    let mut p = String::new();
    p.push_str("You are the code-synthesis stage of the Wild Horse compiler.\n\n");
    p.push_str(&format!("Implement `{}` by editing exactly one file:\n    {}\n\n", hole.node, hole_file.display()));
    p.push_str("Rules:\n");
    p.push_str("- Write only that file. Do not create, edit or delete any other file. Do not run commands; the compiler verifies your work.\n");
    p.push_str("- Replace the `todo!()` bodies. Keep every signature exactly as it is. You may add private helper items in the file.\n");
    p.push_str("- Use only the Rust standard library and the crates declared in the generated Cargo.toml.\n");
    p.push_str("- Use `tracing::{debug, trace}` for diagnostics; never `println!`, `eprintln!` or `dbg!`.\n");
    p.push_str("- Never log, trace or format into errors the values of `sensitive` fields, or of types/parameters holding personal or secret data (emails, names, tokens, passwords); log counts, lengths or ids instead.\n");
    p.push_str("- No `unsafe` unless the spec says `unsafe allow`. The code must be idiomatic, efficient Rust that passes clippy.\n");
    p.push_str("- Calling other functions of the program is allowed: they are in scope through `use crate::<module>::*` and enforce their own contracts.\n\n");
    if let Some(node) = node {
        if let Some(prose) = &node.prose {
            p.push_str("## Intent (from the literate source)\n\n");
            p.push_str(prose.trim());
            p.push_str("\n\n");
        }
        let text = &model.files[node.file].text[node.span.range()];
        p.push_str("## Specification (Wild Horse source)\n\n```wh\n");
        p.push_str(text.trim_end());
        p.push_str("\n```\n\n");
        for dep in wh_model::hash::dependencies(model, &node.id) {
            if let Some(d) = model.nodes.get(&dep) {
                if matches!(d.kind, NodeKind::Struct(_) | NodeKind::Enum(_) | NodeKind::Refined { .. } | NodeKind::Capability(_) | NodeKind::Alias(_)) {
                    let t = &model.files[d.file].text[d.span.range()];
                    p.push_str(&format!("Referenced definition `{dep}`:\n```wh\n{}\n```\n", t.trim_end()));
                }
            }
        }
        if !node.patterns.is_empty() {
            p.push_str(&format!("\nImplementation pattern(s): {}.\n", node.patterns.join(", ")));
        }
    }
    p.push_str(&format!(
        "\n## Rust context\n\nThe generated crate is at {} (read-only). The module file `src/modules/{}.rs` defines the types, the capability traits and the contract-checking wrapper that calls your function; read it for the exact Rust types.\nRequired signature:\n```rust\n{}\n```\n",
        crate_dir.display(),
        node.map_or(String::new(), |n| wh_codegen::rust::module_ident(&n.module)),
        hole.signature
    ));
    if let Some(f) = feedback {
        p.push_str("\n## Your previous attempt was rejected\n\n");
        p.push_str(f.trim());
        p.push_str("\n\nFix the problems and write the complete file again.\n");
    }
    p
}

/// Store every up-to-date (locked, current-key) implementation in the cache,
/// so the committed lockfile of one branch is reusable from another.
pub fn seed_cache(model: &Model, holes: &[HoleSpec], driver_id: &str, cache: &cache::SynthCache) -> usize {
    let mut added = 0;
    for h in holes {
        let key = wh_model::hash::spec_key(model, &h.node, driver_id);
        if hole_state(h, &key) != HoleState::UpToDate {
            continue;
        }
        let ck = cache::SynthCache::key(&key, h);
        if cache.get(&ck).is_some() {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&h.path) else { continue };
        let meta = cache::EntryMeta {
            node: h.node.clone(),
            spec_key: key,
            signature: h.signature.clone(),
            agent: driver_id.to_string(),
            rounds: 0,
            created: cache::now(),
        };
        if cache.put(&ck, &text, &meta).is_ok() {
            added += 1;
        }
    }
    added
}

/// Summaries of hole states for `wh status` and the IDE.
pub fn states(model: &Model, holes: &[HoleSpec], driver_id: &str) -> BTreeMap<String, HoleState> {
    holes
        .iter()
        .map(|h| {
            let key = wh_model::hash::spec_key(model, &h.node, driver_id);
            (h.node.clone(), hole_state_with_prose(h, &key, &prose_key(model, &h.node)))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake;
    impl Driver for Fake {
        fn id(&self) -> String {
            "fake".into()
        }
        fn attempt(&self, _: &SynthesisRequest) -> Result<AttemptOutcome> {
            Ok(AttemptOutcome::default())
        }
        fn spent_usd(&self) -> Option<f64> {
            Some(0.75)
        }
    }

    fn req() -> SynthesisRequest {
        SynthesisRequest { node: "m.f".into(), key: "k".into(), round: 1, workdir: "/tmp".into(), hole_file: "/tmp/f.rs".into(), prompt: String::new() }
    }

    #[test]
    fn signature_comparison_ignores_paths_and_mut() {
        let want = "pub fn step(game: &Game, input: Input, dt: std::time::Duration) -> Game";
        let got = "use std::time::Duration;\npub fn step(game: &Game, input: Input, dt: Duration) -> Game {";
        assert!(squash(got).contains(&squash(want)));
        let got = "pub fn add(mut xs: Vec<u8>, y: crate::m::T) -> u8";
        assert!(squash(got).contains(&squash("pub fn add(xs: Vec<u8>, y: T) -> u8")));
        assert!(!squash("pub fn step(game: Game) -> Game").contains(&squash("pub fn step(game: &Game) -> Game")));
    }

    #[test]
    fn budgets_stop_the_agent() {
        let d = Budgeted::new(Fake, Budget { max_usd: None, max_attempts: Some(2) });
        assert!(d.attempt(&req()).is_ok() && d.attempt(&req()).is_ok());
        let e = d.attempt(&req()).unwrap_err().to_string();
        assert!(e.contains("2 agent attempt(s)"), "{e}");
        let d = Budgeted::new(Fake, Budget { max_usd: Some(0.5), max_attempts: None });
        assert!(d.attempt(&req()).unwrap_err().to_string().contains("$0.75 of $0.50"));
    }

    #[test]
    fn headers_and_soft_staleness() {
        let text = "// wh:key=abc123 prose=deadbeef\nuse x;\n";
        assert_eq!(read_key(text), Some("abc123"));
        assert_eq!(read_prose_key(text), Some("deadbeef"));
        assert_eq!(read_key("// wh:key=old\n"), Some("old"), "headers without prose still parse");
        let dir = std::env::temp_dir().join(format!("wh-soft-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let hole = HoleSpec {
            node: "m.f".into(),
            module_name: "h_f".into(),
            path: dir.join("f.rs"),
            template: String::new(),
            signature: String::new(),
        };
        std::fs::write(&hole.path, text).unwrap();
        assert_eq!(hole_state_with_prose(&hole, "abc123", "deadbeef"), HoleState::UpToDate);
        assert_eq!(hole_state_with_prose(&hole, "abc123", "cafebabe"), HoleState::SoftStale);
        assert!(matches!(hole_state_with_prose(&hole, "other", "deadbeef"), HoleState::Stale { .. }));
        std::fs::write(&hole.path, "// wh:key=abc123\nuse x;\n").unwrap();
        assert_eq!(hole_state_with_prose(&hole, "abc123", "cafebabe"), HoleState::UpToDate, "no recorded prose: not soft-stale");
        let _ = std::fs::remove_dir_all(dir);
    }
}
