//! The one shared app-state entity every panel observes.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use gpui_kit::*;
use wh_doc::Origin;
use wh_edit::Op;
use wh_model::NodeId;

use crate::backend::{Backend, BackendEvent, ChatRole, JobKind, JobPhase, LogLevel, NodeResult, SynthFailure, SynthStatus, TraceEvent};
use crate::project::{Change, Project};

#[derive(Debug, Clone)]
pub enum AppEvent {
    /// The document changed; every view re-renders from the new model.
    Changed(Change),
    /// The shared selection changed; `reveal` asks the document to scroll to it.
    Selected { id: Option<String>, reveal: bool, from: SelectionSource },
    Saved,
    /// The document panel switched to another file (index into `Project::files`).
    ActiveFile,
    /// Source files appeared or went away, or tabs were opened or closed.
    Files,
    /// New spec results (`AppState::tests`).
    Results,
    /// New live values (`AppState::live`), or live values switched on / off.
    LiveValues,
    /// Put the document caret at `offset` of `path` (Problems rows, F8).
    RevealAt { path: String, offset: usize },
    /// Bring the Problems tab forward.
    ShowProblems,
    /// A Project-menu job ended (the workspace shows a toast).
    JobDone { job: JobKind, ok: bool, summary: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ProblemLevel {
    Error,
    Warning,
    Lint,
}

/// One row of the Problems tab: a checker diagnostic, or a node-mapped
/// error from the last job (compiler, tests, gates).
#[derive(Debug, Clone)]
pub struct Problem {
    pub level: ProblemLevel,
    pub path: String,
    pub offset: usize,
    /// 1-based line.
    pub line: usize,
    pub node: Option<NodeId>,
    pub message: String,
    pub source: &'static str,
}

/// The latest live values (`wh_live`), merged across scopes: the selected node's,
/// then the active file's. Views anchor them by `file` + `span` and dim the ones
/// whose `source` no longer matches the text there.
#[derive(Debug, Clone, Default)]
pub struct LiveState {
    pub on: bool,
    /// An evaluation is in flight (the status bar shows a quiet "…").
    pub running: bool,
    /// Why the last evaluation produced nothing (time-out, build trouble), shown quietly.
    pub stale: Option<String>,
    /// node id (or `module#test:name`) → its clauses' values, in source order.
    pub nodes: BTreeMap<NodeId, Vec<wh_live::LiveValue>>,
    /// step id (`module.flow#step[k]`) → values seen on the edge leaving that step.
    pub steps: BTreeMap<String, Vec<wh_live::StepSample>>,
    pub build_errors: Vec<String>,
}

// the lookups serve the document and graph views (ide-ux)
#[allow(dead_code)]
impl LiveState {
    /// The value of one clause.
    pub fn get(&self, node: &str, kind: wh_live::ItemKind, clause_index: usize) -> Option<&wh_live::LiveValue> {
        self.nodes.get(node)?.iter().find(|v| v.kind == kind && v.clause_index == clause_index)
    }

    /// The clause value whose span holds byte `offset` of `path`.
    pub fn at<'a>(&'a self, path: &'a str, offset: usize) -> Option<&'a wh_live::LiveValue> {
        self.in_file(path).find(|v| (v.span.start as usize) <= offset && offset <= v.span.end as usize)
    }

    pub fn in_file<'a>(&'a self, path: &'a str) -> impl Iterator<Item = &'a wh_live::LiveValue> + 'a {
        self.nodes.values().flatten().filter(move |v| v.file == path)
    }

    /// (passing, failing, other) clause counts.
    pub fn counts(&self) -> (usize, usize, usize) {
        let all = self.nodes.values().flatten();
        let (mut pass, mut fail, mut other) = (0, 0, 0);
        for v in all {
            match v.status {
                wh_live::Status::Pass => pass += 1,
                wh_live::Status::Fail | wh_live::Status::Error | wh_live::Status::Crashed => fail += 1,
                _ => other += 1,
            }
        }
        (pass, fail, other)
    }

    /// Fold in a report: a file's report replaces everything from that file, a node's replaces that node.
    pub fn merge(&mut self, scope: &wh_live::LiveScope, report: &wh_live::LiveReport) {
        if let wh_live::LiveScope::File(path) = scope {
            let gone: Vec<NodeId> = self.nodes.iter().filter(|(_, vs)| vs.iter().any(|v| &v.file == path)).map(|(k, _)| k.clone()).collect();
            for k in &gone {
                self.nodes.remove(k);
                let prefix = format!("{k}#");
                self.steps.retain(|s, _| !s.starts_with(&prefix));
            }
        }
        for (k, vs) in &report.nodes {
            self.nodes.insert(k.clone(), vs.clone());
        }
        for (k, ss) in &report.steps {
            self.steps.insert(k.clone(), ss.clone());
        }
        self.build_errors = report.build_errors.clone();
        self.stale = None;
    }
}

/// The Project-menu job in progress.
#[derive(Debug, Clone)]
pub struct JobState {
    pub kind: JobKind,
    pub phase: Option<JobPhase>,
    pub done: usize,
    pub total: usize,
    pub detail: String,
    pub started: Instant,
}

/// An author-agent edit held for review (`[author] apply = "review"`).
#[derive(Debug, Clone)]
pub struct PendingEdit {
    pub id: u64,
    pub label: String,
    pub files: Vec<PendingFile>,
    /// Why the last Accept failed (a merge conflict, usually).
    pub error: Option<String>,
    /// Edits planned by the MCP bridge: applied as one transaction when every hunk is kept and every base still matches.
    planned: Option<Vec<wh_doc::FileEdit>>,
    /// The bridge call waiting for the decision.
    reply: Option<std::sync::mpsc::Sender<Result<Vec<wh_mcp::view::EditedRange>, String>>>,
}

/// One file of a proposed edit, split into hunks the user can keep or skip.
#[derive(Debug, Clone)]
pub struct PendingFile {
    pub path: String,
    pub base: String,
    pub new: String,
    pub hunks: Vec<Hunk>,
}

#[derive(Debug, Clone)]
pub struct Hunk {
    /// Line ranges in the base and in the proposed text.
    pub old: std::ops::Range<usize>,
    pub new: std::ops::Range<usize>,
    /// Unified-diff lines with context: (`' '` / `'+'` / `'-'`, text).
    pub lines: Vec<(char, String)>,
    pub keep: bool,
}

impl PendingFile {
    pub fn new(path: String, base: String, new: String) -> PendingFile {
        let hunks = hunks(&base, &new);
        PendingFile { path, base, new, hunks }
    }

