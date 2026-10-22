//! The real [`Backend`]: the live pipeline of `wh-project`, the compiler
//! agent of `wh-synth` and the author agent of `wh-author`, each run on a
//! background thread and reported to the UI through a channel.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use wh_model::{Model, NodeId};

use crate::backend::{Backend, BackendEvent, ChatRole, JobKind, LogLevel, SynthStatus, TraceEvent};

pub struct LiveBackend {
    project: Arc<wh_project::Project>,
    tx: Sender<BackendEvent>,
    rx: Receiver<BackendEvent>,
    /// Latest model and when it changed (for debounced checking/synthesis).
    pending: Option<(Instant, Model)>,
    debounce: Duration,
    auto: bool,
    /// Set when the compiler agent cannot run (e.g. not signed in): no more automatic attempts.
    auto_disabled: Arc<AtomicBool>,
    busy: Arc<AtomicBool>,
    author: Arc<wh_author::Author>,
    author_busy: Arc<AtomicBool>,
    run_busy: Arc<AtomicBool>,
    /// A Project-menu job is running, and whether it was cancelled.
    job_busy: Arc<AtomicBool>,
    job_cancel: Arc<AtomicBool>,
    /// Live values: on/off, what to evaluate first (active file, selected node), the
    /// document generation (bumped on every change) and the last one that checked clean.
    live: Arc<LiveShared>,
    /// The focus changed: a live-only run is due (debounced).
    live_wanted: Option<Instant>,
    /// The latest model and its generation, for live-only runs.
    current: Option<(u64, Model)>,
    /// The IDE's MCP bridge socket: author edits arrive as structured ops through it.
    bridge: Option<std::path::PathBuf>,
    rust_cache: Mutex<Option<(u64, BTreeMap<String, Option<String>>)>>,
}

/// Live-value state shared with the pipeline threads.
#[derive(Default)]
struct LiveShared {
    on: AtomicBool,
    focus: Mutex<(Option<String>, Option<NodeId>)>,
    generation: AtomicU64,
    /// Generation of the last model that checked without errors (+1; 0 = none).
    clean: AtomicU64,
    /// Generation of the last model checked at all (+1; 0 = none).
    checked: AtomicU64,
}

/// Evaluate live values for the focus, the selected node first (it is the fastest
/// feedback), then the whole active file. Dropped as soon as the document moves on.
fn run_live(project: &wh_project::Project, model: &Model, live: &LiveShared, generation: u64, tx: &Sender<BackendEvent>) {
    if !live.on.load(Ordering::SeqCst) {
        return;
    }
    let (file, node) = live.focus.lock().unwrap().clone();
    let mut scopes = Vec::new();
    if let Some(n) = node.filter(|n| model.nodes.contains_key(n)) {
        scopes.push(wh_live::LiveScope::Node(n));
    }
    if let Some(f) = file.filter(|f| model.files.iter().any(|x| &x.path == f)) {
        scopes.push(wh_live::LiveScope::File(f));
    }
    if scopes.is_empty() {
        return;
    }
    let _ = tx.send(BackendEvent::LivePending { running: true, reason: None });
    for scope in scopes {
        if live.generation.load(Ordering::SeqCst) != generation {
            let _ = tx.send(BackendEvent::LivePending { running: false, reason: Some("the document changed; re-evaluating".into()) });
            return;
        }
        // typing-speed loop: a hung probe costs at most half a second
        let opts = wh_live::LiveOptions { scope: scope.clone(), timeout_ms: 500, run_timeout_secs: 30, ..Default::default() };
        match wh_live::evaluate_with(project, model, &opts) {
            Ok(report) if live.generation.load(Ordering::SeqCst) == generation => {
                let _ = tx.send(BackendEvent::LiveValues { scope, report: Arc::new(report) });
            }
            Ok(_) => {}
            Err(e) => {
                let _ = tx.send(BackendEvent::LivePending { running: false, reason: Some(format!("{e:#}")) });
                return;
            }
        }
    }
    let _ = tx.send(BackendEvent::LivePending { running: false, reason: None });
}

