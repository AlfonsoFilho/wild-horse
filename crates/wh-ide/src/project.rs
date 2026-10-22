//! The IDE's document state without any UI: the shared `DocStore`, the model
//! rebuilt after every transaction, last-good nodes of blocks that do not
//! parse, the shared selection and the layout sidecar.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;
use std::path::{Path, PathBuf};

use wh_doc::{DocStore, FileEdit, Origin, TextEdit};
use wh_edit::Op;
use wh_graph::LayoutStore;
use wh_model::{Model, Node, NodeId, SourceInput};
use wh_syntax::Severity;

/// What a transaction did, for views that react to changes.
#[derive(Debug, Clone)]
pub struct Change {
    pub origin: Origin,
    pub label: String,
    /// Post-transaction byte ranges written, per file path.
    pub ranges: BTreeMap<String, Vec<Range<usize>>>,
}

pub struct Project {
    pub dir: PathBuf,
    pub name: String,
    pub docs: DocStore,
    /// Paths in display order (relative to `dir`).
    pub files: Vec<String>,
    /// Source directories from `wildhorse.toml` (relative to `dir`).
    pub sources: Vec<String>,
    pub model: Model,
    /// `model` plus the last valid nodes of blocks that currently fail to parse.
    pub display: Model,
    /// Nodes of `display` that are stale copies (greyed, read-only).
    pub ghosts: BTreeSet<NodeId>,
    last_good: BTreeMap<NodeId, Node>,
    pub layout: LayoutStore,
    /// Selected graph node id: a model `NodeId`, a step id (`flow#callee[k]`)
    /// or a flow parameter id (`flow#in:param`).
    pub selection: Option<String>,
}

impl Project {
    pub fn open(dir: &Path) -> anyhow::Result<Project> {
        let dir = dir.canonicalize()?;
        let mut name = dir.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let mut sources = vec!["src".to_string()];
        if let Ok(t) = std::fs::read_to_string(dir.join("wildhorse.toml")) {
            if let Ok(v) = t.parse::<toml::Table>() {
                if let Some(p) = v.get("project").and_then(|p| p.as_table()) {
                    if let Some(n) = p.get("name").and_then(|n| n.as_str()) {
                        name = n.to_string();
                    }
                    if let Some(s) = p.get("sources").and_then(|s| s.as_array()) {
                        sources = s.iter().filter_map(|x| x.as_str().map(String::from)).collect();
                    }
                }
            }
        }
        let mut files = Vec::new();
        for s in &sources {
            let mut found: Vec<PathBuf> = walk(&dir.join(s));
            found.sort();
            for p in found {
                if p.to_string_lossy().ends_with(".wh.md") {
                    files.push(p.strip_prefix(&dir).unwrap_or(&p).to_string_lossy().into_owned());
                }
            }
        }
        if files.is_empty() {
            anyhow::bail!("no .wh.md files under {} ({:?})", dir.display(), sources);
        }
        // the module that holds the app goes last; the others keep path order
        let mut docs = DocStore::new();
        for f in &files {
            docs.open(f.clone(), std::fs::read_to_string(dir.join(f))?);
        }
        let layout = LayoutStore::load(&dir);
        let mut p = Project {
            dir,
            name,
            docs,
            files,
            sources,
            model: Model::default(),
            display: Model::default(),
            ghosts: BTreeSet::new(),
            last_good: BTreeMap::new(),
            layout,
            selection: None,
        };
        p.rebuild();
        Ok(p)
    }

    /// Build a project from in-memory sources (tests).
    #[cfg(test)]
    pub fn from_sources(sources: &[(&str, &str)]) -> Project {
        let mut docs = DocStore::new();
        for (p, t) in sources {
            docs.open(*p, *t);
        }
        let mut p = Project {
            dir: std::env::temp_dir(),
            name: "test".into(),
            docs,
            files: sources.iter().map(|(p, _)| p.to_string()).collect(),
            sources: vec![".".into()],
            model: Model::default(),
            display: Model::default(),
            ghosts: BTreeSet::new(),
            last_good: BTreeMap::new(),
            layout: LayoutStore::default(),
            selection: None,
        };
        p.rebuild();
        p
    }

