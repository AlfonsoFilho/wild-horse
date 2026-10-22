//! Per-module Rust emission: types, capabilities, adapters, hole wrappers
//! with contracts and tracing, flows, and app entries.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use wh_model::{FlowInfo, Model, Node, NodeKind, ParamInfo, SigInfo, StepInput, Ty};
use wh_syntax::ast::{self, ClauseKind, ExprKind, FlowStmt, FlowStmtKind, ItemKind, ParamMode};

use crate::expr::{ExprGen, Scope, Var};
use crate::rust::{Names, cap_var, module_ident, rust_keyword_safe};
use crate::{GenOptions, HoleSpec, contracts_cfg};

pub struct ModuleGen<'m> {
    pub model: &'m Model,
    pub module: &'m str,
    pub opts: &'m GenOptions,
    pub out: String,
    pub holes: Vec<HoleSpec>,
    /// (line, node id) markers for mapping compiler errors back to nodes
    pub regions: Vec<(usize, String)>,
}

pub(crate) fn esc(s: &str) -> String {
    format!("{s:?}")
}

/// Tracing level from a node's `trace` clause, or a default; `None` = off.
pub fn trace_level(node: &Node, default: &str) -> Option<String> {
    let clauses: &[ast::Clause] = match &node.item.kind {
        ItemKind::Def(d) => &d.clauses,
        ItemKind::Adapter(a) => &a.clauses,
        _ => &[],
    };
    for c in clauses {
        if let ClauseKind::Trace(l) = &c.kind {
            return if l.name == "off" { None } else { Some(l.name.clone()) };
        }
    }
    Some(default.to_string())
}

pub fn hashable(model: &Model, t: &Ty, depth: u32) -> bool {
    let _ = depth;
    model.type_is_hashable(t)
}

/// Does a value of this type carry invariants that must be checked?
pub fn needs_check(model: &Model, t: &Ty, depth: u32) -> bool {
    if depth > 6 {
        return false;
    }
    match t {
        Ty::Named(id, _) => match model.nodes.get(id).map(|n| &n.kind) {
            Some(NodeKind::Struct(s)) => s.has_invariants || s.fields.iter().any(|f| needs_check(model, &f.ty, depth + 1)),
            _ => false,
        },
        Ty::Option(i) | Ty::List(i) | Ty::Set(i) | Ty::SortedSet(i) | Ty::Array(i, _) => {
            matches!(**i, Ty::Named(..)) && needs_check(model, i, depth + 1)
        }
        Ty::Dict(_, v) | Ty::SortedDict(_, v) => matches!(**v, Ty::Named(..)) && needs_check(model, v, depth + 1),
        _ => false,
    }
}

/// `TooYoung` → `too young`
fn humanize(name: &str) -> String {
    let mut out = String::new();
    for (i, c) in name.chars().enumerate() {
        if c.is_ascii_uppercase() && i > 0 {
            out.push(' ');
        }
        out.push(c.to_ascii_lowercase());
    }
    out
}

fn derive_list(model: &Model, node: &Node, tys: &[Ty], with_debug: bool) -> String {
    let mut d: Vec<String> = vec!["Clone".into(), "PartialEq".into()];
    if with_debug {
        d.insert(0, "Debug".into());
    }
    if tys.iter().all(|t| hashable(model, t, 0)) {
        d.push("Eq".into());
        d.push("Hash".into());
    }
    for extra in &node.derives {
        let e = match extra.as_str() {
            "Serialize" => "serde::Serialize",
            "Deserialize" => "serde::Deserialize",
            "Debug" | "Clone" | "PartialEq" | "Eq" | "Hash" => continue,
            other => other,
        };
        d.push(e.to_string());
    }
    // values that cross an HTTP route or a wasm export travel as JSON
    if crate::targets::is_wire_type(model, &node.id) {
        for e in ["serde::Serialize", "serde::Deserialize"] {
            if !d.iter().any(|x| x == e) {
                d.push(e.to_string());
            }
        }
    }
    format!("#[derive({})]", d.join(", "))
}

fn doc_comment(prose: &Option<String>, indent: &str) -> String {
    let Some(p) = prose else { return String::new() };
    let mut s = String::new();
    for line in p.lines() {
        let _ = writeln!(s, "{indent}///{}{line}", if line.is_empty() { "" } else { " " });
    }
    s
}