pub(crate) fn agent_config(p: &wh_project::Project, role: &str) -> wh_agent::AgentConfig {
    match p.config.agent.get(role) {
        Some(a) => {
            let mut c = wh_agent::AgentConfig::new(&a.command);
            for x in &a.args {
                c = c.arg(x);
            }
            for (k, v) in &a.env {
                c = c.env(k, v);
            }
            c
        }
        None => wh_agent::AgentConfig::claude_agent_acp(),
    }
}

fn level(s: &str) -> LogLevel {
    match s {
        "TRACE" => LogLevel::Trace,
        "DEBUG" => LogLevel::Debug,
        "WARN" => LogLevel::Warn,
        "ERROR" => LogLevel::Error,
        _ => LogLevel::Info,
    }
}

fn log(tx: &Sender<BackendEvent>, level: LogLevel, source: &str, message: impl Into<String>, node: Option<NodeId>) {
    let _ = tx.send(BackendEvent::Log { level, source: source.into(), message: message.into(), node });
}

impl LiveBackend {
    pub fn new(project: wh_project::Project) -> Self {
        let (tx, rx) = channel();
        let author = Arc::new(wh_author::Author::new(project.root.clone(), agent_config(&project, "author")));
        Self {
            debounce: Duration::from_millis(project.config.synthesis.debounce_ms),
            auto: project.config.synthesis.mode == "auto",
            project: Arc::new(project),
            tx,
            rx,
            pending: None,
            auto_disabled: Arc::new(AtomicBool::new(false)),
            busy: Arc::new(AtomicBool::new(false)),
            author,
            author_busy: Arc::new(AtomicBool::new(false)),
            run_busy: Arc::new(AtomicBool::new(false)),
            job_busy: Arc::new(AtomicBool::new(false)),
            job_cancel: Arc::new(AtomicBool::new(false)),
            bridge: None,
            live: Arc::new(LiveShared::default()),
            live_wanted: None,
            current: None,
            rust_cache: Mutex::new(None),
        }
    }

    pub fn with_bridge(mut self, socket: Option<std::path::PathBuf>) -> Self {
        self.bridge = socket;
        self
    }

    /// Check, then (in auto mode) synthesize missing/stale holes; on a background thread.
    fn pipeline(&mut self, model: Model, synth: Option<(Option<NodeId>, bool)>) {
        if self.busy.swap(true, Ordering::SeqCst) {
            return; // one pipeline run at a time; the next change triggers another
        }
        let (project, tx, busy, disabled) = (self.project.clone(), self.tx.clone(), self.busy.clone(), self.auto_disabled.clone());
        let live = self.live.clone();
        let generation = live.generation.load(Ordering::SeqCst);
        // this run evaluates live values after its check (a focus change from now on asks again)
        self.live_wanted = None;
        std::thread::spawn(move || {
            run_pipeline(&project, &model, &tx, synth, &disabled, &live, generation);
            busy.store(false, Ordering::SeqCst);
        });
    }
}

