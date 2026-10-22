//! The seam between the IDE and the build / synthesis / agent machinery.
//!
//! The UI only talks to a [`Backend`]: it forwards user intents (check,
//! synthesize a hole, run the app, ask the author agent) and drains
//! [`BackendEvent`]s on a short timer. [`MockBackend`] fakes plausible
//! events so every panel can be exercised without `wh-project`/`wh-agent`.

// Some variants are only produced by real backends, not the mock.
#![allow(dead_code)]

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use wh_model::{Model, NodeId, NodeKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Trace,
    Debug,
    /// Checker lints (style and documentation hints), shown muted.
    Lint,
    Info,
    Warn,
    Error,
}

impl LogLevel {
    pub fn label(self) -> &'static str {
        match self {
            LogLevel::Trace => "TRACE",
            LogLevel::Debug => "DEBUG",
            LogLevel::Lint => "LINT",
            LogLevel::Info => "INFO",
            LogLevel::Warn => "WARN",
            LogLevel::Error => "ERROR",
        }
    }
}

/// Per-hole synthesis state, shown in the agent panel queue and as node badges.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SynthStatus {
    Stale,
    Queued,
    Synthesizing,
    Verifying { round: u32, of: u32 },
    Accepted,
    /// Reused from the synthesis cache and verified.
    Restored,
    /// Up to date for the spec, but its prose changed since (never re-synthesized automatically).
    SoftStale,
    Failed(String),
}

impl SynthStatus {
    pub fn label(&self) -> String {
        match self {
            SynthStatus::Stale => "◌ stale".into(),
            SynthStatus::Queued => "queued".into(),
            SynthStatus::Synthesizing => "⋯ synthesizing".into(),
            SynthStatus::Verifying { round, of } => format!("⋯ verifying {round}/{of}"),
            SynthStatus::Accepted => "✓ accepted".into(),
            SynthStatus::Restored => "♻ restored from cache".into(),
            SynthStatus::SoftStale => "◐ prose changed".into(),
            SynthStatus::Failed(m) => format!("✗ {m}"),
        }
    }
}

/// A node's spec results from the latest `cargo test` (badges ✓ / ✗,
/// distinct from the synthesis status).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NodeResult {
    pub passed: usize,
    pub failed: usize,
    /// `test name: first line of its output` for each failing test.
    pub failures: Vec<String>,
}

impl NodeResult {
    pub fn ok(&self) -> bool {
        self.failed == 0
    }

    pub fn badge(&self) -> String {
        if self.ok() { format!("✓ {}", self.passed) } else { format!("✗ {}/{}", self.failed, self.passed + self.failed) }
    }

    pub fn summary(&self) -> String {
        if self.ok() {
            format!("{} spec test(s) pass", self.passed)
        } else {
            format!("{} of {} spec test(s) fail: {}", self.failed, self.passed + self.failed, self.failures.join("; "))
        }
    }
}

/// Why a hole's synthesis failed, for the recovery card (UX review A5 / N7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SynthFailure {
    pub node: NodeId,
    /// The gate that rejected the last attempt: "compile / lint", "example", "contract", "property",
    /// "performance", "signature", "stub", "sandbox", "blocked" or "agent".
    pub gate: String,
    /// The compiler message, failing spec output or counterexample (first lines).
    pub detail: String,
    pub rounds: usize,
    /// Holes this one waits for (`Verdict::Blocked`).
    pub blocked_on: Vec<NodeId>,
    pub notes: Vec<String>,
    /// The implementation file, for "Open hole file" / "Pin manual impl".
    pub hole_file: Option<std::path::PathBuf>,
}