    /// The base with only the kept hunks applied.
    pub fn result(&self) -> String {
        let old: Vec<&str> = self.base.split_inclusive('\n').collect();
        let new: Vec<&str> = self.new.split_inclusive('\n').collect();
        let mut out = String::new();
        let mut at = 0;
        for h in &self.hunks {
            out.extend(old[at..h.old.start].iter().copied());
            if h.keep {
                out.extend(new[h.new.clone()].iter().copied());
            } else {
                out.extend(old[h.old.clone()].iter().copied());
            }
            at = h.old.end;
        }
        out.extend(old[at..].iter().copied());
        out
    }
}

impl PendingEdit {
    pub fn paths(&self) -> String {
        self.files.iter().map(|f| f.path.as_str()).collect::<Vec<_>>().join(", ")
    }

    pub fn counts(&self) -> (usize, usize) {
        let all = self.files.iter().map(|f| f.hunks.len()).sum();
        let kept = self.files.iter().map(|f| f.hunks.iter().filter(|h| h.keep).count()).sum();
        (kept, all)
    }
}

/// Change hunks of `base` → `new`, each with two lines of context.
fn hunks(base: &str, new: &str) -> Vec<Hunk> {
    let diff = similar::TextDiff::from_lines(base, new);
    let mut out = Vec::new();
    for group in diff.grouped_ops(2) {
        // the change ops of this group, without the context ops at its ends
        let changes: Vec<&similar::DiffOp> = group.iter().filter(|op| op.tag() != similar::DiffTag::Equal).collect();
        let (Some(first), Some(last)) = (changes.first(), changes.last()) else { continue };
        let old = first.old_range().start..last.old_range().end;
        let new_r = first.new_range().start..last.new_range().end;
        let mut lines = Vec::new();
        for op in &group {
            for change in diff.iter_changes(op) {
                let tag = match change.tag() {
                    similar::ChangeTag::Equal => ' ',
                    similar::ChangeTag::Insert => '+',
                    similar::ChangeTag::Delete => '-',
                };
                lines.push((tag, change.value().trim_end_matches('\n').to_string()));
            }
        }
        out.push(Hunk { old, new: new_r, lines, keep: true });
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionSource {
    Document,
    Graph,
    Inspector,
    Log,
}

#[derive(Debug, Clone)]
pub struct LogEntry {
    pub at: Instant,
    pub level: LogLevel,
    pub source: String,
    pub message: String,
    pub node: Option<NodeId>,
}

pub struct AppState {
    pub project: Project,
    pub backend: Box<dyn Backend>,
    pub log: Vec<LogEntry>,
    pub trace: Vec<(Instant, TraceEvent)>,
    pub chat: Vec<(ChatRole, String)>,
    pub synthesis: Vec<(NodeId, SynthStatus)>,
    /// Agent spending reported this session (USD), and `[synthesis] budget_usd`.
    pub agent_cost: f64,
    pub budget_usd: Option<f64>,
    /// Why holes failed synthesis, until they are accepted (failure cards).
    pub failures: BTreeMap<NodeId, SynthFailure>,
    /// Nodes seen in the latest trace (executed path highlighting).
    pub executed: std::collections::BTreeSet<String>,
    pub started: Instant,
    /// File shown in the document panel; scopes the flow graph.
    pub active_file: usize,
    /// Files with a tab in the document panel, in tab order.
    pub open_tabs: Vec<String>,
    /// Spec results per node from the latest test run.
    pub tests: std::collections::BTreeMap<NodeId, NodeResult>,
    pub tests_running: bool,
    /// Hold author edits for review instead of applying them live.
    pub review: bool,
    pub pending: Vec<PendingEdit>,
    next_pending: u64,
    /// Live values of the clauses near the user (View → Live Values).
    pub live: LiveState,
    /// The running Project-menu job, if any.
    pub job: Option<JobState>,
    /// Node-mapped errors and warnings reported by the last job (compiler, tests, gates).
    pub job_problems: Vec<LogEntry>,
    /// Index into `problems()` of the last F8 / ⇧F8 stop.
    problem_cursor: Option<usize>,
    watcher: Option<crate::watch::Watcher>,
    /// Work sent by the MCP bridge threads, run on the UI thread.
    ui_calls: Option<std::sync::mpsc::Receiver<crate::mcp::UiCall>>,
}

impl EventEmitter<AppEvent> for AppState {}

impl AppState {
    pub fn new(project: Project, backend: Box<dyn Backend>, cx: &mut Context<Self>) -> Self {
        // drain backend events on a short timer
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(100)).await;
                if this.update(cx, |this, cx| this.poll_backend(cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
        let watcher = crate::watch::Watcher::new(&project.dir);
        let review = crate::config::author_review(&project.dir);
        let budget = crate::config::synthesis_budget(&project.dir);
        let mut s = AppState {
            open_tabs: project.files.clone(),
            review,
            project,
            backend,
            log: Vec::new(),
            trace: Vec::new(),
            chat: Vec::new(),
            synthesis: Vec::new(),
            failures: BTreeMap::new(),
            agent_cost: 0.0,
            budget_usd: budget,
            executed: Default::default(),
            started: Instant::now(),
            active_file: 0,
            tests: Default::default(),
            tests_running: false,
            pending: Vec::new(),
            next_pending: 0,
            live: LiveState { on: true, ..Default::default() },
            job: None,
            job_problems: Vec::new(),
            problem_cursor: None,
            ui_calls: None,
            watcher: None,
        };
        match watcher {
            Ok(w) => s.watcher = Some(w),
            Err(e) => s.push_log(LogLevel::Warn, "watch", format!("not watching the project for external changes: {e}"), None),
        }
        let files = s.project.files.len();
        let nodes = s.project.model.nodes.len();
        s.info("ide", format!("opened {} — {files} file(s), {nodes} node(s)", s.project.dir.display()));
        s.backend.set_live(s.live.on);
        for d in &s.project.model.diagnostics {
            let f = &s.project.model.files[d.file];
            let node = s.project.model.node_at(d.file, d.diag.span.start).map(|n| n.id.clone());
            s.log.push(LogEntry {
                at: Instant::now(),
                level: match d.diag.severity {
                    wh_syntax::Severity::Error => LogLevel::Error,
                    wh_syntax::Severity::Warning => LogLevel::Warn,
                    wh_syntax::Severity::Lint => LogLevel::Lint,
                },
                source: "check".into(),
                message: d.diag.render(&f.path, &f.text),
                node,
            });
        }
        s
    }

    pub fn info(&mut self, source: &str, message: impl Into<String>) {
        self.push_log(LogLevel::Info, source, message.into(), None);
    }

    pub fn push_log(&mut self, level: LogLevel, source: &str, message: String, node: Option<NodeId>) {
        self.log.push(LogEntry { at: Instant::now(), level, source: source.into(), message, node });
        if self.log.len() > 2000 {
            self.log.drain(..500);
        }
    }

    fn changed(&mut self, change: Change, cx: &mut Context<Self>) {
        self.backend.document_changed(&self.project.model);
        self.problem_cursor = None;
        cx.emit(AppEvent::Changed(change));
        // a selection the edit (or undo) removed is cleared everywhere
        if let Some(sel) = self.project.selection.clone() {
            if crate::capsule::selection_label(&self.project.display, &sel).is_none() && self.project.selected_node().is_none() {
                self.project.selection = None;
                cx.emit(AppEvent::Selected { id: None, reveal: false, from: SelectionSource::Document });
            }
        }
        cx.notify();
    }

    /// Checker diagnostics (by file, then position) followed by the last job's node-mapped problems.
    pub fn problems(&self) -> Vec<Problem> {
        let m = &self.project.model;
        let mut out: Vec<Problem> = m
            .diagnostics
            .iter()
            .map(|d| {
                let f = &m.files[d.file];
                let (line, _) = wh_syntax::span::line_col(&f.text, d.diag.span.start);
                Problem {
                    level: match d.diag.severity {
                        wh_syntax::Severity::Error => ProblemLevel::Error,
                        wh_syntax::Severity::Warning => ProblemLevel::Warning,
                        wh_syntax::Severity::Lint => ProblemLevel::Lint,
                    },
                    path: f.path.clone(),
                    offset: d.diag.span.start as usize,
                    line: line + 1,
                    node: m.node_at(d.file, d.diag.span.start).map(|n| n.id.clone()),
                    message: d.diag.message.clone(),
                    source: "check",
                }
            })
            .collect();
        out.sort_by(|a, b| (self.project.file_index(&a.path), a.offset).cmp(&(self.project.file_index(&b.path), b.offset)));
        for e in &self.job_problems {
            let Some(n) = e.node.as_deref().and_then(|id| m.nodes.get(id.split('#').next().unwrap_or(id))) else { continue };
            let f = &m.files[n.file];
            let (line, _) = wh_syntax::span::line_col(&f.text, n.span.start);
            out.push(Problem {
                level: if e.level == LogLevel::Error { ProblemLevel::Error } else { ProblemLevel::Warning },
                path: f.path.clone(),
                offset: n.span.start as usize,
                line: line + 1,
                node: e.node.clone(),
                message: e.message.lines().next().unwrap_or("").to_string(),
                source: "job",
            });
        }
        out
    }

    /// Select what is at `offset` of `path` and put the document caret there.
    pub fn reveal_at(&mut self, path: &str, offset: usize, cx: &mut Context<Self>) {
        if let Some(i) = self.project.files.iter().position(|f| f == path) {
            self.set_active_file(i, cx);
        }
        let sel = self.project.selection_at(path, offset);
        if sel.is_some() {
            self.select(sel, false, SelectionSource::Log, cx);
        }
        cx.emit(AppEvent::RevealAt { path: path.to_string(), offset });
        cx.notify();
    }

    /// F8 / ⇧F8: the next or previous error or warning (lints are skipped).
    pub fn step_problem(&mut self, forward: bool, cx: &mut Context<Self>) {
        let ps: Vec<Problem> = self.problems().into_iter().filter(|p| p.level != ProblemLevel::Lint).collect();
        if ps.is_empty() {
            self.info("problems", "no errors or warnings");
            cx.notify();
            return;
        }
        let n = ps.len();
        let k = match (self.problem_cursor, forward) {
            (None, true) => 0,
            (None, false) => n - 1,
            (Some(k), true) => (k + 1) % n,
            (Some(k), false) => (k + n - 1) % n,
        };
        self.problem_cursor = Some(k);
        let p = ps[k].clone();
        self.reveal_at(&p.path, p.offset, cx);
    }

    pub fn show_problems(&mut self, cx: &mut Context<Self>) {
        cx.emit(AppEvent::ShowProblems);
    }

    /// Apply a structured op (graph / inspector gestures).
    pub fn apply_op(&mut self, origin: Origin, op: Op, cx: &mut Context<Self>) -> bool {
        match self.project.apply_op(origin, &op) {
            Ok(change) => {
                self.info(&format!("{origin:?}").to_lowercase(), change.label.clone());
                self.changed(change, cx);
                true
            }
            Err(e) => {
                self.push_log(LogLevel::Warn, "edit", format!("{}: {e}", op.label()), None);
                cx.notify();
                false
            }
        }
    }

    /// Typing in the document panel.
    pub fn edit_text(&mut self, path: &str, range: std::ops::Range<usize>, text: &str, cx: &mut Context<Self>) {
        match self.project.edit_text(Origin::Text, path, range, text) {
            Ok(change) => self.changed(change, cx),
            Err(e) => self.push_log(LogLevel::Error, "edit", e, None),
        }
    }

    pub fn undo(&mut self, cx: &mut Context<Self>) {
        match self.project.undo() {
            Some(c) => {
                self.info("undo", c.label.clone());
                self.changed(c, cx);
            }
            None => self.info("undo", "nothing to undo"),
        }
        cx.notify();
    }

    pub fn redo(&mut self, cx: &mut Context<Self>) {
        match self.project.redo() {
            Some(c) => {
                self.info("redo", c.label.clone());
                self.changed(c, cx);
            }
            None => self.info("redo", "nothing to redo"),
        }
        cx.notify();
    }

    pub fn select(&mut self, id: Option<String>, reveal: bool, from: SelectionSource, cx: &mut Context<Self>) {
        if self.project.selection == id && !reveal {
            return;
        }
        self.project.selection = id.clone();
        self.push_live_focus();
        cx.emit(AppEvent::Selected { id, reveal, from });
        cx.notify();
    }

    pub fn set_active_file(&mut self, i: usize, cx: &mut Context<Self>) {
        if let Some(path) = self.project.files.get(i).cloned() {
            if !self.open_tabs.contains(&path) {
                self.open_tabs.push(path);
                cx.emit(AppEvent::Files);
            }
        }
        if self.active_file != i {
            self.active_file = i;
            self.push_live_focus();
            cx.emit(AppEvent::ActiveFile);
            cx.notify();
        }
    }

    /// The file of the active tab, if any tab is open.
    pub fn active_path(&self) -> Option<String> {
        self.project.files.get(self.active_file).filter(|p| self.open_tabs.contains(p)).cloned()
    }

    pub fn open_tab(&mut self, path: &str, cx: &mut Context<Self>) {
        if let Some(i) = self.project.files.iter().position(|f| f == path) {
            self.set_active_file(i, cx);
            cx.emit(AppEvent::Files);
            cx.notify();
        }
    }

    pub fn close_tab(&mut self, path: &str, cx: &mut Context<Self>) {
        let Some(t) = self.open_tabs.iter().position(|p| p == path) else { return };
        self.open_tabs.remove(t);
        if self.active_path().is_none() {
            // the neighbour tab becomes active
            let next = self.open_tabs.get(t.min(self.open_tabs.len().saturating_sub(1))).cloned();
            if let Some(i) = next.and_then(|p| self.project.files.iter().position(|f| *f == p)) {
                self.active_file = i;
                cx.emit(AppEvent::ActiveFile);
            }
        }
        cx.emit(AppEvent::Files);
        cx.notify();
    }

    /// Keep `active_file` on the same path after the file list changed.
    fn files_changed(&mut self, active: Option<String>, cx: &mut Context<Self>) {
        let files = self.project.files.clone();
        self.open_tabs.retain(|p| files.contains(p));
        self.active_file = active
            .and_then(|a| files.iter().position(|f| *f == a))
            .or_else(|| self.open_tabs.first().and_then(|p| files.iter().position(|f| f == p)))
            .unwrap_or(0);
        cx.emit(AppEvent::Files);
    }

    pub fn save_file(&mut self, path: &str, cx: &mut Context<Self>) -> bool {
        let r = self.project.save_file(path);
        let ok = match r {
            Ok(true) => {
                self.info("save", format!("saved {path}"));
                true
            }
            Ok(false) => true,
            Err(e) => {
                self.push_log(LogLevel::Error, "save", format!("{path}: {e}"), None);
                false
            }
        };
        let _ = self.project.layout.save(&self.project.dir);
        cx.emit(AppEvent::Saved);
        cx.notify();
        ok
    }

    /// Save every dirty file; false if one could not be written.
    pub fn save_all(&mut self, cx: &mut Context<Self>) -> bool {
        let ok = match self.project.save() {
            Ok(files) if files.is_empty() => true,
            Ok(files) => {
                self.info("save", format!("saved {}", files.join(", ")));
                true
            }
            Err(e) => {
                self.push_log(LogLevel::Error, "save", e.to_string(), None);
                false
            }
        };
        cx.emit(AppEvent::Saved);
        cx.notify();
        ok
    }

    pub fn revert_file(&mut self, path: &str, cx: &mut Context<Self>) {
        match self.project.revert_file(path) {
            Ok(Some(c)) => {
                self.info("revert", format!("reverted {path} to the saved text (⌘Z brings the edits back)"));
                self.changed(c, cx);
            }
            Ok(None) => self.info("revert", format!("{path} has no unsaved changes")),
            Err(e) => self.push_log(LogLevel::Error, "revert", format!("{path}: {e}"), None),
        }
        cx.emit(AppEvent::Saved);
        cx.notify();
    }

    /// Bring a source file into the project (created by the IDE or found on disk) and open it.
    pub fn add_file(&mut self, path: &str, text: &str, cx: &mut Context<Self>) {
        let active = self.active_path();
        let change = self.project.add_file(path, text);
        self.files_changed(active, cx);
        self.changed(change, cx);
        self.open_tab(path, cx);
    }

    /// Save the active file's buffer as `to` (see `Project::save_as`).
    pub fn save_as(&mut self, from: &str, to: &std::path::Path, cx: &mut Context<Self>) {
        let active = self.active_path();
        match self.project.save_as(from, to) {
            Ok(Some(rel)) => {
                self.info("save", format!("saved {from} as {rel} (a new module)"));
                self.files_changed(active, cx);
                self.changed(Change { origin: Origin::Disk, label: format!("save as {rel}"), ranges: Default::default() }, cx);
                self.open_tab(&rel, cx);
            }
            Ok(None) => self.info("save", format!("exported {from} to {} (outside the project's sources)", to.display())),
            Err(e) => self.push_log(LogLevel::Error, "save", format!("save as {}: {e}", to.display()), None),
        }
        cx.emit(AppEvent::Saved);
        cx.notify();
    }

    /// Apply external changes to the project folder.
    fn poll_disk(&mut self, cx: &mut Context<Self>) {
        let Some(paths) = self.watcher.as_ref().map(|w| w.drain()) else { return };
        for rel in paths {
            if rel == "wildhorse.toml" {
                let review = crate::config::author_review(&self.project.dir);
                if review != self.review {
                    self.review = review;
                    self.info("config", format!("author edits now apply {}", if review { "after review" } else { "live" }));
                    cx.notify();
                }
                continue;
            }
            if !self.project.is_source(&rel) {
                continue;
            }
            let on_disk = self.project.dir.join(&rel).is_file();
            let known = self.project.files.contains(&rel);
            let active = self.active_path();
            match (on_disk, known) {
                (true, true) => {
                    let dirty = self.project.docs.is_dirty(&rel);
                    match self.project.reload_file(&rel) {
                        Ok(Some(c)) => {
                            self.info("disk", format!("{rel} changed on disk{}", if dirty { "; merged onto the unsaved edits" } else { "" }));
                            self.changed(c, cx);
                        }
                        Ok(None) => {}
                        Err(e) => self.push_log(LogLevel::Error, "disk", format!("{rel} changed on disk and could not be merged: {e}"), None),
                    }
                }
                (true, false) => {
                    let Ok(text) = std::fs::read_to_string(self.project.dir.join(&rel)) else { continue };
                    let c = self.project.add_file(&rel, &text);
                    self.open_tabs.push(rel.clone());
                    self.files_changed(active, cx);
                    self.info("disk", format!("new file {rel}"));
                    self.changed(c, cx);
                }
                (false, true) => {
                    if self.project.docs.is_dirty(&rel) {
                        self.push_log(LogLevel::Warn, "disk", format!("{rel} was deleted on disk; its unsaved edits are dropped from the project"), None);
                    }
                    if let Some(c) = self.project.remove_file(&rel) {
                        self.files_changed(active, cx);
                        self.info("disk", format!("{rel} was deleted"));
                        self.changed(c, cx);
                    }
                }
                (false, false) => {}
            }
        }
    }

    pub fn run_job(&mut self, job: JobKind, cx: &mut Context<Self>) {
        if let Some(j) = &self.job {
            self.push_log(LogLevel::Warn, "job", format!("{} is still running; cancel it first", j.kind.label()), None);
        } else {
            self.backend.run_job(&self.project.model, job);
        }
        cx.notify();
    }

    pub fn cancel_job(&mut self, cx: &mut Context<Self>) {
        self.backend.cancel_job();
        cx.notify();
    }

    /// Failure card: retry with a `note` clause that steers the compiler agent.
    pub fn retry_with_note(&mut self, node: NodeId, note: &str, cx: &mut Context<Self>) {
        let clause = format!("note \"{}\"", note.trim().replace('\\', "\\\\").replace('"', "\\\""));
        if self.apply_op(Origin::Inspector, Op::AddClause { node: node.clone(), clause }, cx) {
            self.synthesize(node, cx);
        }
    }

    /// Failure card: mark the hole file `// wh:pinned` so a hand-written body is kept and never regenerated.
    pub fn pin_manual(&mut self, f: &SynthFailure, cx: &mut Context<Self>) {
        let Some(path) = &f.hole_file else { return };
        match std::fs::read_to_string(path) {
            Ok(text) if text.lines().take(3).any(|l| l.trim() == wh_synth::PINNED) => self.info("synth", format!("{} is already pinned", f.node)),
            Ok(text) => match std::fs::write(path, format!("{}\n{text}", wh_synth::PINNED)) {
                Ok(()) => {
                    self.info("synth", format!("pinned {}: write the body by hand in {}", f.node, path.display()));
                    cx.open_with_system(path);
                }
                Err(e) => self.push_log(LogLevel::Error, "synth", e.to_string(), Some(f.node.clone())),
            },
            Err(e) => self.push_log(LogLevel::Error, "synth", e.to_string(), Some(f.node.clone())),
        }
        cx.notify();
    }

    /// Failure card: ask the author agent to make the spec implementable.
    pub fn ask_author_to_revise(&mut self, f: &SynthFailure, cx: &mut Context<Self>) {
        self.select(Some(f.node.clone()), true, SelectionSource::Log, cx);
        let text = format!(
            "The compiler agent could not implement `{}` ({} gate, {} round(s)):\n{}\nRevise its spec (signature, contracts, examples, notes) so it is correct and implementable, or explain what is wrong with it.",
            f.node, f.gate, f.rounds, f.detail
        );
        self.ask_author(text, cx);
    }

    /// View → Live Values.
    pub fn set_live(&mut self, on: bool, cx: &mut Context<Self>) {
        self.live.on = on;
        if !on {
            self.live = LiveState::default();
        }
        self.backend.set_live(on);
        self.push_live_focus();
        self.info("live", if on { "live values on" } else { "live values off" });
        cx.emit(AppEvent::LiveValues);
        cx.notify();
    }

    /// Tell the backend what to evaluate first: the selected node, then the active file.
    pub fn push_live_focus(&mut self) {
        let node = self.project.selected_node().map(|n| n.id.clone());
        self.backend.live_focus(self.active_path(), node);
    }

    pub fn set_review(&mut self, review: bool, cx: &mut Context<Self>) {
        self.review = review;
        cx.notify();
    }

    /// Hold an author edit for review; `reply` answers a waiting bridge call.
    pub fn propose(
        &mut self,
        label: String,
        files: Vec<(String, String, String)>,
        planned: Option<Vec<wh_doc::FileEdit>>,
        reply: Option<std::sync::mpsc::Sender<Result<Vec<wh_mcp::view::EditedRange>, String>>>,
        cx: &mut Context<Self>,
    ) {
        self.next_pending += 1;
        let files: Vec<PendingFile> = files.into_iter().map(|(p, b, n)| PendingFile::new(p, b, n)).collect();
        let paths = files.iter().map(|f| f.path.as_str()).collect::<Vec<_>>().join(", ");
        self.chat.push((ChatRole::System, format!("proposed edit to {paths}: {label} — review it above")));
        self.pending.push(PendingEdit { id: self.next_pending, label, files, error: None, planned, reply });
        cx.notify();
    }

    /// Keep or skip one hunk of a pending edit.
    pub fn toggle_hunk(&mut self, id: u64, file: usize, hunk: usize, cx: &mut Context<Self>) {
        if let Some(h) = self.pending.iter_mut().find(|p| p.id == id).and_then(|p| p.files.get_mut(file)).and_then(|f| f.hunks.get_mut(hunk)) {
            h.keep = !h.keep;
        }
        cx.notify();
    }

    /// An `Origin::Agent` transaction landed in the store: rebuild and tell every view.
    pub fn agent_edited(&mut self, label: String, ranges: BTreeMap<String, Vec<std::ops::Range<usize>>>, cx: &mut Context<Self>) {
        self.project.rebuild();
        self.changed(Change { origin: Origin::Agent, label, ranges }, cx);
    }

    pub fn accept_pending(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(i) = self.pending.iter().position(|p| p.id == id) else { return };
        let p = self.pending[i].clone();
        let (kept, all) = p.counts();
        if kept == 0 {
            return self.reject_pending(id, cx);
        }
        let unchanged = p.files.iter().all(|f| self.project.text(&f.path) == f.base);
        let mut ranges: BTreeMap<String, Vec<std::ops::Range<usize>>> = BTreeMap::new();
        let result: Result<Vec<wh_mcp::view::EditedRange>, String> = match (unchanged && kept == all, p.planned.clone()) {
            // exactly what the agent planned, as one undo step
            (true, Some(edits)) => match wh_mcp::view::apply_to_store(&mut self.project.docs, &p.label, edits) {
                Ok((edited, _)) => {
                    if let Some(tx) = self.project.docs.history().last() {
                        ranges = tx.changed_ranges();
                    }
                    Ok(edited)
                }
                Err(e) => Err(e.to_string()),
            },
            // only some hunks, or the buffers moved on since: three-way merge each file
            _ => {
                let mut err = None;
                for f in &p.files {
                    match self.project.docs.merge_write(Origin::Agent, p.label.clone(), &f.path, &f.base, &f.result()) {
                        Ok(Some(tx)) => {
                            for (k, v) in tx.changed_ranges() {
                                ranges.entry(k).or_default().extend(v);
                            }
                        }
                        Ok(None) => {}
                        Err(e) => {
                            err = Some(e.to_string());
                            break;
                        }
                    }
                }
                err.map_or(Ok(vec![]), Err)
            }
        };
        match result {
            Ok(edited) => {
                self.pending.remove(i);
                let what = if kept == all { String::new() } else { format!(" ({kept} of {all} hunks)") };
                self.info("agent", format!("accepted: {}{what}", p.label));
                self.agent_edited(p.label.clone(), ranges, cx);
                if let Some(r) = p.reply {
                    let _ = r.send(if kept == all {
                        Ok(edited)
                    } else {
                        Err(format!(
                            "the user applied only {kept} of {all} hunks of this edit and skipped the rest; read the affected nodes again before continuing"
                        ))
                    });
                }
            }
            Err(e) => {
                if !ranges.is_empty() {
                    self.agent_edited(p.label.clone(), ranges, cx);
                }
                self.pending[i].error = Some(e.clone());
                self.push_log(LogLevel::Error, "agent", e, None);
            }
        }
        cx.notify();
    }

    pub fn reject_pending(&mut self, id: u64, cx: &mut Context<Self>) {
        if let Some(i) = self.pending.iter().position(|p| p.id == id) {
            let p = self.pending.remove(i);
            self.info("agent", format!("rejected: {}", p.label));
            if let Some(r) = p.reply {
                let _ = r.send(Err("the user rejected this edit in review; ask what they want instead".into()));
            }
        }
        cx.notify();
    }

    pub fn set_position(&mut self, graph: &str, id: &str, p: wh_graph::Point) {
        self.project.layout.set(graph, id, p);
    }

    pub fn check(&mut self, cx: &mut Context<Self>) {
        self.backend.check(&self.project.model);
        cx.notify();
    }

    pub fn synthesize(&mut self, node: NodeId, cx: &mut Context<Self>) {
        self.backend.synthesize(&self.project.model, &node);
        cx.notify();
    }

    pub fn run_app(&mut self, cx: &mut Context<Self>) {
        self.trace.clear();
        self.executed.clear();
        self.backend.run_app(&self.project.model);
        cx.notify();
    }

    pub fn ask_author(&mut self, text: String, cx: &mut Context<Self>) {
        let sel = self.project.selected_node().map(|n| n.id.clone());
        self.ask_author_scoped(text, sel, cx);
    }

    /// Ask with an explicit context node (the Agent panel's chips decide it).
    pub fn ask_author_scoped(&mut self, text: String, sel: Option<NodeId>, cx: &mut Context<Self>) {
        self.backend.ask_author(&self.project.model, &text, sel.as_ref());
        cx.notify();
    }

    pub fn set_ui_calls(&mut self, rx: std::sync::mpsc::Receiver<crate::mcp::UiCall>) {
        self.ui_calls = Some(rx);
    }

    fn poll_backend(&mut self, cx: &mut Context<Self>) {
        self.poll_disk(cx);
        let calls: Vec<crate::mcp::UiCall> = self.ui_calls.as_ref().map(|rx| rx.try_iter().collect()).unwrap_or_default();
        for f in calls {
            f(self, cx);
        }
        let events = self.backend.poll();
        if events.is_empty() {
            return;
        }
        for e in events {
            match e {
                BackendEvent::Log { level, source, message, node } => {
                    if self.job.is_some() && node.is_some() && matches!(level, LogLevel::Error | LogLevel::Warn) {
                        self.job_problems.push(LogEntry { at: Instant::now(), level, source: source.clone(), message: message.clone(), node: node.clone() });
                    }
                    self.push_log(level, &source, message, node)
                }
                BackendEvent::JobStarted { job } => {
                    self.info("job", format!("{} started", job.label()));
                    self.job_problems.clear();
                    self.job = Some(JobState { kind: job, phase: None, done: 0, total: 0, detail: String::new(), started: Instant::now() });
                }
                BackendEvent::JobProgress { phase, done, total, detail } => {
                    if let Some(j) = &mut self.job {
                        if j.phase != Some(phase) {
                            self.log.push(LogEntry { at: Instant::now(), level: LogLevel::Info, source: "job".into(), message: format!("{}…", phase.label()), node: None });
                        }
                        *j = JobState { phase: Some(phase), done, total, detail, ..j.clone() };
                    }
                }
                BackendEvent::JobFinished { job, ok, summary, artifacts: _, cost_usd } => {
                    self.agent_cost += cost_usd.unwrap_or(0.0);
                    let secs = self.job.take().map_or(0.0, |j| j.started.elapsed().as_secs_f32());
                    let cost = cost_usd.map(|c| format!(", ${c:.2} agent cost")).unwrap_or_default();
                    let summary = format!("{summary} ({secs:.0} s{cost})");
                    let level = if ok { LogLevel::Info } else { LogLevel::Error };
                    self.push_log(level, "job", format!("{} {}: {summary}", job.label(), if ok { "done" } else { "failed" }), None);
                    cx.emit(AppEvent::JobDone { job, ok, summary });
                }
                BackendEvent::AgentCost(usd) => self.agent_cost += usd,
                BackendEvent::LiveValues { scope, report } => {
                    if self.live.on {
                        self.live.merge(&scope, &report);
                        cx.emit(AppEvent::LiveValues);
                    }
                }
                BackendEvent::LivePending { running, reason } => {
                    self.live.running = running;
                    if reason.is_some() {
                        self.live.stale = reason;
                    }
                }
                BackendEvent::SynthFailed(f) => {
                    self.failures.insert(f.node.clone(), f);
                }
                BackendEvent::Synthesis { node, status } => {
                    if matches!(status, SynthStatus::Accepted | SynthStatus::Restored | SynthStatus::Queued) {
                        self.failures.remove(&node);
                    }
                    match self.synthesis.iter_mut().find(|(n, _)| *n == node) {
                        Some(s) => s.1 = status,
                        None => self.synthesis.push((node, status)),
                    }
                }
                BackendEvent::Chat { role, text } => self.chat.push((role, text)),
                BackendEvent::AuthorEdit { path, base, new_text, label } if self.review => {
                    self.propose(label, vec![(path, base, new_text)], None, None, cx);
                }
                BackendEvent::AuthorEdit { path, base, new_text, label } => {
                    let r = self.project.docs.merge_write(Origin::Agent, label, &path, &base, &new_text);
                    match r {
                        Ok(Some(tx)) => {
                            let change = Change { origin: Origin::Agent, label: tx.label.clone(), ranges: tx.changed_ranges() };
                            self.project.rebuild();
                            self.changed(change, cx);
                        }
                        Ok(None) => {}
                        Err(e) => self.push_log(LogLevel::Error, "agent", e.to_string(), None),
                    }
                }
                BackendEvent::Trace(t) => {
                    if let Some(n) = &t.node {
                        self.executed.insert(n.clone());
                    }
                    self.trace.push((Instant::now(), t));
                }
                BackendEvent::RunFinished { ok } => {
                    self.push_log(if ok { LogLevel::Info } else { LogLevel::Error }, "run", format!("run finished ({})", if ok { "ok" } else { "failed" }), None)
                }
                BackendEvent::TestsStarted => self.tests_running = true,
                BackendEvent::NodeResults(results) => {
                    self.tests_running = false;
                    let failed: Vec<&NodeId> = results.iter().filter(|(_, r)| !r.ok()).map(|(n, _)| n).collect();
                    let tests: usize = results.values().map(|r| r.passed + r.failed).sum();
                    let msg = format!("specs: {tests} test(s) over {} node(s), {} node(s) failing", results.len(), failed.len());
                    self.push_log(if failed.is_empty() { LogLevel::Info } else { LogLevel::Warn }, "test", msg, None);
                    for n in failed {
                        self.push_log(LogLevel::Warn, "test", format!("{n}: {}", results[n].summary()), Some(n.clone()));
                    }
                    self.tests = results;
                    cx.emit(AppEvent::Results);
                }
            }
        }
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // `gpui_kit::*` brings gpui's own `test` macro into scope
    #[::core::prelude::v1::test]
    fn hunks_can_be_kept_or_skipped() {
        let base = "a\nb\nc\nd\ne\nf\ng\nh\ni\nj\n";
        let new = "a\nB\nc\nd\ne\nf\ng\nh\nI\nj\nk";
        let mut f = PendingFile::new("x.wh.md".into(), base.into(), new.into());
        assert_eq!(f.hunks.len(), 2, "{:?}", f.hunks);
        assert_eq!(f.result(), new);
        assert!(f.hunks[0].lines.contains(&('+', "B".to_string())) && f.hunks[0].lines.contains(&(' ', "a".to_string())));
        f.hunks[1].keep = false;
        assert_eq!(f.result(), "a\nB\nc\nd\ne\nf\ng\nh\ni\nj\n");
        f.hunks[0].keep = false;
        assert_eq!(f.result(), base);
    }
}