fn run_pipeline(
    project: &wh_project::Project,
    model: &Model,
    tx: &Sender<BackendEvent>,
    synth: Option<(Option<NodeId>, bool)>,
    disabled: &AtomicBool,
    live: &LiveShared,
    generation: u64,
) {
    let (generated, build) = match wh_project::live::check(project, model) {
        Ok(r) => r,
        Err(e) => return log(tx, LogLevel::Error, "build", format!("{e:#}"), None),
    };
    live.checked.store(generation + 1, Ordering::SeqCst);
    let errors: Vec<_> = build.errors().collect();
    for d in &errors {
        log(tx, LogLevel::Error, "cargo", d.rendered.trim_end().to_string(), d.node.clone());
    }
    log(
        tx,
        if errors.is_empty() { LogLevel::Info } else { LogLevel::Error },
        "cargo",
        format!("cargo check: {} error(s)", errors.len()),
        None,
    );
    let driver = wh_synth::acp::AcpDriver::new(agent_config(project, "compiler"));
    let states = wh_synth::states(model, &generated.manifest.holes, &wh_synth::Driver::id(&driver));
    for (node, st) in &states {
        let status = match st {
            wh_synth::HoleState::Missing | wh_synth::HoleState::Stale { .. } => SynthStatus::Stale,
            wh_synth::HoleState::SoftStale => SynthStatus::SoftStale,
            _ => SynthStatus::Accepted,
        };
        let _ = tx.send(BackendEvent::Synthesis { node: node.clone(), status });
    }
    if errors.is_empty() {
        live.clean.store(generation + 1, Ordering::SeqCst);
        run_live(project, model, live, generation, tx);
    }
    if errors.is_empty() && synth.as_ref().is_none_or(|(only, _)| only.is_none()) {
        run_tests(project, model, &generated, tx);
    }
    let Some((only, force)) = synth else { return };
    if !errors.is_empty() && only.is_none() {
        return; // don't burn agent time on a program that doesn't compile
    }
    let max_rounds = project.config.synthesis.max_rounds;
    let ptx = tx.clone();
    let on_progress = move |p: wh_synth::Progress| {
        let (node, status) = match p {
            wh_synth::Progress::Queued { node } => (node, SynthStatus::Queued),
            wh_synth::Progress::Attempt { node, .. } => (node, SynthStatus::Synthesizing),
            wh_synth::Progress::Verifying { node, round } => (node, SynthStatus::Verifying { round: round as u32, of: max_rounds as u32 }),
            wh_synth::Progress::Accepted { node, .. } => (node, SynthStatus::Accepted),
            wh_synth::Progress::Restored { node } => (node, SynthStatus::Restored),
            wh_synth::Progress::Failed { node, reason } => (node, SynthStatus::Failed(reason.lines().next().unwrap_or("").to_string())),
        };
        let _ = ptx.send(BackendEvent::Synthesis { node, status });
    };
    let cache = wh_synth::cache::SynthCache::new(wh_project::synth_cache_dir());
    let (package, target) = (project.gen_options().package, project.target_dir());
    let synthesizer = wh_synth::Synthesizer {
        model,
        root: &project.root,
        package: &package,
        target_dir: &target,
        cache: Some(&cache),
        max_rounds,
        concurrency: project.config.synthesis.concurrency,
        driver: &driver,
        on_progress: &on_progress,
    };
    let jobs = synthesizer.plan(&generated.manifest.holes, only.as_deref(), force);
    if jobs.is_empty() {
        return;
    }
    log(tx, LogLevel::Info, "synth", format!("synthesizing {} hole(s)", jobs.len()), None);
    let reports = synthesizer.run(jobs);
    if reports.iter().any(|r| r.accepted) {
        // new implementations: their specs have run in synthesis, the flows' have not
        run_tests(project, model, &generated, tx);
    }
    for r in &reports {
        if !r.accepted {
            let file = generated.manifest.holes.iter().find(|h| h.node == r.node).map(|h| h.path.clone());
            let f = crate::backend::SynthFailure::from_report(r, file);
            log(tx, LogLevel::Warn, "synth", format!("{} gate: {}", f.gate, f.detail.lines().next().unwrap_or("")), Some(r.node.clone()));
            let _ = tx.send(BackendEvent::SynthFailed(f));
        }
        if !r.accepted && r.rounds.is_empty() {
            // the job failed before any round ran: usually the agent itself
            disabled.store(true, Ordering::SeqCst);
        }
    }
    if let Some(usd) = wh_synth::Driver::spent_usd(&driver) {
        let _ = tx.send(BackendEvent::AgentCost(usd));
    }
    if disabled.load(Ordering::SeqCst) {
        log(tx, LogLevel::Error, "synth", "automatic synthesis paused: the compiler agent could not run (see above); use Synthesize to retry", None);
    }
}

/// `cargo test` the generated crate and report pass/fail per node. Holes that
/// are still `todo!()` stubs get no result (their synthesis status says so).
fn run_tests(project: &wh_project::Project, model: &Model, generated: &wh_codegen::Generated, tx: &Sender<BackendEvent>) {
    let _ = tx.send(BackendEvent::TestsStarted);
    let stubs: Vec<String> = wh_build::unimplemented_holes(&generated.manifest).iter().map(|h| h.node.clone()).collect();
    let result = match wh_project::live::test(project, model, None) {
        Ok(r) => r,
        Err(e) => {
            log(tx, LogLevel::Error, "test", format!("{e:#}"), None);
            let _ = tx.send(BackendEvent::NodeResults(BTreeMap::new()));
            return;
        }
    };
    let _ = tx.send(BackendEvent::NodeResults(crate::jobs::node_results(&result.tests, &stubs)));
}