impl SynthFailure {
    /// Classify a failed job's last verdict.
    pub fn from_report(r: &wh_synth::JobReport, hole_file: Option<std::path::PathBuf>) -> SynthFailure {
        let (gate, detail, blocked_on) = match r.rounds.last().map(|x| &x.verdict) {
            Some(wh_synth::Verdict::Blocked(on)) => ("blocked".to_string(), format!("its specs call holes that are still stubs: {}", on.join(", ")), on.clone()),
            Some(wh_synth::Verdict::Rejected(why)) => (gate_of(why), why.lines().take(14).collect::<Vec<_>>().join("\n"), vec![]),
            _ => ("agent".to_string(), r.notes.first().cloned().unwrap_or_else(|| "the compiler agent did not produce an implementation".into()), vec![]),
        };
        SynthFailure { node: r.node.clone(), gate, detail, rounds: r.rounds.len(), blocked_on, notes: r.notes.clone(), hole_file }
    }
}

/// The gate named by a `wh_synth` rejection message.
pub fn gate_of(why: &str) -> String {
    let first = why.lines().next().unwrap_or("");
    let gate = if first.starts_with("the code does not compile") {
        "compile / lint"
    } else if first.contains("performance gate") {
        "performance"
    } else if first.starts_with("the signature changed") {
        "signature"
    } else if first.contains("todo!()") {
        "stub"
    } else if first.contains("files other than the hole file") {
        "sandbox"
    } else if first.starts_with("specs failed") {
        // the first failing test's name says which kind of spec it is
        let name = why.lines().find_map(|l| l.strip_prefix("--- ")).unwrap_or("");
        if name.contains("prop") {
            "property"
        } else if name.contains("contract") || name.contains("requires") || name.contains("ensures") || name.contains("invariant") {
            "contract"
        } else {
            "example"
        }
    } else if first.contains("failed to build") {
        "test build"
    } else {
        "rejected"
    };
    gate.to_string()
}

/// What a project job (Build, Test, …) is doing right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobPhase {
    Generating,
    Synthesizing,
    Compiling,
    Installing,
    Testing,
    Benchmarking,
    Optimizing,
}

impl JobPhase {
    pub fn label(self) -> &'static str {
        match self {
            JobPhase::Generating => "generating",
            JobPhase::Synthesizing => "synthesizing",
            JobPhase::Compiling => "compiling",
            JobPhase::Installing => "installing",
            JobPhase::Testing => "testing",
            JobPhase::Benchmarking => "benchmarking",
            JobPhase::Optimizing => "optimizing",
        }
    }
}

/// A project job the user started from the Project menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobKind {
    /// `wh build` (`--release` adds the performance and mutation gates).
    Build { release: bool },
    /// `wh synth`: every missing or stale hole (cache first).
    SynthesizeAll,
    /// `wh test`.
    Test,
    /// `wh optimize --node`.
    Optimize(NodeId),
}