impl<'m> ModuleGen<'m> {
    pub(crate) fn names(&self) -> Names<'m> {
        Names::new(self.model, self.module)
    }

    pub(crate) fn eg(&self) -> ExprGen<'m> {
        ExprGen::new(self.model, self.module)
    }

    pub(crate) fn mark(&mut self, id: &str) {
        let line = self.out.matches('\n').count() + 1;
        self.regions.push((line, id.to_string()));
        let _ = writeln!(self.out, "// wh:node {id}");
    }

    pub fn generate(&mut self) {
        let m = module_ident(self.module);
        let _ = writeln!(self.out, "//! Generated from module `{}` by Wild Horse. Do not edit: edit the `.wh.md` source.", self.module);
        let _ = writeln!(self.out, "#![allow(unused_imports, unused_variables, unused_mut, unused_parens, unreachable_code, unreachable_patterns, non_shorthand_field_patterns, dead_code, non_snake_case, clippy::all)]\n");
        let _ = writeln!(self.out, "use crate::prelude::*;");
        // imports from other modules
        if let Some(scope) = self.model.scopes.get(self.module) {
            for (name, id) in &scope.names {
                let n = &self.model.nodes[id];
                if n.module != self.module {
                    let _ = writeln!(self.out, "pub use crate::{}::{};", module_ident(&n.module), rust_keyword_safe(name));
                }
            }
        }
        let _ = writeln!(self.out, "\n#[allow(unused)]\nconst WH_MODULE: &str = {};\n", esc(&m));

        let nodes: Vec<&'m Node> = self.model.nodes.values().filter(|n| n.module == self.module).collect();
        for n in &nodes {
            match &n.kind {
                NodeKind::Alias(t) => {
                    self.mark(&n.id);
                    let generics = match &n.item.kind {
                        ItemKind::Alias(a) if !a.generics.is_empty() => {
                            format!("<{}>", a.generics.iter().map(|g| g.name.name.clone()).collect::<Vec<_>>().join(", "))
                        }
                        _ => String::new(),
                    };
                    let doc = doc_comment(&n.prose, "");
                    let _ = writeln!(self.out, "{doc}pub type {}{generics} = {};\n", n.name, self.names().ty(t));
                }
                NodeKind::Refined { .. } => self.refined(n),
                NodeKind::Struct(_) => self.struct_(n),
                NodeKind::Enum(_) => self.enum_(n),
                NodeKind::Machine(_) => self.machine(n),
                NodeKind::Capability(_) => self.capability(n),
                _ => {}
            }
        }
        for n in &nodes {
            match &n.kind {
                NodeKind::Adapter(_) => self.adapter(n),
                NodeKind::Def(_) => self.def(n),
                NodeKind::Flow(f) => {
                    if let ItemKind::Flow(ast_flow) = &n.item.kind {
                        self.mark(&n.id);
                        let f = f.clone();
                        let level = ast_flow.trace.as_ref().map_or("info".to_string(), |l| l.name.clone());
                        self.flow(&n.id, &n.name, &n.prose, ast_flow, &f, &level);
                    }
                }
                NodeKind::App(a) if a.profile().kind == wh_targets::TargetKind::Server => {
                    self.mark(&n.id);
                    self.server_app(n, a);
                }
                NodeKind::App(a) if a.profile().kind == wh_targets::TargetKind::Web => {
                    self.mark(&n.id);
                    self.web_app(n, a);
                }
                NodeKind::App(a) => {
                    if let (ItemKind::App(ast_app), Some(entry)) = (&n.item.kind, &a.entry) {
                        if let Some(ast_entry) = &ast_app.entry {
                            self.mark(&n.id);
                            let entry = entry.clone();
                            self.flow(&n.id, &format!("{}_entry", n.name), &n.prose, ast_entry, &entry, "info");
                        }
                    }
                }
                _ => {}
            }
        }
        // hole modules
        if !self.holes.is_empty() {
            // Absolute impl roots: `#[path]`s inside `mod imp {}`. Relative impl roots
            // (relative to the generated crate): top-level `#[path]`s, which rustc and
            // cargo-mutants both resolve from this file's directory, re-exported in `imp`.
            let relative = self.holes.iter().any(|h| h.path.is_relative());
            let mut reexports = Vec::new();
            if !relative {
                let _ = writeln!(self.out, "/// Implementations of this module's holes (synthesized or hand-written).");
                let _ = writeln!(self.out, "pub(crate) mod imp {{");
            }
            let pad = if relative { "" } else { "    " };
            for h in &self.holes {
                let unsafe_ok = self.model.nodes.get(&h.node).is_some_and(|n| {
                    n.def().is_some_and(|d| d.clauses.iter().any(|c| matches!(c.kind, ClauseKind::Unsafe { allow: true })))
                });
                if !unsafe_ok {
                    let _ = writeln!(self.out, "{pad}#[deny(unsafe_code)]");
                } else {
                    let _ = writeln!(self.out, "{pad}#[allow(unsafe_code)]");
                }
                // synthesized code is held to correctness/perf lints and must log via tracing
                let _ = writeln!(self.out, "{pad}#[deny(clippy::correctness, clippy::perf, clippy::print_stdout, clippy::print_stderr, clippy::dbg_macro)]");
                let path = if h.path.is_absolute() {
                    h.path.to_string_lossy().to_string()
                } else {
                    // from `src/modules/` to the crate root
                    format!("../../{}", h.path.to_string_lossy().replace('\\', "/"))
                };
                let _ = writeln!(self.out, "{pad}#[path = {}]", esc(&path));
                let _ = writeln!(self.out, "{pad}pub(crate) mod {};", h.module_name);
                reexports.push(h.module_name.clone());
            }
            if relative {
                let _ = writeln!(self.out, "/// Implementations of this module's holes (synthesized or hand-written).");
                let _ = writeln!(self.out, "pub(crate) mod imp {{\n    pub(crate) use super::{{{}}};\n}}", reexports.join(", "));
            } else {
                let _ = writeln!(self.out, "}}");
            }
        }
    }

    // -------------------------------------------------------------- types

    fn refined(&mut self, n: &Node) {
        let NodeKind::Refined { base, generics } = &n.kind else { return };
        let ItemKind::TypeDef(td) = &n.item.kind else { return };
        self.mark(&n.id);
        let names = self.names();
        let b = names.ty(base);
        let g = if generics.is_empty() { String::new() } else { format!("<{}>", generics.join(", ")) };
        let doc = doc_comment(&n.prose, "");
        let derives = derive_list(self.model, n, std::slice::from_ref(base), !n.is_sensitive);
        let copy = if self.model.type_derives_copy(base) { "#[derive(Copy)]\n" } else { "" };
        let ord = if hashable(self.model, base, 0) { "#[derive(PartialOrd, Ord)]\n" } else { "" };
        let rule = td.refinement.as_ref().map(|r| self.model.files[n.file].text[r.span.range()].to_string()).unwrap_or_default();
        // (de)serialization goes through the base type so the refinement is always checked
        let serde = if n.derives.iter().any(|d| d == "Serialize" || d == "Deserialize") || crate::targets::is_wire_type(self.model, &n.id) {
            format!("#[serde(try_from = {b:?}, into = {b:?})]\n")
        } else {
            String::new()
        };
        let check = match &td.refinement {
            Some(r) => {
                let mut scope = Scope::default();
                scope.self_var = Some(Var { name: "self".into(), rust: "value".into(), ty: base.clone(), is_ref: true });
                self.eg().emit_bool(r, &scope)
            }
            None => "true".into(),
        };
        let name = &n.name;
        let into = if *base == Ty::Str { "impl Into<String>".to_string() } else { b.clone() };
        let conv = if *base == Ty::Str { "let value: String = value.into();" } else { "" };
        let _ = write!(
            self.out,
            r#"{doc}{derives}
{copy}{ord}{serde}#[repr(transparent)]
pub struct {name}{g}({b});

impl{g} {name}{g} {{
    pub const RULE: &'static str = {rule_lit};

    /// Validate and wrap a value.
    pub fn parse(value: {into}) -> Result<Self, InvalidValue> {{
        {conv}
        if !Self::check(&value) {{
            return Err(InvalidValue::new({id}, Self::RULE, {shown}));
        }}
        Ok(Self(value))
    }}

    pub fn check(value: &{b}) -> bool {{
        {check}
    }}

    pub fn into_inner(self) -> {b} {{
        self.0
    }}

    pub fn as_inner(&self) -> &{b} {{
        &self.0
    }}
}}

impl{g} std::ops::Deref for {name}{g} {{
    type Target = {b};
    fn deref(&self) -> &{b} {{
        &self.0
    }}
}}

impl{g} TryFrom<{b}> for {name}{g} {{
    type Error = InvalidValue;
    fn try_from(value: {b}) -> Result<Self, InvalidValue> {{
        Self::parse(value)
    }}
}}

impl{g} PartialEq<{b}> for {name}{g} {{
    fn eq(&self, other: &{b}) -> bool {{
        &self.0 == other
    }}
}}

impl{g} From<{name}{g}> for {b} {{
    fn from(v: {name}{g}) -> {b} {{
        v.0
    }}
}}
"#,
            rule_lit = esc(&rule),
            id = esc(&n.id),
            shown = if n.is_sensitive { "\"<redacted>\".to_string()" } else { "format!(\"{value:?}\")" },
        );
        if n.is_sensitive {
            self.redacted_debug(name, &g);
        }
        if *base == Ty::Str {
            let _ = write!(
                self.out,
                r#"
impl PartialEq<str> for {name} {{
    fn eq(&self, other: &str) -> bool {{
        self.0 == other
    }}
}}

impl PartialEq<&str> for {name} {{
    fn eq(&self, other: &&str) -> bool {{
        self.0 == *other
    }}
}}

impl TryFrom<&str> for {name} {{
    type Error = InvalidValue;
    fn try_from(value: &str) -> Result<Self, InvalidValue> {{
        Self::parse(value)
    }}
}}

impl AsRef<str> for {name} {{
    fn as_ref(&self) -> &str {{
        &self.0
    }}
}}
"#
            );
        }
        if matches!(base, Ty::Str | Ty::Int(_) | Ty::Float64 | Ty::Float32 | Ty::Bool | Ty::Char) {
            let _ = write!(
                self.out,
                "\nimpl std::fmt::Display for {name} {{\n    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {{\n        std::fmt::Display::fmt(&self.0, f)\n    }}\n}}\n"
            );
        }
        self.out.push('\n');
    }

    fn struct_(&mut self, n: &Node) {
        let NodeKind::Struct(info) = &n.kind else { return };
        let ItemKind::Struct(s) = &n.item.kind else { return };
        self.mark(&n.id);
        let names = self.names();
        let g = if info.generics.is_empty() { String::new() } else { format!("<{}>", info.generics.join(", ")) };
        let sensitive = n.is_sensitive || info.fields.iter().any(|f| f.is_sensitive);
        let tys: Vec<Ty> = info.fields.iter().map(|f| f.ty.clone()).collect();
        let doc = doc_comment(&n.prose, "");
        let _ = writeln!(self.out, "{doc}{}", derive_list(self.model, n, &tys, !sensitive));
        // deserialized values must satisfy the invariants too: go through a
        // shadow struct and `wh_check` (generic structs keep the plain derive)
        let checked_serde = !s.invariants.is_empty()
            && info.generics.is_empty()
            && (n.derives.iter().any(|d| d == "Deserialize") || crate::targets::is_wire_type(self.model, &n.id));
        let shadow = format!("WhDe{}", n.name);
        if checked_serde {
            let _ = writeln!(self.out, "#[serde(try_from = {})]", esc(&shadow));
        }
        let field_ty = |f: &wh_model::FieldInfo| {
            let boxed = matches!(&f.ty, Ty::Option(inner) if inner.named_id() == Some(n.id.as_str()));
            if boxed { format!("Option<Box<{}>>", n.name) } else { names.ty(&f.ty) }
        };
        let _ = writeln!(self.out, "pub struct {}{g} {{", n.name);
        for f in &info.fields {
            let _ = writeln!(self.out, "    pub {}: {},", rust_keyword_safe(&f.name), field_ty(f));
        }
        let _ = writeln!(self.out, "}}\n");
        if checked_serde {
            let fields: Vec<String> = info.fields.iter().map(|f| format!("    pub {}: {},", rust_keyword_safe(&f.name), field_ty(f))).collect();
            let moves: Vec<String> = info.fields.iter().map(|f| format!("{0}: s.{0}", rust_keyword_safe(&f.name))).collect();
            let _ = writeln!(
                self.out,
                "/// The serialized form of [`{name}`]; deserializing checks the invariants.\n#[doc(hidden)]\n#[derive(serde::Deserialize)]\npub struct {shadow} {{\n{}\n}}\n\nimpl TryFrom<{shadow}> for {name} {{\n    type Error = InvalidValue;\n    fn try_from(s: {shadow}) -> Result<Self, InvalidValue> {{\n        let v = Self {{ {} }};\n        v.wh_check()?;\n        Ok(v)\n    }}\n}}\n",
                fields.join("\n"),
                moves.join(", "),
                name = n.name
            );
        }
        if n.patterns.iter().any(|p| p == "Builder") {
            self.builder(n, s, info, &g);
        }
        if n.is_sensitive {
            self.redacted_debug(&n.name, &g);
        } else if sensitive {
            let _ = writeln!(self.out, "impl{g} std::fmt::Debug for {}{g} {{", n.name);
            let _ = writeln!(self.out, "    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {{");
            let mut chain = format!("        f.debug_struct({})", esc(&n.name));
            for fi in &info.fields {
                if fi.is_sensitive {
                    let _ = write!(chain, "\n            .field({}, &crate::wh_rt::Redacted)", esc(&fi.name));
                } else {
                    let _ = write!(chain, "\n            .field({}, &self.{})", esc(&fi.name), rust_keyword_safe(&fi.name));
                }
            }
            let _ = writeln!(self.out, "{chain}\n            .finish()\n    }}\n}}\n");
        }
        // invariants
        let mut checks = Vec::new();
        for inv in &s.invariants {
            let mut scope = Scope::default();
            scope.self_var = Some(Var { name: "self".into(), rust: "self".into(), ty: Ty::Named(n.id.clone(), vec![]), is_ref: true });
            let src = self.model.files[n.file].text[inv.span.range()].to_string();
            checks.push((self.eg().emit_bool(inv, &scope), src));
        }
        let _ = writeln!(self.out, "impl{g} {}{g} {{", n.name);
        let _ = writeln!(self.out, "    /// Check the struct's invariants (and those of structs it contains).");
        let _ = writeln!(self.out, "    pub fn wh_check(&self) -> Result<(), InvalidValue> {{");
        for f in &info.fields {
            if !needs_check(self.model, &f.ty, 0) {
                continue;
            }
            let fname = rust_keyword_safe(&f.name);
            let line = match &f.ty {
                Ty::Named(..) => format!("self.{fname}.wh_check()?;"),
                Ty::Option(_) => format!("if let Some(v) = &self.{fname} {{ v.wh_check()?; }}"),
                Ty::List(_) | Ty::Set(_) | Ty::SortedSet(_) | Ty::Array(..) => format!("for v in self.{fname}.iter() {{ v.wh_check()?; }}"),
                Ty::Dict(..) | Ty::SortedDict(..) => format!("for v in self.{fname}.values() {{ v.wh_check()?; }}"),
                _ => continue,
            };
            let _ = writeln!(self.out, "        {line}");
        }
        for (code, src) in &checks {
            let _ = writeln!(
                self.out,
                "        if !({code}) {{\n            return Err(InvalidValue::new({}, {}, format!(\"{{self:?}}\")));\n        }}",
                esc(&n.id),
                esc(src)
            );
        }
        let _ = writeln!(self.out, "        Ok(())\n    }}");
        if !checks.is_empty() {
            let params: Vec<String> =
                info.fields.iter().map(|f| format!("{}: {}", rust_keyword_safe(&f.name), names.ty(&f.ty))).collect();
            let inits: Vec<String> = info.fields.iter().map(|f| rust_keyword_safe(&f.name)).collect();
            let _ = writeln!(
                self.out,
                "\n    /// Construct and validate.\n    pub fn new({}) -> Result<Self, InvalidValue> {{\n        let v = Self {{ {} }};\n        v.wh_check()?;\n        Ok(v)\n    }}",
                params.join(", "),
                inits.join(", ")
            );
        }
        let _ = writeln!(self.out, "}}\n");
    }

    /// `pattern Builder`: a generated builder (no agent). Fields are set in
    /// any order; `build` fills in defaults, reports missing fields and
    /// checks the invariants.
    fn builder(&mut self, n: &Node, s: &ast::Struct, info: &wh_model::StructInfo, g: &str) {
        let names = self.names();
        let eg = self.eg();
        let name = &n.name;
        let b = format!("{name}Builder");
        let field_ty = |f: &wh_model::FieldInfo| {
            let boxed = matches!(&f.ty, Ty::Option(inner) if inner.named_id() == Some(n.id.as_str()));
            if boxed { format!("Option<Box<{name}>>") } else { names.ty(&f.ty) }
        };
        let mut out = format!("/// Builder for [`{name}`] (pattern `Builder`): set fields in any order, then [`{b}::build`].\n#[derive(Clone)]\npub struct {b}{g} {{\n");
        for f in &info.fields {
            let _ = writeln!(out, "    {}: Option<{}>,", rust_keyword_safe(&f.name), field_ty(f));
        }
        let nones: Vec<String> = info.fields.iter().map(|f| format!("{}: None", rust_keyword_safe(&f.name))).collect();
        let _ = writeln!(out, "}}\n\nimpl{g} Default for {b}{g} {{\n    fn default() -> Self {{\n        Self {{ {} }}\n    }}\n}}\n", nones.join(", "));
        let _ = writeln!(out, "impl{g} {name}{g} {{\n    /// Start building a `{name}`.\n    pub fn builder() -> {b}{g} {{\n        {b}::default()\n    }}\n}}\n");
        let _ = writeln!(out, "impl{g} {b}{g} {{");
        for f in &info.fields {
            let field = rust_keyword_safe(&f.name);
            let setter = if f.name == "build" { "with_build".to_string() } else { field.clone() };
            let _ = writeln!(out, "    pub fn {setter}(mut self, value: {}) -> Self {{\n        self.{field} = Some(value);\n        self\n    }}\n", field_ty(f));
        }
        let mut inits = Vec::new();
        for (f, af) in info.fields.iter().zip(&s.fields) {
            let field = rust_keyword_safe(&f.name);
            let missing = match &af.default {
                Some(d) => eg.emit_as(d, &f.ty, &Scope::default()),
                None => format!(
                    "return Err(InvalidValue::new({}, {}, String::new()))",
                    esc(&n.id),
                    esc(&format!("field `{}` is required", f.name))
                ),
            };
            inits.push(format!("            {field}: match self.{field} {{\n                Some(v) => v,\n                None => {missing},\n            }},"));
        }
        let _ = writeln!(
            out,
            "    /// Fill in defaults, then check the invariants.\n    pub fn build(self) -> Result<{name}{g}, InvalidValue> {{\n        let v = {name} {{\n{}\n        }};\n        v.wh_check()?;\n        Ok(v)\n    }}\n}}\n",
            inits.join("\n")
        );
        self.out.push_str(&out);
    }

    /// A state machine (spec §8): the state enum, its transition table and a
    /// total `step` function, with a tracing event per transition.
    fn machine(&mut self, n: &Node) {
        let NodeKind::Machine(info) = &n.kind else { return };
        let ItemKind::Machine(m) = &n.item.kind else { return };
        self.mark(&n.id);
        let names = self.names();
        let eg = self.eg();
        let name = &n.name;
        let text = &self.model.files[n.file].text;
        let tys: Vec<Ty> = info.states.variants.iter().flat_map(|v| v.items.iter().map(|i| i.ty.clone())).collect();
        let doc = doc_comment(&n.prose, "");
        let _ = writeln!(self.out, "{doc}{}", derive_list(self.model, n, &tys, true));
        if self.model.type_derives_copy(&Ty::Named(n.id.clone(), vec![])) && !n.derives.iter().any(|d| d == "Copy") {
            let _ = writeln!(self.out, "#[derive(Copy)]");
        }
        let _ = writeln!(self.out, "pub enum {name} {{");
        for v in &info.states.variants {
            if v.items.is_empty() {
                let _ = writeln!(self.out, "    {},", v.name);
            } else {
                let items: Vec<String> = v.items.iter().map(|i| names.ty(&i.ty)).collect();
                let _ = writeln!(self.out, "    {}({}),", v.name, items.join(", "));
            }
        }
        let _ = writeln!(self.out, "}}\n");
        let event = names.ty(&info.event);
        let effect = names.ty(&info.effect);
        let event_enum = self.model.enum_info(&info.event).cloned().unwrap_or_default();
        let event_path = info.event.named_id().map(|id| names.path(id)).unwrap_or_default();
        let lit = |s: &Option<String>| esc(s.as_deref().unwrap_or("_"));
        let mut table = String::new();
        for (t, at) in info.transitions.iter().zip(&m.transitions) {
            let src = |e: &Option<ast::Expr>| e.as_ref().map_or("None".to_string(), |e| format!("Some({})", esc(&text[e.span.range()])));
            let _ = writeln!(
                table,
                "        crate::wh_rt::Transition {{ id: {}, from: {}, event: {}, to: {}, guard: {}, emit: {} }},",
                esc(&t.id),
                lit(&t.from),
                lit(&t.event),
                esc(&t.to),
                src(&at.guard),
                src(&at.emit)
            );
        }
        let ignored: Vec<String> = info.ignores.iter().map(|i| format!("({}, {})", lit(&i.state), lit(&i.event))).collect();
        let states: Vec<String> = info.states.variants.iter().map(|v| esc(&v.name)).collect();
        let name_arms: Vec<String> = info
            .states
            .variants
            .iter()
            .map(|v| format!("            Self::{}{} => {},", v.name, if v.items.is_empty() { "" } else { "(..)" }, esc(&v.name)))
            .collect();
        let event_arms: Vec<String> = event_enum
            .variants
            .iter()
            .map(|v| format!("            {event_path}::{} {{ .. }} => {},", v.name, esc(&v.name)))
            .collect();
        // a state value built by a transition (or `initial`)
        let build = |target: &ast::Expr, scope: &Scope| -> String {
            let (sname, args): (String, Vec<&ast::Expr>) = match &target.kind {
                ExprKind::Call { func, args } => (func.callee_name().unwrap_or("").to_string(), args.iter().map(|a| &a.value).collect()),
                _ => (target.callee_name().unwrap_or("").to_string(), vec![]),
            };
            let slots = info.state(&sname).map(|v| v.items.iter().map(|i| i.ty.clone()).collect::<Vec<_>>()).unwrap_or_default();
            if args.is_empty() {
                return format!("{name}::{sname}");
            }
            let vals: Vec<String> = args.iter().zip(&slots).map(|(a, t)| eg.emit_as(a, t, scope)).collect();
            format!("{name}::{sname}({})", vals.join(", "))
        };
        let level = m.trace.as_ref().map_or("debug".to_string(), |l| l.name.clone());
        let mut arms = String::new();
        for (t, at) in info.transitions.iter().zip(&m.transitions) {
            let mut scope = Scope::default();
            let state_pat = match &t.from {
                None => "_".to_string(),
                Some(f) => {
                    let st = info.state(f);
                    let k = st.map_or(0, |v| v.items.len());
                    if k == 0 {
                        format!("{name}::{f}")
                    } else if t.from_binds.is_empty() {
                        format!("{name}::{f}(..)")
                    } else {
                        for (b, item) in t.from_binds.iter().zip(&st.unwrap().items) {
                            if b != "_" {
                                scope.var(b, &rust_keyword_safe(b), item.ty.clone(), false);
                            }
                        }
                        format!("{name}::{f}({})", t.from_binds.iter().map(|b| rust_keyword_safe(b)).collect::<Vec<_>>().join(", "))
                    }
                }
            };
            let event_pat = match &t.event {
                None => "_".to_string(),
                Some(e) => {
                    let v = event_enum.variants.iter().find(|v| &v.name == e);
                    let items = v.map(|v| v.items.clone()).unwrap_or_default();
                    if items.is_empty() {
                        format!("{event_path}::{e}")
                    } else if t.event_binds.is_empty() {
                        format!("{event_path}::{e} {{ .. }}")
                    } else {
                        for (b, item) in t.event_binds.iter().zip(&items) {
                            if b != "_" {
                                scope.var(b, &rust_keyword_safe(b), item.ty.clone(), false);
                            }
                        }
                        if items.iter().all(|i| i.name.is_some()) {
                            let fs: Vec<String> = items
                                .iter()
                                .zip(&t.event_binds)
                                .map(|(i, b)| format!("{}: {}", rust_keyword_safe(i.name.as_ref().unwrap()), rust_keyword_safe(b)))
                                .collect();
                            format!("{event_path}::{e} {{ {} }}", fs.join(", "))
                        } else {
                            format!("{event_path}::{e}({})", t.event_binds.iter().map(|b| rust_keyword_safe(b)).collect::<Vec<_>>().join(", "))
                        }
                    }
                }
            };
            let guard = at.guard.as_ref().map_or(String::new(), |g| format!(" if {}", eg.emit_bool(g, &scope)));
            let next = build(&at.to, &scope);
            let emitted = match &at.emit {
                Some(e) => format!("Some({})", eg.emit_as(e, &info.effect, &scope)),
                None => "None".to_string(),
            };
            let trace = if level == "off" {
                String::new()
            } else {
                format!(
                    "                tracing::{level}!(wh.node = {}, from = {}, event = {}, to = {}, \"transition\");\n",
                    esc(&t.id),
                    lit(&t.from),
                    lit(&t.event),
                    esc(&t.to)
                )
            };
            let _ = writeln!(arms, "            ({state_pat}, {event_pat}){guard} => {{\n{trace}                ({next}, {emitted})\n            }}");
        }
        let ignored_trace = if level == "off" {
            String::new()
        } else {
            format!(
                "                tracing::trace!(wh.node = {}, state = state.name(), event = Self::event_name(&event), \"no transition\");\n",
                esc(&n.id)
            )
        };
        let initial = m.initial.as_ref().map_or_else(|| "unreachable!()".to_string(), |e| build(e, &Scope::default()));
        let param = rust_keyword_safe(&info.event_param);
        let rebind = if param == "event" { String::new() } else { format!("        let event = {param};\n") };
        let _ = write!(
            self.out,
            r#"impl {name} {{
    /// The states, in the order they first appear.
    pub const STATES: &'static [&'static str] = &[{states}];
    /// The transition table, in matching order (`_` matches anything).
    pub const TRANSITIONS: &'static [crate::wh_rt::Transition] = &[
{table}    ];
    /// `(state, event)` pairs deliberately left alone (`ignore`).
    pub const IGNORED: &'static [(&'static str, &'static str)] = &[{ignored}];

    pub fn initial() -> Self {{
        {initial}
    }}

    /// Name of the current state.
    pub fn name(&self) -> &'static str {{
        match self {{
{name_arms}
        }}
    }}

    /// Name of an event's variant.
    pub fn event_name(event: &{event}) -> &'static str {{
        match event {{
{event_arms}
        }}
    }}

    /// Apply one event: the first matching transition whose guard holds
    /// fires; any other (ignored) pair keeps the state and emits nothing.
    pub fn step(state: {name}, {param}: {event}) -> ({name}, Option<{effect}>) {{
{rebind}        match (state, event) {{
{arms}            (state, event) => {{
{ignored_trace}                (state, None)
            }}
        }}
    }}
}}

