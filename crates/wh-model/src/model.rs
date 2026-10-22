//! The semantic model of a project: every definition resolved to a node with
//! resolved types, plus project-wide diagnostics.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use wh_syntax::ast::{self, Item, ItemKind, ParamMode, TypeArg, TypeExpr, TypeExprKind};
use wh_syntax::{Diagnostic, Span, parse_document};

use crate::ty::{IntTy, NodeId, Ty};

/// One `.wh.md` source given to the model.
#[derive(Debug, Clone)]
pub struct SourceInput {
    pub path: String,
    pub text: String,
}

#[derive(Debug, Clone)]
pub struct SourceFile {
    pub path: String,
    pub module: String,
    pub text: String,
    pub doc: ast::Document,
}

#[derive(Debug, Clone, Serialize)]
pub struct FileDiagnostic {
    pub file: usize,
    pub diag: Diagnostic,
}

#[derive(Debug, Clone, Serialize)]
pub struct ParamInfo {
    pub name: String,
    pub mode: ParamMode,
    pub ty: Ty,
    pub has_default: bool,
    /// The type as written (`UserId | None`); `ty` has aliases expanded.
    pub written: String,
    /// The alias the written type names (looking through `| None` and
    /// collection element types), e.g. `signup.UserId`.
    pub alias: Option<NodeId>,
    /// `sensitive email: Email`: redacted in traces.
    pub is_sensitive: bool,
}