impl JobKind {
    pub fn label(&self) -> String {
        match self {
            JobKind::Build { release: false } => "Build".into(),
            JobKind::Build { release: true } => "Build Release".into(),
            JobKind::SynthesizeAll => "Synthesize All".into(),
            JobKind::Test => "Test".into(),
            JobKind::Optimize(n) => format!("Optimize {n}"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatRole {
    User,
    Agent,
    System,
}

/// One event of a running app's trace (`WH_TRACE_FILE`), mapped to a node.
#[derive(Debug, Clone)]
pub struct TraceEvent {
    pub level: LogLevel,
    /// `wh.node` of the span that emitted the event.
    pub node: Option<NodeId>,
    pub message: String,
    pub duration: Option<Duration>,
}

#[derive(Debug, Clone)]
pub enum BackendEvent {
    Log { level: LogLevel, source: String, message: String, node: Option<NodeId> },
    Synthesis { node: NodeId, status: SynthStatus },
    Chat { role: ChatRole, text: String },
    /// The author agent rewrote a file it read as `base` (applied with `merge_write`).
    AuthorEdit { path: String, base: String, new_text: String, label: String },
    Trace(TraceEvent),
    RunFinished { ok: bool },
    /// The spec tests started running for the current model.
    TestsStarted,
    /// Per-node spec results; replaces the previous set.
    NodeResults(std::collections::BTreeMap<NodeId, NodeResult>),
    /// Live values of the clauses in `scope` (`wh_live`) for the latest checked model.
    LiveValues { scope: wh_live::LiveScope, report: std::sync::Arc<wh_live::LiveReport> },
    /// A live evaluation started (`true`), or ended without a report (`false`, with why): shown quietly as "…".
    LivePending { running: bool, reason: Option<String> },
    /// The compiler agent reported spending this much (USD) in a synthesis run.
    AgentCost(f64),
    /// A hole's synthesis failed (after `Synthesis { status: Failed }`).
    SynthFailed(SynthFailure),
    JobStarted { job: JobKind },
    /// `done` of `total` steps of `phase` (total 0: no count).
    JobProgress { phase: JobPhase, done: usize, total: usize, detail: String },
    JobFinished { job: JobKind, ok: bool, summary: String, artifacts: Vec<std::path::PathBuf>, cost_usd: Option<f64> },
}

pub trait Backend: 'static {
    /// The document changed (any origin); `model` holds the current buffers.
    fn document_changed(&mut self, model: &Model);
    /// Run the checker / `cargo check` and report diagnostics as log events.
    fn check(&mut self, model: &Model);
    /// (Re-)synthesize one hole.
    fn synthesize(&mut self, model: &Model, node: &NodeId);
    /// Build and run the app's entry point with tracing to `WH_TRACE_FILE`.
    fn run_app(&mut self, model: &Model);
    /// Send a message to the author agent, with the current selection as context.
    fn ask_author(&mut self, model: &Model, text: &str, selection: Option<&NodeId>);
    /// Generated Rust for a node, if the backend has it.
    fn generated_rust(&self, _model: &Model, _node: &NodeId) -> Option<String> {
        None
    }
    /// Turn live values (`wh_live`) on or off.
    fn set_live(&mut self, _on: bool) {}
    /// What live values to evaluate first: the selected node, then the active file.
    fn live_focus(&mut self, _file: Option<String>, _node: Option<NodeId>) {}
    /// Start a project job (one at a time); progress arrives as `Job*` events.
    fn run_job(&mut self, model: &Model, job: JobKind);
    /// Stop the running job: no further agent attempts, remaining phases skipped.
    fn cancel_job(&mut self);
    /// Drain pending events (called by the UI every ~100 ms).
    fn poll(&mut self) -> Vec<BackendEvent>;
}

/// No agents, no cargo: echoes intents and plays back scripted events.
#[derive(Default)]
pub struct MockBackend {
    scheduled: VecDeque<(Instant, BackendEvent)>,
    job: Option<JobKind>,
}

impl MockBackend {
    fn at(&mut self, delay_ms: u64, e: BackendEvent) {
        let t = Instant::now() + Duration::from_millis(delay_ms);
        let i = self.scheduled.iter().position(|(at, _)| *at > t).unwrap_or(self.scheduled.len());
        self.scheduled.insert(i, (t, e));
    }

    fn log(&mut self, delay_ms: u64, level: LogLevel, source: &str, message: impl Into<String>, node: Option<NodeId>) {
        self.at(delay_ms, BackendEvent::Log { level, source: source.into(), message: message.into(), node });
    }
}

impl Backend for MockBackend {
    fn document_changed(&mut self, _model: &Model) {}

    fn check(&mut self, model: &Model) {
        let errors = model.diagnostics.iter().filter(|d| d.diag.severity == wh_syntax::Severity::Error).count();
        self.log(0, LogLevel::Info, "check", format!("checking {} nodes (mock backend)…", model.nodes.len()), None);
        for d in &model.diagnostics {
            let f = &model.files[d.file];
            let node = model.node_at(d.file, d.diag.span.start).map(|n| n.id.clone());
            let level = match d.diag.severity {
                wh_syntax::Severity::Error => LogLevel::Error,
                wh_syntax::Severity::Warning => LogLevel::Warn,
                wh_syntax::Severity::Lint => LogLevel::Lint,
            };
            self.log(150, level, "check", d.diag.render(&f.path, &f.text), node);
        }
        self.log(200, LogLevel::Info, "check", format!("check finished: {errors} error(s)"), None);
        if errors > 0 {
            return;
        }
        // pretend every def or flow with examples ran them; those named in
        // WH_IDE_MOCK_FAIL (comma-separated) fail one
        let failing: Vec<String> = std::env::var("WH_IDE_MOCK_FAIL").unwrap_or_default().split(',').map(|s| s.trim().to_string()).collect();
        let mut results = std::collections::BTreeMap::new();
        for n in model.nodes.values() {
            let Some(d) = n.def() else { continue };
            use wh_syntax::ast::ClauseKind;
            let examples = d.clauses.iter().filter(|c| matches!(c.kind, ClauseKind::Example { .. } | ClauseKind::Property { .. })).count();
            if examples == 0 {
                continue;
            }
            let r = if failing.contains(&n.name) {
                NodeResult { passed: examples - 1, failed: 1, failures: vec![format!("{}_example_0: assertion failed (mock)", n.name)] }
            } else {
                NodeResult { passed: examples, failed: 0, failures: vec![] }
            };
            results.insert(n.id.clone(), r);
        }
        self.at(250, BackendEvent::TestsStarted);
        self.at(900, BackendEvent::NodeResults(results));
    }

    fn synthesize(&mut self, _model: &Model, node: &NodeId) {
        let n = node.clone();
        self.at(0, BackendEvent::Synthesis { node: n.clone(), status: SynthStatus::Queued });
        self.at(400, BackendEvent::Synthesis { node: n.clone(), status: SynthStatus::Synthesizing });
        self.at(1200, BackendEvent::Synthesis { node: n.clone(), status: SynthStatus::Verifying { round: 1, of: 4 } });
        self.at(2000, BackendEvent::Synthesis { node: n.clone(), status: SynthStatus::Failed("no compiler agent (mock backend)".into()) });
        self.log(2000, LogLevel::Warn, "synth", format!("{n}: mock backend cannot synthesize"), Some(n));
    }

    fn run_app(&mut self, model: &Model) {
        // replay a plausible trace of the app's entry flow and the flows it calls
        let mut t = 0;
        self.log(t, LogLevel::Info, "run", "running app (mock trace)", None);
        let mut stack: Vec<(NodeId, u32)> = model
            .nodes
            .values()
            .filter(|n| matches!(n.kind, NodeKind::App(_)))
            .map(|n| (n.id.clone(), 0))
            .collect();
        while let Some((id, depth)) = stack.pop() {
            let Some(n) = model.nodes.get(&id) else { continue };
            let steps = match &n.kind {
                NodeKind::Flow(f) => f.steps.clone(),
                NodeKind::App(a) => a.entry.as_ref().map(|e| e.steps.clone()).unwrap_or_default(),
                _ => vec![],
            };
            t += 60;
            self.at(t, BackendEvent::Trace(TraceEvent { level: LogLevel::Info, node: Some(id.clone()), message: format!("enter {}", n.name), duration: None }));
            for s in steps.iter() {
                t += 60;
                let us = 40 + (s.id.len() as u64 * 37) % 400;
                self.at(t, BackendEvent::Trace(TraceEvent {
                    level: LogLevel::Debug,
                    node: Some(s.id.clone()),
                    message: format!("step {}", s.label),
                    duration: Some(Duration::from_micros(us)),
                }));
                if let Some(c) = &s.callee {
                    if matches!(model.nodes.get(c).map(|n| &n.kind), Some(NodeKind::Flow(_))) && depth < 3 {
                        stack.push((c.clone(), depth + 1));
                    }
                }
            }
        }
        self.at(t + 100, BackendEvent::RunFinished { ok: true });
    }

    fn ask_author(&mut self, model: &Model, text: &str, selection: Option<&NodeId>) {
        let ctx = selection.map(|s| format!(" (context: {s})")).unwrap_or_default();
        self.at(0, BackendEvent::Chat { role: ChatRole::User, text: text.to_string() });
        // "mock edit": propose a one-line prose edit above the selected definition
        if text.trim().starts_with("mock edit") {
            let node = selection.and_then(|s| model.nodes.get(s)).or_else(|| model.nodes.values().next());
            if let Some(n) = node {
                let f = &model.files[n.file];
                let block = f.doc.blocks[n.block].span.start as usize;
                let mut new_text = f.text.clone();
                new_text.insert_str(block, &format!("Reviewed `{}` (mock author edit).\n\n", n.name));
                self.at(500, BackendEvent::AuthorEdit { path: f.path.clone(), base: f.text.clone(), new_text, label: format!("note on {}", n.name) });
            }
        }
        self.at(600, BackendEvent::Chat {
            role: ChatRole::Agent,
            text: format!("No author agent is connected yet — this is the mock backend{ctx}."),
        });
    }

    fn run_job(&mut self, model: &Model, job: JobKind) {
        // a scripted walk through the phases a real job reports
        self.at(0, BackendEvent::JobStarted { job: job.clone() });
        self.job = Some(job.clone());
        let holes = model.nodes.values().filter(|n| n.is_hole()).count();
        let mut t = 0;
        let phases: &[JobPhase] = match &job {
            JobKind::Build { release: false } => &[JobPhase::Generating, JobPhase::Synthesizing, JobPhase::Compiling, JobPhase::Installing],
            JobKind::Build { release: true } => &[JobPhase::Generating, JobPhase::Synthesizing, JobPhase::Compiling, JobPhase::Installing, JobPhase::Benchmarking],
            JobKind::SynthesizeAll => &[JobPhase::Generating, JobPhase::Synthesizing],
            JobKind::Test => &[JobPhase::Generating, JobPhase::Testing],
            JobKind::Optimize(_) => &[JobPhase::Optimizing],
        };
        for &phase in phases {
            let total = if phase == JobPhase::Synthesizing { holes } else { 0 };
            for k in 0..=total.min(5) {
                t += 300;
                self.at(t, BackendEvent::JobProgress { phase, done: k, total, detail: "(mock backend)".into() });
            }
        }
        if job == JobKind::Test {
            self.check(model);
        }
        self.at(t + 300, BackendEvent::JobFinished { job, ok: true, summary: "done (mock backend: nothing was built)".into(), artifacts: vec![], cost_usd: None });
    }

    fn cancel_job(&mut self) {
        self.scheduled.retain(|(_, e)| !matches!(e, BackendEvent::JobProgress { .. } | BackendEvent::JobFinished { .. }));
        if let Some(job) = self.job.take() {
            self.at(0, BackendEvent::JobFinished { job, ok: false, summary: "cancelled".into(), artifacts: vec![], cost_usd: None });
        }
    }

    fn poll(&mut self) -> Vec<BackendEvent> {
        let now = Instant::now();
        let mut out = Vec::new();
        while self.scheduled.front().is_some_and(|(t, _)| *t <= now) {
            let e = self.scheduled.pop_front().unwrap().1;
            if let BackendEvent::JobFinished { .. } = e {
                self.job = None;
            }
            out.push(e);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gates_from_rejections() {
        assert_eq!(gate_of("the code does not compile or violates a lint gate:\nerror[E0308]"), "compile / lint");
        assert_eq!(gate_of("specs failed:\n--- wh_tests::signup::check_age_example_1\npanicked"), "example");
        assert_eq!(gate_of("specs failed:\n--- wh_tests::m::f_prop_idempotent\nminimal failing input: x = 3"), "property");
        assert_eq!(gate_of("specs failed:\n--- wh_tests::m::f_contract_ensures_0\n"), "contract");
        assert_eq!(gate_of("the code is correct but fails the performance gate:\nO(n^2)"), "performance");
        assert_eq!(gate_of("the signature changed; it must stay exactly:\nfn f()"), "signature");
        let r = wh_synth::JobReport {
            node: "m.f".into(),
            key: "k".into(),
            accepted: false,
            from_cache: false,
            notes: vec![],
            rounds: vec![wh_synth::RoundReport { round: 1, verdict: wh_synth::Verdict::Blocked(vec!["m.g".into()]), transcript: String::new() }],
        };
        let f = SynthFailure::from_report(&r, None);
        assert_eq!((f.gate.as_str(), f.rounds, f.blocked_on.as_slice()), ("blocked", 1, ["m.g".to_string()].as_slice()));
    }
}