"#,
            states = states.join(", "),
            ignored = ignored.join(", "),
            name_arms = name_arms.join("\n"),
            event_arms = event_arms.join("\n"),
        );
    }

    fn enum_(&mut self, n: &Node) {
        let NodeKind::Enum(info) = &n.kind else { return };
        self.mark(&n.id);
        let names = self.names();
        let g = if info.generics.is_empty() { String::new() } else { format!("<{}>", info.generics.join(", ")) };
        let tys: Vec<Ty> = info.variants.iter().flat_map(|v| v.items.iter().map(|i| i.ty.clone())).collect();
        let doc = doc_comment(&n.prose, "");
        let _ = writeln!(self.out, "{doc}{}", derive_list(self.model, n, &tys, !n.is_sensitive));
        // unit-only enums are passed by value (spec §5), so they must be Copy
        if info.variants.iter().all(|v| v.items.is_empty()) && !n.derives.iter().any(|d| d == "Copy") {
            let _ = writeln!(self.out, "#[derive(Copy)]");
        }
        let _ = writeln!(self.out, "pub enum {}{g} {{", n.name);
        for v in &info.variants {
            if v.items.is_empty() {
                let _ = writeln!(self.out, "    {},", v.name);
            } else if v.items.iter().all(|i| i.name.is_some()) {
                let fields: Vec<String> = v
                    .items
                    .iter()
                    .map(|i| format!("{}: {}", rust_keyword_safe(i.name.as_ref().unwrap()), names.ty(&i.ty)))
                    .collect();
                let _ = writeln!(self.out, "    {} {{ {} }},", v.name, fields.join(", "));
            } else {
                let items: Vec<String> = v.items.iter().map(|i| names.ty(&i.ty)).collect();
                let _ = writeln!(self.out, "    {}({}),", v.name, items.join(", "));
            }
        }
        let _ = writeln!(self.out, "}}\n");
        if n.is_sensitive {
            self.redacted_debug(&n.name, &g);
        }
        for v in &info.variants {
            if v.items.len() == 1 && v.items[0].is_from {
                let src = names.ty(&v.items[0].ty);
                let ctor = match &v.items[0].name {
                    Some(f) => format!("Self::{} {{ {}: e }}", v.name, rust_keyword_safe(f)),
                    None => format!("Self::{}(e)", v.name),
                };
                let _ = writeln!(
                    self.out,
                    "impl From<{src}> for {} {{\n    fn from(e: {src}) -> Self {{\n        {ctor}\n    }}\n}}\n",
                    n.name
                );
            }
        }
        if n.is_error_type {
            let mut arms = String::new();
            for v in &info.variants {
                let words = humanize(&v.name);
                let arm = match v.items.as_slice() {
                    [] => format!("Self::{} => f.write_str({}),", v.name, esc(&words)),
                    [one] if one.name.is_none() && one.ty == Ty::Str => format!("Self::{}(m) => write!(f, \"{}: {{m}}\"),", v.name, words),
                    [one] if one.name.is_none() && one.is_from && self.displays(&one.ty) => {
                        format!("Self::{}(e) => std::fmt::Display::fmt(e, f),", v.name)
                    }
                    [one] if one.name.is_none() && self.displays(&one.ty) => format!("Self::{}(x) => write!(f, \"{}: {{x}}\"),", v.name, words),
                    [one] if one.name.is_none() => format!("Self::{}(x) => write!(f, \"{}: {{x:?}}\"),", v.name, words),
                    _ => format!("other => write!(f, \"{{other:?}}\"),"),
                };
                let _ = writeln!(arms, "            {arm}");
            }
            let _ = writeln!(
                self.out,
                "impl std::fmt::Display for {name} {{\n    #[allow(unreachable_patterns)]\n    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {{\n        match self {{\n{arms}        }}\n    }}\n}}\n\nimpl std::error::Error for {name} {{}}\n",
                name = n.name
            );
        }
    }

    /// `Debug` for a `@sensitive` type: the value never reaches logs or traces.
    fn redacted_debug(&mut self, name: &str, g: &str) {
        let _ = writeln!(
            self.out,
            "impl{g} std::fmt::Debug for {name}{g} {{\n    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {{\n        std::fmt::Debug::fmt(&crate::wh_rt::Redacted, f)\n    }}\n}}\n"
        );
    }

    /// Does a type implement `Display` in generated code? (A `@sensitive`
    /// type does, but error messages use its redacted `Debug` instead.)
    fn displays(&self, t: &Ty) -> bool {
        if self.model.type_is_sensitive(t) {
            return false;
        }
        match t {
            Ty::Str | Ty::Int(_) | Ty::Float32 | Ty::Float64 | Ty::Bool | Ty::Char => true,
            Ty::Named(id, _) => self.model.nodes.get(id).is_some_and(|n| match &n.kind {
                NodeKind::Enum(_) => n.is_error_type,
                NodeKind::Refined { base, .. } => matches!(base, Ty::Str | Ty::Int(_) | Ty::Float32 | Ty::Float64 | Ty::Bool | Ty::Char),
                _ => false,
            }),
            _ => false,
        }
    }

    fn method_sig(&self, name: &str, sig: &SigInfo, receiver: &str) -> String {
        let names = self.names();
        let mut params = vec![receiver.to_string()];
        for p in &sig.params {
            params.push(format!("{}: {}", rust_keyword_safe(&p.name), names.param_ty(&p.ty, &p.mode)));
        }
        let asyncness = if sig.is_async { "async " } else { "" };
        format!("{asyncness}fn {}({}){}", rust_keyword_safe(name), params.join(", "), names.ret_suffix(&sig.ret, &sig.raises))
    }

    fn capability(&mut self, n: &Node) {
        let NodeKind::Capability(c) = &n.kind else { return };
        self.mark(&n.id);
        let doc = doc_comment(&n.prose, "");
        let _ = writeln!(self.out, "{doc}#[allow(async_fn_in_trait)]\npub trait {} {{", n.name);
        for m in &c.methods {
            let _ = writeln!(self.out, "    {};", self.method_sig(&m.name, &m.sig, "&self"));
        }
        let _ = writeln!(self.out, "}}\n");
    }

    pub(crate) fn hole_module_name(name: &str) -> String {
        let mut s = String::from("h_");
        for (i, c) in name.chars().enumerate() {
            if c.is_ascii_uppercase() {
                if i > 0 {
                    s.push('_');
                }
                s.push(c.to_ascii_lowercase());
            } else {
                s.push(c);
            }
        }
        s
    }

    fn adapter(&mut self, n: &Node) {
        let NodeKind::Adapter(a) = &n.kind else { return };
        self.mark(&n.id);
        let names = self.names();
        let hm = Self::hole_module_name(&n.name);
        let doc = doc_comment(&n.prose, "");
        let _ = writeln!(self.out, "{doc}pub struct {} {{", n.name);
        for f in &a.fields {
            let _ = writeln!(self.out, "    pub {}: {},", rust_keyword_safe(&f.name), names.ty(&f.ty));
        }
        let _ = writeln!(self.out, "    pub(crate) state: imp::{hm}::State,\n}}\n");
        let params: Vec<String> = a.fields.iter().map(|f| format!("{}: {}", rust_keyword_safe(&f.name), names.ty(&f.ty))).collect();
        let mut inits: Vec<String> = a.fields.iter().map(|f| rust_keyword_safe(&f.name)).collect();
        inits.push("state: Default::default()".into());
        let _ = writeln!(
            self.out,
            "impl {name} {{\n    pub fn new({}) -> Self {{\n        Self {{ {} }}\n    }}\n}}\n\nimpl std::fmt::Debug for {name} {{\n    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {{\n        f.write_str({})\n    }}\n}}\n",
            params.join(", "),
            inits.join(", "),
            esc(&n.name),
            name = n.name
        );
        let level = trace_level(n, "debug");
        let mut hole_fns = Vec::new();
        for cap in &a.implements {
            let NodeKind::Capability(c) = &self.model.nodes[cap].kind else { continue };
            let _ = writeln!(self.out, "impl {} for {} {{", names.path(cap), n.name);
            for m in &c.methods {
                let node_id = format!("{}.{}", n.id, m.name);
                if let Some(l) = &level {
                    let _ = writeln!(
                        self.out,
                        "    #[tracing::instrument(name = {}, level = {}, skip_all, fields(wh.node = {}))]",
                        esc(&node_id),
                        esc(l),
                        esc(&node_id)
                    );
                }
                let _ = writeln!(self.out, "    {} {{", self.method_sig(&m.name, &m.sig, "&self"));
                let args: Vec<String> = std::iter::once("self".to_string()).chain(m.sig.params.iter().map(|p| rust_keyword_safe(&p.name))).collect();
                let await_ = if m.sig.is_async { ".await" } else { "" };
                let _ = writeln!(self.out, "        let result = imp::{hm}::{}({}){await_};", rust_keyword_safe(&m.name), args.join(", "));
                if m.sig.raises.is_some() {
                    let _ = writeln!(self.out, "        if let Err(e) = &result {{\n            tracing::debug!(wh.node = {}, error = ?e, \"capability call failed\");\n        }}", esc(&node_id));
                }
                let _ = writeln!(self.out, "        result\n    }}");
                let mut hp = vec![format!("this: &{}", n.name)];
                for p in &m.sig.params {
                    hp.push(format!("{}: {}", rust_keyword_safe(&p.name), names.param_ty(&p.ty, &p.mode)));
                }
                let asyncness = if m.sig.is_async { "async " } else { "" };
                hole_fns.push(format!(
                    "pub {asyncness}fn {}({}){} {{\n    todo!(\"synthesize {node_id}\")\n}}",
                    rust_keyword_safe(&m.name),
                    hp.join(", "),
                    names.ret_suffix(&m.sig.ret, &m.sig.raises)
                ));
            }
            let _ = writeln!(self.out, "}}\n");
        }
        let template = format!(
            "use crate::prelude::*;\nuse crate::{}::*;\n\n/// Internal state of `{}`, created with `Default` by `{}::new`.\n#[derive(Default)]\npub struct State {{}}\n\n{}\n",
            module_ident(self.module),
            n.name,
            n.name,
            hole_fns.join("\n\n")
        );
        self.holes.push(HoleSpec {
            node: n.id.clone(),
            module_name: hm,
            path: self.opts.impl_root.join(&self.module.replace('.', "/")).join(format!("{}.rs", n.name)),
            template,
            signature: format!("adapter {}", n.name),
        });
    }

    fn def(&mut self, n: &Node) {
        let NodeKind::Def(info) = &n.kind else { return };
        let Some(def) = n.def() else { return };
        self.mark(&n.id);
        let names = self.names();
        let eg = self.eg();
        let sig = &info.sig;
        let fname = rust_keyword_safe(&n.name);
        let mut params: Vec<String> = names.cap_params(&sig.uses);
        for p in &sig.params {
            params.push(format!("{}: {}", rust_keyword_safe(&p.name), names.param_ty(&p.ty, &p.mode)));
        }
        let ret = names.ret_suffix(&sig.ret, &sig.raises);
        let asyncness = if sig.is_async { "async " } else { "" };
        let doc = doc_comment(&n.prose, "");
        let _ = write!(self.out, "{doc}");
        if let Some(level) = trace_level(n, "debug") {
            let recorded: Vec<String> = sig
                .params
                .iter()
                .map(|p| {
                    let value = if p.is_sensitive { "crate::wh_rt::Redacted".to_string() } else { rust_keyword_safe(&p.name) };
                    format!("{} = ?{value}", rust_keyword_safe(&p.name))
                })
                .collect();
            let mut fields = vec![format!("wh.node = {}", esc(&n.id))];
            fields.extend(recorded);
            let _ = writeln!(
                self.out,
                "#[tracing::instrument(name = {}, level = {}, skip_all, fields({}))]",
                esc(&n.id),
                esc(&level),
                fields.join(", ")
            );
        }
        let _ = writeln!(self.out, "pub {asyncness}fn {fname}({}){ret} {{", params.join(", "));

        // the scope contracts see: parameters (by their passing mode)
        let mut scope = Scope::default();
        for p in &sig.params {
            scope.var(&p.name, &rust_keyword_safe(&p.name), p.ty.clone(), names.param_is_ref(p));
        }
        let cfg = contracts_cfg(self.opts);
        let text = &self.model.files[n.file].text;
        // requires
        for c in &def.clauses {
            if let ClauseKind::Requires(e) = &c.kind {
                let _ = writeln!(
                    self.out,
                    "    #[cfg({cfg})]\n    if !({}) {{\n        crate::wh_rt::contract_violation({}, \"requires\", {});\n    }}",
                    eg.emit_bool(e, &scope),
                    esc(&n.id),
                    esc(&text[e.span.range()])
                );
            }
        }
        // pattern Memoize: a pure def gives equal results for equal inputs,
        // so results are cached per input (bounded; cleared when full)
        let memo = n.patterns.iter().any(|p| p == "Memoize");
        if memo {
            let key_tys: Vec<String> = sig.params.iter().map(|p| names.ty(&p.ty)).collect();
            let keys: Vec<String> = sig
                .params
                .iter()
                .map(|p| eg.owned(&Var { name: p.name.clone(), rust: rust_keyword_safe(&p.name), ty: p.ty.clone(), is_ref: names.param_is_ref(p) }))
                .collect();
            let _ = writeln!(
                self.out,
                "    static WH_MEMO: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<({}), {}>>> = std::sync::OnceLock::new();\n    let wh_key = ({});\n    let wh_memo = WH_MEMO.get_or_init(Default::default);\n    if let Some(hit) = wh_memo.lock().unwrap_or_else(|e| e.into_inner()).get(&wh_key).cloned() {{\n        tracing::trace!(\"memoized\");\n        return hit;\n    }}",
                key_tys.iter().map(|t| format!("{t},")).collect::<String>(),
                names.ret_ty(&sig.ret, &sig.raises),
                keys.iter().map(|k| format!("{k},")).collect::<String>()
            );
        }
        // own params referenced after the call are cloned first (debug only)
        let mut post_scope = scope.clone();
        let needs_post = def.clauses.iter().any(|c| matches!(c.kind, ClauseKind::Ensures(_) | ClauseKind::RaisesWhen { .. }));
        for p in &sig.params {
            if needs_post && (matches!(p.mode, ParamMode::Own) || matches!(p.mode, ParamMode::Mut)) {
                let v = format!("wh_pre_{}", p.name);
                let _ = writeln!(self.out, "    #[cfg({cfg})]\n    let {v} = {}.clone();", rust_keyword_safe(&p.name));
                if matches!(p.mode, ParamMode::Own) {
                    post_scope.vars.retain(|x| x.name != p.name);
                    post_scope.var(&p.name, &v, p.ty.clone(), false);
                }
                post_scope.old.push((p.name.clone(), v));
            }
        }
        // raises-when conditions are evaluated on the inputs
        let mut raises_checks = Vec::new();
        let mut rw_iter = info.raises_when.iter();
        for c in &def.clauses {
            if let ClauseKind::RaisesWhen { when, .. } = &c.kind {
                let rw = rw_iter.next();
                let var = format!("wh_raise_{}", raises_checks.len());
                let _ = writeln!(self.out, "    #[cfg({cfg})]\n    let {var} = {};", eg.emit_bool(when, &scope));
                let pat = rw.map_or("Err(_)".to_string(), |r| eg.err_pattern(&r.error, &r.variant));
                raises_checks.push((var, pat, text[c.span.range()].to_string()));
            }
        }
        let mut call_args: Vec<String> = sig.uses.iter().map(|c| cap_var(c)).collect();
        call_args.extend(sig.params.iter().map(|p| rust_keyword_safe(&p.name)));
        let await_ = if sig.is_async { ".await" } else { "" };
        let target = match &info.extern_path {
            Some(path) => path.clone(),
            None => {
                let hm = Self::hole_module_name(&n.name);
                format!("imp::{hm}::{fname}")
            }
        };
        let _ = writeln!(self.out, "    let result = {target}({}){await_};", call_args.join(", "));
        if !raises_checks.is_empty() || needs_post {
            let _ = writeln!(self.out, "    #[cfg({cfg})]\n    {{");
            for (var, pat, src) in &raises_checks {
                let _ = writeln!(
                    self.out,
                    "        if {var} && !matches!(&result, {pat}) {{\n            crate::wh_rt::contract_violation({}, \"raises\", {});\n        }}",
                    esc(&n.id),
                    esc(src)
                );
            }
            let ensures: Vec<&ast::Expr> = def
                .clauses
                .iter()
                .filter_map(|c| match &c.kind {
                    ClauseKind::Ensures(e) => Some(e),
                    _ => None,
                })
                .collect();
            if !ensures.is_empty() {
                let is_result = sig.raises.is_some();
                let mut es = post_scope.clone();
                es.result = Some(Var { name: "result".into(), rust: "result".into(), ty: sig.ret.clone(), is_ref: is_result });
                let indent = if is_result { "            " } else { "        " };
                if is_result {
                    let _ = writeln!(self.out, "        if let Ok(result) = &result {{");
                }
                for e in ensures {
                    let _ = writeln!(
                        self.out,
                        "{indent}if !({}) {{\n{indent}    crate::wh_rt::contract_violation({}, \"ensures\", {});\n{indent}}}",
                        eg.emit_bool(e, &es),
                        esc(&n.id),
                        esc(&text[e.span.range()])
                    );
                }
                if is_result {
                    let _ = writeln!(self.out, "        }}");
                }
            }
            let _ = writeln!(self.out, "    }}");
        }
        // struct results (and structs nested in them) must satisfy their invariants
        if let Ty::Named(id, _) = &sig.ret {
            if needs_check(self.model, &sig.ret, 0) && matches!(&self.model.nodes[id].kind, NodeKind::Struct(_)) {
                let r = if sig.raises.is_some() { "if let Ok(r) = &result { r.wh_check() } else { Ok(()) }" } else { "result.wh_check()" };
                let _ = writeln!(
                    self.out,
                    "    #[cfg({cfg})]\n    if let Err(e) = {r} {{\n        crate::wh_rt::contract_violation({}, \"invariant\", e.rule);\n    }}",
                    esc(&n.id)
                );
            }
        }
        // a hole may not leave a `mut` argument invalid either
        for p in sig.params.iter().filter(|p| p.mode == ParamMode::Mut && needs_check(self.model, &p.ty, 0)) {
            if matches!(&p.ty, Ty::Named(id, _) if matches!(&self.model.nodes[id].kind, NodeKind::Struct(_))) {
                let _ = writeln!(
                    self.out,
                    "    #[cfg({cfg})]\n    if let Err(e) = {}.wh_check() {{\n        crate::wh_rt::contract_violation({}, \"invariant\", e.rule);\n    }}",
                    rust_keyword_safe(&p.name),
                    esc(&n.id)
                );
            }
        }
        if sig.raises.is_some() {
            let _ = writeln!(self.out, "    if let Err(e) = &result {{\n        tracing::debug!(error = ?e, \"raised\");\n    }}");
        }
        if memo {
            let _ = writeln!(
                self.out,
                "    {{\n        let mut m = wh_memo.lock().unwrap_or_else(|e| e.into_inner());\n        if m.len() >= crate::wh_rt::MEMO_CAPACITY {{\n            m.clear();\n        }}\n        m.insert(wh_key, result.clone());\n    }}"
            );
        }
        let _ = writeln!(self.out, "    tracing::trace!(result = ?result);\n    result\n}}\n");

        if info.extern_path.is_none() {
            let mut hp: Vec<String> = names.cap_params(&sig.uses);
            for p in &sig.params {
                hp.push(format!("{}: {}", rust_keyword_safe(&p.name), names.param_ty(&p.ty, &p.mode)));
            }
            let template = format!(
                "use crate::prelude::*;\nuse crate::{}::*;\n\npub {asyncness}fn {fname}({}){ret} {{\n    todo!(\"synthesize {}\")\n}}\n",
                module_ident(self.module),
                hp.join(", "),
                n.id
            );
            let sig_text = format!("pub {asyncness}fn {fname}({}){ret}", hp.join(", "));
            self.holes.push(HoleSpec {
                node: n.id.clone(),
                module_name: Self::hole_module_name(&n.name),
                path: self.opts.impl_root.join(self.module.replace('.', "/")).join(format!("{}.rs", n.name)),
                template,
                signature: sig_text,
            });
        }
    }

    // -------------------------------------------------------------- flows

    fn flow(&mut self, id: &str, fn_name: &str, prose: &Option<String>, ast_flow: &ast::Flow, info: &FlowInfo, level: &str) {
        let names = self.names();
        let sig = &info.sig;
        let sync = sync_caps(self.model, info, &mut Vec::new());
        let mut params: Vec<String> = sig
            .uses
            .iter()
            .map(|c| {
                if sync.contains(c) {
                    format!("{}: &(impl {} + Sync)", cap_var(c), names.path(c))
                } else {
                    format!("{}: &impl {}", cap_var(c), names.path(c))
                }
            })
            .collect();
        for p in &sig.params {
            params.push(format!("{}: {}", rust_keyword_safe(&p.name), names.param_ty(&p.ty, &p.mode)));
        }
        let ret = names.ret_suffix(&sig.ret, &sig.raises);
        let asyncness = if sig.is_async { "async " } else { "" };
        let doc = doc_comment(prose, "");
        let traced = level != "off";
        let instrument = if traced {
            format!("#[tracing::instrument(name = {}, level = {}, skip_all, fields(wh.node = {}))]\n", esc(id), esc(level), esc(id))
        } else {
            String::new()
        };
        let _ = writeln!(self.out, "{doc}{instrument}pub {asyncness}fn {}({}){ret} {{", rust_keyword_safe(fn_name), params.join(", "));
        let mut fg = FlowGen {
            model: self.model,
            module: self.module,
            info,
            out: String::new(),
            vals: Vec::new(),
            step_vars: BTreeMap::new(),
            uses: count_uses(&ast_flow.body),
            next_step: 0,
            tmp: 0,
            spans: traced,
        };
        for p in &sig.params {
            fg.vals.push(FVal { name: p.name.clone(), rust: rust_keyword_safe(&p.name), ty: p.ty.clone(), is_ref: names.param_is_ref(p) });
        }
        let last = fg.stmts(&ast_flow.body, 1);
        let body = fg.out.clone();
        self.out.push_str(&body);
        if matches!(ast_flow.body.last().map(|s| &s.kind), Some(FlowStmtKind::Return(_))) {
            let _ = writeln!(self.out, "}}\n");
            return;
        }
        let value = match (&last, &sig.ret) {
            (_, Ty::Unit) => {
                if let Some(v) = &last {
                    if v.ty != Ty::Unit {
                        let _ = writeln!(self.out, "    let _ = {};", v.rust);
                    }
                }
                "()".to_string()
            }
            (Some(v), _) => fg.owned(v),
            (None, _) => "unreachable!()".into(),
        };
        let wrapped = if sig.raises.is_some() { format!("Ok({value})") } else { value };
        let _ = writeln!(self.out, "    {wrapped}\n}}\n");
    }
}

