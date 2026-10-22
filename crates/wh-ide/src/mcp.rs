//! The IDE side of the `wh` MCP bridge: a [`wh_mcp::Backend`] over the open
//! project's `DocStore` and model. The author agent's `wh_*` tool calls
//! arrive on bridge threads; each one hops to the UI thread (a closure the
//! app state runs on its poll timer) and waits for the answer. Applied ops
//! are `Origin::Agent` transactions: live in every view, flashed, one undo
//! step, never written to disk. In review mode they wait in the Agent panel
//! for Accept / Reject before the tool call returns.

use std::sync::Arc;
use std::sync::mpsc::{Sender, channel};
use std::time::Duration;

use gpui_kit::Context;
use wh_doc::FileEdit;
use wh_edit::Op;
use wh_mcp::view::{self, ApplyResult, CheckResult, NodeView, Outline, ProseEdit, SpecRun};
use wh_mcp::{ToolError, ToolResult};
use wh_model::{Model, Node};

use crate::state::AppState;

/// Work for the UI thread.
pub type UiCall = Box<dyn FnOnce(&mut AppState, &mut Context<AppState>) + Send>;

/// How long a tool call may wait for the UI (review decisions included).
const UI_TIMEOUT: Duration = Duration::from_secs(30 * 60);

pub struct IdeBridge {
    ui: Sender<UiCall>,
    project: Arc<wh_project::Project>,
}

/// The executable that serves MCP in bridge mode for the author agent:
/// `$WH_MCP_BIN`, else `wh-mcp` or `wh mcp` next to the IDE binary.
pub fn server_program() -> Option<(std::path::PathBuf, Vec<&'static str>)> {
    if let Some(p) = std::env::var_os("WH_MCP_BIN") {
        return Some((p.into(), vec![]));
    }
    let dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    if dir.join("wh-mcp").is_file() {
        return Some((dir.join("wh-mcp"), vec![]));
    }
    dir.join("wh").is_file().then(|| (dir.join("wh"), vec!["mcp"]))
}

enum Applied {
    Done(ApplyResult),
    /// Held for review: the decision, and the model before the edit.
    Review(std::sync::mpsc::Receiver<Result<Vec<view::EditedRange>, String>>, Box<Model>, Vec<String>),
}

impl IdeBridge {
    pub fn new(ui: Sender<UiCall>, project: Arc<wh_project::Project>) -> Self {
        IdeBridge { ui, project }
    }

    fn on_ui<R: Send + 'static>(&self, f: impl FnOnce(&mut AppState, &mut Context<AppState>) -> R + Send + 'static) -> ToolResult<R> {
        let (tx, rx) = channel();
        self.ui
            .send(Box::new(move |s, cx| {
                let _ = tx.send(f(s, cx));
            }))
            .map_err(|_| ToolError::new("the project is no longer open in the IDE"))?;
        rx.recv_timeout(UI_TIMEOUT).map_err(|_| ToolError::new("the IDE did not answer"))
    }

    /// Plan against the live model and apply (or hold for review) as one transaction.
    fn apply(&self, label: String, applied: Vec<String>, plan: impl FnOnce(&Model) -> ToolResult<Vec<FileEdit>> + Send + 'static) -> ToolResult<ApplyResult> {
        let step = self.on_ui(move |s, cx| -> ToolResult<Applied> {
            let edits = plan(&s.project.model)?;
            let before = s.project.model.clone();
            if edits.is_empty() {
                return Ok(Applied::Done(view::apply_result(&before, &before, applied, vec![])));
            }
            if s.review {
                let mut files = Vec::new();
                for fe in &edits {
                    let base = s.project.text(&fe.path).to_string();
                    let new = wh_doc::apply_edits_to(&base, &fe.edits);
                    files.push((fe.path.clone(), base, new));
                }
                let (tx, rx) = channel();
                s.propose(label, files, Some(edits), Some(tx), cx);
                return Ok(Applied::Review(rx, Box::new(before), applied));
            }
            let (edited, _) = view::apply_to_store(&mut s.project.docs, &label, edits)?;
            let ranges = s.project.docs.history().last().map(|t| t.changed_ranges()).unwrap_or_default();
            s.agent_edited(label, ranges, cx);
            Ok(Applied::Done(view::apply_result(&before, &s.project.model, applied, edited)))
        })??;
        match step {
            Applied::Done(r) => Ok(r),
            Applied::Review(rx, before, applied) => {
                let edited = rx.recv_timeout(UI_TIMEOUT).map_err(|_| ToolError::new("the edit was not reviewed (the project closed or timed out)"))?;
                let edited = edited.map_err(ToolError::new)?;
                self.on_ui(move |s, _| view::apply_result(&before, &s.project.model, applied, edited))
            }
        }
    }
}