impl ParamInfo {
    /// A parameter with no written type (synthesized by the compiler).
    pub fn synthetic(name: impl Into<String>, mode: ParamMode, ty: Ty, has_default: bool) -> Self {
        ParamInfo { name: name.into(), mode, written: ty.to_string(), ty, has_default, alias: None, is_sensitive: false }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SigInfo {
    pub params: Vec<ParamInfo>,
    pub ret: Ty,
    pub raises: Option<Ty>,
    pub uses: Vec<NodeId>,
    pub is_async: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct FieldInfo {
    pub name: String,
    pub ty: Ty,
    pub is_key: bool,
    pub is_sensitive: bool,
    pub has_default: bool,
    pub span: Span,
    /// The type as written (`UserId | None`); `ty` has aliases expanded.
    pub written: String,
    /// The alias the written type names (looking through `| None` and
    /// collection element types), e.g. `signup.UserId`.
    pub alias: Option<NodeId>,
}

#[derive(Debug, Clone, Serialize)]
pub struct StructInfo {
    pub generics: Vec<String>,
    pub fields: Vec<FieldInfo>,
    pub has_invariants: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct VariantItemInfo {
    pub name: Option<String>,
    pub ty: Ty,
    pub is_from: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct VariantInfo {
    pub name: String,
    pub items: Vec<VariantItemInfo>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct EnumInfo {
    pub generics: Vec<String>,
    pub variants: Vec<VariantInfo>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MethodInfo {
    pub name: String,
    pub sig: SigInfo,
}

#[derive(Debug, Clone, Serialize)]
pub struct CapabilityInfo {
    pub methods: Vec<MethodInfo>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AdapterInfo {
    pub implements: Vec<NodeId>,
    pub fields: Vec<FieldInfo>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RaisesWhen {
    pub error: Ty,
    pub variant: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DefInfo {
    pub sig: SigInfo,
    pub extern_path: Option<String>,
    pub raises_when: Vec<RaisesWhen>,
}

/// Where a flow step's input comes from.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum StepInput {
    Param(String),
    Step(usize),
    Other,
}

#[derive(Debug, Clone, Serialize)]
pub struct FlowStep {
    /// `module.flow#callee[k]`
    pub id: String,
    pub label: String,
    pub callee: Option<NodeId>,
    pub span: Span,
    pub inputs: Vec<StepInput>,
    pub output: Ty,
    pub raises: Option<Ty>,
    pub bindings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FlowInfo {
    pub sig: SigInfo,
    pub steps: Vec<FlowStep>,
    /// Index of the step whose value the flow returns.
    pub result: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProvideInfo {
    pub capability: NodeId,
    pub adapter: Option<NodeId>,
}

/// Where a routed flow's parameter comes from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum RouteSource {
    /// A `{name}` segment of the path.
    Path,
    /// The query string (`GET`, `DELETE`).
    Query,
    /// The JSON body: the value itself when it is the only body parameter,
    /// else an object keyed by parameter name.
    Body,
}

/// `route POST "/signup" -> register` of a `server` app.
#[derive(Debug, Clone, Serialize)]
pub struct RouteInfo {
    pub method: String,
    pub path: String,
    /// The flow or def handling the route.
    pub target: NodeId,
    /// One entry per parameter of the target, in order.
    pub params: Vec<(String, RouteSource)>,
    pub span: Span,
}

/// `status E.Variant = 404` (`variant: None` maps the whole error type).
#[derive(Debug, Clone, Serialize)]
pub struct StatusInfo {
    pub error: Ty,
    pub variant: Option<String>,
    pub code: u16,
}

#[derive(Debug, Clone, Serialize)]
pub struct AppInfo {
    pub targets: Vec<String>,
    pub provides: Vec<ProvideInfo>,
    pub entry: Option<FlowInfo>,
    /// The app's target profile (`None` when its `target` is invalid).
    pub target: Option<wh_targets::TargetProfile>,
    pub routes: Vec<RouteInfo>,
    pub statuses: Vec<StatusInfo>,
    /// `listen "127.0.0.1:3000"` (server apps).
    pub listen: Option<String>,
    /// Exported flows and defs (web apps).
    pub exports: Vec<NodeId>,
}

impl AppInfo {
    /// The target profile, `native(cli)` when none was resolved.
    pub fn profile(&self) -> wh_targets::TargetProfile {
        self.target.clone().unwrap_or_else(wh_targets::TargetProfile::native_cli)
    }
}

/// One `From on Event [if guard] -> To [emit effect]` line of a machine.
#[derive(Debug, Clone, Serialize)]
pub struct TransitionInfo {
    /// `module.Machine#From.Event[k]` (`_` for a wildcard), counted per pair.
    pub id: String,
    /// Source state, `None` for `_` (any state).
    pub from: Option<String>,
    /// Names bound by the source state's payload (`Locked(code)`), `_` = unbound.
    pub from_binds: Vec<String>,
    /// Event variant, `None` for `_` (any event).
    pub event: Option<String>,
    /// Names bound by the event's payload (`Unlock(c)`).
    pub event_binds: Vec<String>,
    pub to: String,
    pub has_guard: bool,
    pub has_emit: bool,
    pub span: Span,
}

/// `ignore State on Event` (`None` = `_`).
#[derive(Debug, Clone, Serialize)]
pub struct IgnoreInfo {
    pub state: Option<String>,
    pub event: Option<String>,
    pub span: Span,
}

/// A state machine (spec §8). Its states are inferred from the transitions
/// and become an enum named after the machine.
#[derive(Debug, Clone, Serialize)]
pub struct MachineInfo {
    pub event_param: String,
    /// The event enum.
    pub event: Ty,
    /// What a transition may emit (`unit` when the machine declares none).
    pub effect: Ty,
    /// The inferred state enum: one variant per state, payload types
    /// inferred from the values the transitions build.
    pub states: EnumInfo,
    pub initial: Option<String>,
    pub transitions: Vec<TransitionInfo>,
    pub ignores: Vec<IgnoreInfo>,
    /// States reachable from `initial`, with a shortest witness path
    /// (transition indices, unguarded transitions preferred).
    pub paths: BTreeMap<String, Vec<usize>>,
}

impl Default for MachineInfo {
    fn default() -> Self {
        MachineInfo {
            event_param: String::new(),
            event: Ty::Unknown,
            effect: Ty::Unit,
            states: EnumInfo::default(),
            initial: None,
            transitions: vec![],
            ignores: vec![],
            paths: BTreeMap::new(),
        }
    }
}

impl MachineInfo {
    pub fn state(&self, name: &str) -> Option<&VariantInfo> {
        self.states.variants.iter().find(|v| v.name == name)
    }
}

#[derive(Debug, Clone, Serialize)]
pub enum NodeKind {
    Alias(Ty),
    Refined { base: Ty, generics: Vec<String> },
    Struct(StructInfo),
    Enum(EnumInfo),
    Capability(CapabilityInfo),
    Adapter(AdapterInfo),
    Def(DefInfo),
    Flow(FlowInfo),
    Machine(MachineInfo),
    App(AppInfo),
}

impl NodeKind {
    pub fn label(&self) -> &'static str {
        match self {
            NodeKind::Alias(_) => "alias",
            NodeKind::Refined { .. } => "type",
            NodeKind::Struct(_) => "struct",
            NodeKind::Enum(_) => "enum",
            NodeKind::Capability(_) => "capability",
            NodeKind::Adapter(_) => "adapter",
            NodeKind::Def(_) => "def",
            NodeKind::Flow(_) => "flow",
            NodeKind::Machine(_) => "machine",
            NodeKind::App(_) => "app",
        }
    }

    pub fn is_type(&self) -> bool {
        matches!(self, NodeKind::Alias(_) | NodeKind::Refined { .. } | NodeKind::Struct(_) | NodeKind::Enum(_) | NodeKind::Machine(_))
    }

    /// The variants of an enum, or the states of a machine.
    pub fn enum_like(&self) -> Option<&EnumInfo> {
        match self {
            NodeKind::Enum(e) => Some(e),
            NodeKind::Machine(m) => Some(&m.states),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Node {
    pub id: NodeId,
    pub module: String,
    pub name: String,
    pub file: usize,
    pub span: Span,
    pub name_span: Span,
    pub block: usize,
    pub section: Option<usize>,
    /// Prose attached to the node (preceding paragraph and/or docstring).
    pub prose: Option<String>,
    pub derives: Vec<String>,
    pub patterns: Vec<String>,
    pub is_error_type: bool,
    /// `@sensitive` on a type: its values print as `<redacted>` in `Debug`
    /// output and traces.
    pub is_sensitive: bool,
    /// `@target(web, native)` on an adapter: the targets it runs on (empty:
    /// the default, see `TargetProfile::adapter_available`).
    pub available_on: Vec<String>,
    #[serde(skip)]
    pub item: Item,
    pub kind: NodeKind,
}

impl Node {
    pub fn sig(&self) -> Option<&SigInfo> {
        match &self.kind {
            NodeKind::Def(d) => Some(&d.sig),
            NodeKind::Flow(f) => Some(&f.sig),
            _ => None,
        }
    }

    /// A def without `extern` is a hole the compiler agent fills.
    pub fn is_hole(&self) -> bool {
        matches!(&self.kind, NodeKind::Def(d) if d.extern_path.is_none())
    }

    pub fn def(&self) -> Option<&ast::Def> {
        match &self.item.kind {
            ItemKind::Def(d) => Some(d),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct DependsInfo {
    pub crate_name: String,
    pub version: String,
    pub features: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct TestInfo {
    pub module: String,
    pub file: usize,
    pub test: ast::Test,
    pub span: Span,
    pub givens: Vec<ProvideInfo>,
}

#[derive(Debug, Clone, Default)]
pub struct ModuleScope {
    pub file: usize,
    /// Names visible in the module (own definitions and imports) → node id.
    pub names: BTreeMap<String, NodeId>,
    /// `import a.b as c` → `c` ↦ `a.b`
    pub module_aliases: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default)]
pub struct Model {
    pub files: Vec<SourceFile>,
    pub nodes: BTreeMap<NodeId, Node>,
    pub scopes: BTreeMap<String, ModuleScope>,
    pub depends: Vec<DependsInfo>,
    pub tests: Vec<TestInfo>,
    pub diagnostics: Vec<FileDiagnostic>,
}

pub const BUILTIN_TYPES: &[&str] = &[
    "bool", "int", "i8", "i16", "i32", "i64", "i128", "u8", "u16", "u32", "u64", "u128", "usize", "isize",
    "float", "f32", "f64", "str", "char", "bytes", "unit", "duration", "instant", "list", "dict", "sorted_dict",
    "set", "sorted_set", "tuple", "array", "Stream",
];

pub const KNOWN_TRAITS: &[&str] = &[
    "Eq", "Ord", "Hash", "Clone", "Copy", "Default", "Display", "Debug", "Serialize", "Deserialize", "Send", "Sync",
    "PartialEq", "PartialOrd",
];

pub fn module_name_for(path: &str, doc: &ast::Document) -> String {
    if let Some(m) = doc.front_matter.as_ref().and_then(|f| f.module.clone()) {
        return m;
    }
    let file = path.rsplit(['/', '\\']).next().unwrap_or(path);
    file.trim_end_matches(".md").trim_end_matches(".wh").to_string()
}

pub fn is_pascal(s: &str) -> bool {
    s.chars().next().is_some_and(|c| c.is_ascii_uppercase()) && !s.contains('_')
}

pub fn is_snake(s: &str) -> bool {
    let s = s.trim_start_matches('_');
    !s.is_empty() && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

impl Model {
    pub fn build(inputs: &[SourceInput]) -> Model {
        let mut m = Model::default();
        let wh_t = std::time::Instant::now();
        let prof = std::env::var("WH_PROF").is_ok();
        let lap = |l: &str| if prof { eprintln!("{l}: {:.2} ms", wh_t.elapsed().as_secs_f64() * 1e3) };
        for (i, input) in inputs.iter().enumerate() {
            let parsed = parse_document(&input.text);
            for d in parsed.diagnostics {
                m.diagnostics.push(FileDiagnostic { file: i, diag: d });
            }
            let module = module_name_for(&input.path, &parsed.doc);
            m.files.push(SourceFile { path: input.path.clone(), module, text: input.text.clone(), doc: parsed.doc });
        }
        lap("parse");
        m.collect();
        lap("collect");
        m.resolve_imports();
        lap("imports");
        m.resolve_kinds();
        lap("resolve");
        crate::check::check(&mut m);
        lap("check");
        m.diagnostics.sort_by_key(|d| (d.file, d.diag.span.start));
        m
    }

    pub fn error(&mut self, file: usize, msg: impl Into<String>, span: Span) {
        self.diagnostics.push(FileDiagnostic { file, diag: Diagnostic::error(msg, span) });
    }

    pub fn warning(&mut self, file: usize, msg: impl Into<String>, span: Span) {
        self.diagnostics.push(FileDiagnostic { file, diag: Diagnostic::warning(msg, span) });
    }

    pub fn has_errors(&self) -> bool {
        self.diagnostics.iter().any(|d| d.diag.severity == wh_syntax::Severity::Error)
    }

    pub fn node(&self, id: &str) -> Option<&Node> {
        self.nodes.get(id)
    }

    /// The node defined at (or containing) a byte offset of a file.
    pub fn node_at(&self, file: usize, offset: u32) -> Option<&Node> {
        self.nodes.values().filter(|n| n.file == file && n.span.contains(offset)).min_by_key(|n| n.span.len())
    }

    pub fn resolve_name(&self, module: &str, name: &str) -> Option<&NodeId> {
        self.scopes.get(module)?.names.get(name)
    }

    /// Does `[[target]]` in prose from `module` name something linkable:
    /// a definition, a module, or a capability/adapter method?
    pub fn resolves_ref(&self, from: &str, target: &str) -> bool {
        if self.resolve_path(from, target).is_some() || self.scopes.contains_key(target) {
            return true;
        }
        let Some((owner, method)) = target.rsplit_once('.') else { return false };
        let Some(id) = self.resolve_path(from, owner) else { return false };
        match &self.nodes[&id].kind {
            NodeKind::Capability(c) => c.methods.iter().any(|m| m.name == method),
            NodeKind::Adapter(a) => a.implements.iter().any(|cap| match &self.nodes[cap].kind {
                NodeKind::Capability(c) => c.methods.iter().any(|m| m.name == method),
                _ => false,
            }),
            _ => false,
        }
    }

    /// Resolve `name` or `module.name` from module `from`.
    pub fn resolve_path(&self, from: &str, path: &str) -> Option<NodeId> {
        if let Some(id) = self.resolve_name(from, path) {
            return Some(id.clone());
        }
        if self.nodes.contains_key(path) {
            return Some(path.to_string());
        }
        let (m, n) = path.rsplit_once('.')?;
        let scope = self.scopes.get(from)?;
        let real = scope.module_aliases.get(m).map(String::as_str).unwrap_or(m);
        let id = format!("{real}.{n}");
        self.nodes.contains_key(&id).then_some(id)
    }

    // ------------------------------------------------------------ collection

    fn collect(&mut self) {
        let mut nodes = BTreeMap::new();
        let mut diags = Vec::new();
        for (fi, f) in self.files.iter().enumerate() {
            let mut scope = ModuleScope { file: fi, ..Default::default() };
            if self.scopes.contains_key(&f.module) {
                diags.push((fi, format!("module `{}` is defined by more than one file", f.module), Span::new(0, 0)));
            }
            for (bi, b) in f.doc.blocks.iter().enumerate() {
                // block prose belongs to the block's first definition (not to imports/depends)
                let first_def = b.items.iter().position(|i| i.name().is_some());
                for (ii, item) in b.items.iter().enumerate() {
                    match &item.kind {
                        ItemKind::Depends(d) => self.depends.push(DependsInfo {
                            crate_name: d.crate_name.name.clone(),
                            version: d.version.clone(),
                            features: d.features.clone(),
                        }),
                        ItemKind::App(a) => {
                            for d in &a.depends {
                                self.depends.push(DependsInfo {
                                    crate_name: d.crate_name.name.clone(),
                                    version: d.version.clone(),
                                    features: d.features.clone(),
                                });
                            }
                        }
                        ItemKind::Test(t) => self.tests.push(TestInfo {
                            module: f.module.clone(),
                            file: fi,
                            test: t.clone(),
                            span: item.span,
                            givens: vec![],
                        }),
                        _ => {}
                    }
                    let Some(name) = item.name() else { continue };
                    let id = format!("{}.{}", f.module, name.name);
                    if nodes.contains_key(&id) {
                        diags.push((fi, format!("`{}` is defined more than once", name.name), name.span));
                        continue;
                    }
                    let mut prose_parts = Vec::new();
                    if Some(ii) == first_def {
                        if let Some(p) = b.prose {
                            prose_parts.push(f.doc.prose[p].text.clone());
                        }
                    }
                    if let Some(d) = item.docstring() {
                        prose_parts.push(d.to_string());
                    }
                    let mut derives = Vec::new();
                    let mut patterns = Vec::new();
                    let mut is_error_type = false;
                    let mut is_sensitive = false;
                    let mut available_on = Vec::new();
                    for d in &item.decorators {
                        match d.name.name.as_str() {
                            "derive" => derives.extend(d.args.iter().filter_map(|a| a.value.callee_name().map(String::from))),
                            "pattern" => patterns.extend(d.args.iter().filter_map(|a| pattern_name(&a.value))),
                            "error" => is_error_type = true,
                            "target" => {
                                for a in &d.args {
                                    match a.value.callee_name() {
                                        Some(l) if wh_targets::LABELS.contains(&l) => available_on.push(l.to_string()),
                                        _ => diags.push((fi, format!("`@target` takes {}", wh_targets::LABELS.join(", ")), a.value.span)),
                                    }
                                }
                                if !matches!(item.kind, ItemKind::Adapter(_)) {
                                    diags.push((fi, "`@target` applies to adapters".into(), d.name.span));
                                }
                            }
                            "sensitive" => {
                                is_sensitive = true;
                                if !matches!(item.kind, ItemKind::TypeDef(_) | ItemKind::Struct(_) | ItemKind::Enum(_)) {
                                    diags.push((fi, "`@sensitive` applies to refined types, structs and enums".into(), d.name.span));
                                }
                            }
                            other => diags.push((fi, format!("unknown decorator `@{other}`"), d.name.span)),
                        }
                    }
                    let clauses: &[ast::Clause] = match &item.kind {
                        ItemKind::Def(d) => &d.clauses,
                        ItemKind::Adapter(a) => &a.clauses,
                        _ => &[],
                    };
                    for c in clauses {
                        if let ast::ClauseKind::Pattern(t) = &c.kind {
                            if let TypeExprKind::Named { path, .. } = &t.kind {
                                patterns.push(path.last().unwrap().name.clone());
                            }
                        }
                    }
                    scope.names.insert(name.name.clone(), id.clone());
                    nodes.insert(id.clone(), Node {
                        id,
                        module: f.module.clone(),
                        name: name.name.clone(),
                        file: fi,
                        span: item.span,
                        name_span: name.span,
                        block: bi,
                        section: b.section,
                        prose: (!prose_parts.is_empty()).then(|| prose_parts.join("\n\n")),
                        derives,
                        patterns,
                        is_error_type,
                        is_sensitive,
                        available_on,
                        item: item.clone(),
                        kind: NodeKind::Machine(MachineInfo::default()), // placeholder until resolve_kinds
                    });
                }
            }
            self.scopes.insert(f.module.clone(), scope);
        }
        self.nodes = nodes;
        for (fi, msg, span) in diags {
            self.error(fi, msg, span);
        }
    }

    fn resolve_imports(&mut self) {
        let mut diags = Vec::new();
        let mut updates: Vec<(String, String, NodeId)> = Vec::new();
        let mut aliases: Vec<(String, String, String)> = Vec::new();
        for f in &self.files {
            for b in &f.doc.blocks {
                for item in &b.items {
                    let ItemKind::Import(imp) = &item.kind else { continue };
                    let fi = self.scopes[&f.module].file;
                    match imp {
                        ast::Import::From { module, names } => {
                            let m = module.iter().map(|i| i.name.as_str()).collect::<Vec<_>>().join(".");
                            if !self.scopes.contains_key(&m) {
                                diags.push((fi, format!("unknown module `{m}`"), module[0].span));
                                continue;
                            }
                            for n in names {
                                let id = format!("{m}.{}", n.name);
                                if !self.nodes.contains_key(&id) {
                                    diags.push((fi, format!("module `{m}` has no definition `{}`", n.name), n.span));
                                } else if n.name.starts_with('_') {
                                    diags.push((fi, format!("`{}` is private to module `{m}`", n.name), n.span));
                                } else {
                                    updates.push((f.module.clone(), n.name.clone(), id));
                                }
                            }
                        }
                        ast::Import::Module { module, alias } => {
                            let m = module.iter().map(|i| i.name.as_str()).collect::<Vec<_>>().join(".");
                            if !self.scopes.contains_key(&m) {
                                diags.push((fi, format!("unknown module `{m}`"), module[0].span));
                                continue;
                            }
                            let local = alias.as_ref().map_or_else(|| module.last().unwrap().name.clone(), |a| a.name.clone());
                            aliases.push((f.module.clone(), local, m));
                        }
                    }
                }
            }
        }
        for (module, name, id) in updates {
            self.scopes.get_mut(&module).unwrap().names.entry(name).or_insert(id);
        }
        for (module, local, real) in aliases {
            self.scopes.get_mut(&module).unwrap().module_aliases.insert(local, real);
        }
        for (fi, msg, span) in diags {
            self.error(fi, msg, span);
        }
    }

    // ------------------------------------------------------------ types

    /// Resolve a type expression written in `module`.
    pub fn resolve_ty(&self, module: &str, t: &TypeExpr, generics: &[String], diags: &mut Vec<(String, Span)>) -> Ty {
        self.resolve_ty_depth(module, t, generics, diags, 0)
    }

    fn resolve_ty_depth(
        &self,
        module: &str,
        t: &TypeExpr,
        generics: &[String],
        diags: &mut Vec<(String, Span)>,
        depth: u32,
    ) -> Ty {
        match &t.kind {
            TypeExprKind::Error => Ty::Unknown,
            TypeExprKind::Optional(inner) => {
                Ty::Option(Box::new(self.resolve_ty_depth(module, inner, generics, diags, depth)))
            }
            TypeExprKind::Fn { params, ret } => Ty::Fn(
                params.iter().map(|p| self.resolve_ty_depth(module, p, generics, diags, depth)).collect(),
                Box::new(self.resolve_ty_depth(module, ret, generics, diags, depth)),
            ),
            TypeExprKind::Named { path, args } => {
                let mut targs = Vec::new();
                let mut consts = Vec::new();
                for a in args {
                    match a {
                        TypeArg::Type(t) => targs.push(self.resolve_ty_depth(module, t, generics, diags, depth)),
                        TypeArg::Const(e) => {
                            if let ast::ExprKind::Int(v) = &e.kind {
                                consts.push(v.parse::<u64>().unwrap_or(0));
                            }
                        }
                    }
                }
                let name = &path.last().unwrap().name;
                let arity = |n: usize, diags: &mut Vec<(String, Span)>| {
                    if targs.len() != n {
                        diags.push((format!("`{name}` takes {n} type argument(s), found {}", targs.len()), t.span));
                        false
                    } else {
                        true
                    }
                };
                if path.len() == 1 {
                    if generics.contains(name) {
                        return Ty::Param(name.clone());
                    }
                    if let Some(i) = IntTy::from_name(name) {
                        return Ty::Int(i);
                    }
                    let simple = match name.as_str() {
                        "bool" => Some(Ty::Bool),
                        "float" | "f64" => Some(Ty::Float64),
                        "f32" => Some(Ty::Float32),
                        "str" => Some(Ty::Str),
                        "char" => Some(Ty::Char),
                        "bytes" => Some(Ty::Bytes),
                        "unit" => Some(Ty::Unit),
                        "duration" => Some(Ty::Duration),
                        "instant" => Some(Ty::Instant),
                        _ => None,
                    };
                    if let Some(s) = simple {
                        return s;
                    }
                    let b = |t: &Ty| Box::new(t.clone());
                    match name.as_str() {
                        "list" => return if arity(1, diags) { Ty::List(b(&targs[0])) } else { Ty::Unknown },
                        "set" => return if arity(1, diags) { Ty::Set(b(&targs[0])) } else { Ty::Unknown },
                        "sorted_set" => return if arity(1, diags) { Ty::SortedSet(b(&targs[0])) } else { Ty::Unknown },
                        "Stream" => return if arity(1, diags) { Ty::Stream(b(&targs[0])) } else { Ty::Unknown },
                        "dict" => {
                            return if arity(2, diags) { Ty::Dict(b(&targs[0]), b(&targs[1])) } else { Ty::Unknown };
                        }
                        "sorted_dict" => {
                            return if arity(2, diags) { Ty::SortedDict(b(&targs[0]), b(&targs[1])) } else { Ty::Unknown };
                        }
                        "tuple" => return Ty::Tuple(targs),
                        "array" => {
                            if targs.len() != 1 || consts.len() != 1 {
                                diags.push(("`array` takes a type and a length: `array[T, N]`".into(), t.span));
                                return Ty::Unknown;
                            }
                            return Ty::Array(b(&targs[0]), consts[0]);
                        }
                        _ => {}
                    }
                }
                let full = path.iter().map(|i| i.name.as_str()).collect::<Vec<_>>().join(".");
                let Some(id) = self.resolve_path(module, &full) else {
                    diags.push((format!("unknown type `{full}`"), t.span));
                    return Ty::Unknown;
                };
                let node = &self.nodes[&id];
                match &node.item.kind {
                    ItemKind::Alias(a) => {
                        if depth > 16 {
                            diags.push((format!("type alias `{}` is cyclic", node.name), t.span));
                            return Ty::Unknown;
                        }
                        let params: Vec<String> = a.generics.iter().map(|g| g.name.name.clone()).collect();
                        let mut inner_diags = Vec::new();
                        let ty = self.resolve_ty_depth(&node.module, &a.ty, &params, &mut inner_diags, depth + 1);
                        ty.substitute(&params, &targs)
                    }
                    ItemKind::TypeDef(_) | ItemKind::Struct(_) | ItemKind::Enum(_) | ItemKind::Capability(_)
                    | ItemKind::Adapter(_) | ItemKind::Machine(_) => {
                        let expected = generic_count(&node.item);
                        if expected != targs.len() {
                            diags.push((
                                format!("`{}` takes {expected} type argument(s), found {}", node.name, targs.len()),
                                t.span,
                            ));
                        }
                        Ty::Named(id, targs)
                    }
                    _ => {
                        diags.push((format!("`{full}` is a {}, not a type", item_label(&node.item)), t.span));
                        Ty::Unknown
                    }
                }
            }
        }
    }

    /// The alias a written type names, looking through `T | None` and the
    /// element types of `list`/`set`/`sorted_set` (and dict values), so that
    /// `guardian: UserId | None` still refers to `UserId`.
    pub fn written_alias(&self, module: &str, t: &TypeExpr) -> Option<NodeId> {
        match &t.kind {
            TypeExprKind::Optional(inner) => self.written_alias(module, inner),
            TypeExprKind::Named { path, args } => {
                let full = path.iter().map(|i| i.name.as_str()).collect::<Vec<_>>().join(".");
                if path.len() == 1 && matches!(full.as_str(), "list" | "set" | "sorted_set" | "dict" | "sorted_dict") {
                    return match args.last() {
                        Some(TypeArg::Type(inner)) => self.written_alias(module, inner),
                        _ => None,
                    };
                }
                let id = self.resolve_path(module, &full)?;
                matches!(self.nodes.get(&id)?.item.kind, ItemKind::Alias(_)).then_some(id)
            }
            _ => None,
        }
    }

    /// `RegisterError` or `RegisterError.TooYoung` in a `raises` position.
    pub fn resolve_error_ref(&self, module: &str, t: &TypeExpr, diags: &mut Vec<(String, Span)>) -> (Ty, Option<String>) {
        if let TypeExprKind::Named { path, args } = &t.kind {
            if path.len() == 2 && args.is_empty() {
                if let Some(id) = self.resolve_name(module, &path[0].name) {
                    if let NodeKind::Enum(e) = &self.nodes[id].kind {
                        let v = &path[1].name;
                        if e.variants.iter().any(|x| &x.name == v) {
                            return (Ty::Named(id.clone(), vec![]), Some(v.clone()));
                        }
                        diags.push((format!("`{}` has no variant `{v}`", path[0].name), path[1].span));
                        return (Ty::Named(id.clone(), vec![]), None);
                    }
                }
            }
        }
        (self.resolve_ty(module, t, &[], diags), None)
    }

    fn resolve_sig(&self, module: &str, sig: &ast::Signature, is_async: bool, outer: &[String], diags: &mut Vec<(String, Span)>) -> SigInfo {
        let mut generics: Vec<String> = outer.to_vec();
        generics.extend(sig.generics.iter().map(|g| g.name.name.clone()));
        let params = sig
            .params
            .iter()
            .map(|p| ParamInfo {
                name: p.name.name.clone(),
                mode: p.mode.clone(),
                ty: self.resolve_ty(module, &p.ty, &generics, diags),
                has_default: p.default.is_some(),
                written: p.ty.to_string(),
                alias: self.written_alias(module, &p.ty),
                is_sensitive: p.is_sensitive,
            })
            .collect();
        let ret = sig.ret.as_ref().map_or(Ty::Unit, |t| self.resolve_ty(module, t, &generics, diags));
        let raises = sig.raises.as_ref().map(|t| self.resolve_ty(module, t, &generics, diags));
        let mut uses = Vec::new();
        for u in &sig.uses {
            match self.resolve_name(module, &u.name) {
                Some(id) if matches!(self.nodes[id].item.kind, ItemKind::Capability(_)) => uses.push(id.clone()),
                Some(_) => diags.push((format!("`{}` is not a capability", u.name), u.span)),
                None => diags.push((format!("unknown capability `{}`", u.name), u.span)),
            }
        }
        SigInfo { params, ret, raises, uses, is_async }
    }

    fn resolve_fields(&self, module: &str, fields: &[ast::Field], generics: &[String], diags: &mut Vec<(String, Span)>) -> Vec<FieldInfo> {
        fields
            .iter()
            .map(|f| FieldInfo {
                name: f.name.name.clone(),
                ty: self.resolve_ty(module, &f.ty, generics, diags),
                is_key: f.is_key,
                is_sensitive: f.is_sensitive,
                has_default: f.default.is_some(),
                span: f.span,
                written: f.ty.to_string(),
                alias: self.written_alias(module, &f.ty),
            })
            .collect()
    }

    fn resolve_kinds(&mut self) {
        let ids: Vec<NodeId> = self.nodes.keys().cloned().collect();
        let mut kinds: Vec<(NodeId, NodeKind, Vec<(String, Span)>)> = Vec::new();
        // first pass: everything except flows and apps (whose typing needs other nodes' kinds)
        for id in &ids {
            let node = &self.nodes[id];
            let module = node.module.clone();
            let mut d = Vec::new();
            let kind = match &node.item.kind {
                ItemKind::Alias(a) => {
                    let params: Vec<String> = a.generics.iter().map(|g| g.name.name.clone()).collect();
                    NodeKind::Alias(self.resolve_ty(&module, &a.ty, &params, &mut d))
                }
                ItemKind::TypeDef(t) => {
                    let generics: Vec<String> = t.generics.iter().map(|g| g.name.name.clone()).collect();
                    NodeKind::Refined { base: self.resolve_ty(&module, &t.base, &generics, &mut d), generics }
                }
                ItemKind::Struct(s) => {
                    let generics: Vec<String> = s.generics.iter().map(|g| g.name.name.clone()).collect();
                    let fields = self.resolve_fields(&module, &s.fields, &generics, &mut d);
                    NodeKind::Struct(StructInfo { generics, fields, has_invariants: !s.invariants.is_empty() })
                }
                ItemKind::Enum(e) => {
                    let generics: Vec<String> = e.generics.iter().map(|g| g.name.name.clone()).collect();
                    let variants = e
                        .variants
                        .iter()
                        .map(|v| VariantInfo {
                            name: v.name.name.clone(),
                            items: v
                                .items
                                .iter()
                                .map(|it| VariantItemInfo {
                                    name: it.name.as_ref().map(|n| n.name.clone()),
                                    ty: self.resolve_ty(&module, &it.ty, &generics, &mut d),
                                    is_from: it.is_from,
                                })
                                .collect(),
                        })
                        .collect();
                    NodeKind::Enum(EnumInfo { generics, variants })
                }
                ItemKind::Capability(c) => {
                    let generics: Vec<String> = c.generics.iter().map(|g| g.name.name.clone()).collect();
                    NodeKind::Capability(CapabilityInfo {
                        methods: c
                            .methods
                            .iter()
                            .map(|m| MethodInfo {
                                name: m.sig.name.name.clone(),
                                sig: self.resolve_sig(&module, &m.sig, m.is_async, &generics, &mut d),
                            })
                            .collect(),
                    })
                }
                ItemKind::Adapter(a) => {
                    let mut implements = Vec::new();
                    for t in &a.implements {
                        match self.resolve_ty(&module, t, &[], &mut d) {
                            Ty::Named(id, _) if matches!(self.nodes[&id].item.kind, ItemKind::Capability(_)) => implements.push(id),
                            Ty::Unknown => {}
                            other => d.push((format!("`{other}` is not a capability"), t.span)),
                        }
                    }
                    NodeKind::Adapter(AdapterInfo { implements, fields: self.resolve_fields(&module, &a.fields, &[], &mut d) })
                }
                ItemKind::Def(def) => {
                    let sig = self.resolve_sig(&module, &def.sig, def.is_async, &[], &mut d);
                    NodeKind::Def(DefInfo { sig, extern_path: def.extern_path.clone(), raises_when: vec![] })
                }
                ItemKind::Flow(f) => {
                    let sig = self.resolve_sig(&module, &f.sig, false, &[], &mut d);
                    NodeKind::Flow(FlowInfo { sig, steps: vec![], result: None })
                }
                // states are inferred by the checker (it needs expression types)
                ItemKind::Machine(_) => NodeKind::Machine(MachineInfo::default()),
                ItemKind::App(_) => NodeKind::App(AppInfo {
                    targets: vec![],
                    provides: vec![],
                    entry: None,
                    target: None,
                    routes: vec![],
                    statuses: vec![],
                    listen: None,
                    exports: vec![],
                }),
                _ => continue,
            };
            kinds.push((id.clone(), kind, d));
        }
        for (id, kind, d) in kinds {
            let file = self.nodes[&id].file;
            self.nodes.get_mut(&id).unwrap().kind = kind;
            for (msg, span) in d {
                self.error(file, msg, span);
            }
        }
        // raises-when clauses need enum kinds
        for id in &ids {
            let node = &self.nodes[id];
            let ItemKind::Def(def) = &node.item.kind else { continue };
            let mut d = Vec::new();
            let mut rw = Vec::new();
            for c in &def.clauses {
                if let ast::ClauseKind::RaisesWhen { error, .. } = &c.kind {
                    let (ty, variant) = self.resolve_error_ref(&node.module, error, &mut d);
                    rw.push(RaisesWhen { error: ty, variant });
                }
            }
            let file = node.file;
            if let NodeKind::Def(info) = &mut self.nodes.get_mut(id).unwrap().kind {
                info.raises_when = rw;
            }
            for (msg, span) in d {
                self.error(file, msg, span);
            }
        }
    }

    // ------------------------------------------------------------ queries

    pub fn struct_fields(&self, ty: &Ty) -> Option<Vec<FieldInfo>> {
        let Ty::Named(id, args) = ty else { return None };
        match &self.nodes.get(id)?.kind {
            NodeKind::Struct(s) => Some(
                s.fields.iter().map(|f| FieldInfo { ty: f.ty.substitute(&s.generics, args), ..f.clone() }).collect(),
            ),
            NodeKind::Adapter(a) => Some(a.fields.clone()),
            _ => None,
        }
    }

    pub fn refined_base(&self, ty: &Ty) -> Option<Ty> {
        let Ty::Named(id, args) = ty else { return None };
        match &self.nodes.get(id)?.kind {
            NodeKind::Refined { base, generics } => Some(base.substitute(generics, args)),
            _ => None,
        }
    }

    /// Variants of an enum type, or the states of a machine type.
    pub fn enum_info(&self, ty: &Ty) -> Option<&EnumInfo> {
        let Ty::Named(id, _) = ty else { return None };
        self.nodes.get(id)?.kind.enum_like()
    }

    pub fn machine_info(&self, ty: &Ty) -> Option<&MachineInfo> {
        let Ty::Named(id, _) = ty else { return None };
        match &self.nodes.get(id)?.kind {
            NodeKind::Machine(m) => Some(m),
            _ => None,
        }
    }

    /// Can a value raising `from` propagate (`?`) into a context raising `into`?
    pub fn converts_into(&self, from: &Ty, into: &Ty) -> bool {
        if from.is_unknown() || into.is_unknown() || from == into {
            return true;
        }
        self.enum_info(into)
            .is_some_and(|e| e.variants.iter().any(|v| v.items.len() == 1 && v.items[0].is_from && &v.items[0].ty == from))
    }

    /// Capabilities used transitively by a def or flow.
    pub fn uses_of(&self, id: &str) -> BTreeSet<NodeId> {
        let mut out = BTreeSet::new();
        if let Some(n) = self.nodes.get(id) {
            match &n.kind {
                NodeKind::Def(d) => out.extend(d.sig.uses.iter().cloned()),
                NodeKind::Flow(f) => out.extend(f.sig.uses.iter().cloned()),
                _ => {}
            }
        }
        out
    }

    /// Adapters implementing a capability.
    pub fn adapters_of(&self, cap: &str) -> Vec<&Node> {
        self.nodes
            .values()
            .filter(|n| matches!(&n.kind, NodeKind::Adapter(a) if a.implements.iter().any(|c| c == cap)))
            .collect()
    }

    /// The struct whose `key` field has type `ty` (for references).
    pub fn key_owner(&self, ty: &Ty) -> Option<&Node> {
        self.nodes.values().find(|n| match &n.kind {
            NodeKind::Struct(s) => s.fields.iter().any(|f| f.is_key && &f.ty == ty),
            _ => false,
        })
    }

    /// The struct whose `key` field is written with the alias `alias_id`
    /// (`key id: UserId`), so that every `UserId` refers to it even though
    /// the alias expands to a plain `u64` shared with unrelated fields.
    pub fn key_owner_of_alias(&self, alias_id: &str) -> Option<&Node> {
        self.nodes.values().find(|n| match &n.kind {
            NodeKind::Struct(s) => s.fields.iter().any(|f| f.is_key && f.alias.as_deref() == Some(alias_id)),
            _ => false,
        })
    }

    /// The struct a field (or parameter) refers to: by its alias when the
    /// type is written with one, otherwise by its resolved type (refined
    /// types and other named key types). Never matches the referring
    /// struct's own key field.
    pub fn reference_target(&self, alias: Option<&str>, ty: &Ty) -> Option<&Node> {
        if let Some(a) = alias {
            return self.key_owner_of_alias(a);
        }
        let inner = match ty {
            Ty::Option(t) | Ty::List(t) | Ty::Set(t) | Ty::SortedSet(t) => t,
            t => t,
        };
        match inner {
            Ty::Named(..) => self.key_owner(inner),
            _ => None,
        }
    }

    pub fn type_derives_copy(&self, ty: &Ty) -> bool {
        match ty {
            Ty::Bool | Ty::Int(_) | Ty::Float32 | Ty::Float64 | Ty::Char | Ty::Unit | Ty::Duration | Ty::Instant => true,
            Ty::Tuple(ts) => ts.iter().all(|t| self.type_derives_copy(t)),
            Ty::Option(t) | Ty::Array(t, _) => self.type_derives_copy(t),
            Ty::Named(id, _) => self.nodes.get(id).is_some_and(|n| match &n.kind {
                NodeKind::Refined { base, .. } => self.type_derives_copy(base),
                NodeKind::Enum(e) => e.variants.iter().all(|v| v.items.is_empty()),
                NodeKind::Machine(m) => m.states.variants.iter().all(|v| v.items.is_empty()),
                _ => n.derives.iter().any(|d| d == "Copy"),
            }),
            _ => false,
        }
    }
}

/// `Builder`, `Builder(...)` or `Repository[User]` → the pattern's name.
fn pattern_name(e: &ast::Expr) -> Option<String> {
    match &e.kind {
        ast::ExprKind::Index { base, .. } => base.callee_name().map(String::from),
        _ => e.callee_name().map(String::from),
    }
}

/// How a `Repository` capability stores and finds values (spec §9).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RepositoryShape {
    /// The insert-style method (`insert`, `add`, `save`, `put`, `create`, `store`, …).
    pub insert: String,
    /// The find-style method (`find`, `get`, `load`, `fetch`, `lookup`, …), returning `T | None`.
    pub find: String,
    /// The stored type `T`.
    pub item: Ty,
    /// `None`: `insert` returns the key passed to `find`; `Some(f)`: the key
    /// is `T`'s `key` field `f`.
    pub key_field: Option<String>,
}

const INSERT_NAMES: &[&str] = &["insert", "add", "save", "put", "create", "store"];
const FIND_NAMES: &[&str] = &["find", "get", "load", "fetch", "lookup", "read"];

fn named_like(name: &str, stems: &[&str]) -> bool {
    stems.iter().any(|s| name == *s || name.strip_prefix(s).is_some_and(|r| r.starts_with('_')))
}

impl Model {
    /// The type argument of a pattern on a node (`Repository[User]`), if written.
    pub fn pattern_arg(&self, node: &Node, pattern: &str) -> Option<Ty> {
        for d in node.item.decorators.iter().filter(|d| d.name.name == "pattern") {
            for a in &d.args {
                if let ast::ExprKind::Index { base, index } = &a.value.kind {
                    if base.callee_name() == Some(pattern) {
                        let name = index.callee_name()?;
                        return self.resolve_path(&node.module, name).map(|id| Ty::Named(id, vec![]));
                    }
                }
            }
        }
        let clauses: &[ast::Clause] = match &node.item.kind {
            ItemKind::Def(d) => &d.clauses,
            ItemKind::Adapter(a) => &a.clauses,
            _ => &[],
        };
        for c in clauses {
            if let ast::ClauseKind::Pattern(t) = &c.kind {
                if let TypeExprKind::Named { path, args } = &t.kind {
                    if path.last().is_some_and(|p| p.name == pattern) {
                        if let Some(TypeArg::Type(arg)) = args.first() {
                            return Some(self.resolve_ty(&node.module, arg, &[], &mut Vec::new()));
                        }
                    }
                }
            }
        }
        None
    }

    /// Find the insert/find pair of a `Repository[T]` capability: an
    /// insert-style method taking one `T`, and a find-style method taking the
    /// key (returned by insert, or `T`'s `key` field) and returning `T | None`.
    pub fn repository_shape(&self, cap: &str, item: Option<&Ty>) -> Option<RepositoryShape> {
        let NodeKind::Capability(c) = &self.nodes.get(cap)?.kind else { return None };
        for ins in c.methods.iter().filter(|m| named_like(&m.name, INSERT_NAMES) && m.sig.params.len() == 1) {
            let t = &ins.sig.params[0].ty;
            if item.is_some_and(|i| i != t) {
                continue;
            }
            for f in c.methods.iter().filter(|m| named_like(&m.name, FIND_NAMES) && m.sig.params.len() == 1) {
                if f.sig.ret != Ty::Option(Box::new(t.clone())) {
                    continue;
                }
                let key = &f.sig.params[0].ty;
                if ins.sig.ret != Ty::Unit && &ins.sig.ret == key {
                    return Some(RepositoryShape { insert: ins.name.clone(), find: f.name.clone(), item: t.clone(), key_field: None });
                }
                let key_field = self
                    .struct_fields(t)
                    .and_then(|fs| fs.into_iter().find(|fi| fi.is_key && &fi.ty == key))
                    .map(|fi| fi.name);
                if key_field.is_some() {
                    return Some(RepositoryShape { insert: ins.name.clone(), find: f.name.clone(), item: t.clone(), key_field });
                }
            }
        }
        None
    }

    /// Does any method of the capability state a contract (`requires`,
    /// `ensures`, `raises … when`) that adapter contract tests can check?
    pub fn capability_has_contracts(&self, cap: &str) -> bool {
        let Some(ItemKind::Capability(c)) = self.nodes.get(cap).map(|n| &n.item.kind) else { return false };
        c.methods.iter().any(|m| {
            m.clauses.iter().any(|cl| {
                matches!(cl.kind, ast::ClauseKind::Requires(_) | ast::ClauseKind::Ensures(_) | ast::ClauseKind::RaisesWhen { .. })
            })
        })
    }

    /// Is this a `@sensitive` type (whose values are redacted)?
    pub fn type_is_sensitive(&self, t: &Ty) -> bool {
        t.named_id().and_then(|id| self.nodes.get(id)).is_some_and(|n| n.is_sensitive)
    }

    /// Can values of this type be hashed (`Eq + Hash` derivable)?
    pub fn type_is_hashable(&self, t: &Ty) -> bool {
        self.hashable_depth(t, 0)
    }

    fn hashable_depth(&self, t: &Ty, depth: u32) -> bool {
        if depth > 8 {
            return true;
        }
        match t {
            Ty::Float32 | Ty::Float64 | Ty::Dict(..) | Ty::Set(_) | Ty::Fn(..) | Ty::Stream(_) | Ty::Unknown => false,
            Ty::List(t) | Ty::Option(t) | Ty::Array(t, _) | Ty::SortedSet(t) => self.hashable_depth(t, depth + 1),
            Ty::SortedDict(k, v) => self.hashable_depth(k, depth + 1) && self.hashable_depth(v, depth + 1),
            Ty::Tuple(ts) => ts.iter().all(|t| self.hashable_depth(t, depth + 1)),
            Ty::Named(id, args) => {
                args.iter().all(|a| self.hashable_depth(a, depth + 1))
                    && match self.nodes.get(id).map(|n| &n.kind) {
                        Some(NodeKind::Struct(s)) => s.fields.iter().all(|f| self.hashable_depth(&f.ty, depth + 1)),
                        Some(k @ (NodeKind::Enum(_) | NodeKind::Machine(_))) => k
                            .enum_like()
                            .unwrap()
                            .variants
                            .iter()
                            .all(|v| v.items.iter().all(|i| self.hashable_depth(&i.ty, depth + 1))),
                        Some(NodeKind::Refined { base, .. }) => self.hashable_depth(base, depth + 1),
                        _ => false,
                    }
            }
            _ => true,
        }
    }
}

pub fn generic_count(item: &Item) -> usize {
    match &item.kind {
        ItemKind::TypeDef(t) => t.generics.len(),
        ItemKind::Struct(s) => s.generics.len(),
        ItemKind::Enum(e) => e.generics.len(),
        ItemKind::Capability(c) => c.generics.len(),
        ItemKind::Alias(a) => a.generics.len(),
        _ => 0,
    }
}

pub fn item_label(item: &Item) -> &'static str {
    match &item.kind {
        ItemKind::Def(_) => "function",
        ItemKind::Flow(_) => "flow",
        ItemKind::App(_) => "app",
        ItemKind::Machine(_) => "machine",
        ItemKind::Test(_) => "test",
        _ => "definition",
    }
}