/// Capabilities a flow shares across the threads of a sync `parallel`
/// (its own, or one of the flows it calls): they must be `Sync`.
pub(crate) fn sync_caps(model: &Model, info: &FlowInfo, visiting: &mut Vec<String>) -> std::collections::BTreeSet<String> {
    let mut out = std::collections::BTreeSet::new();
    if !info.sig.is_async && info.steps.iter().any(|s| s.label == "parallel") {
        out.extend(info.sig.uses.iter().cloned());
    }
    for callee in info.steps.iter().filter_map(|s| s.callee.as_ref()) {
        if visiting.contains(callee) {
            continue;
        }
        if let Some(NodeKind::Flow(f)) = model.nodes.get(callee).map(|n| &n.kind) {
            visiting.push(callee.clone());
            out.extend(sync_caps(model, f, visiting));
            visiting.pop();
        }
    }
    out.retain(|c| info.sig.uses.contains(c));
    out
}

#[derive(Debug, Clone)]
struct FVal {
    name: String,
    rust: String,
    ty: Ty,
    is_ref: bool,
}

struct FlowGen<'a> {
    model: &'a Model,
    module: &'a str,
    info: &'a FlowInfo,
    out: String,
    vals: Vec<FVal>,
    step_vars: BTreeMap<usize, String>,
    /// how often each name is referenced in the flow body
    uses: BTreeMap<String, usize>,
    next_step: usize,
    tmp: usize,
    /// emit a `wh.step` span per step (off with `trace off`)
    spans: bool,
}

