//! Mapping of WH types and names to Rust (language spec §4, §12).

use wh_model::{Model, NodeKind, ParamInfo, Ty};
use wh_syntax::ast::ParamMode;

pub struct Names<'m> {
    pub model: &'m Model,
    pub module: &'m str,
    /// Always emit `crate::module::Name` (for code resolved in `module` but
    /// placed in another module).
    pub qualified: bool,
}

/// Rust module identifier for a WH module (`a.b` → `a_b`).
pub fn module_ident(module: &str) -> String {
    module.replace('.', "_")
}

/// snake_case binding name used for a capability instance (`Db` → `db`).
pub fn cap_var(cap_id: &str) -> String {
    let short = cap_id.rsplit('.').next().unwrap_or(cap_id);
    let mut out = String::new();
    for (i, c) in short.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    format!("cap_{out}")
}

/// Test function prefix for a node: `signup.normalize_email` → `wh__signup__normalize_email`.
pub fn test_prefix(id: &str) -> String {
    format!("wh__{}", id.replace(['.', '#', '[', ']', ':'], "__"))
}

pub fn rust_keyword_safe(name: &str) -> String {
    const KW: &[&str] = &[
        "as", "break", "const", "continue", "crate", "dyn", "enum", "extern", "false", "fn", "for", "if", "impl",
        "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref", "return", "self", "Self", "static", "struct",
        "super", "trait", "true", "type", "unsafe", "use", "where", "while", "async", "await", "abstract", "become",
        "box", "do", "final", "macro", "override", "priv", "typeof", "unsized", "virtual", "yield", "try", "gen",
    ];
    if KW.contains(&name) { format!("r#{name}") } else { name.to_string() }
}

impl<'m> Names<'m> {
    pub fn new(model: &'m Model, module: &'m str) -> Self {
        Self { model, module, qualified: false }
    }

    /// Path to a definition as seen from the current module.
    pub fn path(&self, id: &str) -> String {
        let node = &self.model.nodes[id];
        let imported = self.model.scopes.get(self.module).and_then(|s| s.names.get(&node.name)).is_some_and(|i| i == id);
        if !self.qualified && (node.module == self.module || imported) {
            return rust_keyword_safe(&node.name);
        }
        format!("crate::{}::{}", module_ident(&node.module), rust_keyword_safe(&node.name))
    }

    pub fn ty(&self, t: &Ty) -> String {
        match t {
            Ty::Bool => "bool".into(),
            Ty::Int(i) => i.rust().into(),
            Ty::Float32 => "f32".into(),
            Ty::Float64 => "f64".into(),
            Ty::Str => "String".into(),
            Ty::Char => "char".into(),
            Ty::Bytes => "Vec<u8>".into(),
            Ty::Unit => "()".into(),
            Ty::Duration => "std::time::Duration".into(),
            Ty::Instant => "std::time::Instant".into(),
            Ty::List(t) => format!("Vec<{}>", self.ty(t)),
            Ty::Dict(k, v) => format!("std::collections::HashMap<{}, {}>", self.ty(k), self.ty(v)),
            Ty::SortedDict(k, v) => format!("std::collections::BTreeMap<{}, {}>", self.ty(k), self.ty(v)),
            Ty::Set(t) => format!("std::collections::HashSet<{}>", self.ty(t)),
            Ty::SortedSet(t) => format!("std::collections::BTreeSet<{}>", self.ty(t)),
            Ty::Tuple(ts) => {
                let inner: Vec<String> = ts.iter().map(|t| self.ty(t)).collect();
                if ts.len() == 1 { format!("({},)", inner[0]) } else { format!("({})", inner.join(", ")) }
            }
            Ty::Array(t, n) => format!("[{}; {n}]", self.ty(t)),
            Ty::Option(t) => format!("Option<{}>", self.ty(t)),
            Ty::Fn(ps, r) => {
                let ps: Vec<String> = ps.iter().map(|t| self.param_ty(t, &ParamMode::Borrow)).collect();
                format!("Box<dyn Fn({}) -> {}>", ps.join(", "), self.ty(r))
            }
            Ty::Stream(t) => format!("Box<dyn Iterator<Item = {}>>", self.ty(t)),
            Ty::Named(id, args) => {
                let p = self.path(id);
                if args.is_empty() {
                    p
                } else {
                    format!("{p}<{}>", args.iter().map(|a| self.ty(a)).collect::<Vec<_>>().join(", "))
                }
            }
            Ty::Param(p) => p.clone(),
            Ty::Unknown => "()".into(),
        }
    }

    pub fn is_copy(&self, t: &Ty) -> bool {
        self.model.type_derives_copy(t)
    }

    /// Type of a parameter given its passing mode.
    pub fn param_ty(&self, t: &Ty, mode: &ParamMode) -> String {
        match mode {
            ParamMode::Own => self.ty(t),
            ParamMode::Mut => format!("&mut {}", self.ty(t)),
            ParamMode::Borrow => {
                if self.is_copy(t) {
                    return self.ty(t);
                }
                match t {
                    Ty::Str => "&str".into(),
                    Ty::Bytes => "&[u8]".into(),
                    Ty::List(e) => format!("&[{}]", self.ty(e)),
                    Ty::Fn(ps, r) => {
                        let ps: Vec<String> = ps.iter().map(|t| self.param_ty(t, &ParamMode::Borrow)).collect();
                        format!("&dyn Fn({}) -> {}", ps.join(", "), self.ty(r))
                    }
                    _ => format!("&{}", self.ty(t)),
                }
            }
        }
    }

    /// Is a parameter received by reference (so it must be cloned to own it)?
    pub fn param_is_ref(&self, p: &ParamInfo) -> bool {
        match p.mode {
            ParamMode::Own => false,
            ParamMode::Mut => true,
            ParamMode::Borrow => !self.is_copy(&p.ty),
        }
    }

    /// ` -> T` for a signature, or nothing for a plain `unit` return.
    pub fn ret_suffix(&self, ret: &Ty, raises: &Option<Ty>) -> String {
        if *ret == Ty::Unit && raises.is_none() { String::new() } else { format!(" -> {}", self.ret_ty(ret, raises)) }
    }

    pub fn ret_ty(&self, ret: &Ty, raises: &Option<Ty>) -> String {
        match raises {
            Some(e) => format!("Result<{}, {}>", self.ty(ret), self.ty(e)),
            None => self.ty(ret),
        }
    }

    /// Capability parameters for a set of capabilities (`cap_db: &impl Db`).
    pub fn cap_params(&self, caps: &[String]) -> Vec<String> {
        caps.iter().map(|c| format!("{}: &impl {}", cap_var(c), self.path(c))).collect()
    }

    pub fn is_refined(&self, t: &Ty) -> bool {
        matches!(t, Ty::Named(id, _) if matches!(self.model.nodes.get(id).map(|n| &n.kind), Some(NodeKind::Refined { .. })))
    }
}