impl Backend for LiveBackend {
    fn document_changed(&mut self, model: &Model) {
        let generation = self.live.generation.fetch_add(1, Ordering::SeqCst) + 1;
        self.current = Some((generation, model.clone()));
        self.pending = Some((Instant::now(), model.clone()));
    }

    fn set_live(&mut self, on: bool) {
        self.live.on.store(on, Ordering::SeqCst);
        if on {
            self.live_wanted = Some(Instant::now());
        }
    }

    fn live_focus(&mut self, file: Option<String>, node: Option<NodeId>) {
        let mut f = self.live.focus.lock().unwrap();
        if *f != (file.clone(), node.clone()) {
            *f = (file, node);
            self.live_wanted = Some(Instant::now());
        }
    }

    fn check(&mut self, model: &Model) {
        if self.current.is_none() {
            self.current = Some((self.live.generation.load(Ordering::SeqCst), model.clone()));
        }
        self.pipeline(model.clone(), None);
    }

    fn synthesize(&mut self, model: &Model, node: &NodeId) {
        self.auto_disabled.store(false, Ordering::SeqCst);
        self.pipeline(model.clone(), Some((Some(node.clone()), true)));
    }

    fn run_app(&mut self, model: &Model) {
        if self.run_busy.swap(true, Ordering::SeqCst) {
            return;
        }
        let (project, tx, busy, model) = (self.project.clone(), self.tx.clone(), self.run_busy.clone(), model.clone());
        std::thread::spawn(move || {
            log(&tx, LogLevel::Info, "run", "building and running the app…", None);
            match wh_project::live::run_app(&project, &model, None, &[]) {
                Ok(out) => {
                    for l in out.stdout.lines() {
                        log(&tx, LogLevel::Info, "stdout", l, None);
                    }
                    for l in out.stderr.lines().filter(|l| !l.trim().is_empty()) {
                        log(&tx, LogLevel::Warn, "stderr", l, None);
                    }
                    for t in out.trace {
                        let _ = tx.send(BackendEvent::Trace(TraceEvent { level: level(&t.level), node: t.node, message: t.message, duration: t.duration }));
                    }
                    let _ = tx.send(BackendEvent::RunFinished { ok: out.success });
                }
                Err(e) => {
                    log(&tx, LogLevel::Error, "run", format!("{e:#}"), None);
                    let _ = tx.send(BackendEvent::RunFinished { ok: false });
                }
            }
            busy.store(false, Ordering::SeqCst);
        });
    }

    fn ask_author(&mut self, model: &Model, text: &str, selection: Option<&NodeId>) {
        if self.author_busy.swap(true, Ordering::SeqCst) {
            let _ = self.tx.send(BackendEvent::Chat { role: ChatRole::System, text: "the author agent is still working on the previous request".into() });
            return;
        }
        let buffers: BTreeMap<String, String> = model.files.iter().map(|f| (f.path.clone(), f.text.clone())).collect();
        let sel = selection.and_then(|id| {
            let n = model.nodes.get(id)?;
            Some((id.clone(), model.files[n.file].text[n.span.range()].to_string()))
        });
        let (author, tx, busy, text) = (self.author.clone(), self.tx.clone(), self.author_busy.clone(), text.to_string());
        // with the bridge, the agent edits through the `wh` tools (ops land in the store
        // directly); without it, its file edits come back as `AuthorEdit`s
        let server = self.bridge.as_ref().and_then(|socket| {
            let (program, prefix) = crate::mcp::server_program()?;
            Some(wh_mcp::author::bridge_mode_server(program, &prefix, socket))
        });
        std::thread::spawn(move || {
            let reply = Arc::new(Mutex::new(String::new()));
            let (r, etx) = (reply.clone(), tx.clone());
            let on_event = move |e: wh_author::AuthorEvent| match e {
                wh_author::AuthorEvent::Text(t) => r.lock().unwrap().push_str(&t),
                wh_author::AuthorEvent::Tool(t) => {
                    let _ = etx.send(BackendEvent::Chat { role: ChatRole::System, text: format!("→ {t}") });
                }
                wh_author::AuthorEvent::Edit(e) => {
                    let _ = etx.send(BackendEvent::AuthorEdit { path: e.path, base: e.base, new_text: e.new_text, label: e.label });
                }
                wh_author::AuthorEvent::Error(m) => {
                    let _ = etx.send(BackendEvent::Chat { role: ChatRole::System, text: m });
                }
                wh_author::AuthorEvent::Finished => {}
            };
            let result = author.ask_with(&text, buffers, sel.as_ref().map(|(a, b)| (a.as_str(), b.as_str())), server, on_event);
            let reply = std::mem::take(&mut *reply.lock().unwrap());
            if !reply.trim().is_empty() {
                let _ = tx.send(BackendEvent::Chat { role: ChatRole::Agent, text: reply });
            }
            if let Err(e) = result {
                let _ = tx.send(BackendEvent::Chat { role: ChatRole::System, text: format!("{e:#}") });
            }
            busy.store(false, Ordering::SeqCst);
        });
    }