fn count_uses(stmts: &[FlowStmt]) -> BTreeMap<String, usize> {
    fn walk(e: &ast::Expr, m: &mut BTreeMap<String, usize>) {
        match &e.kind {
            ExprKind::Name(n) => *m.entry(n.clone()).or_default() += 1,
            ExprKind::Attr { base, .. } => walk(base, m),
            ExprKind::Call { func, args } => {
                walk(func, m);
                args.iter().for_each(|a| walk(&a.value, m));
            }
            ExprKind::Tuple(xs) | ExprKind::List(xs) => xs.iter().for_each(|x| walk(x, m)),
            _ => {}
        }
    }
    fn walk_stmts(ss: &[FlowStmt], m: &mut BTreeMap<String, usize>) {
        for s in ss {
            match &s.kind {
                FlowStmtKind::Bind { value, .. } | FlowStmtKind::Expr(value) | FlowStmtKind::Return(Some(value)) => {
                    walk(&value.head, m);
                    value.steps.iter().for_each(|x| walk(x, m));
                }
                FlowStmtKind::Return(None) => {}
                FlowStmtKind::Match { scrutinee, arms } => {
                    walk(scrutinee, m);
                    arms.iter().for_each(|a| walk_stmts(&a.body, m));
                }
            }
        }
    }
    let mut m = BTreeMap::new();
    walk_stmts(stmts, &mut m);
    m
}