    pub fn text(&self, path: &str) -> &str {
        self.docs.text(path).unwrap_or("")
    }

    pub fn file_index(&self, path: &str) -> Option<usize> {
        self.model.files.iter().position(|f| f.path == path)
    }

    /// Re-parse and re-check after a transaction.
    pub fn rebuild(&mut self) {
        let inputs: Vec<SourceInput> =
            self.files.iter().map(|f| SourceInput { path: f.clone(), text: self.text(f).to_string() }).collect();
        let model = Model::build(&inputs);
        // which blocks fail to parse, per file
        let mut broken: BTreeMap<usize, Vec<wh_syntax::Span>> = BTreeMap::new();
        for (fi, input) in inputs.iter().enumerate() {
            let parsed = wh_syntax::parse_document(&input.text);
            for d in parsed.diagnostics.iter().filter(|d| d.severity == Severity::Error) {
                if let Some(b) = parsed.doc.blocks.iter().find(|b| b.span.contains(d.span.start)) {
                    broken.entry(fi).or_default().push(b.span);
                }
            }
        }
        let in_broken = |n: &Node| broken.get(&n.file).is_some_and(|bs| bs.iter().any(|b| b.contains(n.span.start)));
        let mut display = model.clone();
        let mut ghosts = BTreeSet::new();
        for (id, old) in &self.last_good {
            if !model.nodes.contains_key(id) && broken.contains_key(&old.file) {
                display.nodes.insert(id.clone(), old.clone());
                ghosts.insert(id.clone());
            }
        }
        // a node parsed from a broken block may be partial: prefer the last good copy
        for (id, n) in &model.nodes {
            if in_broken(n) {
                if let Some(old) = self.last_good.get(id) {
                    display.nodes.insert(id.clone(), Node { span: n.span, name_span: n.name_span, ..old.clone() });
                    ghosts.insert(id.clone());
                }
            }
        }
        let files_ok: BTreeSet<usize> = (0..inputs.len()).filter(|f| !broken.contains_key(f)).collect();
        self.last_good.retain(|id, n| model.nodes.contains_key(id) || !files_ok.contains(&n.file));
        for (id, n) in &model.nodes {
            if !in_broken(n) {
                self.last_good.insert(id.clone(), n.clone());
            }
        }
        self.model = model;
        self.display = display;
        self.ghosts = ghosts;
    }

    pub fn apply_edits(&mut self, origin: Origin, label: &str, edits: Vec<FileEdit>) -> Result<Change, String> {
        let tx = self.docs.apply(origin, label, edits).map_err(|e| e.to_string())?;
        let change = Change { origin, label: tx.label.clone(), ranges: tx.changed_ranges() };
        self.rebuild();
        Ok(change)
    }

    /// Replace `range` of `path` (typing in the document panel). Typing
    /// bursts in one file coalesce into a single undo step.
    pub fn edit_text(&mut self, origin: Origin, path: &str, range: Range<usize>, text: &str) -> Result<Change, String> {
        let edits = vec![FileEdit { path: path.to_string(), edits: vec![TextEdit::replace(range, text)] }];
        if origin != Origin::Text {
            return self.apply_edits(origin, "edit", edits);
        }
        let tx = self.docs.apply_coalescing(origin, "typing", edits, path).map_err(|e| e.to_string())?;
        let change = Change { origin, label: tx.label.clone(), ranges: tx.changed_ranges() };
        self.rebuild();
        Ok(change)
    }

