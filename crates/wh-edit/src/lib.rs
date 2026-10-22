//! Structured edit operations: what the graph, data model, inspector and the
//! author agent do to a program, expressed as minimal text edits on the
//! literate source. Prose, comments and formatting outside the edited range
//! are never touched, and every plan is re-parsed before it is returned so an
//! operation can never leave the document unparseable.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use wh_doc::{FileEdit, TextEdit};
use wh_model::{Model, Node, NodeId};
use wh_syntax::ast::{ClauseKind, FlowStmtKind, ItemKind, TypeArg, TypeExprKind};
use wh_syntax::lexer::{TokenKind, lex};
use wh_syntax::{Severity, Span, parse_document};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Cardinality {
    /// `T`
    One,
    /// `T | None`
    Optional,
    /// `list[T]`
    Many,
    /// `set[T]`
    Set,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    /// New `def` in its own block at the end of `section` (or of the file).
    AddDef { file: String, section: Option<usize>, signature: String, clauses: Vec<String> },
    /// New `struct` in its own block.
    AddStruct { file: String, section: Option<usize>, name: String, fields: Vec<(String, String)> },
    /// Any new definition given as source text, in its own block.
    AddItem { file: String, section: Option<usize>, source: String, prose: Option<String> },
    AddField { owner: NodeId, name: String, ty: String },
    RetypeField { owner: NodeId, field: String, ty: String },
    RenameField { owner: NodeId, field: String, new_name: String },
    RemoveField { owner: NodeId, field: String },
    SetCardinality { owner: NodeId, field: String, cardinality: Cardinality },
    AddVariant { owner: NodeId, variant: String },
    AddClause { node: NodeId, clause: String },
    ReplaceClause { node: NodeId, index: usize, clause: String },
    RemoveClause { node: NodeId, index: usize },
    EditSignature { node: NodeId, signature: String },
    SetPattern { node: NodeId, pattern: Option<String> },
    /// Insert `|> callee` as the `position`-th step of pipeline statement `stmt`.
    InsertFlowStep { flow: NodeId, stmt: usize, position: usize, callee: String },
    RemoveFlowStep { flow: NodeId, stmt: usize, position: usize },
    /// Move the `from`-th `|>` step of pipeline statement `stmt` to position `to`.
    MoveFlowStep { flow: NodeId, stmt: usize, from: usize, to: usize },
    /// Insert the binding `name = value` as statement `index` of a flow body
    /// (DAG form: a new node fed by earlier bindings or parameters).
    InsertFlowBinding { flow: NodeId, index: usize, name: String, value: String },
    Rename { node: NodeId, new_name: String },
    RemoveNode { node: NodeId },
}

impl Op {
    pub fn label(&self) -> String {
        match self {
            Op::AddDef { signature, .. } => format!("add def {}", signature.split('(').next().unwrap_or("")),
            Op::AddStruct { name, .. } => format!("add struct {name}"),
            Op::AddItem { .. } => "add definition".into(),
            Op::AddField { name, .. } => format!("add field {name}"),
            Op::RetypeField { field, .. } => format!("retype field {field}"),
            Op::RenameField { field, new_name, .. } => format!("rename field {field} → {new_name}"),
            Op::RemoveField { field, .. } => format!("remove field {field}"),
            Op::SetCardinality { field, .. } => format!("change cardinality of {field}"),
            Op::AddVariant { variant, .. } => format!("add variant {variant}"),
            Op::AddClause { .. } => "add clause".into(),
            Op::ReplaceClause { .. } => "edit clause".into(),
            Op::RemoveClause { .. } => "remove clause".into(),
            Op::EditSignature { .. } => "edit signature".into(),
            Op::SetPattern { .. } => "set pattern".into(),
            Op::InsertFlowStep { callee, .. } => format!("insert step {callee}"),
            Op::RemoveFlowStep { .. } => "remove step".into(),
            Op::MoveFlowStep { .. } => "move step".into(),
            Op::InsertFlowBinding { name, value, .. } => format!("bind {name} = {value}"),
            Op::Rename { new_name, .. } => format!("rename to {new_name}"),
            Op::RemoveNode { node } => format!("remove {node}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditError {
    UnknownNode(String),
    UnknownFile(String),
    Invalid(String),
    /// The edit would introduce parse errors; the message says which.
    WouldBreak(String),
}

impl std::fmt::Display for EditError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EditError::UnknownNode(n) => write!(f, "unknown node `{n}`"),
            EditError::UnknownFile(n) => write!(f, "unknown file `{n}`"),
            EditError::Invalid(m) => write!(f, "{m}"),
            EditError::WouldBreak(m) => write!(f, "the edit would break the document: {m}"),
        }
    }
}