impl<'a> FlowGen<'a> {
    fn names(&self) -> Names<'a> {
        Names::new(self.model, self.module)
    }

    fn owned(&self, v: &FVal) -> String {
        let eg = ExprGen::new(self.model, self.module);
        eg.owned(&Var { name: v.name.clone(), rust: v.rust.clone(), ty: v.ty.clone(), is_ref: v.is_ref })
    }

    fn lookup(&self, n: &str) -> Option<FVal> {
        self.vals.iter().rev().find(|v| v.name == n).cloned()
    }

    /// Pass a flow value to a parameter, moving when it is used only once.
    fn pass(&self, v: &FVal, p: &ParamInfo) -> String {
        let names = self.names();
        if names.is_copy(&p.ty) {
            return if v.is_ref { format!("*{}", v.rust) } else { v.rust.clone() };
        }
        match p.mode {
            ParamMode::Borrow => if v.is_ref { v.rust.clone() } else { format!("&{}", v.rust) },
            ParamMode::Mut => format!("&mut {}", v.rust),
            ParamMode::Own => {
                let single = self.uses.get(&v.name).copied().unwrap_or(0) <= 1;
                if !v.is_ref && (single || v.name.starts_with("wh_s")) { v.rust.clone() } else { self.owned(v) }
            }
        }
    }

    fn scope(&self) -> Scope {
        let mut s = Scope::default();
        for v in &self.vals {
            s.var(&v.name, &v.rust, v.ty.clone(), v.is_ref);
        }
        s.caps = ExprGen::caps_from_vars(&self.info.sig.uses, false);
        s
    }

    /// Emit a call step; returns its value.
    fn is_fn_call(&self, e: &ast::Expr) -> bool {
        match &e.kind {
            ExprKind::Call { func, .. } => func
                .callee_name()
                .and_then(|n| self.model.resolve_name(self.module, n))
                .is_some_and(|id| self.model.nodes[id].sig().is_some()),
            _ => false,
        }
    }

    fn step(&mut self, callee_expr: &ast::Expr, extra: &[ast::Arg], piped: Option<FVal>, indent: usize) -> FVal {
        // nested calls in arguments are steps of their own, numbered before this one (as in the model)
        let comb_name = callee_expr.callee_name().filter(|n| wh_model::check::FLOW_COMBINATORS.contains(n));
        let is_combinator = comb_name.is_some() && comb_name.and_then(|n| self.model.resolve_name(self.module, n)).is_none();
        let mut nested: Vec<Option<FVal>> = vec![None; extra.len()];
        if is_combinator && comb_name == Some("fold") {
            // the initial value may be a step of its own (numbered before the fold, as in the model)
            if let Some(i) = extra.iter().position(|a| a.name.is_none()) {
                if self.is_fn_call(&extra[i].value) {
                    nested[i] = Some(self.value(&extra[i].value, indent));
                }
            }
        }
        if !is_combinator {
            for (i, a) in extra.iter().enumerate() {
                if self.is_fn_call(&a.value) {
                    nested[i] = Some(self.value(&a.value, indent));
                }
            }
        }
        let idx = self.next_step;
        self.next_step += 1;
        let step = self.info.steps.get(idx).cloned();
        let pad = "    ".repeat(indent);
        let var = format!("wh_s{idx}");
        let Some(step) = step else {
            let _ = writeln!(self.out, "{pad}let {var} = unreachable!();");
            return FVal { name: var.clone(), rust: var, ty: Ty::Unknown, is_ref: false };
        };
        let span_open = if self.spans {
            format!("let _wh_span = tracing::debug_span!(\"wh.step\", wh.node = {}).entered();", esc(&step.id))
        } else {
            String::new()
        };
        // combinators
        if let Some((comb, fname)) = step.label.split_once(':') {
            let input = piped.unwrap_or(FVal { name: "_".into(), rust: "()".into(), ty: Ty::Unknown, is_ref: false });
            let callee = step.callee.as_ref().map(|c| self.names().path(c)).unwrap_or_else(|| fname.to_string());
            let sig = step.callee.as_ref().and_then(|c| self.model.nodes[c].sig().cloned());
            let raises = sig.as_ref().is_some_and(|s| s.raises.is_some());
            let caps: Vec<String> = sig.as_ref().map(|s| s.uses.iter().map(|c| cap_var(c)).collect()).unwrap_or_default();
            let await_ = if sig.as_ref().is_some_and(|s| s.is_async) { ".await" } else { "" };
            let elem_arg = |p: Option<&ParamInfo>, this: &Self| match p {
                Some(p) if matches!(p.mode, ParamMode::Own) => "x.clone()".to_string(),
                Some(p) if this.names().is_copy(&p.ty) => "*x".to_string(),
                _ => "x".to_string(),
            };
            let call_with = |args: Vec<String>| {
                let mut all = caps.clone();
                all.extend(args);
                format!("{callee}({}){await_}", all.join(", "))
            };
            let failed = format!(".inspect_err(|e| tracing::debug!(wh.node = {}, error = ?e, \"step failed\"))?", esc(&step.id));
            let try_ = if raises { failed.as_str() } else { "" };
            let first = sig.as_ref().and_then(|s| s.params.first().cloned());
            let iter = format!("{}.iter()", input.rust);
            let code = match comb {
                "map" | "filter" | "each" => {
                    let call = call_with(vec![elem_arg(first.as_ref(), self)]);
                    match comb {
                        "map" if raises => format!("{iter}.map(|x| {call}).collect::<Result<Vec<_>, _>>()?"),
                        "map" => format!("{iter}.map(|x| {call}).collect::<Vec<_>>()"),
                        "filter" => format!("{iter}.filter(|x| {{ let x = *x; {call} }}).cloned().collect::<Vec<_>>()"),
                        _ if raises => format!("{{ for x in {iter} {{ {call}?; }} }}"),
                        _ => format!("{{ for x in {iter} {{ {call}; }} }}"),
                    }
                }
                "flat_map" => {
                    let call = call_with(vec![elem_arg(first.as_ref(), self)]);
                    format!("{{\n{pad}    let mut out = Vec::new();\n{pad}    for x in {iter} {{\n{pad}        out.extend({call}{try_});\n{pad}    }}\n{pad}    out\n{pad}    }}")
                }
                "fold" => {
                    let params = sig.as_ref().map(|s| s.params.clone()).unwrap_or_default();
                    let acc_ty = params.first().map_or(Ty::Unknown, |p| p.ty.clone());
                    let init_i = extra.iter().position(|a| a.name.is_none()).unwrap_or(0);
                    let init = match (&nested[init_i], extra.get(init_i)) {
                        (Some(v), _) => self.owned(v),
                        (None, Some(a)) => match &a.value.kind {
                            ExprKind::Name(n) if self.lookup(n).is_some() => self.owned(&self.lookup(n).unwrap()),
                            _ => ExprGen::new(self.model, self.module).emit_as(&a.value, &acc_ty, &self.scope()),
                        },
                        _ => "Default::default()".into(),
                    };
                    let acc = match params.first() {
                        Some(p) if matches!(p.mode, ParamMode::Borrow) && !self.names().is_copy(&p.ty) => "&acc".to_string(),
                        Some(p) if matches!(p.mode, ParamMode::Mut) => "&mut acc".to_string(),
                        _ => "acc".to_string(),
                    };
                    let call = call_with(vec![acc, elem_arg(params.get(1), self)]);
                    format!("{{\n{pad}    let mut acc = {init};\n{pad}    for x in {iter} {{\n{pad}        acc = {call}{try_};\n{pad}    }}\n{pad}    acc\n{pad}    }}")
                }
                _ => {
                    // retry: every attempt gets the piped value; errors are retried
                    // `times` attempts in all, with an optional doubling backoff
                    let times = extra
                        .iter()
                        .find(|a| a.name.as_ref().is_some_and(|n| n.name == "times"))
                        .and_then(|a| match &a.value.kind {
                            ExprKind::Int(v) => v.parse::<u32>().ok(),
                            _ => None,
                        })
                        .unwrap_or(1);
                    let backoff = extra
                        .iter()
                        .find(|a| a.name.as_ref().is_some_and(|n| n.name == "backoff"))
                        .map(|a| ExprGen::new(self.model, self.module).emit(&a.value, &Scope::default()));
                    let arg = match (&first, input.name.as_str()) {
                        (None, _) | (_, "_") => vec![],
                        (Some(p), _) if matches!(p.mode, ParamMode::Own) || self.names().is_copy(&p.ty) => {
                            vec![if self.names().is_copy(&p.ty) && input.is_ref { format!("*{}", input.rust) } else { self.owned(&input) }]
                        }
                        (Some(_), _) => vec![if input.is_ref { input.rust.clone() } else { format!("&{}", input.rust) }],
                    };
                    let call = call_with(arg);
                    let sleep = backoff.map_or(String::new(), |b| format!("\n{pad}                std::thread::sleep({b} * 2u32.pow(attempt - 1));"));
                    format!(
                        "{{\n{pad}    let mut attempt = 1u32;\n{pad}    let r = loop {{\n{pad}        match {call} {{\n{pad}            Ok(v) => break Ok(v),\n{pad}            Err(e) if attempt < {times} => {{\n{pad}                tracing::debug!(wh.node = {}, attempt, error = ?e, \"retrying\");{sleep}\n{pad}                attempt += 1;\n{pad}            }}\n{pad}            Err(e) => break Err(e),\n{pad}        }}\n{pad}    }};\n{pad}    r{failed}\n{pad}    }}",
                        esc(&step.id)
                    )
                }
            };
            let _ = writeln!(self.out, "{pad}let {var} = {{\n{pad}    {span_open}\n{pad}    {code}\n{pad}}};");
            self.step_vars.insert(idx, var.clone());
            return FVal { name: var.clone(), rust: var, ty: step.output.clone(), is_ref: false };
        }
        let Some(callee) = step.callee.clone() else {
            return FVal { name: var.clone(), rust: var, ty: Ty::Unknown, is_ref: false };
        };
        let node = &self.model.nodes[&callee];
        let sig = node.sig().cloned().unwrap();
        let mut args: Vec<String> = sig.uses.iter().map(|c| cap_var(c)).collect();
        let mut params = sig.params.iter();
        if let Some(p) = &piped {
            if let Some(param) = params.next() {
                args.push(self.pass(p, param));
            }
        }
        let remaining: Vec<&ParamInfo> = params.collect();
        let scope = self.scope();
        let eg = ExprGen::new(self.model, self.module);
        let mut positional = extra.iter().enumerate().filter(|(_, a)| a.name.is_none());
        for param in remaining {
            let given = extra
                .iter()
                .enumerate()
                .find(|(_, a)| a.name.as_ref().is_some_and(|n| n.name == param.name))
                .or_else(|| positional.next());
            let code = match given {
                Some((i, _)) if nested[i].is_some() => self.pass(nested[i].as_ref().unwrap(), param),
                Some((_, a)) => match &a.value.kind {
                    ExprKind::Name(n) if self.lookup(n).is_some() => self.pass(&self.lookup(n).unwrap(), param),
                    _ => eg.pass(eg.emit_as(&a.value, &param.ty, &scope), param),
                },
                None => "Default::default()".into(),
            };
            args.push(code);
        }
        let path = self.names().path(&callee);
        let mut call = format!("{path}({})", args.join(", "));
        if sig.is_async {
            call.push_str(".await");
        }
        if sig.raises.is_some() {
            call = format!(
                "{call}.inspect_err(|e| tracing::debug!(wh.node = {}, error = ?e, \"step failed\"))?",
                esc(&step.id)
            );
        }
        if self.info.sig.is_async {
            let _ = writeln!(self.out, "{pad}let {var} = {call};");
        } else {
            let _ = writeln!(self.out, "{pad}let {var} = {{\n{pad}    {span_open}\n{pad}    {call}\n{pad}}};");
        }
        self.step_vars.insert(idx, var.clone());
        FVal { name: var.clone(), rust: var, ty: sig.ret.clone(), is_ref: false }
    }

    fn value(&mut self, e: &ast::Expr, indent: usize) -> FVal {
        match &e.kind {
            ExprKind::Name(n) if self.lookup(n).is_some() => self.lookup(n).unwrap(),
            ExprKind::Name(_) => self.step(e, &[], None, indent),
            ExprKind::Call { func, args }
                if func.callee_name() == Some("parallel") && self.model.resolve_name(self.module, "parallel").is_none() =>
            {
                self.parallel(args, indent)
            }
            ExprKind::Call { func, args } => {
                let is_fn = func
                    .callee_name()
                    .and_then(|n| self.model.resolve_name(self.module, n))
                    .is_some_and(|id| self.model.nodes[id].sig().is_some());
                if is_fn {
                    return self.step(func, args, None, indent);
                }
                self.expr_value(e, indent)
            }
            _ => self.expr_value(e, indent),
        }
    }

    /// `parallel(a(..), b(..))`: sync branches run on scoped threads (so the
    /// capabilities they use must be `Sync`), async ones are joined; each
    /// branch's error propagates after all of them finished.
    fn parallel(&mut self, args: &[ast::Arg], indent: usize) -> FVal {
        let pad = "    ".repeat(indent);
        let names = self.names();
        let err = self.info.sig.raises.as_ref().map(|t| names.ty(t));
        let is_async = self.info.sig.is_async;
        let mut branches = Vec::new();
        for a in args {
            let saved = std::mem::take(&mut self.out);
            let v = self.value(&a.value, indent + 3);
            let code = std::mem::replace(&mut self.out, saved);
            let value = if v.is_ref { self.owned(&v) } else { v.rust.clone() };
            let ty = names.ty(&v.ty);
            branches.push((code, value, ty));
        }
        let idx = self.next_step;
        self.next_step += 1;
        let var = format!("wh_s{idx}");
        let id = self.info.steps.get(idx).map(|s| s.id.clone()).unwrap_or_default();
        let out_ty = self.info.steps.get(idx).map_or(Ty::Unknown, |s| s.output.clone());
        let ok = |value: &str, ty: &str| match &err {
            Some(e) => format!("Ok::<{ty}, {e}>({value})"),
            None => value.to_string(),
        };
        let results: Vec<String> = (0..branches.len()).map(|i| format!("wh_r{i}")).collect();
        let joined = if is_async {
            let futs: Vec<String> = branches
                .iter()
                .map(|(code, value, ty)| format!("async {{\n{code}{pad}            {}\n{pad}        }}", ok(value, ty)))
                .collect();
            // join(a, join(b, c)) → (ra, (rb, rc))
            let mut fut = futs.last().cloned().unwrap_or_default();
            let mut pat = results.last().cloned().unwrap_or_default();
            for (f, r) in futs.iter().zip(&results).rev().skip(1) {
                fut = format!("crate::wh_rt::join({f}, {fut})");
                pat = format!("({r}, {pat})");
            }
            format!("{pad}    let {pat} = {fut}.await;\n")
        } else {
            let mut code = format!("{pad}    let wh_parent = tracing::Span::current();\n{pad}    let ({}) = std::thread::scope(|wh_scope| {{\n", results.join(", "));
            for (i, (body, value, ty)) in branches.iter().enumerate() {
                let ret = err.as_ref().map_or(ty.clone(), |e| format!("Result<{ty}, {e}>"));
                let _ = write!(
                    code,
                    "{pad}        let wh_h{i} = wh_scope.spawn(|| -> {ret} {{\n{pad}            let _wh_in = wh_parent.enter();\n{body}{pad}            {}\n{pad}        }});\n",
                    ok(value, ty)
                );
            }
            let joins: Vec<String> =
                (0..branches.len()).map(|i| format!("wh_h{i}.join().unwrap_or_else(|e| std::panic::resume_unwind(e))")).collect();
            let _ = write!(code, "{pad}        ({})\n{pad}    }});\n", joins.join(", "));
            code
        };
        let values: Vec<String> = results.iter().map(|r| if err.is_some() { format!("{r}?") } else { r.clone() }).collect();
        let span_open = if self.spans && !is_async {
            format!("{pad}    let _wh_span = tracing::debug_span!(\"wh.step\", wh.node = {}).entered();\n", esc(&id))
        } else {
            String::new()
        };
        let tuple = if values.len() == 1 { format!("({},)", values[0]) } else { format!("({})", values.join(", ")) };
        let _ = writeln!(self.out, "{pad}let {var} = {{\n{span_open}{joined}{pad}    {tuple}\n{pad}}};");
        self.step_vars.insert(idx, var.clone());
        FVal { name: var.clone(), rust: var, ty: out_ty, is_ref: false }
    }

    fn expr_value(&mut self, e: &ast::Expr, indent: usize) -> FVal {
        let eg = ExprGen::new(self.model, self.module);
        let scope = self.scope();
        let ty = eg.ty(e, &scope);
        let code = eg.emit(e, &scope);
        self.tmp += 1;
        let var = format!("wh_v{}", self.tmp);
        let _ = writeln!(self.out, "{}let {var} = {code};", "    ".repeat(indent));
        FVal { name: var.clone(), rust: var, ty, is_ref: false }
    }

    fn flow_expr(&mut self, fe: &ast::FlowExpr, indent: usize) -> FVal {
        let mut v = self.value(&fe.head, indent);
        for s in &fe.steps {
            v = match &s.kind {
                ExprKind::Call { func, args } => self.step(func, args, Some(v), indent),
                _ => self.step(s, &[], Some(v), indent),
            };
        }
        v
    }

    fn stmts(&mut self, stmts: &[FlowStmt], indent: usize) -> Option<FVal> {
        let pad = "    ".repeat(indent);
        let mut last = None;
        for s in stmts {
            last = None;
            match &s.kind {
                FlowStmtKind::Bind { targets, value } => {
                    let v = self.flow_expr(value, indent);
                    if targets.len() == 1 {
                        let name = rust_keyword_safe(&targets[0].name);
                        let _ = writeln!(self.out, "{pad}let {name} = {};", v.rust);
                        self.vals.push(FVal { name: targets[0].name.clone(), rust: name, ty: v.ty, is_ref: v.is_ref });
                    } else {
                        let ns: Vec<String> = targets.iter().map(|t| rust_keyword_safe(&t.name)).collect();
                        let _ = writeln!(self.out, "{pad}let ({}) = {};", ns.join(", "), v.rust);
                        let tys = match &v.ty {
                            Ty::Tuple(ts) => ts.clone(),
                            _ => vec![Ty::Unknown; targets.len()],
                        };
                        for (t, ty) in targets.iter().zip(tys) {
                            self.vals.push(FVal { name: t.name.clone(), rust: rust_keyword_safe(&t.name), ty, is_ref: false });
                        }
                    }
                }
                FlowStmtKind::Expr(fe) => last = Some(self.flow_expr(fe, indent)),
                FlowStmtKind::Return(v) => {
                    let value = match v {
                        Some(fe) => {
                            let v = self.flow_expr(fe, indent);
                            if self.info.sig.ret == Ty::Unit { "()".to_string() } else { self.owned(&v) }
                        }
                        None => "()".into(),
                    };
                    let wrapped = if self.info.sig.raises.is_some() { format!("Ok({value})") } else { value };
                    let _ = writeln!(self.out, "{pad}return {wrapped};");
                    return None;
                }
                FlowStmtKind::Match { scrutinee, arms } => {
                    let sv = self.value(scrutinee, indent);
                    // the match itself is a step in the model
                    let idx = self.next_step;
                    self.next_step += 1;
                    let out_ty = self.info.steps.get(idx).map(|s| s.output.clone()).unwrap_or(Ty::Unit);
                    let var = format!("wh_s{idx}");
                    let path = sv.ty.named_id().map(|id| self.names().path(id)).unwrap_or_default();
                    let en = self.model.enum_info(&sv.ty).cloned();
                    let _ = writeln!(self.out, "{pad}let {var} = match {} {{", sv.rust);
                    for arm in arms {
                        let (vname, binds): (String, Vec<String>) = match &arm.pattern.kind {
                            ExprKind::Name(n) => (n.clone(), vec![]),
                            ExprKind::Call { func, args } => (
                                func.callee_name().unwrap_or("").to_string(),
                                args.iter().map(|a| a.value.callee_name().unwrap_or("_").to_string()).collect(),
                            ),
                            _ => continue,
                        };
                        let variant = en.as_ref().and_then(|e| e.variants.iter().find(|v| v.name == vname)).cloned();
                        let saved = self.vals.len();
                        let pat = match &variant {
                            Some(v) if v.items.is_empty() => format!("{path}::{vname}"),
                            Some(v) if v.items.iter().all(|i| i.name.is_some()) => {
                                let fs: Vec<String> = v
                                    .items
                                    .iter()
                                    .zip(&binds)
                                    .map(|(i, b)| format!("{}: {}", rust_keyword_safe(i.name.as_ref().unwrap()), rust_keyword_safe(b)))
                                    .collect();
                                format!("{path}::{vname} {{ {} }}", fs.join(", "))
                            }
                            _ => format!("{path}::{vname}({})", binds.iter().map(|b| rust_keyword_safe(b)).collect::<Vec<_>>().join(", ")),
                        };
                        if let Some(v) = &variant {
                            for (b, item) in binds.iter().zip(&v.items) {
                                self.vals.push(FVal { name: b.clone(), rust: rust_keyword_safe(b), ty: item.ty.clone(), is_ref: sv.is_ref });
                            }
                        }
                        let _ = writeln!(self.out, "{pad}    {pat} => {{");
                        let v = self.stmts(&arm.body, indent + 2);
                        let val = match v {
                            Some(v) if out_ty != Ty::Unit => self.owned(&v),
                            _ => "()".into(),
                        };
                        let _ = writeln!(self.out, "{pad}        {val}\n{pad}    }}");
                        self.vals.truncate(saved);
                    }
                    let _ = writeln!(self.out, "{pad}}};");
                    last = Some(FVal { name: var.clone(), rust: var, ty: out_ty, is_ref: false });
                }
            }
        }
        last
    }
}

#[allow(dead_code)]
fn step_input_name(i: &StepInput) -> String {
    match i {
        StepInput::Param(p) => p.clone(),
        StepInput::Step(s) => format!("wh_s{s}"),
        StepInput::Other => "_".into(),
    }
}
