//! Model-level pieces of the tools, shared by every [`Backend`](crate::Backend):
//! what the agent sees (outline, node views, diagnostics) and how its edits
//! are planned (ops and prose) and applied to a `DocStore`.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use wh_doc::{DocStore, FileEdit, Origin, TextEdit};
use wh_edit::Op;
use wh_model::{Model, Node, NodeKind, SigInfo, SourceInput};
use wh_syntax::lexer::{TokenKind, lex};
use wh_syntax::{Severity, Span, parse_document};

use crate::{ToolError, ToolResult};

/// Hole state of a node as the backend knows it (`missing`, `synthesized`,
/// `stale`, `pinned`, …), or `None` when it is not a hole or unknown.
pub type HoleStateFn<'a> = &'a dyn Fn(&Node) -> Option<String>;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiagCounts {
    pub errors: usize,
    pub warnings: usize,
    pub lints: usize,
}

impl DiagCounts {
    fn add(&mut self, s: Severity) {
        match s {
            Severity::Error => self.errors += 1,
            Severity::Warning => self.warnings += 1,
            Severity::Lint => self.lints += 1,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiagView {
    pub path: String,
    /// 1-based.
    pub line: usize,
    pub column: usize,
    pub severity: String,
    pub message: String,
    /// The definition the diagnostic is in, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Outline {
    pub files: Vec<FileOutline>,
    pub diagnostics: DiagCounts,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileOutline {
    pub path: String,
    pub module: String,
    /// Definitions before the first heading.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub nodes: Vec<NodeSummary>,
    /// `index` is what ops take as `section`.
    pub sections: Vec<SectionOutline>,
    pub diagnostics: DiagCounts,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SectionOutline {
    pub index: usize,
    pub level: u8,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<usize>,
    pub nodes: Vec<NodeSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeSummary {
    pub id: String,
    pub kind: String,
    /// The definition's header line.
    pub signature: String,
    pub line: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hole: Option<String>,
    #[serde(skip_serializing_if = "is_zero")]
    pub errors: usize,
    #[serde(skip_serializing_if = "is_zero")]
    pub warnings: usize,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeView {
    pub id: String,
    pub kind: String,
    pub module: String,
    pub path: String,
    pub line: usize,
    pub end_line: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub section: Option<SectionRef>,
    /// The prose paragraph(s) explaining the definition.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prose: Option<String>,
    pub source: String,
    pub signature: String,
    /// Resolved parameter and return types of a def or flow.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved: Option<ResolvedSig>,
    /// Kind-specific facts: fields, variants, methods, flow steps, …
    #[serde(skip_serializing_if = "Value::is_null")]
    pub details: Value,
    /// Clauses of a def, by the index `replace_clause`/`remove_clause` take.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub clauses: Vec<ClauseView>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub patterns: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub derives: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hole: Option<String>,
    pub diagnostics: Vec<DiagView>,
    /// Definitions this one refers to.
    pub uses: Vec<String>,
    /// Definitions that refer to this one.
    pub used_by: Vec<String>,
    /// `[[id]]` references in prose, as `path:line`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub mentioned_in: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SectionRef {
    pub index: usize,
    pub title: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedSig {
    pub params: Vec<ParamView>,
    pub returns: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raises: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub uses: Vec<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub is_async: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParamView {
    pub name: String,
    pub mode: String,
    pub ty: String,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub has_default: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClauseView {
    pub index: usize,
    pub kind: String,
    pub text: String,
}

/// A region the edit wrote, in the text after the edit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditedRange {
    pub path: String,
    /// 1-based, inclusive.
    pub start_line: usize,
    pub end_line: usize,
    /// The edited lines as they read now (empty for a pure deletion).
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApplyResult {
    /// Labels of the applied ops (one undo step in the IDE).
    pub applied: Vec<String>,
    pub edited: Vec<EditedRange>,
    pub counts: DiagCounts,
    /// Errors and warnings that were not there before the edit.
    pub introduced: Vec<DiagView>,
    /// All errors and warnings after the edit.
    pub diagnostics: Vec<DiagView>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckResult {
    pub counts: DiagCounts,
    pub diagnostics: Vec<DiagView>,
    /// `cargo check` of the generated crate, when asked for.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cargo: Option<CargoReport>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CargoReport {
    pub success: bool,
    pub diagnostics: Vec<CargoDiag>,
    /// Set when cargo could not run at all.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CargoDiag {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    pub level: String,
    pub message: String,
    pub rendered: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpecRun {
    pub success: bool,
    pub passed: usize,
    pub failed: usize,
    pub tests: Vec<TestView>,
    /// Build errors that kept the tests from running.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub build_errors: Vec<CargoDiag>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestView {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    pub passed: bool,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub output: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProseMode {
    /// Replace the prose (of a definition, or a section's introduction).
    #[default]
    Replace,
    /// Add a paragraph after the existing prose (for a section or file: at its end).
    Append,
    /// Add a paragraph before the existing prose.
    Prepend,
}

/// `wh_edit_prose` arguments.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProseEdit {
    /// A node id (`module.name`): the prose right before its block;
    /// `<file or module>#<section index or title>`: a section's introduction;
    /// `<file or module>`: the file's introduction.
    pub target: String,
    /// Markdown paragraphs; no headings and no fenced code.
    pub text: String,
    #[serde(default)]
    pub mode: ProseMode,
    /// With `append` on a section or file: start a new subsection with this title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub heading: Option<String>,
}

// ---------------------------------------------------------------------------
// reading the model

fn line_of(text: &str, offset: u32) -> usize {
    wh_syntax::span::line_col(text, offset).0 + 1
}

fn severity(s: Severity) -> &'static str {
    match s {
        Severity::Error => "error",
        Severity::Warning => "warning",
        Severity::Lint => "lint",
    }
}

/// Every model diagnostic, in file order.
pub fn diagnostics(model: &Model) -> Vec<DiagView> {
    model
        .diagnostics
        .iter()
        .map(|d| {
            let f = &model.files[d.file];
            let (line, col) = wh_syntax::span::line_col(&f.text, d.diag.span.start);
            DiagView {
                path: f.path.clone(),
                line: line + 1,
                column: col + 1,
                severity: severity(d.diag.severity).into(),
                message: d.diag.message.clone(),
                node: model.node_at(d.file, d.diag.span.start).map(|n| n.id.clone()),
            }
        })
        .collect()
}

pub fn counts(model: &Model) -> DiagCounts {
    let mut c = DiagCounts::default();
    for d in &model.diagnostics {
        c.add(d.diag.severity);
    }
    c
}

/// Model diagnostics as a `wh_check` result (without cargo).
pub fn check_result(model: &Model) -> CheckResult {
    CheckResult { counts: counts(model), diagnostics: diagnostics(model), cargo: None }
}

/// The header line of a definition (`def f(x: int) -> int`, `struct User`).
fn header(model: &Model, n: &Node) -> String {
    let text = &model.files[n.file].text;
    let src = &text[n.span.range()];
    let line = src.lines().find(|l| !l.trim_start().starts_with('@')).unwrap_or("").trim();
    line.strip_suffix(':').unwrap_or(line).trim_end().to_string()
}

pub fn outline(model: &Model, hole: HoleStateFn) -> Outline {
    let diags = diagnostics(model);
    let mut per_node: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
    for d in &diags {
        if let Some(n) = &d.node {
            let e = per_node.entry(n.as_str()).or_default();
            match d.severity.as_str() {
                "error" => e.0 += 1,
                "warning" => e.1 += 1,
                _ => {}
            }
        }
    }
    let mut files: Vec<FileOutline> = model
        .files
        .iter()
        .map(|f| FileOutline {
            path: f.path.clone(),
            module: f.module.clone(),
            nodes: Vec::new(),
            sections: f
                .doc
                .sections
                .iter()
                .enumerate()
                .map(|(i, s)| SectionOutline { index: i, level: s.level, title: s.title.clone(), parent: s.parent, nodes: Vec::new() })
                .collect(),
            diagnostics: DiagCounts::default(),
        })
        .collect();
    for d in &model.diagnostics {
        files[d.file].diagnostics.add(d.diag.severity);
    }
    let mut nodes: Vec<&Node> = model.nodes.values().collect();
    nodes.sort_by_key(|n| (n.file, n.span.start));
    for n in nodes {
        let (errors, warnings) = per_node.get(n.id.as_str()).copied().unwrap_or_default();
        let summary = NodeSummary {
            id: n.id.clone(),
            kind: n.kind.label().into(),
            signature: header(model, n),
            line: line_of(&model.files[n.file].text, n.span.start),
            hole: hole(n),
            errors,
            warnings,
        };
        let f = &mut files[n.file];
        match n.section.and_then(|s| f.sections.get_mut(s)) {
            Some(s) => s.nodes.push(summary),
            None => f.nodes.push(summary),
        }
    }
    Outline { files, diagnostics: counts(model) }
}

fn sig_view(sig: &SigInfo) -> ResolvedSig {
    ResolvedSig {
        params: sig
            .params
            .iter()
            .map(|p| ParamView {
                name: p.name.clone(),
                mode: format!("{:?}", p.mode).to_lowercase(),
                ty: p.ty.to_string(),
                has_default: p.has_default,
            })
            .collect(),
        returns: sig.ret.to_string(),
        raises: sig.raises.as_ref().map(|t| t.to_string()),
        uses: sig.uses.clone(),
        is_async: sig.is_async,
    }
}

fn details(n: &Node) -> Value {
    let fields = |fs: &[wh_model::FieldInfo]| -> Vec<Value> {
        fs.iter()
            .map(|f| json!({ "name": f.name, "ty": f.ty.to_string(), "key": f.is_key, "sensitive": f.is_sensitive, "has_default": f.has_default }))
            .collect()
    };
    let steps = |f: &wh_model::FlowInfo| -> Vec<Value> {
        f.steps
            .iter()
            .map(|s| json!({ "id": s.id, "label": s.label, "callee": s.callee, "output": s.output.to_string(), "raises": s.raises.as_ref().map(|t| t.to_string()) }))
            .collect()
    };
    match &n.kind {
        NodeKind::Alias(t) => json!({ "alias_of": t.to_string() }),
        NodeKind::Refined { base, generics } => json!({ "base": base.to_string(), "generics": generics }),
        NodeKind::Struct(s) => json!({ "generics": s.generics, "fields": fields(&s.fields), "has_invariants": s.has_invariants }),
        NodeKind::Enum(e) => json!({
            "generics": e.generics,
            "variants": e.variants.iter().map(|v| json!({
                "name": v.name,
                "items": v.items.iter().map(|i| json!({ "name": i.name, "ty": i.ty.to_string(), "from": i.is_from })).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
        }),
        NodeKind::Capability(c) => json!({
            "methods": c.methods.iter().map(|m| json!({ "name": m.name, "sig": sig_view(&m.sig) })).collect::<Vec<_>>(),
        }),
        NodeKind::Adapter(a) => json!({ "implements": a.implements, "fields": fields(&a.fields) }),
        NodeKind::Def(d) => json!({
            "extern": d.extern_path,
            "raises_when": d.raises_when.iter().map(|r| json!({ "error": r.error.to_string(), "variant": r.variant })).collect::<Vec<_>>(),
        }),
        NodeKind::Flow(f) => json!({ "steps": steps(f), "result_step": f.result }),
        NodeKind::Machine(m) => json!({
            "event": m.event.to_string(),
            "effect": m.effect.to_string(),
            "states": m.states.variants.iter().map(|v| v.name.clone()).collect::<Vec<_>>(),
            "initial": m.initial,
        }),
        NodeKind::App(a) => json!({
            "targets": a.targets,
            "provides": a.provides.iter().map(|p| json!({ "capability": p.capability, "adapter": p.adapter })).collect::<Vec<_>>(),
            "entry_steps": a.entry.as_ref().map(steps),
        }),
    }
}

/// Definitions referred to by each node's source (by identifier resolution).
pub fn references(model: &Model) -> BTreeMap<String, BTreeSet<String>> {
    let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (id, n) in &model.nodes {
        let text = &model.files[n.file].text;
        let (toks, _) = lex(&text[n.span.range()], n.span.start);
        let sig: Vec<_> = toks.iter().filter(|t| !t.kind.is_trivia()).collect();
        let set = out.entry(id.clone()).or_default();
        for (i, t) in sig.iter().enumerate() {
            if t.kind != TokenKind::Ident || t.span == n.name_span {
                continue;
            }
            let name = &text[t.span.range()];
            let after_dot = i > 0 && sig[i - 1].kind == TokenKind::Dot;
            let target = if after_dot {
                match i.checked_sub(2).map(|j| sig[j]) {
                    Some(owner) if owner.kind == TokenKind::Ident => {
                        model.resolve_path(&n.module, &format!("{}.{name}", &text[owner.span.range()]))
                    }
                    _ => None,
                }
            } else {
                model.resolve_name(&n.module, name).cloned()
            };
            if let Some(target) = target.filter(|t| t != id && model.nodes.contains_key(t)) {
                set.insert(target);
            }
        }
    }
    out
}

pub fn node_view(model: &Model, id: &str, hole: HoleStateFn) -> ToolResult<NodeView> {
    let n = model.nodes.get(id).ok_or_else(|| unknown_node(model, id))?;
    let f = &model.files[n.file];
    let refs = references(model);
    let clauses = n
        .def()
        .map(|d| {
            d.clauses
                .iter()
                .enumerate()
                .map(|(i, c)| {
                    let text = f.text[c.span.range()].to_string();
                    let kind = text.split(|c: char| !(c.is_alphanumeric() || c == '_')).next().unwrap_or("").to_string();
                    ClauseView { index: i, kind, text }
                })
                .collect()
        })
        .unwrap_or_default();
    let mentioned_in = model
        .files
        .iter()
        .flat_map(|ff| {
            ff.doc
                .refs
                .iter()
                .filter(|r| model.resolve_path(&ff.module, &r.target).as_deref() == Some(id))
                .map(|r| format!("{}:{}", ff.path, line_of(&ff.text, r.span.start)))
        })
        .collect();
    Ok(NodeView {
        id: n.id.clone(),
        kind: n.kind.label().into(),
        module: n.module.clone(),
        path: f.path.clone(),
        line: line_of(&f.text, n.span.start),
        end_line: line_of(&f.text, n.span.end),
        section: n.section.and_then(|s| f.doc.sections.get(s).map(|sec| SectionRef { index: s, title: sec.title.clone() })),
        prose: n.prose.clone(),
        source: f.text[n.span.range()].to_string(),
        signature: header(model, n),
        resolved: n.sig().map(sig_view),
        details: details(n),
        clauses,
        patterns: n.patterns.clone(),
        derives: n.derives.clone(),
        hole: hole(n),
        diagnostics: diagnostics(model).into_iter().filter(|d| d.node.as_deref() == Some(id)).collect(),
        uses: refs.get(id).map(|s| s.iter().cloned().collect()).unwrap_or_default(),
        used_by: refs.iter().filter(|(_, s)| s.contains(id)).map(|(k, _)| k.clone()).collect(),
        mentioned_in,
    })
}

fn unknown_node(model: &Model, id: &str) -> ToolError {
    let short = id.rsplit('.').next().unwrap_or(id);
    let similar: Vec<&str> = model.nodes.keys().filter(|k| k.rsplit('.').next() == Some(short)).map(String::as_str).collect();
    if similar.is_empty() {
        ToolError::new(format!("unknown node `{id}` (node ids are `module.name`; see wh_outline)"))
    } else {
        ToolError::new(format!("unknown node `{id}`; did you mean {}?", similar.join(", ")))
    }
}

// ---------------------------------------------------------------------------
// planning edits

/// The model of a set of file texts (paths relative to the project root).
pub fn build_model(texts: &BTreeMap<String, String>) -> Model {
    let inputs: Vec<SourceInput> = texts.iter().map(|(p, t)| SourceInput { path: p.clone(), text: t.clone() }).collect();
    Model::build(&inputs)
}

/// The model of a document store's current texts.
pub fn store_model(store: &DocStore) -> Model {
    build_model(&store.paths().map(|p| (p.to_string(), store.text(p).unwrap_or_default().to_string())).collect())
}

fn texts_of(model: &Model) -> BTreeMap<String, String> {
    model.files.iter().map(|f| (f.path.clone(), f.text.clone())).collect()
}

/// Minimal edits turning `before` into `after`, per changed file.
fn diff(before: &BTreeMap<String, String>, after: &BTreeMap<String, String>) -> Vec<FileEdit> {
    after
        .iter()
        .filter(|(p, t)| before.get(*p) != Some(*t))
        .map(|(p, t)| FileEdit { path: p.clone(), edits: wh_doc::diff_edits(&before[p], t) })
        .filter(|fe| !fe.edits.is_empty())
        .collect()
}

/// Plan a sequence of ops: each is planned (and parse-validated by
/// `wh_edit::plan`) against the model left by the ones before it. Returns
/// the combined edits against `model`'s texts, or the first failure.
pub fn plan_ops(model: &Model, ops: &[Op]) -> ToolResult<Vec<FileEdit>> {
    if ops.is_empty() {
        return Err(ToolError::new("no ops given"));
    }
    let start = texts_of(model);
    let mut texts = start.clone();
    let mut current: Option<Model> = None;
    for (i, op) in ops.iter().enumerate() {
        let m = current.as_ref().unwrap_or(model);
        let edits = wh_edit::plan(m, op).map_err(|e| ToolError::new(format!("op {i} ({}): {e}", op.label())))?;
        for fe in &edits {
            let t = texts.get_mut(&fe.path).ok_or_else(|| ToolError::new(format!("unknown file `{}`", fe.path)))?;
            *t = wh_edit::apply_to(t, &fe.edits);
        }
        if i + 1 < ops.len() {
            current = Some(build_model(&texts));
        }
    }
    Ok(diff(&start, &texts))
}

/// Resolve `<file path or module>` to a file index.
fn file_index(model: &Model, s: &str) -> Option<usize> {
    model.files.iter().position(|f| f.path == s).or_else(|| model.files.iter().position(|f| f.module == s))
}

fn check_prose_text(text: &str, heading: bool) -> ToolResult<()> {
    for line in text.lines() {
        let t = line.trim_start();
        if t.starts_with("```") || t.starts_with("~~~") {
            return Err(ToolError::new("prose may not contain fenced code; change definitions with wh_apply_ops"));
        }
        if t.starts_with('#') && t.trim_start_matches('#').starts_with(' ') && !heading {
            return Err(ToolError::new("prose may not contain headings; use `heading` with mode `append` to start a section"));
        }
    }
    if text.trim().is_empty() {
        return Err(ToolError::new("the prose text is empty"));
    }
    Ok(())
}

/// Insert a paragraph at `at`, separated from its neighbours by blank lines.
fn insert_paragraph(text: &str, at: usize, para: &str) -> TextEdit {
    let before = &text[..at];
    let after = &text[at..];
    let mut s = String::new();
    if !before.is_empty() {
        if !before.ends_with('\n') {
            s.push('\n');
        }
        if !before.ends_with("\n\n") {
            s.push('\n');
        }
    }
    s.push_str(para);
    if after.is_empty() {
        s.push('\n');
    } else if after.starts_with("\n\n") {
    } else if after.starts_with('\n') {
        s.push('\n');
    } else {
        s.push_str("\n\n");
    }
    TextEdit::insert(at, s)
}

/// The prose run that starts right after `from` (only whitespace between).
fn prose_after(doc: &wh_syntax::ast::Document, text: &str, from: usize) -> Option<Span> {
    doc.prose.iter().find(|p| p.span.start as usize >= from && text[from..p.span.start as usize].trim().is_empty()).map(|p| p.span)
}

/// Plan a prose edit. Fenced `wh` blocks are never touched: the plan is
/// rejected if any block's code would change or a parse error appears.
pub fn plan_prose(model: &Model, edit: &ProseEdit) -> ToolResult<Vec<FileEdit>> {
    let para = edit.text.trim();
    check_prose_text(para, false)?;
    let (fi, e) = if let Some(n) = model.nodes.get(&edit.target) {
        if edit.heading.is_some() {
            return Err(ToolError::new("`heading` only applies to a section or file target"));
        }
        let f = &model.files[n.file];
        let block = &f.doc.blocks[n.block];
        let existing = block.prose.map(|i| f.doc.prose[i].span);
        let e = match (edit.mode, existing) {
            (ProseMode::Replace, Some(p)) => TextEdit::replace(p.range(), para),
            (ProseMode::Append, Some(p)) => insert_paragraph(&f.text, p.end as usize, para),
            (ProseMode::Prepend, Some(p)) => insert_paragraph(&f.text, p.start as usize, para),
            (_, None) => insert_paragraph(&f.text, block.span.start as usize, para),
        };
        (n.file, e)
    } else {
        let (file, section) = match edit.target.split_once('#') {
            Some((f, s)) => (f, Some(s.trim())),
            None => (edit.target.as_str(), None),
        };
        let fi = file_index(model, file).ok_or_else(|| {
            ToolError::new(format!(
                "`{}` is not a node id, file or module (targets: `module.name`, `<file or module>`, `<file or module>#<section index or title>`)",
                edit.target
            ))
        })?;
        let f = &model.files[fi];
        let (body_start, end, level) = match section {
            Some(s) => {
                let idx = match s.parse::<usize>() {
                    Ok(i) if i < f.doc.sections.len() => i,
                    Ok(i) => return Err(ToolError::new(format!("`{}` has no section {i}", f.path))),
                    Err(_) => {
                        let found: Vec<usize> =
                            f.doc.sections.iter().enumerate().filter(|(_, x)| x.title.eq_ignore_ascii_case(s)).map(|(i, _)| i).collect();
                        match found.as_slice() {
                            [i] => *i,
                            [] => return Err(ToolError::new(format!("`{}` has no section titled `{s}`", f.path))),
                            _ => return Err(ToolError::new(format!("several sections are titled `{s}` ({found:?}); use the index"))),
                        }
                    }
                };
                let sec = &f.doc.sections[idx];
                (sec.heading_span.end as usize, sec.span.end as usize, sec.level)
            }
            None => (f.doc.front_matter.as_ref().map_or(0, |fm| fm.span.end as usize), f.text.len(), 1),
        };
        let intro = prose_after(&f.doc, &f.text, body_start).filter(|p| (p.start as usize) < end);
        let e = match (edit.mode, &edit.heading) {
            (ProseMode::Append, Some(h)) => {
                check_prose_text(h, false)?;
                let hashes = "#".repeat((level as usize + 1).min(6));
                insert_paragraph(&f.text, end, &format!("{hashes} {}\n\n{para}", h.trim()))
            }
            (_, Some(_)) => return Err(ToolError::new("`heading` needs mode `append`")),
            (ProseMode::Replace, None) => match intro {
                Some(p) => TextEdit::replace(p.range(), para),
                None => insert_paragraph(&f.text, body_start, para),
            },
            (ProseMode::Prepend, None) => insert_paragraph(&f.text, intro.map_or(body_start, |p| p.start as usize), para),
            (ProseMode::Append, None) => insert_paragraph(&f.text, end, para),
        };
        (fi, e)
    };
    let f = &model.files[fi];
    let after = wh_doc::apply_edits_to(&f.text, std::slice::from_ref(&e));
    let parsed = parse_document(&after);
    let code = |doc: &wh_syntax::ast::Document, text: &str| -> Vec<String> {
        doc.blocks.iter().map(|b| text[b.content_span.range()].to_string()).collect()
    };
    if code(&parsed.doc, &after) != code(&f.doc, &f.text) {
        return Err(ToolError::new("the prose edit would change a code block; prose edits must leave `wh` blocks untouched"));
    }
    let before_errors = parse_document(&f.text).diagnostics.iter().filter(|d| d.severity == Severity::Error).count();
    if parsed.diagnostics.iter().filter(|d| d.severity == Severity::Error).count() > before_errors {
        return Err(ToolError::new("the prose edit would break the document"));
    }
    Ok(vec![FileEdit { path: f.path.clone(), edits: vec![e] }])
}

// ---------------------------------------------------------------------------
// applying edits

/// Apply planned edits to a store as one `Agent` transaction and report what
/// was written. Returns `(edited ranges, changed paths)`.
pub fn apply_to_store(store: &mut DocStore, label: &str, edits: Vec<FileEdit>) -> ToolResult<(Vec<EditedRange>, Vec<String>)> {
    if edits.is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }
    let tx = store.apply(Origin::Agent, label, edits)?;
    let ranges = tx.changed_ranges();
    let mut out = Vec::new();
    for (path, rs) in &ranges {
        let text = store.text(path).unwrap_or_default();
        for r in rs {
            let start_line = line_of(text, r.start as u32);
            let end_line = line_of(text, r.end.saturating_sub(1).max(r.start) as u32);
            let ls = text[..r.start].rfind('\n').map_or(0, |i| i + 1);
            let le = text[r.end..].find('\n').map_or(text.len(), |i| r.end + i);
            let snippet = if r.is_empty() { String::new() } else { text[ls..le].to_string() };
            out.push(EditedRange { path: path.clone(), start_line, end_line, text: snippet });
        }
    }
    Ok((out, ranges.into_keys().collect()))
}

/// The result of an applied edit: diagnostics after it, and which are new.
pub fn apply_result(before: &Model, after: &Model, applied: Vec<String>, edited: Vec<EditedRange>) -> ApplyResult {
    let key = |d: &DiagView| (d.path.clone(), d.severity.clone(), d.message.clone());
    let old: BTreeSet<_> = diagnostics(before).iter().map(key).collect();
    let now: Vec<DiagView> = diagnostics(after).into_iter().filter(|d| d.severity != "lint").collect();
    let introduced = now.iter().filter(|d| !old.contains(&key(d))).cloned().collect();
    ApplyResult { applied, edited, counts: counts(after), introduced, diagnostics: now }
}

/// One label for a batch of ops (the undo step's name).
pub fn ops_label(ops: &[Op]) -> String {
    match ops {
        [op] => format!("author: {}", op.label()),
        _ => format!("author: {} edits", ops.len()),
    }
}

/// Convert a cargo result for the agent.
pub fn cargo_diags(r: &wh_build::BuildResult) -> Vec<CargoDiag> {
    r.diagnostics
        .iter()
        .filter(|d| d.level == "error" || d.level == "warning")
        .map(|d| CargoDiag { node: d.node.clone(), level: d.level.clone(), message: d.message.clone(), rendered: d.rendered.clone() })
        .collect()
}

pub fn spec_run(r: &wh_build::BuildResult) -> SpecRun {
    let tests: Vec<TestView> = r
        .tests
        .iter()
        .map(|t| TestView { name: t.name.clone(), node: t.node.clone(), passed: t.passed, output: t.output.clone() })
        .collect();
    let passed = tests.iter().filter(|t| t.passed).count();
    SpecRun {
        success: r.success,
        passed,
        failed: tests.len() - passed,
        tests,
        build_errors: cargo_diags(r).into_iter().filter(|d| d.level == "error").collect(),
    }
}