    /// Plan a structured op against the live model and apply it as one transaction.
    pub fn apply_op(&mut self, origin: Origin, op: &Op) -> Result<Change, String> {
        let ghost = match op {
            Op::AddField { owner, .. }
            | Op::RetypeField { owner, .. }
            | Op::RenameField { owner, .. }
            | Op::RemoveField { owner, .. }
            | Op::SetCardinality { owner, .. }
            | Op::AddVariant { owner, .. } => self.ghosts.contains(owner),
            Op::AddClause { node, .. }
            | Op::ReplaceClause { node, .. }
            | Op::RemoveClause { node, .. }
            | Op::EditSignature { node, .. }
            | Op::SetPattern { node, .. }
            | Op::Rename { node, .. }
            | Op::RemoveNode { node } => self.ghosts.contains(node),
            Op::InsertFlowStep { flow, .. }
            | Op::RemoveFlowStep { flow, .. }
            | Op::MoveFlowStep { flow, .. }
            | Op::InsertFlowBinding { flow, .. } => self.ghosts.contains(flow),
            _ => false,
        };
        if ghost {
            return Err("that definition is in a block that does not parse; fix the text first".into());
        }
        let edits = wh_edit::plan(&self.model, op).map_err(|e| e.to_string())?;
        if edits.is_empty() {
            return Err("nothing to change".into());
        }
        if let Op::Rename { node, new_name } = op {
            let new_id = match node.rsplit_once('.') {
                Some((m, _)) => format!("{m}.{new_name}"),
                None => new_name.clone(),
            };
            self.layout.rekey(node, &new_id);
            if self.selection.as_deref() == Some(node.as_str()) {
                self.selection = Some(new_id);
            }
        }
        self.apply_edits(origin, &op.label(), edits)
    }

    pub fn undo(&mut self) -> Option<Change> {
        let tx = self.docs.undo()?;
        // after undo the redo entry's `edits` are in post-undo coordinates
        let mut ranges: BTreeMap<String, Vec<Range<usize>>> = BTreeMap::new();
        for fe in &tx.edits {
            ranges.entry(fe.path.clone()).or_default().extend(fe.edits.iter().map(|e| e.range.start..e.range.end));
        }
        let change = Change { origin: tx.origin, label: format!("undo {}", tx.label), ranges };
        self.rebuild();
        Some(change)
    }

    pub fn redo(&mut self) -> Option<Change> {
        let tx = self.docs.redo()?;
        let change = Change { origin: tx.origin, label: format!("redo {}", tx.label), ranges: tx.changed_ranges() };
        self.rebuild();
        Some(change)
    }

    /// Write dirty files and the layout sidecar.
    pub fn save(&mut self) -> anyhow::Result<Vec<String>> {
        let mut saved = Vec::new();
        for f in self.files.clone() {
            if self.docs.is_dirty(&f) {
                std::fs::write(self.dir.join(&f), self.text(&f))?;
                self.docs.mark_saved(&f);
                saved.push(f);
            }
        }
        self.layout.save(&self.dir)?;
        Ok(saved)
    }

    pub fn is_dirty(&self) -> bool {
        self.files.iter().any(|f| self.docs.is_dirty(f))
    }

    pub fn dirty_files(&self) -> Vec<String> {
        self.files.iter().filter(|f| self.docs.is_dirty(f)).cloned().collect()
    }

    /// Write one file if it has unsaved changes.
    pub fn save_file(&mut self, path: &str) -> anyhow::Result<bool> {
        if !self.docs.is_dirty(path) {
            return Ok(false);
        }
        std::fs::write(self.dir.join(path), self.text(path))?;
        self.docs.mark_saved(path);
        Ok(true)
    }

    /// Is `rel` (relative to `dir`) a `.wh.md` file under one of the sources?
    pub fn is_source(&self, rel: &str) -> bool {
        rel.ends_with(".wh.md")
            && self.sources.iter().any(|s| {
                let s = s.trim_matches('/');
                s.is_empty() || s == "." || rel.starts_with(&format!("{s}/"))
            })
    }

    /// `abs` relative to the project directory, if it is inside it.
    pub fn rel_path(&self, abs: &Path) -> Option<String> {
        let abs = match abs.canonicalize() {
            Ok(p) => p,
            // a file that does not exist yet: canonicalize its folder
            Err(_) => abs.parent()?.canonicalize().ok()?.join(abs.file_name()?),
        };
        Some(abs.strip_prefix(&self.dir).ok()?.to_string_lossy().replace('\\', "/"))
    }

    /// The first source directory (where new files go).
    pub fn source_dir(&self) -> PathBuf {
        let s = self.sources.first().map(|s| s.trim_matches('/')).unwrap_or("src");
        if s.is_empty() || s == "." { self.dir.clone() } else { self.dir.join(s) }
    }

