//! Compiles specification expressions (contracts, examples, properties,
//! scenario tests) to Rust. Spec code favours clarity over speed: every
//! sub-expression evaluates to an owned value (cloning where needed), and
//! Python-like coercions (numeric widening, refined ↔ base, literal → refined,
//! implicit `Some`) are made explicit using the types from `wh_model::Typer`.

use wh_model::{Env, Model, NodeKind, ParamInfo, Ty, Typer};
use wh_syntax::ast::{Arg, BinOp, CmpOp, Expr, ExprKind, ParamMode, Quantifier, StrKind, UnaryOp};

use crate::rust::{Names, cap_var, rust_keyword_safe};

#[derive(Debug, Clone)]
pub struct Var {
    pub name: String,
    pub rust: String,
    pub ty: Ty,
    /// The Rust variable holds a reference (`&T`, `&str`, `&[T]`).
    pub is_ref: bool,
}

#[derive(Debug, Clone, Default)]
pub struct Scope {
    pub vars: Vec<Var>,
    /// capability id → Rust expression of an instance (`&cap_db` or `cap_db`)
    pub caps: Vec<(String, String)>,
    pub result: Option<Var>,
    pub self_var: Option<Var>,
    /// param name → Rust variable holding its pre-call clone
    pub old: Vec<(String, String)>,
}

impl Scope {
    pub fn var(&mut self, name: &str, rust: &str, ty: Ty, is_ref: bool) {
        self.vars.push(Var { name: name.into(), rust: rust.into(), ty, is_ref });
    }

    fn lookup(&self, name: &str) -> Option<&Var> {
        self.vars.iter().rev().find(|v| v.name == name)
    }

    pub fn env(&self) -> Env {
        let mut env = Env::default();
        for v in &self.vars {
            env.vars.push((v.name.clone(), v.ty.clone()));
        }
        env.result = self.result.as_ref().map(|v| v.ty.clone());
        env.self_ty = self.self_var.as_ref().map(|v| v.ty.clone());
        env.allow_old = true;
        env
    }
}

pub struct ExprGen<'m> {
    pub names: Names<'m>,
}

/// Arithmetic on integers is mathematical in specs: it is evaluated in `i128`.
fn is_arith(e: &Expr) -> bool {
    matches!(&e.kind, ExprKind::Binary { op, .. } if !matches!(op, BinOp::And | BinOp::Or))
        || matches!(&e.kind, ExprKind::Unary { op: UnaryOp::Neg, .. })
}

fn str_lit(s: &str) -> String {
    format!("{s:?}")
}

impl<'m> ExprGen<'m> {
    pub fn new(model: &'m Model, module: &'m str) -> Self {
        Self { names: Names::new(model, module) }
    }

