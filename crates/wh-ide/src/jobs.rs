//! Project jobs started from the Project menu: Build (Release), Synthesize
//! All, Test and Optimize. They do what `wh build|synth|test|optimize` do,
//! on the live model (unsaved buffers included), and report phases,
//! node-mapped problems and results as [`BackendEvent`]s.
//!
//! Cancel takes effect at once for the agent (no further attempts) and
//! between phases for cargo, which is never killed half-way.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::Sender;

use anyhow::{Result, bail};
use wh_model::Model;

use crate::backend::{BackendEvent, JobKind, JobPhase, LogLevel, NodeResult, SynthStatus};

/// Stops calling the agent once the job is cancelled.
struct Cancellable<D: wh_synth::Driver> {
    inner: D,
    cancel: Arc<AtomicBool>,
}

impl<D: wh_synth::Driver> wh_synth::Driver for Cancellable<D> {
    fn id(&self) -> String {
        self.inner.id()
    }

    fn attempt(&self, req: &wh_synth::SynthesisRequest) -> Result<wh_synth::AttemptOutcome> {
        if self.cancel.load(Ordering::SeqCst) {
            bail!("cancelled");
        }
        self.inner.attempt(req)
    }

    fn spent_usd(&self) -> Option<f64> {
        self.inner.spent_usd()
    }
}

type Agent = wh_synth::Budgeted<Cancellable<wh_synth::acp::AcpDriver>>;

pub struct Job<'a> {
    pub project: &'a wh_project::Project,
    pub model: &'a Model,
    pub tx: &'a Sender<BackendEvent>,
    pub cancel: Arc<AtomicBool>,
    pub compiler: wh_agent::AgentConfig,
}

/// How a job ended.
struct Outcome {
    ok: bool,
    summary: String,
    artifacts: Vec<PathBuf>,
}