    /// A source file appeared (on disk, or created by the IDE). A path the
    /// store still has open is merged from disk, keeping its undo history.
    pub fn add_file(&mut self, path: &str, text: &str) -> Change {
        if self.docs.text(path).is_some() {
            let _ = self.docs.reload_from_disk(path, text);
        } else {
            self.docs.open(path.to_string(), text.to_string());
        }
        if !self.files.iter().any(|f| f == path) {
            self.files.push(path.to_string());
            self.files.sort();
        }
        self.rebuild();
        Change { origin: Origin::Disk, label: format!("add {path}"), ranges: BTreeMap::new() }
    }

    /// A source file went away: its buffer closes, and with it every undo
    /// step that touched it (`DocStore::close`).
    pub fn remove_file(&mut self, path: &str) -> Option<Change> {
        let i = self.files.iter().position(|f| f == path)?;
        self.files.remove(i);
        self.docs.close(path);
        self.rebuild();
        Some(Change { origin: Origin::Disk, label: format!("remove {path}"), ranges: BTreeMap::new() })
    }

    /// The file changed on disk: merge the change onto unsaved edits.
    pub fn reload_file(&mut self, path: &str) -> Result<Option<Change>, String> {
        let disk = std::fs::read_to_string(self.dir.join(path)).map_err(|e| e.to_string())?;
        let change = match self.docs.reload_from_disk(path, &disk).map_err(|e| e.to_string())? {
            Some(tx) => Some(Change { origin: Origin::Disk, label: tx.label.clone(), ranges: tx.changed_ranges() }),
            None => None,
        };
        if change.is_some() {
            self.rebuild();
        }
        Ok(change)
    }

    /// Throw away unsaved edits of `path` (one undoable step).
    pub fn revert_file(&mut self, path: &str) -> Result<Option<Change>, String> {
        let disk = std::fs::read_to_string(self.dir.join(path)).map_err(|e| e.to_string())?;
        let edits = wh_doc::diff_edits(self.text(path), &disk);
        if edits.is_empty() {
            self.docs.mark_saved(path);
            return Ok(None);
        }
        let change = self.apply_edits(Origin::Disk, &format!("revert {path}"), vec![FileEdit { path: path.to_string(), edits }])?;
        self.docs.mark_saved(path);
        Ok(Some(change))
    }

    /// Write `from`'s buffer to `to`. Inside the sources the copy becomes a
    /// new module named after the file, and `from` goes back to its saved
    /// text (its unsaved edits moved to the copy). Returns the copy's path
    /// when it joined the project.
    pub fn save_as(&mut self, from: &str, to: &Path) -> anyhow::Result<Option<String>> {
        let rel = self.rel_path(to).filter(|r| self.is_source(r));
        let mut text = self.text(from).to_string();
        if rel.is_some() {
            let stem = to.file_name().map(|f| f.to_string_lossy().trim_end_matches(".wh.md").to_string()).unwrap_or_default();
            text = set_module(&text, &crate::config::snake_ident(&stem));
        }
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(to, &text)?;
        let Some(rel) = rel else { return Ok(None) };
        self.add_file(&rel, &text);
        if rel != from {
            self.revert_file(from).map_err(anyhow::Error::msg)?;
        }
        Ok(Some(rel))
    }

    /// The model node a selection refers to (a step's or param's flow).
    pub fn selected_node(&self) -> Option<&Node> {
        let sel = self.selection.as_deref()?;
        let id = sel.split('#').next().unwrap_or(sel);
        self.display.nodes.get(id)
    }

    /// Where to reveal a selection in the document: (file path, byte span).
    pub fn reveal_span(&self, sel: &str) -> Option<(String, wh_syntax::Span)> {
        let (base, rest) = match sel.split_once('#') {
            Some((b, r)) => (b, Some(r)),
            None => (sel, None),
        };
        let n = self.display.nodes.get(base)?;
        let path = self.display.files.get(n.file)?.path.clone();
        if rest.is_some() {
            let steps = match &n.kind {
                wh_model::NodeKind::Flow(f) => Some(&f.steps),
                wh_model::NodeKind::App(a) => a.entry.as_ref().map(|e| &e.steps),
                _ => None,
            };
            if let Some(s) = steps.and_then(|ss| ss.iter().find(|s| s.id == sel)) {
                return Some((path, s.span));
            }
        }
        Some((path, n.span))
    }