    fn generated_rust(&self, model: &Model, node: &NodeId) -> Option<String> {
        // cheap to regenerate, but the inspector asks often: cache per model text
        let key = model.files.iter().fold(0u64, |h, f| {
            h.wrapping_mul(31).wrapping_add(f.text.len() as u64).wrapping_add(f.text.bytes().map(u64::from).sum::<u64>())
        });
        let mut cache = self.rust_cache.lock().unwrap();
        if cache.as_ref().is_none_or(|(k, _)| *k != key) {
            *cache = Some((key, BTreeMap::new()));
        }
        let (_, map) = cache.as_mut().unwrap();
        map.entry(node.clone()).or_insert_with(|| wh_project::live::generated_rust(&self.project, model, node)).clone()
    }

    fn run_job(&mut self, model: &Model, job: JobKind) {
        if self.job_busy.swap(true, Ordering::SeqCst) {
            log(&self.tx, LogLevel::Warn, "job", format!("{} not started: another job is running (Project → Cancel stops it)", job.label()), None);
            return;
        }
        self.job_cancel.store(false, Ordering::SeqCst);
        let (project, tx, model) = (self.project.clone(), self.tx.clone(), model.clone());
        let (busy, job_busy, cancel) = (self.busy.clone(), self.job_busy.clone(), self.job_cancel.clone());
        std::thread::spawn(move || {
            // the live check writes the same generated crate: wait for it, then hold it off
            while busy.swap(true, Ordering::SeqCst) {
                if cancel.load(Ordering::SeqCst) {
                    let _ = tx.send(BackendEvent::JobFinished { job, ok: false, summary: "cancelled".into(), artifacts: vec![], cost_usd: None });
                    job_busy.store(false, Ordering::SeqCst);
                    return;
                }
                std::thread::sleep(Duration::from_millis(200));
            }
            let compiler = agent_config(&project, "compiler");
            crate::jobs::Job { project: &project, model: &model, tx: &tx, cancel, compiler }.run(job);
            busy.store(false, Ordering::SeqCst);
            job_busy.store(false, Ordering::SeqCst);
        });
    }

    fn cancel_job(&mut self) {
        if self.job_busy.load(Ordering::SeqCst) {
            self.job_cancel.store(true, Ordering::SeqCst);
            log(&self.tx, LogLevel::Warn, "job", "cancelling: no further agent attempts; cargo finishes its current step", None);
        }
    }