    fn model(&self) -> &'m Model {
        self.names.model
    }

    pub fn ty(&self, e: &Expr, scope: &Scope) -> Ty {
        let mut t = Typer::new(self.model(), self.names.module);
        t.ty(e, &scope.env())
    }

    /// Owned value of a variable.
    pub fn owned(&self, v: &Var) -> String {
        if !v.is_ref {
            return if self.names.is_copy(&v.ty) { v.rust.clone() } else { format!("{}.clone()", v.rust) };
        }
        if self.names.is_copy(&v.ty) {
            return format!("(*{})", v.rust);
        }
        match &v.ty {
            Ty::Str => format!("{}.to_string()", v.rust),
            Ty::List(_) | Ty::Bytes => format!("{}.to_vec()", v.rust),
            _ => format!("{}.clone()", v.rust),
        }
    }

    /// Strip refined types (deeply through containers) to their base values.
    fn deep_base(&self, code: String, ty: &Ty) -> (String, Ty) {
        if let Some(base) = self.model().refined_base(ty) {
            let (c, t) = self.deep_base(format!("{code}.into_inner()"), &base);
            return (c, t);
        }
        match ty {
            Ty::List(inner) if self.has_refined(inner) => {
                let (c, t) = self.deep_base("x".into(), inner);
                (format!("{code}.into_iter().map(|x| {c}).collect::<Vec<_>>()"), Ty::List(Box::new(t)))
            }
            Ty::Set(inner) if self.has_refined(inner) => {
                let (c, t) = self.deep_base("x".into(), inner);
                (format!("{code}.into_iter().map(|x| {c}).collect::<std::collections::HashSet<_>>()"), Ty::Set(Box::new(t)))
            }
            Ty::Option(inner) if self.has_refined(inner) => {
                let (c, t) = self.deep_base("x".into(), inner);
                (format!("{code}.map(|x| {c})"), Ty::Option(Box::new(t)))
            }
            _ => (code, ty.clone()),
        }
    }

    fn has_refined(&self, t: &Ty) -> bool {
        if self.names.is_refined(t) {
            return true;
        }
        match t {
            Ty::List(i) | Ty::Set(i) | Ty::Option(i) => self.has_refined(i),
            _ => false,
        }
    }

    /// Generate `e` as an owned value of its own type.
    pub fn emit(&self, e: &Expr, scope: &Scope) -> String {
        let ty = self.ty(e, scope);
        self.emit_inner(e, scope, &ty)
    }

    /// Generate `e` coerced to `expected`.
    pub fn emit_as(&self, e: &Expr, expected: &Ty, scope: &Scope) -> String {
        let actual = self.ty(e, scope);
        if let (Ty::Int(i), true) = (expected, is_arith(e) && actual.is_int()) {
            return format!("(({}) as {})", self.emit(e, scope), i.rust());
        }
        if &actual == expected || expected.is_unknown() {
            return self.emit_inner(e, scope, expected);
        }
        // literals adopt the expected numeric type
        if let ExprKind::Int(v) = &e.kind {
            if let Ty::Int(i) = expected {
                return format!("{v}{}", i.rust());
            }
            if matches!(expected, Ty::Float64 | Ty::Float32) {
                return format!("{v}.0{}", if *expected == Ty::Float32 { "f32" } else { "f64" });
            }
        }
        if let ExprKind::List(items) = &e.kind {
            if let Ty::List(inner) = expected {
                let xs: Vec<String> = items.iter().map(|i| self.emit_as(i, inner, scope)).collect();
                return format!("vec![{}]", xs.join(", "));
            }
        }
        if let (ExprKind::Tuple(items), Ty::Tuple(ts)) = (&e.kind, expected) {
            if items.len() == ts.len() {
                let xs: Vec<String> = items.iter().zip(ts).map(|(i, t)| self.emit_as(i, t, scope)).collect();
                return if xs.len() == 1 { format!("({},)", xs[0]) } else { format!("({})", xs.join(", ")) };
            }
        }
        if let ExprKind::None = e.kind {
            return "None".into();
        }
        match expected {
            Ty::Option(inner) if !matches!(actual, Ty::Option(_)) => {
                return format!("Some({})", self.emit_as(e, inner, scope));
            }
            Ty::Named(id, _) if self.names.is_refined(expected) && !self.names.is_refined(&actual) => {
                let base = self.model().refined_base(expected).unwrap();
                let inner = self.emit_as(e, &base, scope);
                let src = self.names.path(id);
                return format!("{src}::parse({inner}).expect(\"value satisfies {}\")", self.model().nodes[id].name);
            }
            Ty::Int(i) if actual.is_numeric() => return format!("(({}) as {})", self.emit(e, scope), i.rust()),
            Ty::Float64 if actual.is_numeric() => return format!("(({}) as f64)", self.emit(e, scope)),
            _ => {}
        }
        if self.names.is_refined(&actual) && !self.names.is_refined(expected) {
            let (c, _) = self.deep_base(self.emit(e, scope), &actual);
            return c;
        }
        self.emit(e, scope)
    }

    pub fn emit_bool(&self, e: &Expr, scope: &Scope) -> String {
        self.emit_as(e, &Ty::Bool, scope)
    }

    fn emit_inner(&self, e: &Expr, scope: &Scope, ty: &Ty) -> String {
        match &e.kind {
            ExprKind::Error => "unreachable!()".into(),
            ExprKind::Int(v) => match ty {
                Ty::Int(i) => format!("{v}{}", i.rust()),
                _ => format!("{v}i64"),
            },
            ExprKind::Float(v) => {
                let v = if v.contains(['.', 'e', 'E']) { v.clone() } else { format!("{v}.0") };
                format!("{v}f64")
            }
            ExprKind::Str { value, kind: StrKind::Bytes } => format!("{}.as_bytes().to_vec()", str_lit(value)),
            ExprKind::Str { value, .. } => format!("String::from({})", str_lit(value)),
            ExprKind::Bool(b) => b.to_string(),
            ExprKind::None => "None".into(),
            ExprKind::Duration { value, unit } => {
                let f = match unit.as_str() {
                    "ns" => "from_nanos",
                    "us" => "from_micros",
                    "ms" => "from_millis",
                    _ => "from_secs",
                };
                format!("std::time::Duration::{f}({value})")
            }
            ExprKind::Name(n) => self.name(n, scope),
            ExprKind::Attr { base, attr } => {
                if let ExprKind::Name(b) = &base.kind {
                    if scope.lookup(b).is_none() && b != "result" && b != "self" {
                        if let Some(id) = self.model().resolve_name(self.names.module, b) {
                            if matches!(self.model().nodes[id].kind, NodeKind::Enum(_) | NodeKind::Machine(_)) {
                                return format!("{}::{}", self.names.path(id), attr.name);
                            }
                        }
                    }
                }
                let bt = self.ty(base, scope);
                let b = self.emit(base, scope);
                if let Ty::Tuple(_) = bt {
                    return format!("{b}.{}", attr.name);
                }
                format!("{b}.{}", rust_keyword_safe(&attr.name))
            }
            ExprKind::Call { func, args } => self.call(func, args, scope, true),
            ExprKind::Index { base, index } => {
                let bt = self.ty(base, scope);
                let b = self.emit(base, scope);
                match &bt {
                    Ty::Dict(k, _) | Ty::SortedDict(k, _) => format!("{b}[&{}].clone()", self.emit_as(index, k, scope)),
                    Ty::Str => format!("{b}.chars().nth(({}) as usize).map(String::from).unwrap_or_default()", self.emit(index, scope)),
                    _ => format!("{b}[({}) as usize].clone()", self.emit(index, scope)),
                }
            }
            ExprKind::Slice { base, lo, hi } => {
                let bt = self.ty(base, scope);
                let b = self.emit(base, scope);
                let lo = lo.as_ref().map_or("0".to_string(), |x| format!("(({}) as usize)", self.emit(x, scope)));
                if bt == Ty::Str {
                    let take = hi.as_ref().map_or("usize::MAX".to_string(), |x| format!("(({}) as usize).saturating_sub({lo})", self.emit(x, scope)));
                    return format!("{b}.chars().skip({lo}).take({take}).collect::<String>()");
                }
                let hi = hi.as_ref().map_or(String::new(), |x| format!("(({}) as usize)", self.emit(x, scope)));
                format!("{b}[{lo}..{hi}].to_vec()")
            }
            ExprKind::Unary { op: UnaryOp::Not, expr } => format!("(!{})", self.emit_bool(expr, scope)),
            ExprKind::Unary { op: UnaryOp::Neg, expr } => {
                let t = self.ty(expr, scope);
                let (c, b) = self.deep_base(self.emit(expr, scope), &t);
                if b.is_int() { format!("(-(({c}) as i128))") } else { format!("(-{c})") }
            }
            ExprKind::Binary { op: BinOp::And, lhs, rhs } => format!("({} && {})", self.emit_bool(lhs, scope), self.emit_bool(rhs, scope)),
            ExprKind::Binary { op: BinOp::Or, lhs, rhs } => format!("({} || {})", self.emit_bool(lhs, scope), self.emit_bool(rhs, scope)),
            ExprKind::Binary { op, lhs, rhs } => self.arith(*op, lhs, rhs, scope),
            ExprKind::Compare { first, rest } => {
                let mut parts = Vec::new();
                let mut prev: &Expr = first;
                for (op, rhs) in rest {
                    parts.push(self.compare(prev, *op, rhs, scope));
                    prev = rhs;
                }
                if parts.len() == 1 { parts.remove(0) } else { format!("({})", parts.join(" && ")) }
            }
            ExprKind::IfElse { then, cond, otherwise } => {
                format!("(if {} {{ {} }} else {{ {} }})", self.emit_bool(cond, scope), self.emit_as(then, ty, scope), self.emit_as(otherwise, ty, scope))
            }
            ExprKind::Lambda { params, body } => {
                let mut inner = scope.clone();
                let ps: Vec<String> = params.iter().map(|p| rust_keyword_safe(&p.name)).collect();
                for p in params {
                    inner.var(&p.name, &rust_keyword_safe(&p.name), Ty::Unknown, false);
                }
                format!("(move |{}| {})", ps.join(", "), self.emit(body, &inner))
            }
            ExprKind::List(items) => {
                let inner = match ty {
                    Ty::List(t) => (**t).clone(),
                    _ => Ty::Unknown,
                };
                let xs: Vec<String> = items.iter().map(|i| self.emit_as(i, &inner, scope)).collect();
                format!("vec![{}]", xs.join(", "))
            }
            ExprKind::Tuple(items) => {
                let xs: Vec<String> = items.iter().map(|i| self.emit(i, scope)).collect();
                if xs.len() == 1 { format!("({},)", xs[0]) } else { format!("({})", xs.join(", ")) }
            }
            ExprKind::Comprehension { elt, var, iter, cond, .. } => {
                let (it_code, el_ty) = self.iter(iter, scope);
                let v = rust_keyword_safe(&var.name);
                let mut inner = scope.clone();
                inner.var(&var.name, &v, el_ty, false);
                let elt = self.emit(elt, &inner);
                match cond {
                    Some(c) => format!(
                        "{it_code}.filter_map(|{v}| if {} {{ Some({elt}) }} else {{ None }}).collect::<Vec<_>>()",
                        self.emit_bool(c, &inner)
                    ),
                    None => format!("{it_code}.map(|{v}| {elt}).collect::<Vec<_>>()"),
                }
            }
            ExprKind::Quantified { q, var, iter, body } => {
                let (it_code, el_ty) = self.iter(iter, scope);
                let v = rust_keyword_safe(&var.name);
                let mut inner = scope.clone();
                inner.var(&var.name, &v, el_ty, false);
                let f = if *q == Quantifier::Forall { "all" } else { "any" };
                format!("{it_code}.{f}(|{v}| {})", self.emit_bool(body, &inner))
            }
        }
    }

    /// An owning iterator over a collection expression, and its element type.
    fn iter(&self, e: &Expr, scope: &Scope) -> (String, Ty) {
        let t = self.ty(e, scope);
        let base = self.model().refined_base(&t).unwrap_or(t.clone());
        let code = self.emit(e, scope);
        let code = if self.names.is_refined(&t) { format!("{code}.into_inner()") } else { code };
        match &base {
            Ty::Str => (format!("{code}.chars().collect::<Vec<_>>().into_iter()"), Ty::Char),
            Ty::Dict(k, _) | Ty::SortedDict(k, _) => (format!("{code}.into_keys()"), (**k).clone()),
            other => (format!("{code}.into_iter()"), other.element().unwrap_or(Ty::Unknown)),
        }
    }

    fn name(&self, n: &str, scope: &Scope) -> String {
        if let Some(v) = scope.lookup(n) {
            return self.owned(v);
        }
        match n {
            "result" => return scope.result.as_ref().map_or("unreachable!()".into(), |v| self.owned(v)),
            "self" => return scope.self_var.as_ref().map_or("unreachable!()".into(), |v| self.owned(v)),
            _ => {}
        }
        if let Some(id) = self.model().resolve_name(self.names.module, n) {
            let node = &self.model().nodes[id];
            return match node.kind {
                NodeKind::Adapter(_) => format!("{}::new()", self.names.path(id)),
                _ => self.names.path(id),
            };
        }
        // unit variant
        for id in self.model().scopes.get(self.names.module).map(|s| s.names.values().collect::<Vec<_>>()).unwrap_or_default() {
            if let NodeKind::Enum(e) = &self.model().nodes[id].kind {
                if e.variants.iter().any(|v| v.name == n) {
                    return format!("{}::{n}", self.names.path(id));
                }
            }
        }
        rust_keyword_safe(n)
    }

    fn arith(&self, op: BinOp, lhs: &Expr, rhs: &Expr, scope: &Scope) -> String {
        let lt = self.ty(lhs, scope);
        let rt = self.ty(rhs, scope);
        let (l, lb) = self.deep_base(self.emit(lhs, scope), &lt);
        let (r, rb) = self.deep_base(self.emit(rhs, scope), &rt);
        if lb == Ty::Str && rb == Ty::Str && op == BinOp::Add {
            return format!("format!(\"{{}}{{}}\", {l}, {r})");
        }
        if matches!(lb, Ty::List(_)) && op == BinOp::Add {
            return format!("[{l}, {r}].concat()");
        }
        if lb == Ty::Duration {
            let o = if op == BinOp::Add { "+" } else { "-" };
            return format!("({l} {o} {r})");
        }
        let float = matches!(lb, Ty::Float64 | Ty::Float32) || matches!(rb, Ty::Float64 | Ty::Float32) || op == BinOp::Div;
        let (l, r) = if float {
            (format!("(({l}) as f64)"), format!("(({r}) as f64)"))
        } else {
            // mathematical integers: no overflow, no mixed-width errors
            (format!("(({l}) as i128)"), format!("(({r}) as i128)"))
        };
        match op {
            BinOp::Add => format!("({l} + {r})"),
            BinOp::Sub => format!("({l} - {r})"),
            BinOp::Mul => format!("({l} * {r})"),
            BinOp::Div => format!("({l} / {r})"),
            BinOp::FloorDiv if float => format!("({l} / {r}).floor()"),
            BinOp::FloorDiv => format!("{l}.div_euclid({r})"),
            BinOp::Mod if float => format!("{l}.rem_euclid({r})"),
            BinOp::Mod => format!("{l}.rem_euclid({r})"),
            BinOp::Pow if float => format!("{l}.powf({r})"),
            BinOp::Pow => format!("{l}.pow(({r}) as u32)"),
            BinOp::And | BinOp::Or => unreachable!(),
        }
    }

    fn compare(&self, lhs: &Expr, op: CmpOp, rhs: &Expr, scope: &Scope) -> String {
        match op {
            CmpOp::Is | CmpOp::IsNot => {
                let l = self.emit(lhs, scope);
                let neg = if op == CmpOp::IsNot { "!" } else { "" };
                return match &rhs.kind {
                    ExprKind::None => format!("{l}.{}()", if op == CmpOp::Is { "is_none" } else { "is_some" }),
                    ExprKind::Name(v) => {
                        let lt = self.ty(lhs, scope);
                        let path = lt.named_id().map(|id| self.names.path(id)).unwrap_or_default();
                        format!("{neg}matches!({l}, {path}::{v} {{ .. }})")
                    }
                    _ => "false".into(),
                };
            }
            CmpOp::In | CmpOp::NotIn => {
                let neg = if op == CmpOp::NotIn { "!" } else { "" };
                let rt = self.ty(rhs, scope);
                let rbase = self.model().refined_base(&rt).unwrap_or(rt.clone());
                let r = self.emit(rhs, scope);
                let r = if self.names.is_refined(&rt) { format!("{r}.into_inner()") } else { r };
                return match &rbase {
                    Ty::Str => {
                        let lt = self.ty(lhs, scope);
                        let (l, _) = self.deep_base(self.emit(lhs, scope), &lt);
                        format!("{neg}{r}.contains({l}.as_str())")
                    }
                    Ty::Dict(k, _) | Ty::SortedDict(k, _) => format!("{neg}{r}.contains_key(&{})", self.emit_as(lhs, k, scope)),
                    other => {
                        let el = other.element().unwrap_or(Ty::Unknown);
                        format!("{neg}{r}.contains(&{})", self.emit_as(lhs, &el, scope))
                    }
                };
            }
            _ => {}
        }
        let o = match op {
            CmpOp::Eq => "==",
            CmpOp::NotEq => "!=",
            CmpOp::Lt => "<",
            CmpOp::Le => "<=",
            CmpOp::Gt => ">",
            CmpOp::Ge => ">=",
            _ => unreachable!(),
        };
        let lt = self.ty(lhs, scope);
        let rt = self.ty(rhs, scope);
        let arith = is_arith(lhs) || is_arith(rhs);
        if lt == rt && !lt.is_unknown() && !arith {
            return format!("({} {o} {})", self.emit(lhs, scope), self.emit(rhs, scope));
        }
        // literal on one side adopts the other side's type
        let is_lit = |e: &Expr| {
            matches!(e.kind, ExprKind::Int(_) | ExprKind::Float(_) | ExprKind::Str { .. } | ExprKind::List(_) | ExprKind::Tuple(_) | ExprKind::None)
        };
        let (l, lb) = self.deep_base(self.emit(lhs, scope), &lt);
        let (r, rb) = self.deep_base(self.emit(rhs, scope), &rt);
        if lb.is_numeric() && rb.is_numeric() {
            if lb == rb && !arith {
                return format!("({l} {o} {r})");
            }
            if matches!(lb, Ty::Float64 | Ty::Float32) || matches!(rb, Ty::Float64 | Ty::Float32) {
                return format!("((({l}) as f64) {o} (({r}) as f64))");
            }
            if is_lit(rhs) && !arith {
                return format!("({l} {o} {})", self.emit_as(rhs, &lb, scope));
            }
            return format!("((({l}) as i128) {o} (({r}) as i128))");
        }
        if lb == rb {
            return format!("({l} {o} {r})");
        }
        if is_lit(rhs) {
            return format!("({l} {o} {})", self.emit_as(rhs, &lb, scope));
        }
        if is_lit(lhs) {
            return format!("({} {o} {r})", self.emit_as(lhs, &rb, scope));
        }
        if let Ty::Option(inner) = &lb {
            if **inner == rb {
                return format!("({l} {o} Some({r}))");
            }
        }
        format!("({l} {o} {r})")
    }

    /// Pass an argument to a parameter according to its mode.
    pub fn pass(&self, value: String, p: &ParamInfo) -> String {
        match p.mode {
            ParamMode::Own => value,
            ParamMode::Mut => format!("&mut {value}"),
            ParamMode::Borrow if self.names.is_copy(&p.ty) => value,
            ParamMode::Borrow => format!("&{value}"),
        }
    }

    fn args_for(&self, params: &[ParamInfo], args: &[Arg], scope: &Scope, defaults: &dyn Fn(&str) -> Option<String>) -> Vec<String> {
        let mut out = Vec::new();
        let mut positional = args.iter().filter(|a| a.name.is_none());
        for p in params {
            let given = args
                .iter()
                .find(|a| a.name.as_ref().is_some_and(|n| n.name == p.name))
                .or_else(|| positional.next());
            let v = match given {
                Some(a) => self.emit_as(&a.value, &p.ty, scope),
                None => defaults(&p.name).unwrap_or_else(|| "Default::default()".into()),
            };
            out.push(v);
        }
        out
    }

    /// A call. With `unwrap`, raising callees are unwrapped (spec semantics:
    /// an unexpected error fails the check); without, the `Result` is kept.
    pub fn call(&self, func: &Expr, args: &[Arg], scope: &Scope, unwrap: bool) -> String {
        let model = self.model();
        let module = self.names.module;
        let target = match &func.kind {
            ExprKind::Name(n) if scope.lookup(n).is_none() => model.resolve_name(module, n).cloned().map(|id| (id, None)),
            ExprKind::Attr { base, attr } => match &base.kind {
                ExprKind::Name(b) if scope.lookup(b).is_none() && b != "result" && b != "self" => {
                    match model.resolve_name(module, b) {
                        Some(id) if matches!(model.nodes[id].kind, NodeKind::Enum(_) | NodeKind::Refined { .. } | NodeKind::Machine(_)) => {
                            Some((id.clone(), Some(attr.name.clone())))
                        }
                        _ => model.resolve_path(module, &format!("{b}.{}", attr.name)).map(|id| (id, None)),
                    }
                }
                _ => None,
            },
            _ => None,
        };
        if let ExprKind::Name(n) = &func.kind {
            if scope.lookup(n).is_none() && target.is_none() {
                if let Some(code) = self.builtin(n, args, scope) {
                    return code;
                }
                // unqualified variant constructor
                if let Some(id) = model.scopes.get(module).and_then(|s| {
                    s.names.values().find(|id| matches!(&model.nodes[*id].kind, NodeKind::Enum(e) if e.variants.iter().any(|v| &v.name == n)))
                }) {
                    return self.variant(id, n, args, scope);
                }
            }
        }
        let Some((id, member)) = target else {
            // method call on a value
            if let ExprKind::Attr { base, attr } = &func.kind {
                return self.method(base, &attr.name, args, scope);
            }
            let f = self.emit(func, scope);
            let a: Vec<String> = args.iter().map(|a| self.emit(&a.value, scope)).collect();
            return format!("({f})({})", a.join(", "));
        };
        let node = &model.nodes[&id];
        let path = self.names.path(&id);
        match (&node.kind, member) {
            (NodeKind::Enum(_), Some(v)) => self.variant(&id, &v, args, scope),
            (NodeKind::Machine(m), Some(v)) if v == "step" => {
                let state = Ty::Named(id.clone(), vec![]);
                let arg = |i: usize, t: &Ty| args.get(i).map_or("unreachable!()".into(), |a| self.emit_as(&a.value, t, scope));
                format!("{path}::step({}, {})", arg(0, &state), arg(1, &m.event))
            }
            (NodeKind::Machine(_), Some(v)) if v == "initial" => format!("{path}::initial()"),
            (NodeKind::Machine(_), Some(v)) => self.variant(&id, &v, args, scope),
            (NodeKind::Refined { base, .. }, _) => {
                let v = args.first().map_or("Default::default()".into(), |a| self.emit_as(&a.value, base, scope));
                format!("{path}::parse({v}).expect(\"value satisfies {}\")", node.name)
            }
            (NodeKind::Struct(s), _) => {
                let fields = model.struct_fields(&Ty::Named(id.clone(), vec![])).unwrap_or_default();
                let params: Vec<ParamInfo> = fields
                    .iter()
                    .map(|f| ParamInfo::synthetic(f.name.clone(), ParamMode::Own, f.ty.clone(), f.has_default))
                    .collect();
                let ast_fields = match &node.item.kind {
                    wh_syntax::ast::ItemKind::Struct(st) => st.fields.clone(),
                    _ => vec![],
                };
                let defaults = |name: &str| {
                    let f = ast_fields.iter().find(|f| f.name.name == name)?;
                    let fi = fields.iter().find(|x| x.name == name)?;
                    Some(match &f.default {
                        Some(d) => self.emit_as(d, &fi.ty, &Scope::default()),
                        None => "Default::default()".into(),
                    })
                };
                let vals = self.args_for(&params, args, scope, &defaults);
                let inits: Vec<String> =
                    params.iter().zip(vals).map(|(p, v)| format!("{}: {v}", rust_keyword_safe(&p.name))).collect();
                let _ = s;
                format!("{path} {{ {} }}", inits.join(", "))
            }
            (NodeKind::Adapter(a), _) => {
                let params: Vec<ParamInfo> = a
                    .fields
                    .iter()
                    .map(|f| ParamInfo::synthetic(f.name.clone(), ParamMode::Own, f.ty.clone(), f.has_default))
                    .collect();
                let vals = self.args_for(&params, args, scope, &|_| None);
                format!("{path}::new({})", vals.join(", "))
            }
            (NodeKind::Def(_) | NodeKind::Flow(_), _) => {
                let sig = node.sig().unwrap();
                let mut call_args: Vec<String> = sig
                    .uses
                    .iter()
                    .map(|c| {
                        scope.caps.iter().find(|(id, _)| id == c).map_or_else(
                            || format!("{{ compile_error!(\"no `{}` capability in scope\") }}", c),
                            |(_, code)| code.clone(),
                        )
                    })
                    .collect();
                let vals = self.args_for(&sig.params, args, scope, &|_| None);
                for (p, v) in sig.params.iter().zip(vals) {
                    call_args.push(self.pass(v, p));
                }
                let mut code = format!("{path}({})", call_args.join(", "));
                if sig.is_async {
                    code = format!("crate::wh_rt::block_on({code})");
                }
                if sig.raises.is_some() && unwrap {
                    code = format!("{code}.unwrap_or_else(|e| panic!(\"`{}` raised {{e:?}}\"))", node.name);
                }
                code
            }
            _ => format!("compile_error!(\"`{}` cannot be called\")", node.name),
        }
    }

    fn variant(&self, enum_id: &str, v: &str, args: &[Arg], scope: &Scope) -> String {
        let path = self.names.path(enum_id);
        let Some(en) = self.model().enum_info(&Ty::Named(enum_id.to_string(), vec![])) else {
            return format!("{path}::{v}");
        };
        let Some(var) = en.variants.iter().find(|x| x.name == v) else { return format!("{path}::{v}") };
        if var.items.is_empty() {
            return format!("{path}::{v}");
        }
        let named = var.items.iter().all(|i| i.name.is_some());
        let params: Vec<ParamInfo> = var
            .items
            .iter()
            .enumerate()
            .map(|(i, it)| ParamInfo::synthetic(it.name.clone().unwrap_or_else(|| format!("_{i}")), ParamMode::Own, it.ty.clone(), false))
            .collect();
        let vals = self.args_for(&params, args, scope, &|_| None);
        if named {
            let inits: Vec<String> = params.iter().zip(vals).map(|(p, v)| format!("{}: {v}", rust_keyword_safe(&p.name))).collect();
            format!("{path}::{v} {{ {} }}", inits.join(", "))
        } else {
            format!("{path}::{v}({})", vals.join(", "))
        }
    }

    fn method(&self, base: &Expr, m: &str, args: &[Arg], scope: &Scope) -> String {
        let bt = self.ty(base, scope);
        let (b, base_ty) = self.deep_base(self.emit(base, scope), &bt);
        let arg = |i: usize, t: &Ty| args.get(i).map_or("Default::default()".into(), |a| self.emit_as(&a.value, t, scope));
        match &base_ty {
            Ty::Str => match m {
                "lower" => format!("{b}.to_lowercase()"),
                "upper" => format!("{b}.to_uppercase()"),
                "strip" => format!("{b}.trim().to_string()"),
                "lstrip" => format!("{b}.trim_start().to_string()"),
                "rstrip" => format!("{b}.trim_end().to_string()"),
                "title" => format!("{b}.split(' ').map(|w| {{ let mut c = w.chars(); c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default() }}).collect::<Vec<_>>().join(\" \")"),
                "startswith" => format!("{b}.starts_with({}.as_str())", arg(0, &Ty::Str)),
                "endswith" => format!("{b}.ends_with({}.as_str())", arg(0, &Ty::Str)),
                "contains" => format!("{b}.contains({}.as_str())", arg(0, &Ty::Str)),
                "split" => format!("{b}.split({}.as_str()).map(String::from).collect::<Vec<_>>()", arg(0, &Ty::Str)),
                "lines" => format!("{b}.lines().map(String::from).collect::<Vec<_>>()"),
                "replace" => format!("{b}.replace({}.as_str(), {}.as_str())", arg(0, &Ty::Str), arg(1, &Ty::Str)),
                "len" => format!("({b}.chars().count() as i64)"),
                "count" => format!("({b}.matches({}.as_str()).count() as i64)", arg(0, &Ty::Str)),
                "find" => format!("{b}.find({}.as_str()).map_or(-1i64, |i| i as i64)", arg(0, &Ty::Str)),
                "isdigit" => format!("(!{b}.is_empty() && {b}.chars().all(|c| c.is_ascii_digit()))"),
                "isalpha" => format!("(!{b}.is_empty() && {b}.chars().all(char::is_alphabetic))"),
                "is_empty" => format!("{b}.is_empty()"),
                _ => format!("compile_error!(\"unknown str method `{m}`\")"),
            },
            Ty::List(t) | Ty::Set(t) | Ty::SortedSet(t) | Ty::Array(t, _) => match m {
                "contains" => format!("{b}.contains(&{})", arg(0, t)),
                "is_empty" => format!("{b}.is_empty()"),
                "len" => format!("({b}.len() as i64)"),
                "first" => format!("{b}.into_iter().next()"),
                "last" => format!("{b}.into_iter().last()"),
                _ => format!("compile_error!(\"unknown method `{m}`\")"),
            },
            Ty::Dict(k, _) | Ty::SortedDict(k, _) => match m {
                "keys" => format!("{b}.into_keys().collect::<Vec<_>>()"),
                "values" => format!("{b}.into_values().collect::<Vec<_>>()"),
                "get" => format!("{b}.get(&{}).cloned()", arg(0, k)),
                "contains" => format!("{b}.contains_key(&{})", arg(0, k)),
                "is_empty" => format!("{b}.is_empty()"),
                "len" => format!("({b}.len() as i64)"),
                _ => format!("compile_error!(\"unknown method `{m}`\")"),
            },
            Ty::Option(_) => match m {
                "unwrap" => format!("{b}.unwrap()"),
                "is_some" => format!("{b}.is_some()"),
                "is_none" => format!("{b}.is_none()"),
                _ => format!("compile_error!(\"unknown method `{m}`\")"),
            },
            _ => {
                let a: Vec<String> = args.iter().map(|a| self.emit(&a.value, scope)).collect();
                format!("{b}.{m}({})", a.join(", "))
            }
        }
    }

    fn builtin(&self, n: &str, args: &[Arg], scope: &Scope) -> Option<String> {
        let first = args.first().map(|a| &a.value);
        let it = |e: &Expr| self.iter(e, scope).0;
        Some(match (n, first) {
            ("len", Some(x)) => {
                let t = self.ty(x, scope);
                let base = self.model().refined_base(&t).unwrap_or(t.clone());
                let (c, _) = self.deep_base(self.emit(x, scope), &t);
                if base == Ty::Str { format!("({c}.chars().count() as i64)") } else { format!("({c}.len() as i64)") }
            }
            ("all", Some(x)) => format!("{}.all(|b| b)", it(x)),
            ("any", Some(x)) => format!("{}.any(|b| b)", it(x)),
            ("sum", Some(x)) => format!("{}.sum::<{}>()", it(x), self.names.ty(&self.iter(x, scope).1)),
            ("min", Some(x)) if args.len() == 1 => format!("{}.min().expect(\"min of an empty collection\")", it(x)),
            ("max", Some(x)) if args.len() == 1 => format!("{}.max().expect(\"max of an empty collection\")", it(x)),
            ("min" | "max", Some(_)) => {
                let xs: Vec<String> = args.iter().map(|a| self.emit(&a.value, scope)).collect();
                format!("[{}].into_iter().{n}().unwrap()", xs.join(", "))
            }
            ("sorted", Some(x)) => format!("{{ let mut v: Vec<_> = {}.collect(); v.sort(); v }}", it(x)),
            ("list", Some(x)) => format!("{}.collect::<Vec<_>>()", it(x)),
            ("set", Some(x)) => format!("{}.collect::<std::collections::HashSet<_>>()", it(x)),
            ("matches", Some(x)) => {
                let re = match args.get(1).map(|a| &a.value.kind) {
                    Some(ExprKind::Str { value, .. }) => value.clone(),
                    _ => String::new(),
                };
                let t = self.ty(x, scope);
                let (c, _) = self.deep_base(self.emit(x, scope), &t);
                format!("crate::wh_regex!({}).is_match(&{c})", str_lit(&re))
            }
            ("abs", Some(x)) => format!("({}).abs()", self.emit(x, scope)),
            ("str", Some(x)) => format!("({}).to_string()", self.emit(x, scope)),
            ("int", Some(x)) => format!("(({}) as i64)", self.emit(x, scope)),
            ("float", Some(x)) => format!("(({}) as f64)", self.emit(x, scope)),
            ("env", Some(x)) => format!("std::env::var({}).unwrap_or_default()", self.emit(x, scope)),
            ("old", Some(x)) => match &x.kind {
                ExprKind::Name(p) => match scope.old.iter().find(|(n, _)| n == p) {
                    Some((_, v)) => format!("{v}.clone()"),
                    None => self.emit(x, scope),
                },
                _ => self.emit(x, scope),
            },
            _ => return None,
        })
    }

    /// Rust pattern for `raises E` / `raises E.Variant` checks.
    pub fn err_pattern(&self, err: &Ty, variant: &Option<String>) -> String {
        match (err.named_id(), variant) {
            (Some(id), Some(v)) => format!("Err({}::{v} {{ .. }})", self.names.path(id)),
            _ => "Err(_)".into(),
        }
    }

    /// Instance expressions for capabilities, for calls inside generated code.
    pub fn caps_from_vars(caps: &[String], by_ref: bool) -> Vec<(String, String)> {
        caps.iter().map(|c| (c.clone(), if by_ref { format!("&{}", cap_var(c)) } else { cap_var(c) })).collect()
    }
}