    /// Selection for a cursor at `offset` in `path`: the innermost definition,
    /// or the flow step under the cursor.
    pub fn selection_at(&self, path: &str, offset: usize) -> Option<String> {
        let fi = self.file_index(path)?;
        let n = self.model.node_at(fi, offset as u32)?;
        let steps = match &n.kind {
            wh_model::NodeKind::Flow(f) => Some(&f.steps),
            wh_model::NodeKind::App(a) => a.entry.as_ref().map(|e| &e.steps),
            _ => None,
        };
        if let Some(s) = steps.and_then(|ss| ss.iter().find(|s| s.span.contains(offset as u32))) {
            return Some(s.id.clone());
        }
        Some(n.id.clone())
    }
}

/// Point a document's front matter at `module` (adding the line if missing).
fn set_module(text: &str, module: &str) -> String {
    if let Some(rest) = text.strip_prefix("---\n") {
        if let Some(end) = rest.find("\n---") {
            let fm = &rest[..end];
            let lines: Vec<String> = fm
                .lines()
                .map(|l| if l.trim_start().starts_with("module:") { format!("module: {module}") } else { l.to_string() })
                .collect();
            let mut fm = lines.join("\n");
            if !fm.lines().any(|l| l.starts_with("module:")) {
                fm = format!("module: {module}\n{fm}");
            }
            return format!("---\n{}{}", fm.trim_end_matches('\n'), &rest[end..]);
        }
    }
    format!("---\nmodule: {module}\n---\n{text}")
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                out.extend(walk(&p));
            } else {
                out.push(p);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: &str = "# M\n\n## A\n\n```wh\ndef f(x: int) -> int\n```\n\n## B\n\n```wh\ndef g(x: int) -> int\n\nflow h(x: int) -> int:\n    x\n    |> f\n```\n";

    #[test]
    fn broken_block_keeps_last_good_nodes() {
        let mut p = Project::from_sources(&[("m.wh.md", SRC)]);
        assert!(p.model.nodes.contains_key("m.h"));
        // break the second block
        let at = SRC.find("flow h").unwrap();
        p.edit_text(Origin::Text, "m.wh.md", at..at + 4, "flo(").unwrap();
        assert!(p.ghosts.contains("m.h"), "ghosts: {:?}", p.ghosts);
        assert!(p.display.nodes.contains_key("m.h"));
        assert!(p.display.nodes.contains_key("m.f"));
        assert!(!p.ghosts.contains("m.f"));
        // ops on a ghost are refused
        let r = p.apply_op(Origin::Graph, &Op::Rename { node: "m.h".into(), new_name: "k".into() });
        assert!(r.is_err());
        // fixing the text revives it
        p.undo().unwrap();
        assert!(p.ghosts.is_empty());
        assert!(p.model.nodes.contains_key("m.h"));
    }

    #[test]
    fn save_writes_dirty_files_and_layout() {
        let dir = std::env::temp_dir().join(format!("wh-ide-save-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("wildhorse.toml"), "[project]\nname = \"t\"\nsources = [\"src\"]\n").unwrap();
        std::fs::write(dir.join("src/m.wh.md"), SRC).unwrap();
        let mut p = Project::open(&dir).unwrap();
        assert_eq!(p.name, "t");
        assert_eq!(p.files, ["src/m.wh.md"]);
        p.apply_op(Origin::Graph, &Op::Rename { node: "m.f".into(), new_name: "f2".into() }).unwrap();
        p.layout.set("flow", "m.f2", wh_graph::Point::new(1.0, 2.0));
        assert!(p.is_dirty());
        assert_eq!(p.save().unwrap(), ["src/m.wh.md"]);
        assert!(!p.is_dirty());
        assert!(std::fs::read_to_string(dir.join("src/m.wh.md")).unwrap().contains("|> f2"));
        assert!(wh_graph::LayoutStore::load(&dir).positions("flow").contains_key("m.f2"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn files_come_and_go() {
        let dir = std::env::temp_dir().join(format!("wh-ide-files-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("wildhorse.toml"), "[project]\nname = \"t\"\nsources = [\"src\"]\n").unwrap();
        std::fs::write(dir.join("src/m.wh.md"), SRC).unwrap();
        let mut p = Project::open(&dir).unwrap();
        assert!(p.is_source("src/x.wh.md") && !p.is_source("x.wh.md") && !p.is_source("src/x.md"));
        assert_eq!(p.rel_path(&dir.join("src/new.wh.md")).as_deref(), Some("src/new.wh.md"));

        // an external edit merges onto an unsaved one
        p.apply_op(Origin::Graph, &Op::Rename { node: "m.g".into(), new_name: "g2".into() }).unwrap();
        std::fs::write(dir.join("src/m.wh.md"), SRC.replace("# M", "# Module M")).unwrap();
        let c = p.reload_file("src/m.wh.md").unwrap().expect("a change");
        assert_eq!(c.origin, Origin::Disk);
        assert!(p.text("src/m.wh.md").starts_with("# Module M") && p.text("src/m.wh.md").contains("def g2"));
        assert!(p.reload_file("src/m.wh.md").unwrap().is_none(), "same disk text again: no change");
        // revert drops the unsaved rename, as one undo step
        p.revert_file("src/m.wh.md").unwrap().unwrap();
        assert!(!p.is_dirty() && p.text("src/m.wh.md").contains("def g("));
        p.undo().unwrap();
        assert!(p.text("src/m.wh.md").contains("def g2"));

        // new and deleted files
        std::fs::write(dir.join("src/n.wh.md"), crate::config::new_file_text("n")).unwrap();
        p.add_file("src/n.wh.md", &crate::config::new_file_text("n"));
        assert_eq!(p.files, ["src/m.wh.md", "src/n.wh.md"]);
        assert!(p.model.files.iter().any(|f| f.module == "n"));
        p.remove_file("src/n.wh.md").unwrap();
        assert_eq!(p.files, ["src/m.wh.md"]);
        assert!(p.docs.text("src/n.wh.md").is_none(), "the buffer of a deleted file is closed");
        p.undo().unwrap(); // touches only m.wh.md; must not panic
        p.add_file("src/n.wh.md", &crate::config::new_file_text("n"));
        assert_eq!(p.files.len(), 2);

        // save as: a new module, the original back to its saved text
        let to = dir.join("src/copy.wh.md");
        let rel = p.save_as("src/m.wh.md", &to).unwrap();
        assert_eq!(rel.as_deref(), Some("src/copy.wh.md"));
        let copy = std::fs::read_to_string(&to).unwrap();
        assert!(copy.starts_with("---\nmodule: copy\n---\n"), "{copy}");
        assert!(copy.contains("def g2"));
        assert!(p.text("src/m.wh.md").contains("def g("));
        assert!(p.model.files.iter().any(|f| f.module == "copy"));
        assert_eq!(set_module("---\nmodule: a\ntitle: x\n---\n# A\n", "b"), "---\nmodule: b\ntitle: x\n---\n# A\n");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn ops_undo_and_selection_at() {
        let mut p = Project::from_sources(&[("m.wh.md", SRC)]);
        let c = p.apply_op(Origin::Graph, &Op::InsertFlowStep { flow: "m.h".into(), stmt: 0, position: 1, callee: "g".into() }).unwrap();
        assert!(p.text("m.wh.md").contains("|> f\n    |> g"));
        assert_eq!(c.origin, Origin::Graph);
        assert!(!c.ranges["m.wh.md"].is_empty());
        let off = p.text("m.wh.md").find("|> g").unwrap() + 3;
        let sel = p.selection_at("m.wh.md", off).unwrap();
        assert!(sel.starts_with("m.h#"), "{sel}");
        assert_eq!(p.reveal_span(&sel).unwrap().0, "m.wh.md");
        p.undo().unwrap();
        assert!(!p.text("m.wh.md").contains("|> g"));
        p.redo().unwrap();
        assert!(p.text("m.wh.md").contains("|> g"));
    }
}