impl std::error::Error for EditError {}

type R<T> = Result<T, EditError>;

/// Plan an operation against the model's current file texts.
pub fn plan(model: &Model, op: &Op) -> R<Vec<FileEdit>> {
    let mut p = Planner { model, edits: BTreeMap::new() };
    p.op(op)?;
    let edits: Vec<FileEdit> =
        p.edits.into_iter().map(|(path, edits)| FileEdit { path, edits }).filter(|f| !f.edits.is_empty()).collect();
    validate(model, &edits)?;
    Ok(edits)
}

/// Apply edits to the model's texts and make sure no new parse errors appear.
fn validate(model: &Model, edits: &[FileEdit]) -> R<()> {
    for fe in edits {
        let file = model.files.iter().find(|f| f.path == fe.path).ok_or_else(|| EditError::UnknownFile(fe.path.clone()))?;
        let after = apply_to(&file.text, &fe.edits);
        let before_errors = parse_document(&file.text).diagnostics.iter().filter(|d| d.severity == Severity::Error).count();
        let parsed = parse_document(&after);
        let errors: Vec<_> = parsed.diagnostics.iter().filter(|d| d.severity == Severity::Error).collect();
        if errors.len() > before_errors {
            let d = &errors[0];
            return Err(EditError::WouldBreak(d.render(&fe.path, &after)));
        }
    }
    Ok(())
}

pub fn apply_to(text: &str, edits: &[TextEdit]) -> String {
    let mut sorted: Vec<&TextEdit> = edits.iter().collect();
    sorted.sort_by_key(|e| e.range.start);
    let mut out = String::new();
    let mut cursor = 0;
    for e in sorted {
        out.push_str(&text[cursor..e.range.start]);
        out.push_str(&e.text);
        cursor = e.range.end;
    }
    out.push_str(&text[cursor..]);
    out
}

struct Planner<'m> {
    model: &'m Model,
    edits: BTreeMap<String, Vec<TextEdit>>,
}

fn line_start(text: &str, offset: usize) -> usize {
    text[..offset].rfind('\n').map_or(0, |i| i + 1)
}

fn line_end(text: &str, offset: usize) -> usize {
    text[offset..].find('\n').map_or(text.len(), |i| offset + i)
}

fn indent_at(text: &str, offset: usize) -> String {
    let ls = line_start(text, offset);
    text[ls..].chars().take_while(|c| *c == ' ').collect()
}