/// Synthesis state as the agent should see it, from what the IDE last heard.
fn hole_state(s: &AppState, n: &Node) -> Option<String> {
    if !n.is_hole() {
        return None;
    }
    Some(s.synthesis.iter().find(|(id, _)| *id == n.id).map_or("unknown".into(), |(_, st)| st.label()))
}

impl wh_mcp::Backend for IdeBridge {
    fn outline(&self) -> ToolResult<Outline> {
        self.on_ui(|s, _| view::outline(&s.project.model, &|n| hole_state(s, n)))
    }

    fn get_node(&self, id: &str) -> ToolResult<NodeView> {
        let id = id.to_string();
        self.on_ui(move |s, _| view::node_view(&s.project.model, &id, &|n| hole_state(s, n)))?
    }

    fn apply_ops(&self, ops: &[Op]) -> ToolResult<ApplyResult> {
        let ops = ops.to_vec();
        let label = view::ops_label(&ops);
        let applied = ops.iter().map(Op::label).collect();
        self.apply(label, applied, move |m| view::plan_ops(m, &ops))
    }

    fn edit_prose(&self, edit: &ProseEdit) -> ToolResult<ApplyResult> {
        let edit = edit.clone();
        self.apply("author: prose".into(), vec!["edit prose".into()], move |m| view::plan_prose(m, &edit))
    }

    fn check(&self, cargo: bool) -> ToolResult<CheckResult> {
        let (mut result, model) = self.on_ui(|s, _| (view::check_result(&s.project.model), s.project.model.clone()))?;
        if cargo && !model.has_errors() {
            // off the UI thread, on a snapshot of the live buffers
            result.cargo = Some(match wh_project::live::check(&self.project, &model) {
                Ok((_, r)) => view::CargoReport { success: r.success, diagnostics: view::cargo_diags(&r), error: None },
                Err(e) => view::CargoReport { success: false, diagnostics: vec![], error: Some(format!("{e:#}")) },
            });
        }
        Ok(result)
    }

    fn run_specs(&self, id: Option<&str>) -> ToolResult<SpecRun> {
        let model = self.on_ui(|s, _| s.project.model.clone())?;
        let r = wh_project::live::test(&self.project, &model, id)?;
        Ok(view::spec_run(&r))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bridge's planning and result plumbing, without a UI thread: the
    /// same `wh_mcp::view` calls `apply` makes, on a project's store.
    #[test]
    fn planned_ops_apply_as_one_agent_transaction() {
        let mut p = crate::project::Project::from_sources(&[(
            "m.wh.md",
            "# M\n\n```wh\ndef f(x: int) -> int\n\nflow h(x: int) -> int:\n    x\n    |> f\n```\n",
        )]);
        let ops = vec![Op::Rename { node: "m.f".into(), new_name: "g".into() }];
        let edits = view::plan_ops(&p.model, &ops).unwrap();
        let before = p.model.clone();
        let (edited, paths) = view::apply_to_store(&mut p.docs, &view::ops_label(&ops), edits).unwrap();
        p.rebuild();
        assert_eq!(paths, ["m.wh.md"]);
        assert!(p.text("m.wh.md").contains("def g(") && p.text("m.wh.md").contains("|> g"));
        assert_eq!(p.docs.history().last().unwrap().origin, wh_doc::Origin::Agent);
        let r = view::apply_result(&before, &p.model, ops.iter().map(Op::label).collect(), edited);
        assert!(r.introduced.is_empty(), "{:?}", r.introduced);
        // one undo step brings it all back
        p.undo().unwrap();
        assert!(p.text("m.wh.md").contains("def f("));
    }
}
