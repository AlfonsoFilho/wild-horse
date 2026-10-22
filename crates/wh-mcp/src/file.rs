//! File mode: the CLI's backend. It loads a project, keeps its sources in a
//! `DocStore` and writes the edited files after every successful apply.
//! Changes made on disk by someone else are merged in before each call.

use std::path::Path;
use std::sync::Mutex;

use wh_doc::DocStore;
use wh_model::{Model, Node};
use wh_project::Project;

use crate::view::{self, ApplyResult, CheckResult, NodeView, Outline, ProseEdit, SpecRun};
use crate::{Backend, Op, ToolError, ToolResult};

pub struct FileBackend {
    project: Project,
    state: Mutex<State>,
}

struct State {
    store: DocStore,
    model: Model,
    /// Store version the model was built from.
    model_version: u64,
}

impl FileBackend {
    pub fn open(root: impl AsRef<Path>) -> anyhow::Result<Self> {
        let project = Project::load(root)?;
        let mut store = DocStore::new();
        for s in project.read_sources()? {
            store.open(s.path, s.text);
        }
        let model = view::store_model(&store);
        let model_version = store.version();
        Ok(Self { project, state: Mutex::new(State { store, model, model_version }) })
    }

    pub fn project(&self) -> &Project {
        &self.project
    }

    /// The current text of a source (relative path), as the backend sees it.
    pub fn text(&self, path: &str) -> Option<String> {
        self.state.lock().unwrap().store.text(path).map(String::from)
    }

    /// Lock the state after merging disk changes and refreshing the model.
    fn synced(&self) -> ToolResult<std::sync::MutexGuard<'_, State>> {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let on_disk = self.project.source_paths();
        let gone: Vec<String> = st.store.paths().filter(|p| !on_disk.contains(&p.to_string())).map(String::from).collect();
        for p in gone {
            st.store.close(&p);
        }
        for p in on_disk {
            let Ok(disk) = std::fs::read_to_string(self.project.root.join(&p)) else { continue };
            if st.store.text(&p).is_none() {
                st.store.open(p, disk);
            } else {
                st.store.reload_from_disk(&p, &disk)?;
            }
        }
        if st.store.version() != st.model_version {
            st.model = view::store_model(&st.store);
            st.model_version = st.store.version();
        }
        Ok(st)
    }

    fn hole_state(&self, n: &Node) -> Option<String> {
        if !n.is_hole() {
            return None;
        }
        let path = self.project.impl_dir().join(n.module.replace('.', "/")).join(format!("{}.rs", n.name));
        let state = match std::fs::read_to_string(path) {
            Err(_) => "missing",
            Ok(t) if t.contains("todo!(\"synthesize ") => "missing",
            Ok(t) if t.lines().take(3).any(|l| l.trim() == "// wh:pinned") => "pinned",
            Ok(t) if t.starts_with("// wh:key=") => "synthesized",
            Ok(_) => "manual",
        };
        Some(state.into())
    }

    fn commit(&self, st: &mut State, label: String, applied: Vec<String>, edits: Vec<wh_doc::FileEdit>) -> ToolResult<ApplyResult> {
        let (edited, paths) = view::apply_to_store(&mut st.store, &label, edits)?;
        for p in &paths {
            let text = st.store.text(p).unwrap_or_default().to_string();
            std::fs::write(self.project.root.join(p), text).map_err(|e| ToolError::new(format!("writing {p}: {e}")))?;
            st.store.mark_saved(p);
        }
        let after = view::store_model(&st.store);
        let result = view::apply_result(&st.model, &after, applied, edited);
        st.model = after;
        st.model_version = st.store.version();
        Ok(result)
    }
}

impl Backend for FileBackend {
    fn outline(&self) -> ToolResult<Outline> {
        let st = self.synced()?;
        Ok(view::outline(&st.model, &|n| self.hole_state(n)))
    }

    fn get_node(&self, id: &str) -> ToolResult<NodeView> {
        let st = self.synced()?;
        view::node_view(&st.model, id, &|n| self.hole_state(n))
    }

    fn apply_ops(&self, ops: &[Op]) -> ToolResult<ApplyResult> {
        let mut st = self.synced()?;
        let edits = view::plan_ops(&st.model, ops)?;
        let applied = ops.iter().map(Op::label).collect();
        self.commit(&mut st, view::ops_label(ops), applied, edits)
    }

    fn edit_prose(&self, edit: &ProseEdit) -> ToolResult<ApplyResult> {
        let mut st = self.synced()?;
        let edits = view::plan_prose(&st.model, edit)?;
        let label = format!("author: prose of {}", edit.target);
        self.commit(&mut st, label.clone(), vec![label], edits)
    }

    fn check(&self, cargo: bool) -> ToolResult<CheckResult> {
        let st = self.synced()?;
        let mut r = view::check_result(&st.model);
        if cargo {
            let model = st.model.clone();
            drop(st);
            r.cargo = Some(match wh_project::live::check(&self.project, &model) {
                Ok((_, b)) => view::CargoReport { success: b.success, diagnostics: view::cargo_diags(&b), error: None },
                Err(e) => view::CargoReport { success: false, diagnostics: Vec::new(), error: Some(format!("{e:#}")) },
            });
        }
        Ok(r)
    }

    fn run_specs(&self, id: Option<&str>) -> ToolResult<SpecRun> {
        let st = self.synced()?;
        if let Some(id) = id {
            view::node_view(&st.model, id, &|_| None)?;
        }
        let model = st.model.clone();
        drop(st);
        let r = wh_project::live::test(&self.project, &model, id)?;
        Ok(view::spec_run(&r))
    }
}