impl Job<'_> {
    fn log(&self, level: LogLevel, message: impl Into<String>, node: Option<String>) {
        let _ = self.tx.send(BackendEvent::Log { level, source: "job".into(), message: message.into(), node });
    }

    fn phase(&self, phase: JobPhase, done: usize, total: usize, detail: impl Into<String>) {
        let _ = self.tx.send(BackendEvent::JobProgress { phase, done, total, detail: detail.into() });
    }

    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }

    fn agent(&self) -> Agent {
        let c = &self.project.config.synthesis;
        let budget = wh_synth::Budget { max_usd: c.budget_usd, max_attempts: c.max_attempts };
        let inner = Cancellable { inner: wh_synth::acp::AcpDriver::new(self.compiler.clone()), cancel: self.cancel.clone() };
        wh_synth::Budgeted::new(inner, budget)
    }

    /// Run `kind` to the end and report `JobFinished`.
    pub fn run(&self, kind: JobKind) {
        let _ = self.tx.send(BackendEvent::JobStarted { job: kind.clone() });
        let agent = self.agent();
        let out = match self.steps(&kind, &agent) {
            Ok(o) => o,
            Err(e) => Outcome { ok: false, summary: format!("{e:#}"), artifacts: vec![] },
        };
        let out = if self.cancelled() { Outcome { ok: false, summary: "cancelled".into(), ..out } } else { out };
        let _ = self.tx.send(BackendEvent::JobFinished {
            job: kind,
            ok: out.ok,
            summary: out.summary,
            artifacts: out.artifacts,
            cost_usd: wh_synth::Driver::spent_usd(&agent),
        });
    }

    fn steps(&self, kind: &JobKind, agent: &Agent) -> Result<Outcome> {
        let errors = self.model.diagnostics.iter().filter(|d| d.diag.severity == wh_syntax::Severity::Error).count();
        if errors > 0 {
            bail!("the document has {errors} error(s); fix them first");
        }
        self.phase(JobPhase::Generating, 0, 0, "");
        let g = self.project.generate(self.model)?;
        match kind {
            JobKind::SynthesizeAll => {
                let (ok, summary) = self.synthesize(&g, agent)?;
                Ok(Outcome { ok, summary, artifacts: vec![] })
            }
            JobKind::Build { release } => self.build(&g, agent, *release),
            JobKind::Test => self.test(),
            JobKind::Optimize(node) => self.optimize(&g, node, agent),
        }
    }

    /// Missing or stale holes, cache first; `(all accepted, summary)`.
    fn synthesize(&self, g: &wh_codegen::Generated, agent: &Agent) -> Result<(bool, String)> {
        let p = self.project;
        let cache = wh_synth::cache::SynthCache::new(wh_project::synth_cache_dir());
        let (package, target) = (p.gen_options().package, p.target_dir());
        let done = AtomicUsize::new(0);
        let total = AtomicUsize::new(0);
        let tx = self.tx.clone();
        let max_rounds = p.config.synthesis.max_rounds;
        let on_progress = |ev: wh_synth::Progress| {
            let (node, status, finished) = match ev {
                wh_synth::Progress::Queued { node } => (node, SynthStatus::Queued, false),
                wh_synth::Progress::Attempt { node, .. } => (node, SynthStatus::Synthesizing, false),
                wh_synth::Progress::Verifying { node, round } => (node, SynthStatus::Verifying { round: round as u32, of: max_rounds as u32 }, false),
                wh_synth::Progress::Accepted { node, .. } => (node, SynthStatus::Accepted, true),
                wh_synth::Progress::Restored { node } => (node, SynthStatus::Restored, true),
                wh_synth::Progress::Failed { node, reason } => (node, SynthStatus::Failed(reason.lines().next().unwrap_or("").to_string()), true),
            };
            if finished {
                done.fetch_add(1, Ordering::SeqCst);
            }
            let detail = format!("{node}: {}", status.label());
            let _ = tx.send(BackendEvent::Synthesis { node, status });
            let _ = tx.send(BackendEvent::JobProgress {
                phase: JobPhase::Synthesizing,
                done: done.load(Ordering::SeqCst),
                total: total.load(Ordering::SeqCst),
                detail,
            });
        };
        let synth = wh_synth::Synthesizer {
            model: self.model,
            root: &p.root,
            package: &package,
            target_dir: &target,
            cache: Some(&cache),
            max_rounds,
            concurrency: p.config.synthesis.concurrency,
            driver: agent,
            on_progress: &on_progress,
        };
        let jobs = synth.plan(&g.manifest.holes, None, false);
        if jobs.is_empty() {
            return Ok((true, "all holes are up to date".into()));
        }
        total.store(jobs.len(), Ordering::SeqCst);
        self.phase(JobPhase::Synthesizing, 0, jobs.len(), format!("cache first, then {}", wh_synth::Driver::id(agent)));
        let reports = synth.run(jobs);
        for r in reports.iter().filter(|r| !r.accepted) {
            let file = g.manifest.holes.iter().find(|h| h.node == r.node).map(|h| h.path.clone());
            let f = crate::backend::SynthFailure::from_report(r, file);
            self.log(LogLevel::Error, format!("{} gate: {}", f.gate, f.detail.lines().next().unwrap_or("")), Some(r.node.clone()));
            let _ = self.tx.send(BackendEvent::SynthFailed(f));
        }
        for note in reports.iter().flat_map(|r| &r.notes) {
            self.log(LogLevel::Info, note.clone(), None);
        }
        let failed = reports.iter().filter(|r| !r.accepted).count();
        let cached = reports.iter().filter(|r| r.from_cache).count();
        let summary = format!("{} accepted ({cached} from cache), {failed} failed, {} agent attempt(s)", reports.len() - failed, agent.attempts());
        Ok((failed == 0, summary))
    }

    fn build(&self, g: &wh_codegen::Generated, agent: &Agent, release: bool) -> Result<Outcome> {
        let p = self.project;
        let (synth_ok, synth_summary) = self.synthesize(g, agent)?;
        if self.cancelled() {
            bail!("cancelled");
        }
        let missing = wh_build::unimplemented_holes(&g.manifest);
        if !missing.is_empty() {
            for h in &missing {
                self.log(LogLevel::Error, format!("hole not implemented: {}", h.node), Some(h.node.clone()));
            }
            bail!("{} hole(s) are not implemented ({synth_summary})", missing.len());
        }
        if !synth_ok {
            self.log(LogLevel::Warn, format!("synthesis: {synth_summary}"), None);
        }
        self.phase(JobPhase::Compiling, 0, 0, if release { "cargo build --release" } else { "cargo build" });
        let (gen_dir, target) = (p.gen_dir(), p.target_dir());
        let cargo = wh_build::Cargo { crate_dir: &gen_dir, target_dir: &target, manifest: &g.manifest };
        let r = cargo.build(release)?;
        for d in r.errors() {
            self.log(LogLevel::Error, d.rendered.trim_end().to_string(), d.node.clone());
        }
        if !r.success {
            bail!("cargo build failed with {} error(s)", r.errors().count());
        }
        self.phase(JobPhase::Installing, 0, 0, p.bin_dir().display().to_string());
        let bins = p.install_bins(&r.artifacts)?;
        for b in &bins {
            self.log(LogLevel::Info, format!("built {}", b.display()), None);
        }
        if release {
            if self.cancelled() {
                bail!("cancelled");
            }
            if !self.perf_gates()? {
                return Ok(Outcome { ok: false, summary: "performance gates failed (see the log)".into(), artifacts: bins });
            }
        }
        let names: Vec<String> = bins.iter().filter_map(|b| b.file_name().map(|f| f.to_string_lossy().into_owned())).collect();
        Ok(Outcome { ok: true, summary: format!("built {} into {}", names.join(", "), p.bin_dir().display()), artifacts: bins })
    }

    /// `wh build --release`: benchmarks against complexity / budget / alloc / baseline gates, then mutation gates.
    fn perf_gates(&self) -> Result<bool> {
        let (p, model) = (self.project, self.model);
        let nodes = wh_perf::perf_nodes(p, model);
        let mut ok = true;
        if !nodes.is_empty() {
            self.phase(JobPhase::Benchmarking, 0, nodes.len(), format!("{} hole(s)", nodes.len()));
            let opts = wh_perf::PerfOptions { nodes: Some(nodes), baselines: wh_perf::BaselinePolicy::IfMissing, ..Default::default() };
            let report = wh_perf::bench_project(p, model, &opts)?;
            for s in &report.skipped {
                self.log(LogLevel::Info, format!("benchmark skipped: {}", s.reason), Some(s.node.clone()));
            }
            for n in &report.nodes {
                let class = n.complexity.as_ref().map(|c| format!(", fits {:?} (exponent {:.2})", c.class, c.exponent)).unwrap_or_default();
                let level = if n.failures().is_empty() { LogLevel::Info } else { LogLevel::Error };
                self.log(level, format!("benchmark {}{class}", n.node), Some(n.node.clone()));
            }
            for (node, msg) in report.failures() {
                self.log(LogLevel::Error, msg, Some(node));
            }
            ok &= report.passed();
        }
        let mutated: Vec<String> = model
            .nodes
            .values()
            .filter(|n| n.is_hole() && wh_perf::PerfSpec::of(model, &n.id).is_some_and(|s| s.mutate.is_some()))
            .map(|n| n.id.clone())
            .collect();
        for (k, id) in mutated.iter().enumerate() {
            if self.cancelled() {
                bail!("cancelled");
            }
            self.phase(JobPhase::Benchmarking, k, mutated.len(), format!("mutation testing {id}"));
            let r = wh_perf::mutation_score_with(p, model, id, &Default::default())?;
            match (&r.status, &r.gate) {
                (wh_perf::MutationStatus::NotInstalled { hint }, _) => {
                    self.log(LogLevel::Warn, format!("declares `mutate`, but cargo-mutants is not installed ({hint})"), Some(id.clone()))
                }
                (_, Some(g)) if g.status == wh_perf::gates::GateStatus::Fail => {
                    self.log(LogLevel::Error, format!("mutation gate failed: {}", g.detail), Some(id.clone()));
                    ok = false;
                }
                (status, _) => self.log(LogLevel::Info, format!("mutation {status:?}"), Some(id.clone())),
            }
        }
        Ok(ok)
    }

    fn test(&self) -> Result<Outcome> {
        self.phase(JobPhase::Testing, 0, 0, "cargo test");
        let r = wh_project::live::test(self.project, self.model, None)?;
        for d in r.errors() {
            self.log(LogLevel::Error, d.rendered.trim_end().to_string(), d.node.clone());
        }
        let results = node_results(&r.tests, &[]);
        let failed = r.tests.iter().filter(|t| !t.passed).count();
        let summary = format!("{} test(s), {failed} failed", r.tests.len());
        let _ = self.tx.send(BackendEvent::NodeResults(results));
        Ok(Outcome { ok: r.success && failed == 0, summary, artifacts: vec![] })
    }

    fn optimize(&self, g: &wh_codegen::Generated, node: &str, agent: &Agent) -> Result<Outcome> {
        let tx = self.tx.clone();
        let target = node.to_string();
        let opts = wh_perf::OptimizeOptions {
            on_progress: Some(Arc::new(move |ev| {
                let detail = match ev {
                    wh_perf::OptimizeProgress::Measuring { label, .. } => format!("measuring {label}"),
                    wh_perf::OptimizeProgress::Attempt { round, variant, .. } => format!("round {round}, variant {variant}: asking the agent"),
                    wh_perf::OptimizeProgress::Verifying { label, .. } => format!("verifying {label}"),
                    wh_perf::OptimizeProgress::Benchmarking { label, .. } => format!("benchmarking {label}"),
                    wh_perf::OptimizeProgress::Verdict { label, accepted, reason, .. } => {
                        let level = if accepted { LogLevel::Info } else { LogLevel::Warn };
                        let msg = format!("{label}: {}", reason.lines().next().unwrap_or(""));
                        let _ = tx.send(BackendEvent::Log { level, source: "optimize".into(), message: msg.clone(), node: Some(target.clone()) });
                        msg
                    }
                    wh_perf::OptimizeProgress::Done { .. } => "done".into(),
                };
                let _ = tx.send(BackendEvent::JobProgress { phase: JobPhase::Optimizing, done: 0, total: 0, detail });
            })),
            ..Default::default()
        };
        self.phase(JobPhase::Optimizing, 0, 0, format!("variant tournament for {node}"));
        let r = wh_perf::optimize(self.project, self.model, node, agent, &opts)?;
        for note in &r.notes {
            self.log(LogLevel::Info, note.clone(), Some(node.to_string()));
        }
        let summary = match (&r.accepted, r.speedup) {
            (Some(label), Some(s)) => format!("accepted {label}, {s:.2}× faster"),
            (Some(label), None) => format!("accepted {label}"),
            (None, _) => "no variant beat the current implementation".into(),
        };
        // the optimized code replaces the cached implementation for this spec
        if r.accepted.is_some() {
            if let Some(h) = g.manifest.holes.iter().find(|h| h.node == node) {
                let text = std::fs::read_to_string(&h.path).unwrap_or_default();
                if let Some(key) = wh_synth::read_key(&text) {
                    let cache = wh_synth::cache::SynthCache::new(wh_project::synth_cache_dir());
                    let meta = wh_synth::cache::EntryMeta {
                        node: node.to_string(),
                        spec_key: key.to_string(),
                        signature: h.signature.clone(),
                        agent: wh_synth::Driver::id(agent),
                        rounds: 0,
                        created: wh_synth::cache::now(),
                    };
                    let _ = cache.put(&wh_synth::cache::SynthCache::key(key, h), &text, &meta);
                }
            }
        }
        Ok(Outcome { ok: true, summary, artifacts: vec![] })
    }
}