    fn poll(&mut self) -> Vec<BackendEvent> {
        if let Some((at, _)) = &self.pending {
            if at.elapsed() >= self.debounce && !self.busy.load(Ordering::SeqCst) {
                let (_, model) = self.pending.take().unwrap();
                if !model.has_errors() {
                    let synth = (self.auto && !self.auto_disabled.load(Ordering::SeqCst)).then_some((None, false));
                    self.pipeline(model, synth);
                }
            }
        }
        // the focus moved (another file, another node): live values only, when the
        // current model already checked clean and nothing else holds the generated crate
        // (while a check or job runs, wait: a check evaluates live values itself)
        if self.live_wanted.is_some_and(|t| t.elapsed() >= Duration::from_millis(300)) && self.pending.is_none() && !self.busy.load(Ordering::SeqCst) {
            let generation = self.live.generation.load(Ordering::SeqCst);
            let clean = self.live.clean.load(Ordering::SeqCst) == generation + 1;
            match &self.current {
                Some((g, model)) if *g == generation && clean && self.live.on.load(Ordering::SeqCst) => {
                    if !self.busy.swap(true, Ordering::SeqCst) {
                        self.live_wanted = None;
                        let (project, tx, busy, live, model) = (self.project.clone(), self.tx.clone(), self.busy.clone(), self.live.clone(), model.clone());
                        std::thread::spawn(move || {
                            run_live(&project, &model, &live, generation, &tx);
                            busy.store(false, Ordering::SeqCst);
                        });
                    }
                }
                // this model was never checked: check it (live values follow a clean check)
                Some((g, model)) if *g == generation && self.live.on.load(Ordering::SeqCst) && self.live.checked.load(Ordering::SeqCst) != generation + 1 => {
                    self.live_wanted = None;
                    if !model.has_errors() {
                        self.pending = Some((Instant::now() - self.debounce, model.clone()));
                    }
                }
                _ => self.live_wanted = None,
            }
        }
        self.rx.try_iter().collect()
    }
}

