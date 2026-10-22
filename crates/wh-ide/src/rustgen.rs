//! A readable preview of the Rust a node maps to (language spec §12), shown
//! in the inspector until the backend provides the real generated code.

use wh_model::{Model, Node, NodeKind, ParamInfo, SigInfo, Ty};
use wh_syntax::ast::ParamMode;

pub fn rust_ty(t: &Ty) -> String {
    match t {
        Ty::Bool => "bool".into(),
        Ty::Int(i) => i.rust().into(),
        Ty::Float32 => "f32".into(),
        Ty::Float64 => "f64".into(),
        Ty::Str => "String".into(),
        Ty::Char => "char".into(),
        Ty::Bytes => "Vec<u8>".into(),
        Ty::Unit => "()".into(),
        Ty::Duration => "Duration".into(),
        Ty::Instant => "Instant".into(),
        Ty::List(t) => format!("Vec<{}>", rust_ty(t)),
        Ty::Dict(k, v) => format!("HashMap<{}, {}>", rust_ty(k), rust_ty(v)),
        Ty::SortedDict(k, v) => format!("BTreeMap<{}, {}>", rust_ty(k), rust_ty(v)),
        Ty::Set(t) => format!("HashSet<{}>", rust_ty(t)),
        Ty::SortedSet(t) => format!("BTreeSet<{}>", rust_ty(t)),
        Ty::Tuple(ts) => format!("({})", ts.iter().map(rust_ty).collect::<Vec<_>>().join(", ")),
        Ty::Array(t, n) => format!("[{}; {n}]", rust_ty(t)),
        Ty::Option(t) => format!("Option<{}>", rust_ty(t)),
        Ty::Fn(ps, r) => format!("impl Fn({}) -> {}", ps.iter().map(rust_ty).collect::<Vec<_>>().join(", "), rust_ty(r)),
        Ty::Stream(t) => format!("impl Stream<Item = {}>", rust_ty(t)),
        Ty::Named(id, args) => {
            let short = id.rsplit('.').next().unwrap_or(id);
            if args.is_empty() {
                short.to_string()
            } else {
                format!("{short}<{}>", args.iter().map(rust_ty).collect::<Vec<_>>().join(", "))
            }
        }
        Ty::Param(p) => p.clone(),
        Ty::Unknown => "_".into(),
    }
}

fn param(model: &Model, p: &ParamInfo) -> String {
    let ty = match p.mode {
        ParamMode::Own => rust_ty(&p.ty),
        ParamMode::Mut => format!("&mut {}", rust_ty(&p.ty)),
        _ if model.type_derives_copy(&p.ty) => rust_ty(&p.ty),
        _ => match &p.ty {
            Ty::Str => "&str".into(),
            Ty::Bytes => "&[u8]".into(),
            Ty::List(t) => format!("&[{}]", rust_ty(t)),
            t => format!("&{}", rust_ty(t)),
        },
    };
    format!("{}: {ty}", p.name)
}

fn sig(model: &Model, name: &str, s: &SigInfo) -> String {
    let mut params: Vec<String> = s.params.iter().map(|p| param(model, p)).collect();
    for u in &s.uses {
        let c = u.rsplit('.').next().unwrap_or(u);
        params.push(format!("{}: &impl {c}", c.to_lowercase()));
    }
    let ret = match &s.raises {
        Some(e) => format!("Result<{}, {}>", rust_ty(&s.ret), rust_ty(e)),
        None => rust_ty(&s.ret),
    };
    let asyncness = if s.is_async { "async " } else { "" };
    format!("pub {asyncness}fn {name}({}) -> {ret}", params.join(", "))
}

pub fn preview(model: &Model, n: &Node) -> String {
    match &n.kind {
        NodeKind::Def(d) => {
            let s = sig(model, &n.name, &d.sig);
            match &d.extern_path {
                Some(p) => format!("{s} {{\n    {p}(..)\n}}"),
                None => format!(
                    "#[inline]\n{s} {{\n    // contract wrapper (debug_assert!) around the hole\n    imp::{}(..)\n}}\n\n// impl/{}/{}.rs — synthesized by the compiler agent",
                    n.name, n.module, n.name
                ),
            }
        }
        NodeKind::Flow(f) => {
            let mut out = format!("{} {{\n", sig(model, &n.name, &f.sig));
            // steps in source order, with unresolved calls kept as visible errors (G8)
            let mut lines: Vec<(u32, String)> =
                f.steps.iter().map(|st| (st.span.start, format!("    let _ = {}(..){};\n", st.label, if st.raises.is_some() { "?" } else { "" }))).collect();
            for d in model.diagnostics.iter().filter(|d| d.file == n.file && d.diag.span.start >= n.span.start && d.diag.span.end <= n.span.end) {
                if let Some(name) = d.diag.message.strip_prefix("unknown function `").and_then(|m| m.strip_suffix('`')) {
                    lines.push((d.diag.span.start, format!("    // ✗ {name}(..): unknown function, so this step is missing\n")));
                }
            }
            lines.sort_by_key(|(at, _)| *at);
            for (_, l) in lines {
                out.push_str(&l);
            }
            out.push('}');
            out
        }
        NodeKind::Struct(s) => {
            let mut out = format!("#[derive(Debug, Clone, PartialEq)]\npub struct {} {{\n", n.name);
            for f in &s.fields {
                out.push_str(&format!("    pub {}: {},\n", f.name, rust_ty(&f.ty)));
            }
            out.push('}');
            out
        }
        NodeKind::Enum(e) => {
            let mut out = format!("#[derive(Debug, Clone, PartialEq)]\npub enum {} {{\n", n.name);
            for v in &e.variants {
                if v.items.is_empty() {
                    out.push_str(&format!("    {},\n", v.name));
                } else {
                    let items: Vec<String> = v.items.iter().map(|i| rust_ty(&i.ty)).collect();
                    out.push_str(&format!("    {}({}),\n", v.name, items.join(", ")));
                }
            }
            out.push('}');
            out
        }
        NodeKind::Refined { base, .. } => {
            format!("#[repr(transparent)]\npub struct {}({});\n\nimpl {} {{\n    pub fn parse(v: {}) -> Result<Self, InvalidValue> {{ .. }}\n}}", n.name, rust_ty(base), n.name, rust_ty(base))
        }
        NodeKind::Alias(t) => format!("pub type {} = {};", n.name, rust_ty(t)),
        NodeKind::Capability(c) => {
            let mut out = format!("pub trait {} {{\n", n.name);
            for m in &c.methods {
                out.push_str(&format!("    {};\n", sig(model, &m.name, &m.sig).replacen("pub ", "", 1).replacen("(", "(&self, ", 1).replace("(&self, )", "(&self)")));
            }
            out.push('}');
            out
        }
        NodeKind::Adapter(a) => {
            let caps: Vec<&str> = a.implements.iter().map(|c| c.rsplit('.').next().unwrap_or(c)).collect();
            format!("pub struct {} {{ .. }}\n\nimpl {} for {} {{ /* holes */ }}", n.name, caps.join(" + "), n.name)
        }
        NodeKind::App(_) => "fn main() { /* wires adapters, runs the entry flow */ }".into(),
        NodeKind::Machine(_) => "// state machines generate code in M2".into(),
    }
}