/// Spec outcomes grouped by node; holes in `stubs` (still `todo!()`) get no result.
pub fn node_results(tests: &[wh_build::TestOutcome], stubs: &[String]) -> BTreeMap<String, NodeResult> {
    let mut out: BTreeMap<String, NodeResult> = BTreeMap::new();
    for t in tests {
        let Some(node) = t.node.clone().filter(|n| !stubs.contains(n)) else { continue };
        let r = out.entry(node).or_default();
        if t.passed {
            r.passed += 1;
        } else {
            r.failed += 1;
            let why = t.output.lines().find(|l| l.contains("panicked") || l.contains("assert")).unwrap_or("").trim();
            r.failures.push(if why.is_empty() { t.name.clone() } else { format!("{}: {why}", t.name) });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::time::Duration;

    fn copy(from: &Path, to: &Path) {
        std::fs::create_dir_all(to).unwrap();
        for e in std::fs::read_dir(from).unwrap().flatten() {
            let p = e.path();
            if e.file_name() == ".wh" {
                continue;
            }
            if p.is_dir() {
                copy(&p, &to.join(e.file_name()));
            } else {
                std::fs::copy(&p, to.join(e.file_name())).unwrap();
            }
        }
    }

    fn run(dir: &Path, kind: JobKind) -> Vec<BackendEvent> {
        let project = wh_project::Project::load(dir).unwrap();
        let model = project.model().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        // the project's own agent: synthesis keys include its id. Every hole of todo is
        // implemented for that key, so it is never started.
        let compiler = crate::live_backend::agent_config(&project, "compiler");
        Job { project: &project, model: &model, tx: &tx, cancel: Arc::new(AtomicBool::new(false)), compiler }.run(kind);
        rx.recv_timeout(Duration::from_secs(1)).ok().into_iter().chain(rx.try_iter()).collect()
    }

    fn finished(events: &[BackendEvent]) -> (bool, String) {
        events
            .iter()
            .find_map(|e| match e {
                BackendEvent::JobFinished { ok, summary, .. } => Some((*ok, summary.clone())),
                _ => None,
            })
            .expect("a JobFinished event")
    }

    #[test]
    fn build_test_and_synthesize_todo() {
        let _cargo = crate::live_backend::CARGO_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        let root = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."));
        let dir = root.join("target/wh-ide-tests/todo-jobs");
        let _ = std::fs::remove_dir_all(&dir);
        copy(&root.join("examples/todo"), &dir);

        let ev = run(&dir, JobKind::SynthesizeAll);
        assert_eq!(finished(&ev), (true, "all holes are up to date".to_string()), "{ev:#?}");

        let ev = run(&dir, JobKind::Build { release: false });
        let (ok, summary) = finished(&ev);
        assert!(ok, "{summary}\n{ev:#?}");
        assert!(dir.join(".wh/bin/todo").is_file(), "{summary}");
        let phases: Vec<JobPhase> = ev.iter().filter_map(|e| if let BackendEvent::JobProgress { phase, .. } = e { Some(*phase) } else { None }).collect();
        assert!(phases.contains(&JobPhase::Compiling) && phases.contains(&JobPhase::Installing), "{phases:?}");

        let ev = run(&dir, JobKind::Test);
        let (ok, summary) = finished(&ev);
        assert!(ok, "{summary}");
        let results = ev.iter().find_map(|e| if let BackendEvent::NodeResults(r) = e { Some(r.clone()) } else { None }).unwrap();
        assert!(!results.is_empty() && results.values().all(|r| r.ok()), "{results:#?}");

        // a cancelled job reports so and stops before compiling
        let project = wh_project::Project::load(&dir).unwrap();
        let model = project.model().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(true));
        Job { project: &project, model: &model, tx: &tx, cancel, compiler: wh_agent::AgentConfig::new("/nonexistent/agent") }.run(JobKind::Build { release: true });
        let ev: Vec<BackendEvent> = rx.try_iter().collect();
        assert_eq!(finished(&ev), (false, "cancelled".to_string()));
        assert!(!ev.iter().any(|e| matches!(e, BackendEvent::JobProgress { phase: JobPhase::Compiling, .. })), "{ev:#?}");
    }

    /// Spends real agent time: `cargo test -p wh-ide -- --ignored optimize_with_the_real_agent`.
    #[test]
    #[ignore]
    fn optimize_with_the_real_agent() {
        let _cargo = crate::live_backend::CARGO_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        let root = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."));
        let dir = root.join("target/wh-ide-tests/signup-optimize");
        let _ = std::fs::remove_dir_all(&dir);
        copy(&root.join("examples/signup"), &dir);
        let ev = run(&dir, JobKind::Optimize("signup.dedupe_emails".into()));
        for e in &ev {
            match e {
                BackendEvent::JobProgress { phase, detail, .. } => eprintln!("[{}] {detail}", phase.label()),
                BackendEvent::Log { level, message, .. } => eprintln!("{} {message}", level.label()),
                BackendEvent::JobFinished { ok, summary, cost_usd, .. } => eprintln!("finished ok={ok}: {summary} (cost {cost_usd:?})"),
                _ => {}
            }
        }
        let (ok, summary) = finished(&ev);
        assert!(ok, "{summary}");
    }
}