impl<'m> Planner<'m> {
    fn node(&self, id: &str) -> R<&'m Node> {
        self.model.nodes.get(id).ok_or_else(|| EditError::UnknownNode(id.to_string()))
    }

    fn text(&self, file: usize) -> &'m str {
        &self.model.files[file].text
    }

    fn push(&mut self, file: usize, e: TextEdit) {
        self.edits.entry(self.model.files[file].path.clone()).or_default().push(e);
    }

    fn file_index(&self, path: &str) -> R<usize> {
        self.model.files.iter().position(|f| f.path == path).ok_or_else(|| EditError::UnknownFile(path.to_string()))
    }

    /// Insert a line after the indented body ending at `body_end`, indented like the body's first line.
    fn append_body_line(&mut self, file: usize, body: Span, line: &str) {
        let text = self.text(file);
        let indent = indent_at(text, body.start as usize);
        let at = line_end(text, body.end as usize);
        self.push(file, TextEdit::insert(at, format!("\n{indent}{line}")));
    }

    fn new_block(&mut self, file: usize, section: Option<usize>, prose: &str, code: &str) -> R<()> {
        let f = &self.model.files[file];
        let at = match section {
            Some(s) => {
                let sec = f.doc.sections.get(s).ok_or_else(|| EditError::Invalid(format!("no section {s}")))?;
                // after the last child content of the section: just before the next heading
                sec.span.end as usize
            }
            None => f.text.len(),
        };
        let text = &f.text;
        let before = &text[..at];
        let mut s = String::new();
        if !before.ends_with('\n') && !before.is_empty() {
            s.push('\n');
        }
        if !before.ends_with("\n\n") && !before.is_empty() {
            s.push('\n');
        }
        s.push_str(prose.trim());
        s.push_str("\n\n```wh\n");
        s.push_str(code.trim_end());
        s.push_str("\n```\n");
        if at < text.len() {
            s.push('\n');
        }
        self.push(file, TextEdit::insert(at, s));
        Ok(())
    }

    fn op(&mut self, op: &Op) -> R<()> {
        match op {
            Op::AddDef { file, section, signature, clauses } => {
                let fi = self.file_index(file)?;
                let mut code = format!("def {}", signature.trim());
                if !clauses.is_empty() {
                    code.push(':');
                    for c in clauses {
                        code.push_str("\n    ");
                        code.push_str(c.trim());
                    }
                }
                let name = signature.split('(').next().unwrap_or("").trim().to_string();
                self.new_block(fi, *section, &format!("TODO: explain what `{name}` is for."), &code)
            }
            Op::AddStruct { file, section, name, fields } => {
                let fi = self.file_index(file)?;
                let mut code = format!("struct {name}:");
                if fields.is_empty() {
                    return Err(EditError::Invalid("a struct needs at least one field".into()));
                }
                for (n, t) in fields {
                    code.push_str(&format!("\n    {n}: {t}"));
                }
                self.new_block(fi, *section, &format!("TODO: explain what a `{name}` is."), &code)
            }
            Op::AddItem { file, section, source, prose } => {
                let fi = self.file_index(file)?;
                let prose = prose.clone().unwrap_or_else(|| "TODO: explain this definition.".into());
                self.new_block(fi, *section, &prose, source)
            }
            Op::AddField { owner, name, ty } => {
                let node = self.node(owner)?;
                let ItemKind::Struct(s) = &node.item.kind else {
                    return Err(EditError::Invalid(format!("`{owner}` is not a struct")));
                };
                if s.fields.iter().any(|f| &f.name.name == name) {
                    return Err(EditError::Invalid(format!("`{}` already has a field `{name}`", node.name)));
                }
                // insert after the last field (before invariants, which conventionally come last)
                let anchor = s.fields.last().map_or(s.body_span, |f| f.span);
                let text = self.text(node.file);
                let indent = indent_at(text, s.body_span.start as usize);
                let at = line_end(text, anchor.end as usize);
                self.push(node.file, TextEdit::insert(at, format!("\n{indent}{name}: {ty}")));
                Ok(())
            }
            Op::RetypeField { owner, field, ty } => {
                let (node, f) = self.field(owner, field)?;
                self.push(node.file, TextEdit::replace(f.ty.span.range(), ty.clone()));
                Ok(())
            }
            Op::RenameField { owner, field, new_name } => {
                let (node, f) = self.field(owner, field)?;
                self.push(node.file, TextEdit::replace(f.name.span.range(), new_name.clone()));
                Ok(())
            }
            Op::RemoveField { owner, field } => {
                let (node, f) = self.field(owner, field)?;
                let ItemKind::Struct(s) = &node.item.kind else { unreachable!() };
                if s.fields.len() == 1 {
                    return Err(EditError::Invalid("cannot remove the last field of a struct".into()));
                }
                let text = self.text(node.file);
                let start = line_start(text, f.span.start as usize);
                let end = (line_end(text, f.span.end as usize) + 1).min(text.len());
                self.push(node.file, TextEdit::delete(start..end));
                Ok(())
            }
            Op::SetCardinality { owner, field, cardinality } => {
                let (node, f) = self.field(owner, field)?;
                let text = self.text(node.file);
                let core = match &f.ty.kind {
                    TypeExprKind::Optional(inner) => &text[inner.span.range()],
                    TypeExprKind::Named { path, args }
                        if path.len() == 1 && matches!(path[0].name.as_str(), "list" | "set") && args.len() == 1 =>
                    {
                        match &args[0] {
                            TypeArg::Type(t) => &text[t.span.range()],
                            TypeArg::Const(_) => &text[f.ty.span.range()],
                        }
                    }
                    _ => &text[f.ty.span.range()],
                };
                let new = match cardinality {
                    Cardinality::One => core.to_string(),
                    Cardinality::Optional => format!("{core} | None"),
                    Cardinality::Many => format!("list[{core}]"),
                    Cardinality::Set => format!("set[{core}]"),
                };
                // a default of None makes no sense once the field is no longer optional
                let mut end = f.ty.span.end as usize;
                if *cardinality != Cardinality::Optional {
                    if let Some(d) = &f.default {
                        if matches!(d.kind, wh_syntax::ast::ExprKind::None) {
                            end = d.span.end as usize;
                        }
                    }
                }
                self.push(node.file, TextEdit::replace(f.ty.span.start as usize..end, new));
                Ok(())
            }
            Op::AddVariant { owner, variant } => {
                let node = self.node(owner)?;
                let ItemKind::Enum(e) = &node.item.kind else {
                    return Err(EditError::Invalid(format!("`{owner}` is not an enum")));
                };
                self.append_body_line(node.file, e.body_span, variant.trim());
                Ok(())
            }
            Op::AddClause { node, clause } => {
                let n = self.node(node)?;
                match &n.item.kind {
                    ItemKind::Def(d) => match d.body_span {
                        Some(body) if !body.is_empty() => self.append_body_line(n.file, body, clause.trim()),
                        _ => {
                            let text = self.text(n.file);
                            let indent = indent_at(text, d.sig.span.start as usize);
                            let at = n.span.end as usize;
                            let colon = if text[..at].trim_end().ends_with(':') { "" } else { ":" };
                            self.push(n.file, TextEdit::insert(at, format!("{colon}\n{indent}    {}", clause.trim())));
                        }
                    },
                    _ => return Err(EditError::Invalid(format!("`{node}` does not take clauses"))),
                }
                Ok(())
            }
            Op::ReplaceClause { node, index, clause } => {
                let (n, span) = self.clause(node, *index)?;
                self.push(n.file, TextEdit::replace(span.range(), clause.trim().to_string()));
                Ok(())
            }
            Op::RemoveClause { node, index } => {
                let (n, span) = self.clause(node, *index)?;
                let d = n.def().unwrap();
                let text = self.text(n.file);
                if d.clauses.len() == 1 && d.doc.is_none() {
                    // remove the whole body including the `:`
                    let colon = text[..span.start as usize].rfind(':').unwrap_or(span.start as usize);
                    let end = line_end(text, span.end as usize);
                    self.push(n.file, TextEdit::delete(colon..end));
                } else {
                    let start = line_start(text, span.start as usize);
                    let end = (line_end(text, span.end as usize) + 1).min(text.len());
                    self.push(n.file, TextEdit::delete(start..end));
                }
                Ok(())
            }
            Op::EditSignature { node, signature } => {
                let n = self.node(node)?;
                let span = match &n.item.kind {
                    ItemKind::Def(d) => d.sig.span,
                    ItemKind::Flow(f) => f.sig.span,
                    _ => return Err(EditError::Invalid(format!("`{node}` has no signature"))),
                };
                self.push(n.file, TextEdit::replace(span.range(), signature.trim().to_string()));
                Ok(())
            }
            Op::SetPattern { node, pattern } => {
                let n = self.node(node)?;
                let Some(d) = n.def() else { return Err(EditError::Invalid(format!("`{node}` is not a def"))) };
                let existing = d.clauses.iter().position(|c| matches!(c.kind, ClauseKind::Pattern(_)));
                match (existing, pattern) {
                    (Some(i), Some(p)) => self.op(&Op::ReplaceClause { node: node.clone(), index: i, clause: format!("pattern {p}") }),
                    (Some(i), None) => self.op(&Op::RemoveClause { node: node.clone(), index: i }),
                    (None, Some(p)) => self.op(&Op::AddClause { node: node.clone(), clause: format!("pattern {p}") }),
                    (None, None) => Ok(()),
                }
            }
            Op::InsertFlowStep { flow, stmt, position, callee } => {
                let n = self.node(flow)?;
                let fe = self.pipeline(n, *stmt)?;
                if *position > fe.steps.len() {
                    return Err(EditError::Invalid(format!("the pipeline has only {} steps", fe.steps.len())));
                }
                let text = self.text(n.file);
                let prev_end = if *position == 0 { fe.head.span.end } else { fe.steps[*position - 1].span.end } as usize;
                let multiline = match fe.steps.first() {
                    Some(first) => text[fe.head.span.end as usize..first.span.start as usize].contains('\n'),
                    None => false,
                };
                let insert = if multiline {
                    let indent = indent_at(text, fe.steps[0].span.start as usize);
                    format!("\n{indent}|> {callee}")
                } else {
                    format!(" |> {callee}")
                };
                self.push(n.file, TextEdit::insert(prev_end, insert));
                Ok(())
            }
            Op::RemoveFlowStep { flow, stmt, position } => {
                let n = self.node(flow)?;
                let fe = self.pipeline(n, *stmt)?;
                let step = fe.steps.get(*position).ok_or_else(|| EditError::Invalid("no such step".into()))?;
                let prev_end = if *position == 0 { fe.head.span.end } else { fe.steps[*position - 1].span.end } as usize;
                self.push(n.file, TextEdit::delete(prev_end..step.span.end as usize));
                Ok(())
            }
            Op::MoveFlowStep { flow, stmt, from, to } => {
                let n = self.node(flow)?;
                let fe = self.pipeline(n, *stmt)?;
                let k = fe.steps.len();
                if *from >= k || *to >= k {
                    return Err(EditError::Invalid(format!("the pipeline has only {k} steps")));
                }
                // reorder the step texts; the separators (`|>`, line breaks, comments) stay put
                let text = self.text(n.file);
                let mut order: Vec<usize> = (0..k).collect();
                let moved = order.remove(*from);
                order.insert(*to, moved);
                for (slot, &src) in order.iter().enumerate() {
                    if slot != src {
                        let new = text[fe.steps[src].span.range()].to_string();
                        self.push(n.file, TextEdit::replace(fe.steps[slot].span.range(), new));
                    }
                }
                Ok(())
            }
            Op::InsertFlowBinding { flow, index, name, value } => {
                let n = self.node(flow)?;
                let body = self.flow_body(n)?;
                if !wh_model::is_snake(name) || wh_syntax::is_hard_keyword(name) {
                    return Err(EditError::Invalid(format!("`{name}` is not a valid snake_case name")));
                }
                let bound = |s: &wh_syntax::ast::FlowStmt| match &s.kind {
                    FlowStmtKind::Bind { targets, .. } => targets.iter().any(|t| &t.name == name),
                    _ => false,
                };
                if body.iter().any(bound) {
                    return Err(EditError::Invalid(format!("`{}` already binds `{name}`", n.name)));
                }
                let (Some(first), Some(last)) = (body.first(), body.last()) else {
                    return Err(EditError::Invalid("the flow has an empty body".into()));
                };
                let text = self.text(n.file);
                let indent = indent_at(text, first.span.start as usize);
                let line = format!("{name} = {}", value.trim());
                match body.get(*index) {
                    Some(s) => {
                        let at = line_start(text, s.span.start as usize);
                        self.push(n.file, TextEdit::insert(at, format!("{indent}{line}\n")));
                    }
                    None if *index == body.len() => {
                        let at = line_end(text, last.span.end as usize);
                        self.push(n.file, TextEdit::insert(at, format!("\n{indent}{line}")));
                    }
                    None => return Err(EditError::Invalid(format!("the flow has only {} statements", body.len()))),
                }
                Ok(())
            }
            Op::Rename { node, new_name } => self.rename(node, new_name),
            Op::RemoveNode { node } => {
                let n = self.node(node)?;
                let f = &self.model.files[n.file];
                let block = &f.doc.blocks[n.block];
                let text = &f.text;
                if block.items.len() == 1 {
                    let end = (block.span.end as usize).min(text.len());
                    let end = if text[end..].starts_with('\n') { end + 1 } else { end };
                    self.push(n.file, TextEdit::delete(block.span.start as usize..end));
                } else {
                    let start = line_start(text, n.span.start as usize);
                    let mut end = (line_end(text, n.span.end as usize) + 1).min(text.len());
                    // swallow one blank separator line
                    if text[end..].starts_with('\n') {
                        end += 1;
                    }
                    self.push(n.file, TextEdit::delete(start..end));
                }
                Ok(())
            }
        }
    }

    fn field(&self, owner: &str, field: &str) -> R<(&'m Node, &'m wh_syntax::ast::Field)> {
        let node = self.node(owner)?;
        let ItemKind::Struct(s) = &node.item.kind else {
            return Err(EditError::Invalid(format!("`{owner}` is not a struct")));
        };
        let f = s
            .fields
            .iter()
            .find(|f| f.name.name == field)
            .ok_or_else(|| EditError::Invalid(format!("`{}` has no field `{field}`", node.name)))?;
        Ok((node, f))
    }

    fn clause(&self, node: &str, index: usize) -> R<(&'m Node, Span)> {
        let n = self.node(node)?;
        let d = n.def().ok_or_else(|| EditError::Invalid(format!("`{node}` is not a def")))?;
        let c = d.clauses.get(index).ok_or_else(|| EditError::Invalid(format!("`{}` has no clause {index}", n.name)))?;
        Ok((n, c.span))
    }

    fn flow_body(&self, n: &'m Node) -> R<&'m [wh_syntax::ast::FlowStmt]> {
        match &n.item.kind {
            ItemKind::Flow(f) => Ok(&f.body),
            ItemKind::App(a) => Ok(&a.entry.as_ref().ok_or_else(|| EditError::Invalid("app has no entry".into()))?.body),
            _ => Err(EditError::Invalid(format!("`{}` is not a flow", n.name))),
        }
    }

    fn pipeline(&self, n: &'m Node, stmt: usize) -> R<&'m wh_syntax::ast::FlowExpr> {
        let body = self.flow_body(n)?;
        let s = body.get(stmt).ok_or_else(|| EditError::Invalid(format!("flow has no statement {stmt}")))?;
        match &s.kind {
            FlowStmtKind::Expr(fe) | FlowStmtKind::Bind { value: fe, .. } | FlowStmtKind::Return(Some(fe)) => Ok(fe),
            _ => Err(EditError::Invalid("that statement is not a pipeline".into())),
        }
    }

    fn rename(&mut self, id: &str, new_name: &str) -> R<()> {
        let node = self.node(id)?;
        let old = node.name.clone();
        let is_type = node.kind.is_type()
            || matches!(node.item.kind, ItemKind::Capability(_) | ItemKind::Adapter(_) | ItemKind::Machine(_));
        let ok = if is_type { wh_model::is_pascal(new_name) } else { wh_model::is_snake(new_name) };
        if !ok || wh_syntax::is_hard_keyword(new_name) {
            return Err(EditError::Invalid(format!(
                "`{new_name}` is not a valid {} name",
                if is_type { "PascalCase type" } else { "snake_case" }
            )));
        }
        if self.model.scopes.get(&node.module).is_some_and(|s| s.names.contains_key(new_name)) {
            return Err(EditError::Invalid(format!("`{new_name}` already exists in module `{}`", node.module)));
        }
        for (fi, f) in self.model.files.iter().enumerate() {
            let Some(scope) = self.model.scopes.get(&f.module) else { continue };
            let visible = scope.names.get(&old).map(String::as_str) == Some(id);
            let module_aliases: Vec<&str> = scope
                .module_aliases
                .iter()
                .filter(|(_, real)| **real == node.module)
                .map(|(a, _)| a.as_str())
                .chain(std::iter::once(node.module.as_str()))
                .collect();
            for b in &f.doc.blocks {
                let (toks, _) = lex(&f.text[b.content_span.range()], b.content_span.start);
                let sig: Vec<_> = toks.iter().filter(|t| !t.kind.is_trivia()).collect();
                for (i, t) in sig.iter().enumerate() {
                    if t.kind != TokenKind::Ident || &f.text[t.span.range()] != old {
                        continue;
                    }
                    let prev = i.checked_sub(1).map(|j| sig[j]);
                    let next = sig.get(i + 1);
                    let after_dot = prev.is_some_and(|p| p.kind == TokenKind::Dot);
                    let refers = if after_dot {
                        let owner = i.checked_sub(2).map(|j| &f.text[sig[j].span.range()]);
                        owner.is_some_and(|o| module_aliases.contains(&o))
                    } else {
                        // `name:` declares a field/parameter and `name =` a binding/keyword
                        // argument, but only at the start of a line or argument
                        let starts_decl = match prev {
                            None => true,
                            Some(p) => {
                                matches!(p.kind, TokenKind::LParen | TokenKind::Comma)
                                    || matches!(&f.text[p.span.range()], "own" | "mut" | "key")
                                    || f.text[p.span.end as usize..t.span.start as usize].contains('\n')
                            }
                        };
                        let declares = starts_decl
                            && next.is_some_and(|n| matches!(n.kind, TokenKind::Colon | TokenKind::Eq))
                            && t.span != node.name_span;
                        visible && !declares
                    };
                    if refers || t.span == node.name_span {
                        self.push(fi, TextEdit::replace(t.span.range(), new_name.to_string()));
                    }
                }
            }
            for r in &f.doc.refs {
                if self.model.resolve_path(&f.module, &r.target).as_deref() == Some(id) {
                    let new_target = match r.target.rsplit_once('.') {
                        Some((m, _)) => format!("{m}.{new_name}"),
                        None => new_name.to_string(),
                    };
                    self.push(fi, TextEdit::replace(r.span.range(), format!("[[{new_target}]]")));
                }
            }
        }
        Ok(())
    }
}

/// Where a flow step sits in the text, for choosing between a pipeline insert
/// and a new binding: `(stmt, position, binds, last)` — the statement index,
/// the `|>` position (`None` for the head), the names the statement binds and
/// whether the step is the last element of its statement.
pub fn step_site(model: &Model, flow: &str, step: usize) -> Option<(usize, Option<usize>, Vec<String>, bool)> {
    let (stmt, pos) = pipeline_position(model, flow, step)?;
    let node = model.nodes.get(flow)?;
    let body = match &node.item.kind {
        ItemKind::Flow(f) => &f.body,
        ItemKind::App(a) => &a.entry.as_ref()?.body,
        _ => return None,
    };
    let (binds, fe) = match &body.get(stmt)?.kind {
        FlowStmtKind::Bind { targets, value } => (targets.iter().map(|t| t.name.clone()).collect(), value),
        FlowStmtKind::Expr(fe) | FlowStmtKind::Return(Some(fe)) => (vec![], fe),
        _ => return None,
    };
    let last = match pos {
        None => fe.steps.is_empty(),
        Some(p) => p + 1 == fe.steps.len(),
    };
    Some((stmt, pos, binds, last))
}

/// Number of statements in a flow's body (for appending a binding).
pub fn flow_len(model: &Model, flow: &str) -> Option<usize> {
    match &model.nodes.get(flow)?.item.kind {
        ItemKind::Flow(f) => Some(f.body.len()),
        ItemKind::App(a) => Some(a.entry.as_ref()?.body.len()),
        _ => None,
    }
}

/// Map a flow step (index into the model's `FlowInfo::steps`) to its pipeline
/// position `(stmt, position)` for `InsertFlowStep`/`RemoveFlowStep`.
/// Position counts `|>` steps; a head call is position `None`.
pub fn pipeline_position(model: &Model, flow: &str, step: usize) -> Option<(usize, Option<usize>)> {
    let node = model.nodes.get(flow)?;
    let span = match &node.kind {
        wh_model::NodeKind::Flow(f) => f.steps.get(step)?.span,
        wh_model::NodeKind::App(a) => a.entry.as_ref()?.steps.get(step)?.span,
        _ => return None,
    };
    let body = match &node.item.kind {
        ItemKind::Flow(f) => &f.body,
        ItemKind::App(a) => &a.entry.as_ref()?.body,
        _ => return None,
    };
    for (si, s) in body.iter().enumerate() {
        let fe = match &s.kind {
            FlowStmtKind::Expr(fe) | FlowStmtKind::Bind { value: fe, .. } | FlowStmtKind::Return(Some(fe)) => fe,
            _ => continue,
        };
        if fe.head.span == span {
            return Some((si, None));
        }
        if let Some(p) = fe.steps.iter().position(|st| st.span == span) {
            return Some((si, Some(p)));
        }
    }
    None
}