/// The slow tests drive cargo in the shared per-user target dir: one at a time.
#[cfg(test)]
pub(crate) static CARGO_TESTS: Mutex<()> = Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn copy(from: &Path, to: &Path) {
        std::fs::create_dir_all(to).unwrap();
        for e in std::fs::read_dir(from).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                copy(&p, &to.join(e.file_name()));
            } else {
                std::fs::copy(&p, to.join(e.file_name())).unwrap();
            }
        }
    }

    #[test]
    fn live_values_follow_a_clean_check_and_the_focus() {
        let _cargo = CARGO_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        let root = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."));
        let dir = root.join("target/wh-ide-tests/signup-live");
        let _ = std::fs::remove_dir_all(&dir);
        copy(&root.join("examples/signup"), &dir);
        copy(&root.join("crates/wh-codegen/tests/fixtures/signup_impl"), &dir.join("impl"));
        let project = wh_project::Project::load(&dir).unwrap();
        let model = project.model().unwrap();
        let node = "signup.normalize_email".to_string();
        let file = model.files[model.nodes[&node].file].path.clone();
        let mut b = LiveBackend::new(project);
        b.set_live(true);
        b.live_focus(Some(file.clone()), Some(node.clone()));
        b.check(&model);
        let events = wait_for(&mut b, |e| matches!(e, BackendEvent::LivePending { running: false, .. }));
        let reports: Vec<(wh_live::LiveScope, Arc<wh_live::LiveReport>)> = events
            .iter()
            .filter_map(|e| if let BackendEvent::LiveValues { scope, report } = e { Some((scope.clone(), report.clone())) } else { None })
            .collect();
        let scopes: Vec<&wh_live::LiveScope> = reports.iter().map(|(s, _)| s).collect();
        assert_eq!(scopes, [&wh_live::LiveScope::Node(node.clone()), &wh_live::LiveScope::File(file.clone())], "{events:#?}");
        let mut st = crate::state::LiveState { on: true, ..Default::default() };
        for (scope, r) in &reports {
            st.merge(scope, r);
        }
        let first = st.nodes[&node].first().cloned().expect("values for the node");
        assert!(st.get(&node, first.kind, first.clause_index).is_some());
        assert_eq!(st.at(&file, first.span.start as usize).map(|v| &v.node), Some(&node));
        let (pass, fail, _) = st.counts();
        assert!(pass > 0 && fail == 0, "{:#?}", st.nodes);

        // the focus moves to another file: live values only, no new check
        let other = model.files.iter().map(|f| f.path.clone()).find(|p| *p != file).unwrap();
        b.live_focus(Some(other.clone()), None);
        let events = wait_for(&mut b, |e| matches!(e, BackendEvent::LiveValues { scope: wh_live::LiveScope::File(p), .. } if *p == other));
        assert!(!events.iter().any(|e| matches!(e, BackendEvent::Log { message, .. } if message.starts_with("cargo check"))), "{events:#?}");
    }

    #[test]
    fn test_outcomes_group_by_node() {
        let t = |name: &str, node: Option<&str>, passed: bool, output: &str| wh_build::TestOutcome {
            name: name.into(),
            node: node.map(String::from),
            passed,
            output: output.into(),
        };
        let tests = [
            t("a_example_0", Some("m.a"), true, ""),
            t("a_example_1", Some("m.a"), false, "thread 'a_example_1' panicked at src/lib.rs:3:5:\nassertion failed"),
            t("b_example_0", Some("m.b"), true, ""),
            t("c_example_0", Some("m.c"), false, "not yet implemented"),
            t("other", None, true, ""),
        ];
        let r = crate::jobs::node_results(&tests, &["m.c".to_string()]);
        assert_eq!(r.len(), 2, "stubs and unmapped tests get no result: {r:?}");
        assert_eq!((r["m.a"].passed, r["m.a"].failed), (1, 1));
        assert!(r["m.a"].failures[0].starts_with("a_example_1: thread 'a_example_1' panicked"), "{:?}", r["m.a"].failures);
        assert_eq!(r["m.a"].badge(), "✗ 1/2");
        assert_eq!(r["m.b"].badge(), "✓ 1");
    }

    fn wait_for(b: &mut LiveBackend, mut done: impl FnMut(&BackendEvent) -> bool) -> Vec<BackendEvent> {
        let start = Instant::now();
        let mut all = Vec::new();
        while start.elapsed() < Duration::from_secs(240) {
            for e in b.poll() {
                let stop = done(&e);
                all.push(e);
                if stop {
                    return all;
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("timed out; events so far: {all:#?}");
    }

    #[test]
    fn check_and_run_with_trace() {
        let _cargo = CARGO_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        let root = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."));
        let dir = root.join("target/wh-ide-tests/signup");
        let _ = std::fs::remove_dir_all(&dir);
        copy(&root.join("examples/signup"), &dir);
        copy(&root.join("crates/wh-codegen/tests/fixtures/signup_impl"), &dir.join("impl"));
        let project = wh_project::Project::load(&dir).unwrap();
        let model = project.model().unwrap();
        let mut b = LiveBackend::new(project);

        b.check(&model);
        let events = wait_for(&mut b, |e| matches!(e, BackendEvent::Synthesis { node, .. } if node == "signup.validate"));
        assert!(events.iter().any(|e| matches!(e, BackendEvent::Log { message, .. } if message == "cargo check: 0 error(s)")), "{events:#?}");
        // reference impls carry no key: they count as hand-written, not stale
        assert!(events.iter().any(|e| matches!(e, BackendEvent::Synthesis { node, status: SynthStatus::Accepted } if node == "signup.normalize_email")));
        // then the specs run, with results per node
        let events = wait_for(&mut b, |e| matches!(e, BackendEvent::NodeResults(_)));
        let results = events.iter().find_map(|e| if let BackendEvent::NodeResults(r) = e { Some(r.clone()) } else { None }).unwrap();
        let r = results.get("signup.normalize_email").unwrap_or_else(|| panic!("{results:#?}"));
        assert!(r.passed > 0 && r.ok(), "{r:?}");

        b.run_app(&model);
        let events = wait_for(&mut b, |e| matches!(e, BackendEvent::RunFinished { .. }));
        // no arguments: a usage error, reported and traced
        assert!(events.iter().any(|e| matches!(e, BackendEvent::RunFinished { ok: false })));
        assert!(events.iter().any(|e| matches!(e, BackendEvent::Log { message, .. } if message.contains("usage"))), "{events:#?}");
        let traced: Vec<&str> = events
            .iter()
            .filter_map(|e| match e {
                BackendEvent::Trace(t) => t.node.as_deref(),
                _ => None,
            })
            .collect();
        assert!(traced.contains(&"main.parse_cli"), "{traced:?}");
        assert!(traced.contains(&"main.signup_cli"), "{traced:?}");

        // the inspector's generated Rust comes from real codegen
        let code = b.generated_rust(&model, &"signup.check_age".to_string()).unwrap();
        assert!(code.contains("pub fn check_age(form: ValidForm) -> Result<ValidForm, RegisterError>"), "{code}");
    }
}
