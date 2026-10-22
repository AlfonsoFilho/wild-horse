//! Static semantics (language spec §13): expression typing, flow composition,
//! purity and capabilities, apps, tests and literate lints.

use std::collections::{BTreeMap, BTreeSet};

use wh_syntax::ast::{
    self, Arg, BinOp, ClauseKind, CmpOp, Expr, ExprKind, FlowExpr, FlowStmt, FlowStmtKind, ItemKind, UnaryOp,
};
use wh_syntax::{Diagnostic, Span};

use crate::model::*;
use crate::ty::{IntTy, NodeId, Ty};

// ------------------------------------------------------------------ environment

#[derive(Debug, Clone, Default)]
pub struct Env {
    pub vars: Vec<(String, Ty)>,
    pub result: Option<Ty>,
    pub self_ty: Option<Ty>,
    pub allow_old: bool,
}

impl Env {
    pub fn with(mut self, name: &str, ty: Ty) -> Self {
        self.vars.push((name.to_string(), ty));
        self
    }

    pub fn lookup(&self, name: &str) -> Option<&Ty> {
        self.vars.iter().rev().find(|(n, _)| n == name).map(|(_, t)| t)
    }

    /// Environment for the clauses of a def: its parameters.
    pub fn for_sig(sig: &SigInfo) -> Self {
        let mut env = Env::default();
        for p in &sig.params {
            env.vars.push((p.name.clone(), p.ty.clone()));
        }
        env
    }
}

pub const BUILTIN_FUNCTIONS: &[&str] = &[
    "len", "all", "any", "sum", "min", "max", "sorted", "set", "list", "matches", "abs", "str", "int", "float", "old",
    "env",
];

/// Lenient compatibility used inside spec expressions (Python-like coercions:
/// numeric widening, literals into refined types, implicit `Some`).
pub fn compatible(model: &Model, expected: &Ty, actual: &Ty) -> bool {
    if expected.is_unknown() || actual.is_unknown() || expected == actual {
        return true;
    }
    if expected.is_numeric() && actual.is_numeric() {
        return true;
    }
    if let Some(base) = model.refined_base(expected) {
        if compatible(model, &base, actual) {
            return true;
        }
    }
    if let Some(base) = model.refined_base(actual) {
        if compatible(model, expected, &base) {
            return true;
        }
    }
    match (expected, actual) {
        (Ty::Param(_), _) | (_, Ty::Param(_)) => true,
        (Ty::Option(a), Ty::Option(b)) => compatible(model, a, b),
        (Ty::Option(a), b) => compatible(model, a, b),
        (Ty::List(a), Ty::List(b))
        | (Ty::Set(a), Ty::Set(b))
        | (Ty::SortedSet(a), Ty::SortedSet(b))
        | (Ty::Stream(a), Ty::Stream(b))
        | (Ty::Array(a, _), Ty::Array(b, _))
        | (Ty::List(a), Ty::Array(b, _)) => compatible(model, a, b),
        (Ty::Dict(k1, v1), Ty::Dict(k2, v2)) | (Ty::SortedDict(k1, v1), Ty::SortedDict(k2, v2)) => {
            compatible(model, k1, k2) && compatible(model, v1, v2)
        }
        (Ty::Tuple(a), Ty::Tuple(b)) => a.len() == b.len() && a.iter().zip(b).all(|(x, y)| compatible(model, x, y)),
        (Ty::Named(a, x), Ty::Named(b, y)) => a == b && x.iter().zip(y).all(|(p, q)| compatible(model, p, q)),
        _ => false,
    }
}

/// Strict assignability for flow wiring (real Rust values flow between steps).
pub fn assignable(param: &Ty, value: &Ty) -> bool {
    if param.is_unknown() || value.is_unknown() || param == value {
        return true;
    }
    match (param, value) {
        (Ty::Param(_), _) => true,
        (Ty::List(a), Ty::List(b)) | (Ty::Option(a), Ty::Option(b)) | (Ty::Set(a), Ty::Set(b)) => assignable(a, b),
        _ => false,
    }
}

// ------------------------------------------------------------------ expression typer

pub struct Typer<'m> {
    pub model: &'m Model,
    pub module: &'m str,
    pub diags: Vec<(String, Span)>,
    /// Capability-using functions called from this expression (purity check).
    pub impure_calls: Vec<(NodeId, Span)>,
}

pub fn str_method(name: &str) -> Option<Ty> {
    Some(match name {
        "lower" | "upper" | "strip" | "lstrip" | "rstrip" | "replace" | "title" => Ty::Str,
        "startswith" | "endswith" | "contains" | "isdigit" | "isalpha" | "is_empty" => Ty::Bool,
        "split" | "lines" => Ty::List(Box::new(Ty::Str)),
        "len" | "find" | "count" => Ty::Int(IntTy::I64),
        _ => return None,
    })
}

impl<'m> Typer<'m> {
    pub fn new(model: &'m Model, module: &'m str) -> Self {
        Self { model, module, diags: Vec::new(), impure_calls: Vec::new() }
    }

    /// A literal used where `expected` is wanted must satisfy its refinement
    /// (spec §13 rule 9); reported at the literal.
    pub fn check_literal(&mut self, expected: &Ty, e: &Expr) {
        if let Some(msg) = crate::eval::check_literal(self.model, expected, e) {
            self.diags.push((msg, e.span));
        }
    }

    /// A call with literal arguments must satisfy the callee's `requires`
    /// where they can be decided at check time (same subset as refinements).
    fn check_requires(&mut self, node: &Node, params: &[ParamInfo], args: &[Arg], span: Span) {
        let Some(def) = node.def() else { return };
        let mut vals: BTreeMap<String, crate::eval::Value> = BTreeMap::new();
        let mut positional = args.iter().filter(|a| a.name.is_none());
        for (p, ap) in params.iter().zip(&def.sig.params) {
            let given = args.iter().find(|a| a.name.as_ref().is_some_and(|n| n.name == p.name)).or_else(|| positional.next());
            let v = match given {
                Some(a) => crate::eval::literal(&a.value),
                None => ap.default.as_ref().and_then(crate::eval::literal),
            };
            if let Some(v) = v {
                vals.insert(p.name.clone(), v);
            }
        }
        if vals.is_empty() {
            return;
        }
        for c in &def.clauses {
            let ClauseKind::Requires(e) = &c.kind else { continue };
            if crate::eval::eval_with(e, &|n| vals.get(n).cloned()) == Some(crate::eval::Value::Bool(false)) {
                let src = &self.model.files[node.file].text[e.span.range()];
                self.diags.push((format!("this call violates `requires {src}` of `{}`", node.name), span));
            }
        }
    }

    fn err(&mut self, msg: impl Into<String>, span: Span) -> Ty {
        self.diags.push((msg.into(), span));
        Ty::Unknown
    }

    fn resolve(&self, name: &str) -> Option<&'m Node> {
        let id = self.model.resolve_name(self.module, name)?;
        self.model.nodes.get(id)
    }

    pub fn expect_bool(&mut self, e: &Expr, env: &Env, what: &str) {
        let t = self.ty(e, env);
        if !matches!(t, Ty::Bool | Ty::Unknown) {
            self.err(format!("{what} must be a `bool` expression, found `{t}`"), e.span);
        }
    }

    pub fn ty(&mut self, e: &Expr, env: &Env) -> Ty {
        match &e.kind {
            ExprKind::Error => Ty::Unknown,
            ExprKind::Int(_) => Ty::Int(IntTy::I64),
            ExprKind::Float(_) => Ty::Float64,
            ExprKind::Str { kind: ast::StrKind::Bytes, .. } => Ty::Bytes,
            ExprKind::Str { .. } => Ty::Str,
            ExprKind::Bool(_) => Ty::Bool,
            ExprKind::None => Ty::Option(Box::new(Ty::Unknown)),
            ExprKind::Duration { .. } => Ty::Duration,
            ExprKind::Name(n) => self.name(n, e.span, env),
            ExprKind::Attr { base, attr } => self.attr(base, attr, env),
            ExprKind::Call { func, args } => self.call(func, args, e.span, env),
            ExprKind::Index { base, index } => {
                let bt = self.ty(base, env);
                self.ty(index, env);
                match &bt {
                    Ty::List(t) | Ty::Array(t, _) => (**t).clone(),
                    Ty::Dict(_, v) | Ty::SortedDict(_, v) => (**v).clone(),
                    Ty::Str => Ty::Str,
                    Ty::Tuple(_) | Ty::Unknown => Ty::Unknown,
                    other => self.err(format!("`{other}` cannot be indexed"), base.span),
                }
            }
            ExprKind::Slice { base, lo, hi } => {
                for x in [lo, hi].into_iter().flatten() {
                    self.ty(x, env);
                }
                self.ty(base, env)
            }
            ExprKind::Unary { op: UnaryOp::Not, expr } => {
                self.expect_bool(expr, env, "operand of `not`");
                Ty::Bool
            }
            ExprKind::Unary { op: UnaryOp::Neg, expr } => {
                let t = self.ty(expr, env);
                let t = self.model.refined_base(&t).unwrap_or(t);
                if !t.is_numeric() && !t.is_unknown() {
                    return self.err(format!("cannot negate `{t}`"), e.span);
                }
                t
            }
            ExprKind::Binary { op: op @ (BinOp::And | BinOp::Or), lhs, rhs } => {
                let what = if *op == BinOp::And { "operand of `and`" } else { "operand of `or`" };
                self.expect_bool(lhs, env, what);
                self.expect_bool(rhs, env, what);
                Ty::Bool
            }
            ExprKind::Binary { op, lhs, rhs } => {
                let a = self.ty(lhs, env);
                let b = self.ty(rhs, env);
                let a = self.model.refined_base(&a).unwrap_or(a);
                let b = self.model.refined_base(&b).unwrap_or(b);
                match (&a, &b) {
                    (Ty::Unknown, _) | (_, Ty::Unknown) => Ty::Unknown,
                    (Ty::Str, Ty::Str) if *op == BinOp::Add => Ty::Str,
                    (Ty::List(_), Ty::List(_)) if *op == BinOp::Add => a,
                    (x, y) if x.is_numeric() && y.is_numeric() => {
                        if matches!(x, Ty::Float64 | Ty::Float32) || matches!(y, Ty::Float64 | Ty::Float32) || *op == BinOp::Div {
                            Ty::Float64
                        } else {
                            x.clone()
                        }
                    }
                    (Ty::Duration, Ty::Duration) if matches!(op, BinOp::Add | BinOp::Sub) => Ty::Duration,
                    _ => self.err(format!("unsupported operands `{a}` and `{b}` for `{op:?}`"), e.span),
                }
            }
            ExprKind::Compare { first, rest } => {
                let mut prev = self.ty(first, env);
                for (op, rhs) in rest {
                    if matches!(op, CmpOp::Is | CmpOp::IsNot) {
                        match &rhs.kind {
                            ExprKind::None => {
                                if !matches!(prev, Ty::Option(_) | Ty::Unknown) {
                                    self.err(format!("`is None` on a value of type `{prev}`, which is never None"), rhs.span);
                                }
                            }
                            ExprKind::Name(v) => {
                                let ok = self.model.enum_info(&prev).is_some_and(|en| en.variants.iter().any(|x| &x.name == v));
                                if !ok && !prev.is_unknown() {
                                    self.err(format!("`{v}` is not a variant of `{prev}`"), rhs.span);
                                }
                            }
                            _ => {
                                self.err("`is` must be followed by `None` or a variant name", rhs.span);
                            }
                        }
                        continue;
                    }
                    let r = self.ty(rhs, env);
                    match op {
                        CmpOp::In | CmpOp::NotIn => {
                            let r_base = self.model.refined_base(&r).unwrap_or(r.clone());
                            match r_base.element() {
                                Some(el) => {
                                    if !compatible(self.model, &el, &prev) && !(r_base == Ty::Str) {
                                        self.err(format!("`{prev}` can never be in a `{r}`"), e.span);
                                    }
                                }
                                None => {
                                    self.err(format!("`in` needs a collection, found `{r}`"), rhs.span);
                                }
                            }
                        }
                        _ => {
                            if !compatible(self.model, &prev, &r) && !compatible(self.model, &r, &prev) {
                                self.err(format!("cannot compare `{prev}` with `{r}`"), e.span);
                            }
                        }
                    }
                    prev = r;
                }
                Ty::Bool
            }
            ExprKind::IfElse { then, cond, otherwise } => {
                self.expect_bool(cond, env, "condition");
                let t = self.ty(then, env);
                self.ty(otherwise, env);
                t
            }
            ExprKind::Lambda { params, body } => {
                let mut inner = env.clone();
                for p in params {
                    inner = inner.with(&p.name, Ty::Unknown);
                }
                let r = self.ty(body, &inner);
                Ty::Fn(params.iter().map(|_| Ty::Unknown).collect(), Box::new(r))
            }
            ExprKind::List(items) => {
                let mut el = Ty::Unknown;
                for i in items {
                    let t = self.ty(i, env);
                    if el.is_unknown() {
                        el = t;
                    }
                }
                Ty::List(Box::new(el))
            }
            ExprKind::Tuple(items) => Ty::Tuple(items.iter().map(|i| self.ty(i, env)).collect()),
            ExprKind::Comprehension { elt, var, iter, cond, .. } => {
                let it = self.ty(iter, env);
                let el = self.iter_element(&it, iter.span);
                let inner = env.clone().with(&var.name, el);
                if let Some(c) = cond {
                    self.expect_bool(c, &inner, "comprehension condition");
                }
                Ty::List(Box::new(self.ty(elt, &inner)))
            }
            ExprKind::Quantified { var, iter, body, .. } => {
                let it = self.ty(iter, env);
                let el = self.iter_element(&it, iter.span);
                let inner = env.clone().with(&var.name, el);
                self.expect_bool(body, &inner, "quantified body");
                Ty::Bool
            }
        }
    }

    fn iter_element(&mut self, it: &Ty, span: Span) -> Ty {
        let base = self.model.refined_base(it).unwrap_or(it.clone());
        match base.element() {
            Some(t) => t,
            None => self.err(format!("cannot iterate over `{it}`"), span),
        }
    }

    fn name(&mut self, n: &str, span: Span, env: &Env) -> Ty {
        if let Some(t) = env.lookup(n) {
            return t.clone();
        }
        match n {
            "result" => {
                return match &env.result {
                    Some(t) => t.clone(),
                    None => self.err("`result` is only available in `ensures`", span),
                };
            }
            "self" => {
                return match &env.self_ty {
                    Some(t) => t.clone(),
                    None => self.err("`self` is only available in refinements and invariants", span),
                };
            }
            _ => {}
        }
        if let Some(node) = self.resolve(n) {
            match &node.kind {
                NodeKind::Def(d) => {
                    return Ty::Fn(d.sig.params.iter().map(|p| p.ty.clone()).collect(), Box::new(d.sig.ret.clone()));
                }
                NodeKind::Flow(f) => {
                    return Ty::Fn(f.sig.params.iter().map(|p| p.ty.clone()).collect(), Box::new(f.sig.ret.clone()));
                }
                NodeKind::Adapter(_) => return Ty::Named(node.id.clone(), vec![]),
                _ => return self.err(format!("`{n}` is a type, not a value"), span),
            }
        }
        // unit variant of an enum in scope
        if let Some(en) = self.enum_with_variant(n) {
            return Ty::Named(en, vec![]);
        }
        self.err(format!("unknown name `{n}`"), span)
    }

    fn enum_with_variant(&self, v: &str) -> Option<NodeId> {
        let scope = self.model.scopes.get(self.module)?;
        scope.names.values().find(|id| {
            matches!(&self.model.nodes[*id].kind, NodeKind::Enum(e) if e.variants.iter().any(|x| x.name == v))
        }).cloned()
    }

    fn attr(&mut self, base: &Expr, attr: &ast::Ident, env: &Env) -> Ty {
        // `Enum.Variant`, `module.name`
        if let ExprKind::Name(b) = &base.kind {
            if env.lookup(b).is_none() && b != "result" && b != "self" {
                if let Some(node) = self.resolve(b) {
                    if let Some(e) = node.kind.enum_like() {
                        if e.variants.iter().any(|v| v.name == attr.name) {
                            return Ty::Named(node.id.clone(), vec![]);
                        }
                        let what = if matches!(node.kind, NodeKind::Machine(_)) { "state" } else { "variant" };
                        return self.err(format!("`{b}` has no {what} `{}`", attr.name), attr.span);
                    }
                }
                let path = format!("{b}.{}", attr.name);
                if let Some(id) = self.model.resolve_path(self.module, &path) {
                    let node = &self.model.nodes[&id];
                    if let Some(sig) = node.sig() {
                        return Ty::Fn(sig.params.iter().map(|p| p.ty.clone()).collect(), Box::new(sig.ret.clone()));
                    }
                }
            }
        }
        let bt = self.ty(base, env);
        self.field_of(&bt, attr)
    }

    fn field_of(&mut self, bt: &Ty, attr: &ast::Ident) -> Ty {
        if bt.is_unknown() {
            return Ty::Unknown;
        }
        if let Some(fields) = self.model.struct_fields(bt) {
            if let Some(f) = fields.iter().find(|f| f.name == attr.name) {
                return f.ty.clone();
            }
            return self.err(format!("`{bt}` has no field `{}`", attr.name), attr.span);
        }
        if let Some(base) = self.model.refined_base(bt) {
            return self.field_of(&base, attr);
        }
        match bt {
            Ty::Tuple(ts) => {
                if let Ok(i) = attr.name.parse::<usize>() {
                    if let Some(t) = ts.get(i) {
                        return t.clone();
                    }
                }
                self.err(format!("tuple `{bt}` has no element `{}`", attr.name), attr.span)
            }
            _ => self.err(format!("`{bt}` has no field `{}`", attr.name), attr.span),
        }
    }

    fn check_args(&mut self, params: &[ParamInfo], args: &[Arg], span: Span, env: &Env, what: &str) {
        let required = params.iter().filter(|p| !p.has_default).count();
        let positional = args.iter().filter(|a| a.name.is_none()).count();
        let named: Vec<&Arg> = args.iter().filter(|a| a.name.is_some()).collect();
        if positional > params.len() {
            self.err(format!("{what} takes {} argument(s), found {}", params.len(), args.len()), span);
        }
        let mut given = vec![false; params.len()];
        for (i, a) in args.iter().filter(|a| a.name.is_none()).enumerate() {
            let t = self.ty(&a.value, env);
            if let Some(p) = params.get(i) {
                given[i] = true;
                if !compatible(self.model, &p.ty, &t) {
                    self.err(format!("argument `{}` expects `{}`, found `{t}`", p.name, p.ty), a.value.span);
                } else {
                    self.check_literal(&p.ty, &a.value);
                }
            }
        }
        for a in named {
            let n = a.name.as_ref().unwrap();
            let t = self.ty(&a.value, env);
            match params.iter().position(|p| p.name == n.name) {
                Some(i) => {
                    given[i] = true;
                    if !compatible(self.model, &params[i].ty, &t) {
                        self.err(format!("argument `{}` expects `{}`, found `{t}`", n.name, params[i].ty), a.value.span);
                    } else {
                        self.check_literal(&params[i].ty, &a.value);
                    }
                }
                None => {
                    self.err(format!("{what} has no parameter `{}`", n.name), n.span);
                }
            }
        }
        let missing: Vec<&str> =
            params.iter().zip(&given).filter(|(p, g)| !**g && !p.has_default).map(|(p, _)| p.name.as_str()).collect();
        if !missing.is_empty() && positional <= params.len() {
            self.err(format!("{what} is missing argument(s): {}", missing.join(", ")), span);
        }
        let _ = required;
    }

    fn call(&mut self, func: &Expr, args: &[Arg], span: Span, env: &Env) -> Ty {
        match &func.kind {
            ExprKind::Name(n) if env.lookup(n).is_none() => {
                if BUILTIN_FUNCTIONS.contains(&n.as_str()) && self.resolve(n).is_none() {
                    return self.builtin(n, args, span, env);
                }
                if let Some(node) = self.resolve(n) {
                    return self.call_node(node, args, span, env);
                }
                if let Some(en) = self.enum_with_variant(n) {
                    for a in args {
                        self.ty(&a.value, env);
                    }
                    return Ty::Named(en, vec![]);
                }
                for a in args {
                    self.ty(&a.value, env);
                }
                self.err(format!("unknown function `{n}`"), func.span)
            }
            ExprKind::Attr { base, attr } => {
                if let ExprKind::Name(b) = &base.kind {
                    if env.lookup(b).is_none() && b != "result" && b != "self" {
                        if let Some(node) = self.resolve(b) {
                            match &node.kind {
                                NodeKind::Machine(m) if attr.name == "step" => {
                                    let state = Ty::Named(node.id.clone(), vec![]);
                                    let params = [
                                        ParamInfo::synthetic("state", ast::ParamMode::Own, state.clone(), false),
                                        ParamInfo::synthetic(m.event_param.clone(), ast::ParamMode::Own, m.event.clone(), false),
                                    ];
                                    self.check_args(&params, args, span, env, &format!("`{b}.step`"));
                                    return Ty::Tuple(vec![state, Ty::Option(Box::new(m.effect.clone()))]);
                                }
                                NodeKind::Machine(_) if attr.name == "initial" => {
                                    self.check_args(&[], args, span, env, &format!("`{b}.initial`"));
                                    return Ty::Named(node.id.clone(), vec![]);
                                }
                                NodeKind::Machine(m) => {
                                    for a in args {
                                        self.ty(&a.value, env);
                                    }
                                    if !m.states.variants.iter().any(|v| v.name == attr.name) {
                                        return self.err(format!("`{b}` has no state `{}`; machines also offer `step` and `initial`", attr.name), attr.span);
                                    }
                                    return Ty::Named(node.id.clone(), vec![]);
                                }
                                NodeKind::Enum(e) => {
                                    for a in args {
                                        self.ty(&a.value, env);
                                    }
                                    if !e.variants.iter().any(|v| v.name == attr.name) {
                                        return self.err(format!("`{b}` has no variant `{}`", attr.name), attr.span);
                                    }
                                    return Ty::Named(node.id.clone(), vec![]);
                                }
                                NodeKind::Refined { .. } if attr.name == "parse" => {
                                    for a in args {
                                        self.ty(&a.value, env);
                                    }
                                    return Ty::Named(node.id.clone(), vec![]);
                                }
                                _ => {}
                            }
                        }
                        let path = format!("{b}.{}", attr.name);
                        if let Some(id) = self.model.resolve_path(self.module, &path) {
                            let node = &self.model.nodes[&id];
                            return self.call_node(node, args, span, env);
                        }
                    }
                }
                // method call on a value
                let bt = self.ty(base, env);
                for a in args {
                    self.ty(&a.value, env);
                }
                self.method(&bt, attr)
            }
            _ => {
                let ft = self.ty(func, env);
                for a in args {
                    self.ty(&a.value, env);
                }
                match ft {
                    Ty::Fn(_, r) => *r,
                    Ty::Unknown => Ty::Unknown,
                    other => self.err(format!("`{other}` is not callable"), func.span),
                }
            }
        }
    }

    fn method(&mut self, bt: &Ty, attr: &ast::Ident) -> Ty {
        let base = self.model.refined_base(bt).unwrap_or(bt.clone());
        let m = attr.name.as_str();
        let r = match &base {
            Ty::Unknown => Some(Ty::Unknown),
            Ty::Str => str_method(m),
            Ty::List(t) | Ty::Set(t) | Ty::SortedSet(t) | Ty::Array(t, _) => match m {
                "contains" | "is_empty" => Some(Ty::Bool),
                "first" | "last" => Some(Ty::Option(t.clone())),
                "len" => Some(Ty::Int(IntTy::I64)),
                _ => None,
            },
            Ty::Dict(k, v) | Ty::SortedDict(k, v) => match m {
                "keys" => Some(Ty::List(k.clone())),
                "values" => Some(Ty::List(v.clone())),
                "get" => Some(Ty::Option(v.clone())),
                "contains" | "is_empty" => Some(Ty::Bool),
                "len" => Some(Ty::Int(IntTy::I64)),
                _ => None,
            },
            Ty::Option(t) => match m {
                "unwrap" => Some((**t).clone()),
                "is_some" | "is_none" => Some(Ty::Bool),
                _ => None,
            },
            _ => None,
        };
        match r {
            Some(t) => t,
            None => self.err(format!("`{bt}` has no method `{m}`"), attr.span),
        }
    }

    fn call_node(&mut self, node: &'m Node, args: &[Arg], span: Span, env: &Env) -> Ty {
        match &node.kind {
            NodeKind::Def(d) => {
                if !d.sig.uses.is_empty() {
                    self.impure_calls.push((node.id.clone(), span));
                }
                self.check_args(&d.sig.params, args, span, env, &format!("`{}`", node.name));
                self.check_requires(node, &d.sig.params, args, span);
                d.sig.ret.clone()
            }
            NodeKind::Flow(f) => {
                if !f.sig.uses.is_empty() {
                    self.impure_calls.push((node.id.clone(), span));
                }
                self.check_args(&f.sig.params, args, span, env, &format!("`{}`", node.name));
                f.sig.ret.clone()
            }
            NodeKind::Struct(_) | NodeKind::Adapter(_) => {
                let fields = self.model.struct_fields(&Ty::Named(node.id.clone(), vec![])).unwrap_or_default();
                let params: Vec<ParamInfo> = fields
                    .iter()
                    .map(|f| ParamInfo {
                        name: f.name.clone(),
                        mode: ast::ParamMode::Own,
                        ty: f.ty.clone(),
                        has_default: f.has_default,
                        written: f.written.clone(),
                        alias: f.alias.clone(),
                        is_sensitive: f.is_sensitive,
                    })
                    .collect();
                self.check_args(&params, args, span, env, &format!("`{}`", node.name));
                Ty::Named(node.id.clone(), vec![])
            }
            NodeKind::Refined { base, .. } => {
                if args.len() != 1 {
                    return self.err(format!("`{}` takes exactly one value", node.name), span);
                }
                let t = self.ty(&args[0].value, env);
                if !compatible(self.model, base, &t) {
                    self.err(format!("`{}` wraps `{base}`, found `{t}`", node.name), args[0].value.span);
                } else {
                    self.check_literal(&Ty::Named(node.id.clone(), vec![]), &args[0].value);
                }
                Ty::Named(node.id.clone(), vec![])
            }
            _ => self.err(format!("`{}` cannot be called", node.name), span),
        }
    }

    fn builtin(&mut self, n: &str, args: &[Arg], span: Span, env: &Env) -> Ty {
        let tys: Vec<Ty> = args.iter().map(|a| self.ty(&a.value, env)).collect();
        let arity = |s: &mut Self, k: usize| {
            if tys.len() != k {
                s.err(format!("`{n}` takes {k} argument(s), found {}", tys.len()), span);
                false
            } else {
                true
            }
        };
        let elem = |s: &mut Self, t: &Ty| {
            let base = s.model.refined_base(t).unwrap_or(t.clone());
            match base.element() {
                Some(e) => e,
                None => s.err(format!("`{n}` needs a collection, found `{t}`"), span),
            }
        };
        match n {
            "len" => {
                if arity(self, 1) {
                    elem(self, &tys[0]);
                }
                Ty::Int(IntTy::I64)
            }
            "all" | "any" => {
                if arity(self, 1) {
                    let e = elem(self, &tys[0]);
                    if !matches!(e, Ty::Bool | Ty::Unknown) {
                        self.err(format!("`{n}` needs booleans, found elements of type `{e}`"), span);
                    }
                }
                Ty::Bool
            }
            "sum" | "min" | "max" => {
                if tys.len() == 1 {
                    return elem(self, &tys[0]);
                }
                tys.first().cloned().unwrap_or(Ty::Unknown)
            }
            "sorted" | "list" => {
                if arity(self, 1) {
                    return Ty::List(Box::new(elem(self, &tys[0])));
                }
                Ty::Unknown
            }
            "set" => {
                if arity(self, 1) {
                    return Ty::Set(Box::new(elem(self, &tys[0])));
                }
                Ty::Unknown
            }
            "matches" => {
                if arity(self, 2) {
                    match &args[1].value.kind {
                        ExprKind::Str { value, .. } => {
                            if let Err(e) = regex::Regex::new(value) {
                                let first = e.to_string().lines().last().unwrap_or("").trim().to_string();
                                self.err(format!("invalid regular expression: {first}"), args[1].value.span);
                            }
                        }
                        _ => {
                            self.err("`matches` needs a regular expression string literal", args[1].value.span);
                        }
                    }
                }
                Ty::Bool
            }
            "abs" => tys.first().cloned().unwrap_or(Ty::Unknown),
            "str" | "env" => Ty::Str,
            "int" => Ty::Int(IntTy::I64),
            "float" => Ty::Float64,
            "old" => {
                if !env.allow_old {
                    self.err("`old` is only available in `ensures`", span);
                }
                tys.first().cloned().unwrap_or(Ty::Unknown)
            }
            _ => Ty::Unknown,
        }
    }
}

// ------------------------------------------------------------------ checker

struct Checker {
    diags: Vec<FileDiagnostic>,
}

impl Checker {
    fn push(&mut self, file: usize, d: Vec<(String, Span)>) {
        for (msg, span) in d {
            self.diags.push(FileDiagnostic { file, diag: Diagnostic::error(msg, span) });
        }
    }

    fn typer_diags(&mut self, file: usize, t: Typer<'_>, pure_context: bool) {
        if pure_context {
            for (id, span) in &t.impure_calls {
                self.diags.push(FileDiagnostic {
                    file,
                    diag: Diagnostic::error(
                        format!("spec expressions must be pure, but `{id}` uses capabilities"),
                        *span,
                    ),
                });
            }
        }
        self.push(file, t.diags);
    }
}

pub const KNOWN_PATTERNS: &[&str] =
    &["Transform", "Pipeline", "StateMachine", "Repository", "Adapter", "InMemory", "Builder", "Strategy", "Memoize", "GameLoop"];
/// Built-in flow combinators (spec §7); a definition with the same name wins.
pub const FLOW_COMBINATORS: &[&str] = &["map", "filter", "each", "fold", "flat_map", "retry"];
pub const OPTIMIZE_TARGETS: &[&str] = &["speed", "latency", "throughput", "memory", "size"];
pub const TRACE_LEVELS: &[&str] = &["off", "error", "warn", "info", "debug", "trace"];
pub const LOG_FORMATS: &[&str] = &["pretty", "compact", "json"];

pub fn check(model: &mut Model) {
    let wh_t = std::time::Instant::now();
    let prof = std::env::var("WH_PROF").is_ok();
    let lap = |l: &str| if prof { eprintln!("  check.{l}: {:.2} ms", wh_t.elapsed().as_secs_f64() * 1e3) };
    let mut c = Checker { diags: Vec::new() };
    let ids: Vec<NodeId> = model.nodes.keys().cloned().collect();

    // machines first: their inferred states are types other nodes use
    for id in &ids {
        let node = &model.nodes[id];
        let ItemKind::Machine(m) = &node.item.kind else { continue };
        let (info, d) = crate::machine::analyze_machine(model, node, m);
        let file = node.file;
        if let Some(l) = &m.trace {
            if !TRACE_LEVELS.contains(&l.name.as_str()) {
                c.push(file, vec![(format!("unknown trace level `{}`", l.name), l.span)]);
            }
        }
        c.push(file, d);
        model.nodes.get_mut(id).unwrap().kind = NodeKind::Machine(info);
    }

    for id in &ids {
        let node = &model.nodes[id];
        let file = node.file;
        // naming conventions
        let type_like = matches!(
            node.item.kind,
            ItemKind::Alias(_) | ItemKind::TypeDef(_) | ItemKind::Struct(_) | ItemKind::Enum(_)
                | ItemKind::Capability(_) | ItemKind::Adapter(_) | ItemKind::Machine(_)
        );
        if type_like && !is_pascal(&node.name) {
            c.push(file, vec![(format!("type names are PascalCase: `{}`", node.name), node.name_span)]);
        }
        if !type_like && !is_snake(&node.name) {
            c.push(file, vec![(format!("function, flow and app names are snake_case: `{}`", node.name), node.name_span)]);
        }
        for p in &node.patterns {
            if !KNOWN_PATTERNS.contains(&p.as_str()) {
                c.push(file, vec![(format!("unknown pattern `{p}`"), node.name_span)]);
            } else {
                let d = check_pattern(model, node, p);
                c.push(file, d);
            }
        }
        match (&node.item.kind, &node.kind) {
            (ItemKind::TypeDef(t), NodeKind::Refined { base, .. }) => {
                if let Some(r) = &t.refinement {
                    let mut typer = Typer::new(model, &node.module);
                    let env = Env { self_ty: Some(base.clone()), ..Default::default() };
                    typer.expect_bool(r, &env, "refinement");
                    c.typer_diags(file, typer, true);
                }
            }
            (ItemKind::Struct(s), NodeKind::Struct(info)) => {
                let keys: Vec<_> = s.fields.iter().filter(|f| f.is_key).collect();
                if keys.len() > 1 {
                    c.push(file, vec![("a struct can have at most one `key` field".into(), keys[1].span)]);
                }
                let mut typer = Typer::new(model, &node.module);
                let env = Env { self_ty: Some(Ty::Named(id.clone(), vec![])), ..Default::default() };
                for inv in &s.invariants {
                    typer.expect_bool(inv, &env, "invariant");
                }
                for (f, fi) in s.fields.iter().zip(&info.fields) {
                    if let Some(d) = &f.default {
                        let t = typer.ty(d, &Env::default());
                        if !compatible(model, &fi.ty, &t) {
                            typer.diags.push((format!("default for `{}` should be `{}`, found `{t}`", f.name.name, fi.ty), d.span));
                        } else {
                            typer.check_literal(&fi.ty, d);
                        }
                    }
                }
                let mut seen = BTreeSet::new();
                for f in &s.fields {
                    if !seen.insert(&f.name.name) {
                        typer.diags.push((format!("duplicate field `{}`", f.name.name), f.name.span));
                    }
                    if !is_snake(&f.name.name) {
                        typer.diags.push((format!("field names are snake_case: `{}`", f.name.name), f.name.span));
                    }
                }
                // direct self-containment has infinite size
                for fi in &info.fields {
                    if fi.ty == Ty::Named(id.clone(), vec![]) {
                        typer.diags.push((
                            format!("`{}` contains itself directly; use `{} | None` or a list", node.name, node.name),
                            fi.span,
                        ));
                    }
                }
                c.typer_diags(file, typer, true);
            }
            (ItemKind::Enum(e), NodeKind::Enum(_)) => {
                let mut seen = BTreeSet::new();
                for v in &e.variants {
                    if !seen.insert(&v.name.name) {
                        c.push(file, vec![(format!("duplicate variant `{}`", v.name.name), v.name.span)]);
                    }
                    if !is_pascal(&v.name.name) {
                        c.push(file, vec![(format!("variant names are PascalCase: `{}`", v.name.name), v.name.span)]);
                    }
                    if v.items.iter().any(|i| i.is_from) && v.items.len() != 1 {
                        c.push(file, vec![("a `from` variant must have exactly one item".into(), v.span)]);
                    }
                }
                if e.variants.is_empty() {
                    c.push(file, vec![(format!("enum `{}` has no variants", node.name), node.name_span)]);
                }
            }
            (ItemKind::Def(def), NodeKind::Def(info)) => {
                check_def(model, &mut c, node, def, info);
            }
            (ItemKind::Capability(cap), NodeKind::Capability(info)) => {
                let mut typer = Typer::new(model, &node.module);
                for (m, mi) in cap.methods.iter().zip(&info.methods) {
                    let env = Env::for_sig(&mi.sig);
                    let ensures_env = Env { result: Some(mi.sig.ret.clone()), allow_old: true, ..env.clone() };
                    for cl in &m.clauses {
                        match &cl.kind {
                            ClauseKind::Requires(e) => typer.expect_bool(e, &env, "`requires`"),
                            ClauseKind::Ensures(e) => typer.expect_bool(e, &ensures_env, "`ensures`"),
                            ClauseKind::RaisesWhen { error, when } => {
                                if mi.sig.raises.is_none() {
                                    typer.diags.push(("`raises … when` needs `raises E` in the signature".into(), cl.span));
                                }
                                model.resolve_error_ref(&node.module, error, &mut typer.diags);
                                typer.expect_bool(when, &env, "`raises … when` condition");
                            }
                            ClauseKind::Note(_) | ClauseKind::Pure => {}
                            _ => typer.diags.push((
                                "capability methods take `requires`, `ensures`, `raises … when` and `note` clauses; state examples as scenario tests".into(),
                                cl.span,
                            )),
                        }
                    }
                }
                c.typer_diags(file, typer, true);
            }
            (ItemKind::Adapter(a), NodeKind::Adapter(info)) => {
                let tested = node.patterns.iter().any(|p| p == "Adapter" || p == "InMemory");
                if tested && info.implements.iter().any(|cap| model.capability_has_contracts(cap)) {
                    if let Some(f) = a.fields.iter().find(|f| f.default.is_none()) {
                        c.diags.push(FileDiagnostic {
                            file,
                            diag: Diagnostic::warning(
                                format!("`{}` gets no contract tests: its field `{}` has no default to build it with", node.name, f.name.name),
                                f.span,
                            ),
                        });
                    }
                }
                for cl in &a.clauses {
                    if !matches!(cl.kind, ClauseKind::Pattern(_) | ClauseKind::Note(_) | ClauseKind::Unsafe { .. } | ClauseKind::Trace(_)) {
                        c.push(file, vec![("adapters only take `pattern`, `note`, `unsafe` and `trace` clauses".into(), cl.span)]);
                    }
                }
            }
            _ => {}
        }
    }

    lap("nodes");
    // flows: two passes so flows calling flows see inferred capabilities;
    // only the final pass's diagnostics are kept
    let mut flow_diags = Vec::new();
    for _ in 0..2 {
        let mut pass = Checker { diags: Vec::new() };
        let mut updates = Vec::new();
        for id in &ids {
            let node = &model.nodes[id];
            if let (ItemKind::Flow(f), NodeKind::Flow(info)) = (&node.item.kind, &node.kind) {
                if let Some(l) = &f.trace {
                    if !TRACE_LEVELS.contains(&l.name.as_str()) {
                        pass.push(node.file, vec![(format!("unknown trace level `{}`", l.name), l.span)]);
                    }
                }
                let fi = analyze_flow(model, &mut pass, node.file, &node.module, id, f, &info.sig);
                updates.push((id.clone(), fi));
            }
        }
        for (id, fi) in updates {
            model.nodes.get_mut(&id).unwrap().kind = NodeKind::Flow(fi);
        }
        flow_diags = pass.diags;
    }
    c.diags.extend(flow_diags);

    lap("flows");
    // apps
    for id in &ids {
        let node = &model.nodes[id];
        let ItemKind::App(app) = &node.item.kind else { continue };
        let info = analyze_app(model, &mut c, node, app);
        model.nodes.get_mut(id).unwrap().kind = NodeKind::App(info);
    }

    // a web app is a library of its own: it cannot share the crate with binaries
    let apps: Vec<(&Node, bool)> = model
        .nodes
        .values()
        .filter_map(|n| match &n.kind {
            NodeKind::App(a) => Some((n, a.profile().kind == wh_targets::TargetKind::Web)),
            _ => None,
        })
        .collect();
    if apps.len() > 1 {
        for (n, _) in apps.iter().filter(|(_, web)| *web) {
            c.push(n.file, vec![("a web(wasm) app must be the only app of its project".into(), n.name_span)]);
        }
    }

    lap("apps");
    // tests
    let tests = std::mem::take(&mut model.tests);
    let mut checked = Vec::new();
    for mut t in tests {
        let mut typer = Typer::new(model, &t.module);
        let mut provided = BTreeSet::new();
        for g in &t.test.givens {
            let p = resolve_provide(model, &t.module, g, &mut typer.diags);
            provided.insert(p.capability.clone());
            t.givens.push(p);
        }
        for e in &t.test.expects {
            typer.ty(&e.expr, &Env::default());
            if let Some(r) = &e.raises {
                let mut d = Vec::new();
                model.resolve_error_ref(&t.module, r, &mut d);
                typer.diags.extend(d);
            }
        }
        let needed: Vec<(NodeId, Span)> = typer.impure_calls.clone();
        for (callee, span) in needed {
            for cap in model.uses_of(&callee) {
                if !provided.contains(&cap) {
                    typer.diags.push((format!("`{callee}` uses `{cap}`; add `given {} = …`", short(&cap)), span));
                }
            }
        }
        c.typer_diags(t.file, typer, false);
        checked.push(t);
    }
    model.tests = checked;

    lap("tests");
    // references in prose and literate lints
    for (fi, f) in model.files.iter().enumerate() {
        for r in &f.doc.refs {
            if !model.resolves_ref(&f.module, &r.target) {
                c.diags.push(FileDiagnostic {
                    file: fi,
                    diag: Diagnostic::warning(format!("reference to unknown definition `{}`", r.target), r.span),
                });
            }
        }
    }
    for node in model.nodes.values() {
        if node.prose.is_none() && !matches!(node.kind, NodeKind::App(_)) {
            c.diags.push(FileDiagnostic {
                file: node.file,
                diag: Diagnostic::lint(format!("`{}` has no prose or docstring explaining it", node.name), node.name_span),
            });
        }
    }

    // declared crates
    let declared: BTreeSet<&str> = model.depends.iter().map(|d| d.crate_name.as_str()).collect();
    for node in model.nodes.values() {
        if let NodeKind::Def(DefInfo { extern_path: Some(p), .. }) = &node.kind {
            let krate = p.split("::").next().unwrap_or("");
            if !["std", "core", "alloc"].contains(&krate) && !declared.contains(krate) {
                c.diags.push(FileDiagnostic {
                    file: node.file,
                    diag: Diagnostic::error(format!("crate `{krate}` is used but not declared; add `depends {krate} = \"…\"`"), node.name_span),
                });
            }
        }
    }

    lap("lints");
    model.diagnostics.extend(c.diags);
}

/// Which kind of node each pattern of the catalog applies to (spec §9),
/// plus the shape rules that need more than the node itself.
fn check_pattern(model: &Model, node: &Node, p: &str) -> Vec<(String, Span)> {
    let mut d = Vec::new();
    let applies: &[&str] = match p {
        "Transform" | "Memoize" | "GameLoop" => &["def"],
        "Pipeline" => &["flow"],
        "StateMachine" => &["machine"],
        "Repository" => &["capability", "adapter"],
        "Adapter" | "InMemory" => &["adapter"],
        "Builder" => &["struct"],
        "Strategy" => &["capability"],
        _ => return d,
    };
    let label = node.kind.label();
    if !applies.contains(&label) {
        d.push((format!("pattern `{p}` applies to {}, not to a {label}", applies.join(" or ")), node.name_span));
        return d;
    }
    match p {
        "Builder" => {
            if let Some(t) = model.pattern_arg(node, p) {
                if t.named_id() != Some(node.id.as_str()) {
                    d.push((format!("`Builder[{t}]` on `{}` must name the struct itself", node.name), node.name_span));
                }
            }
        }
        "Repository" => {
            let item = model.pattern_arg(node, p);
            let caps: Vec<NodeId> = match &node.kind {
                NodeKind::Adapter(a) => a.implements.clone(),
                _ => vec![node.id.clone()],
            };
            if !caps.iter().any(|cap| model.repository_shape(cap, item.as_ref()).is_some()) {
                d.push((
                    "pattern `Repository` needs an insert-style method taking one `T` (`insert`, `add`, `save`, …) and a find-style method (`find`, `get`, …) taking the key and returning `T | None`; the key is what insert returns or `T`'s `key` field".into(),
                    node.name_span,
                ));
            }
        }
        "Strategy" => {
            if let NodeKind::Capability(c) = &node.kind {
                if c.methods.len() != 1 {
                    d.push(("pattern `Strategy` needs a capability with exactly one method".into(), node.name_span));
                }
            }
        }
        "Pipeline" => {
            if let ItemKind::Flow(f) = &node.item.kind {
                if f.body.len() != 1 || !matches!(f.body[0].kind, FlowStmtKind::Expr(_) | FlowStmtKind::Return(Some(_))) {
                    d.push(("pattern `Pipeline` needs a flow that is a single `|>` chain".into(), node.name_span));
                }
            }
        }
        _ => {}
    }
    d
}

fn short(id: &str) -> &str {
    id.rsplit('.').next().unwrap_or(id)
}

fn check_def(model: &Model, c: &mut Checker, node: &Node, def: &ast::Def, info: &DefInfo) {
    let file = node.file;
    let mut typer = Typer::new(model, &node.module);
    let env = Env::for_sig(&info.sig);
    let ensures_env = Env { result: Some(info.sig.ret.clone()), allow_old: true, ..env.clone() };
    let mut seen = BTreeSet::new();
    for (p, pi) in def.sig.params.iter().zip(&info.sig.params) {
        if let Some(d) = &p.default {
            let t = typer.ty(d, &Env::default());
            if !compatible(model, &pi.ty, &t) {
                typer.diags.push((format!("default for `{}` should be `{}`, found `{t}`", p.name.name, pi.ty), d.span));
            } else {
                typer.check_literal(&pi.ty, d);
            }
        }
    }
    for p in &def.sig.params {
        if !seen.insert(&p.name.name) {
            typer.diags.push((format!("duplicate parameter `{}`", p.name.name), p.name.span));
        }
        if !is_snake(&p.name.name) {
            typer.diags.push((format!("parameter names are snake_case: `{}`", p.name.name), p.name.span));
        }
    }
    for cl in &def.clauses {
        match &cl.kind {
            ClauseKind::Requires(e) => typer.expect_bool(e, &env, "`requires`"),
            ClauseKind::Ensures(e) => typer.expect_bool(e, &ensures_env, "`ensures`"),
            ClauseKind::RaisesWhen { when, .. } => {
                if info.sig.raises.is_none() {
                    typer.diags.push(("`raises … when` needs `raises E` in the signature".into(), cl.span));
                }
                typer.expect_bool(when, &env, "`raises … when` condition")
            }
            ClauseKind::Example { expr, raises } => {
                if let Some(r) = raises {
                    let (err, _) = model.resolve_error_ref(&node.module, r, &mut typer.diags);
                    typer.ty(expr, &Env::default());
                    if let Some(sig_err) = &info.sig.raises {
                        if !model.converts_into(&err, sig_err) && &err != sig_err && !err.is_unknown() {
                            typer.diags.push((format!("`{}` raises `{sig_err}`, not `{err}`", node.name), r.span));
                        }
                    } else if !err.is_unknown() {
                        // examples may test raising callees; only warn when it's this def
                        if expr.callee_name() == Some(&node.name) {
                            typer.diags.push((format!("`{}` does not raise", node.name), r.span));
                        }
                    }
                } else {
                    typer.expect_bool(expr, &Env::default(), "`example`");
                }
            }
            ClauseKind::Property { params, given, body, .. } => {
                let mut penv = Env::default();
                let mut pd = Vec::new();
                for p in params {
                    let t = model.resolve_ty(&node.module, &p.ty, &[], &mut pd);
                    penv = penv.with(&p.name.name, t);
                }
                typer.diags.extend(pd);
                for (n, g) in given {
                    if !params.iter().any(|p| p.name.name == n.name) {
                        typer.diags.push((format!("`given {}` does not name a property parameter", n.name), n.span));
                    }
                    typer.ty(g, &Env::default());
                }
                typer.expect_bool(body, &penv, "property body");
            }
            ClauseKind::Bench { params, body, .. } => {
                let mut benv = env.clone();
                let mut pd = Vec::new();
                for p in params {
                    let t = model.resolve_ty(&node.module, &p.ty, &[], &mut pd);
                    benv = benv.with(&p.name.name, t);
                }
                typer.diags.extend(pd);
                typer.ty(body, &benv);
            }
            ClauseKind::Mutate(e) => {
                let menv = Env::default().with("score", Ty::Float64);
                typer.expect_bool(e, &menv, "`mutate` threshold");
            }
            ClauseKind::Optimize(t) => {
                if !OPTIMIZE_TARGETS.contains(&t.name.as_str()) {
                    typer.diags.push((format!("unknown optimization target `{}`; use one of {}", t.name, OPTIMIZE_TARGETS.join(", ")), t.span));
                }
            }
            ClauseKind::Budget { time, .. } => {
                if !matches!(time.kind, ExprKind::Duration { .. }) {
                    typer.diags.push(("`budget` needs a duration such as `50us`".into(), time.span));
                }
            }
            ClauseKind::Pure => {
                if !info.sig.uses.is_empty() {
                    typer.diags.push(("a `pure` def cannot use capabilities".into(), cl.span));
                }
            }
            ClauseKind::Trace(l) => {
                if !TRACE_LEVELS.contains(&l.name.as_str()) {
                    typer.diags.push((format!("unknown trace level `{}`; use one of {}", l.name, TRACE_LEVELS.join(", ")), l.span));
                }
            }
            ClauseKind::Pattern(_) | ClauseKind::Note(_) | ClauseKind::Complexity(_) | ClauseKind::Alloc(_)
            | ClauseKind::Unsafe { .. } | ClauseKind::NoPanic => {}
        }
    }
    // pattern shape rules
    if node.patterns.iter().any(|p| p == "Transform") {
        if !info.sig.uses.is_empty() {
            typer.diags.push(("pattern `Transform` requires a pure def".into(), node.name_span));
        }
        if info.sig.params.len() != 1 {
            typer.diags.push(("pattern `Transform` requires exactly one parameter".into(), node.name_span));
        }
    }
    if node.patterns.iter().any(|p| p == "Memoize") {
        if !info.sig.uses.is_empty() {
            typer.diags.push(("pattern `Memoize` requires a pure def".into(), node.name_span));
        }
        if !def.sig.generics.is_empty() || info.sig.is_async {
            typer.diags.push(("pattern `Memoize` needs a non-generic, non-async def".into(), node.name_span));
        }
        for (p, pi) in def.sig.params.iter().zip(&info.sig.params) {
            if pi.mode == ast::ParamMode::Mut {
                typer.diags.push(("pattern `Memoize` cannot cache a def that mutates its argument".into(), p.span));
            } else if !model.type_is_hashable(&pi.ty) {
                typer.diags.push((format!("pattern `Memoize` needs `Hash` inputs, but `{}: {}` cannot be hashed", pi.name, pi.ty), p.span));
            }
        }
    }
    c.typer_diags(file, typer, true);
}

// ------------------------------------------------------------------ flows

struct FlowCtx<'a> {
    model: &'a Model,
    module: &'a str,
    flow_id: &'a str,
    sig: &'a SigInfo,
    steps: Vec<FlowStep>,
    counts: BTreeMap<String, usize>,
    diags: Vec<(String, Span)>,
    caps: BTreeSet<NodeId>,
    is_async: bool,
}

#[derive(Clone)]
struct Val {
    ty: Ty,
    src: StepInput,
}

impl<'a> FlowCtx<'a> {
    fn new_step(&mut self, label: &str, callee: Option<NodeId>, span: Span, inputs: Vec<StepInput>, output: Ty, raises: Option<Ty>) -> usize {
        let k = self.counts.entry(label.to_string()).or_insert(0);
        let id = format!("{}#{}[{}]", self.flow_id, label, k);
        *k += 1;
        self.steps.push(FlowStep { id, label: label.to_string(), callee, span, inputs, output, raises, bindings: vec![] });
        self.steps.len() - 1
    }

    fn lookup(&self, env: &[(String, Val)], name: &str) -> Option<Val> {
        env.iter().rev().find(|(n, _)| n == name).map(|(_, v)| v.clone())
    }

    fn typer_env(&self, env: &[(String, Val)]) -> Env {
        Env { vars: env.iter().map(|(n, v)| (n.clone(), v.ty.clone())).collect(), ..Default::default() }
    }

    fn check_raises(&mut self, raises: &Option<Ty>, callee: &str, span: Span) {
        let Some(err) = raises else { return };
        match &self.sig.raises {
            None => self.diags.push((format!("`{callee}` raises `{err}` but this flow does not declare `raises`"), span)),
            Some(into) => {
                if !self.model.converts_into(err, into) {
                    self.diags.push((
                        format!("`{callee}` raises `{err}`, which does not convert into `{into}`; add a `from {err}` variant to `{into}`"),
                        span,
                    ));
                }
            }
        }
    }

    /// A call `callee(args…)` with an optional piped first argument.
    fn call_step(&mut self, callee_expr: &Expr, extra: &[Arg], piped: Option<Val>, env: &[(String, Val)], span: Span) -> Val {
        let (name, path) = match &callee_expr.kind {
            ExprKind::Name(n) => (n.clone(), n.clone()),
            ExprKind::Attr { base, attr } => match &base.kind {
                ExprKind::Name(b) => (attr.name.clone(), format!("{b}.{}", attr.name)),
                _ => return self.bad_step(callee_expr.span),
            },
            _ => return self.bad_step(callee_expr.span),
        };
        // combinators over lists
        if FLOW_COMBINATORS.contains(&name.as_str()) && self.model.resolve_name(self.module, &name).is_none() {
            return self.combinator(&name, extra, piped, env, span);
        }
        let Some(id) = self.model.resolve_path(self.module, &path) else {
            self.diags.push((format!("unknown function `{path}`"), callee_expr.span));
            return Val { ty: Ty::Unknown, src: StepInput::Other };
        };
        let node = &self.model.nodes[&id];
        let Some(sig) = node.sig().cloned() else {
            self.diags.push((format!("`{path}` is a {}, not a function or flow", node.kind.label()), callee_expr.span));
            return Val { ty: Ty::Unknown, src: StepInput::Other };
        };
        let mut inputs = Vec::new();
        let mut params = sig.params.iter();
        if let Some(p) = piped {
            match params.next() {
                Some(param) => {
                    if !assignable(&param.ty, &p.ty) {
                        self.diags.push((
                            format!("`{name}` expects `{}` for `{}`, but the piped value is `{}`", param.ty, param.name, p.ty),
                            span,
                        ));
                    }
                }
                None => self.diags.push((format!("`{name}` takes no parameters, but a value is piped into it"), span)),
            }
            inputs.push(p.src);
        }
        let tenv = self.typer_env(env);
        let remaining: Vec<&ParamInfo> = params.collect();
        let positional: Vec<&Arg> = extra.iter().filter(|a| a.name.is_none()).collect();
        if positional.len() > remaining.len() {
            self.diags.push((format!("`{name}` takes {} argument(s), found {}", sig.params.len(), sig.params.len() - remaining.len() + positional.len()), span));
        }
        for (i, a) in extra.iter().enumerate() {
            let param = match &a.name {
                Some(n) => remaining.iter().find(|p| p.name == n.name).copied(),
                None => remaining.get(i).copied(),
            };
            // a nested call to a function or flow is a step of its own (graph node + span)
            if self.is_fn_call(&a.value) {
                let v = self.value(&a.value, env);
                if let Some(param) = param {
                    if !assignable(&param.ty, &v.ty) {
                        self.diags.push((format!("argument `{}` of `{name}` expects `{}`, found `{}`", param.name, param.ty, v.ty), a.value.span));
                    }
                }
                inputs.push(v.src);
                continue;
            }
            let mut typer = Typer::new(self.model, self.module);
            let t = typer.ty(&a.value, &tenv);
            if let Some(param) = param {
                typer.check_literal(&param.ty, &a.value);
            }
            self.diags.extend(typer.diags);
            if let Some(param) = param {
                if !assignable(&param.ty, &t) && !compatible(self.model, &param.ty, &t) {
                    self.diags.push((format!("argument `{}` of `{name}` expects `{}`, found `{t}`", param.name, param.ty), a.value.span));
                }
            } else if let Some(n) = &a.name {
                self.diags.push((format!("`{name}` has no parameter `{}`", n.name), n.span));
            }
            inputs.push(match &a.value.kind {
                ExprKind::Name(n) => self.lookup(env, n).map_or(StepInput::Other, |v| v.src),
                ExprKind::Attr { base, .. } => match &base.kind {
                    ExprKind::Name(n) => self.lookup(env, n).map_or(StepInput::Other, |v| v.src),
                    _ => StepInput::Other,
                },
                _ => StepInput::Other,
            });
        }
        let given = extra.len() + usize::from(!inputs.is_empty() && extra.len() < inputs.len());
        let required = sig.params.iter().filter(|p| !p.has_default).count();
        if given < required {
            self.diags.push((format!("`{name}` needs {required} argument(s), found {given}"), span));
        }
        self.check_raises(&sig.raises, &name, span);
        let caps = self.model.uses_of(&id);
        self.caps.extend(caps);
        self.is_async |= sig.is_async;
        let idx = self.new_step(&name, Some(id), span, inputs, sig.ret.clone(), sig.raises.clone());
        Val { ty: sig.ret, src: StepInput::Step(idx) }
    }

    fn is_fn_call(&self, e: &Expr) -> bool {
        match &e.kind {
            ExprKind::Call { func, .. } => func
                .callee_name()
                .and_then(|n| self.model.resolve_name(self.module, n))
                .is_some_and(|id| self.model.nodes[id].sig().is_some()),
            _ => false,
        }
    }

    fn bad_step(&mut self, span: Span) -> Val {
        self.diags.push(("a flow step must name a function or flow, optionally with arguments".into(), span));
        Val { ty: Ty::Unknown, src: StepInput::Other }
    }

    fn combinator(&mut self, name: &str, args: &[Arg], piped: Option<Val>, env: &[(String, Val)], span: Span) -> Val {
        let unknown = Val { ty: Ty::Unknown, src: StepInput::Other };
        // `retry` takes the piped value itself (if any); the others iterate a piped list
        let input = match (name, piped) {
            ("retry", p) => p,
            (_, Some(p)) => Some(p),
            (_, None) => {
                self.diags.push((format!("`{name}` must receive a piped list: `xs |> {name}(f)`"), span));
                return unknown;
            }
        };
        let elem = match (name, &input) {
            ("retry", _) | (_, None) => Ty::Unknown,
            (_, Some(v)) => match &v.ty {
                Ty::List(t) => (**t).clone(),
                Ty::Unknown => Ty::Unknown,
                other => {
                    self.diags.push((format!("`{name}` needs a list, found `{other}`"), span));
                    Ty::Unknown
                }
            },
        };
        let positional: Vec<&Arg> = args.iter().filter(|a| a.name.is_none()).collect();
        let (f_arg, init) = match name {
            "fold" if positional.len() == 2 => (positional[1], Some(positional[0])),
            "fold" => {
                self.diags.push(("`fold` takes an initial value and a function: `xs |> fold(init, f)`".into(), span));
                return unknown;
            }
            _ if positional.len() == 1 => (positional[0], None),
            _ => {
                self.diags.push((format!("`{name}` takes exactly one function"), span));
                return unknown;
            }
        };
        for a in args.iter().filter(|a| a.name.is_some()) {
            let n = a.name.as_ref().unwrap();
            let ok = match (name, n.name.as_str()) {
                ("retry", "times") => {
                    let good = matches!(&a.value.kind, ExprKind::Int(v) if v.parse::<u32>().is_ok_and(|t| t >= 1));
                    if !good {
                        self.diags.push(("`times` is an integer literal of at least 1".into(), a.value.span));
                    }
                    true
                }
                ("retry", "backoff") => {
                    if !matches!(a.value.kind, ExprKind::Duration { .. }) {
                        self.diags.push(("`backoff` is a duration such as `50ms`".into(), a.value.span));
                    }
                    true
                }
                _ => false,
            };
            if !ok {
                self.diags.push((format!("`{name}` has no parameter `{}`", n.name), n.span));
            }
        }
        if name == "retry" && !args.iter().any(|a| a.name.as_ref().is_some_and(|n| n.name == "times")) {
            self.diags.push(("`retry` needs the number of attempts: `retry(f, times=3)`".into(), span));
        }
        let fname = f_arg.value.callee_name().unwrap_or("").to_string();
        let Some(node) = self.model.resolve_name(self.module, &fname).and_then(|id| self.model.nodes.get(id)).filter(|n| n.sig().is_some()) else {
            self.diags.push((format!("`{name}` needs the name of a function, found `{fname}`"), f_arg.value.span));
            return unknown;
        };
        let sig = node.sig().unwrap().clone();
        let want = |i: usize| sig.params.get(i).map(|p| p.ty.clone());
        let mut inputs: Vec<StepInput> = input.iter().map(|v| v.src.clone()).collect();
        let ty = match name {
            "map" | "filter" | "each" | "flat_map" => {
                if sig.params.len() != 1 {
                    self.diags.push((format!("`{fname}` must take exactly one parameter to be used with `{name}`"), f_arg.value.span));
                }
                if let Some(p) = want(0) {
                    if !assignable(&p, &elem) {
                        self.diags.push((format!("`{fname}` expects `{p}`, but the list holds `{elem}`"), f_arg.value.span));
                    }
                }
                match name {
                    "map" => Ty::List(Box::new(sig.ret.clone())),
                    "filter" => {
                        if !matches!(sig.ret, Ty::Bool | Ty::Unknown) {
                            self.diags.push((format!("`filter` needs a predicate, but `{fname}` returns `{}`", sig.ret), f_arg.value.span));
                        }
                        input.as_ref().map_or(Ty::Unknown, |v| v.ty.clone())
                    }
                    "flat_map" => match &sig.ret {
                        Ty::List(_) | Ty::Unknown => sig.ret.clone(),
                        other => {
                            self.diags.push((format!("`flat_map` needs a function returning a list, but `{fname}` returns `{other}`"), f_arg.value.span));
                            Ty::Unknown
                        }
                    },
                    _ => Ty::Unit,
                }
            }
            "fold" => {
                if sig.params.len() != 2 {
                    self.diags.push((format!("`{fname}` must take (accumulator, element) to be used with `fold`"), f_arg.value.span));
                }
                let acc = want(0).unwrap_or(Ty::Unknown);
                if let Some(p) = want(1) {
                    if !assignable(&p, &elem) {
                        self.diags.push((format!("`{fname}` expects `{p}`, but the list holds `{elem}`"), f_arg.value.span));
                    }
                }
                if !assignable(&acc, &sig.ret) {
                    self.diags.push((format!("`{fname}` must return its accumulator type `{acc}`, found `{}`", sig.ret), f_arg.value.span));
                }
                let init = init.unwrap();
                let v = if self.is_fn_call(&init.value) {
                    self.value(&init.value, env)
                } else {
                    let mut typer = Typer::new(self.model, self.module);
                    let ty = typer.ty(&init.value, &self.typer_env(env));
                    typer.check_literal(&acc, &init.value);
                    self.diags.extend(typer.diags);
                    let src = match &init.value.kind {
                        ExprKind::Name(n) => self.lookup(env, n).map_or(StepInput::Other, |v| v.src),
                        _ => StepInput::Other,
                    };
                    Val { ty, src }
                };
                if !assignable(&acc, &v.ty) && !compatible(self.model, &acc, &v.ty) {
                    self.diags.push((format!("the initial value of `fold` should be `{acc}`, found `{}`", v.ty), init.value.span));
                }
                inputs.push(v.src);
                acc
            }
            _ => {
                // retry: the piped value is the first argument of every attempt
                match (&input, want(0)) {
                    (Some(v), Some(p)) if !assignable(&p, &v.ty) => {
                        self.diags.push((format!("`{fname}` expects `{p}`, but the piped value is `{}`", v.ty), span));
                    }
                    (Some(_), None) => self.diags.push((format!("`{fname}` takes no parameters, but a value is piped into it"), span)),
                    (None, Some(_)) if sig.params.iter().any(|p| !p.has_default) => {
                        self.diags.push((format!("`{fname}` needs an argument: pipe it in, `x |> retry({fname}, times=3)`"), span));
                    }
                    _ => {}
                }
                if sig.params.len() > 1 {
                    self.diags.push((format!("`retry` calls `{fname}` with the piped value only, but it takes {} parameters", sig.params.len()), f_arg.value.span));
                }
                if sig.raises.is_none() {
                    self.diags.push((format!("`retry` needs a function that raises, but `{fname}` never fails"), f_arg.value.span));
                }
                sig.ret.clone()
            }
        };
        self.check_raises(&sig.raises, &fname, span);
        self.caps.extend(self.model.uses_of(&node.id));
        self.is_async |= sig.is_async;
        let raises = if name == "filter" || name == "map" || name == "each" { None } else { sig.raises.clone() };
        let idx = self.new_step(&format!("{name}:{fname}"), Some(node.id.clone()), span, inputs, ty.clone(), raises);
        Val { ty, src: StepInput::Step(idx) }
    }

    /// `parallel(a(...), b(...), ...)`: each argument is a step of its own,
    /// then a `parallel` step joins their values into a tuple.
    fn parallel(&mut self, args: &[Arg], env: &[(String, Val)], span: Span) -> Val {
        if args.len() < 2 || args.iter().any(|a| a.name.is_some()) {
            self.diags.push(("`parallel` takes two or more steps: `parallel(charge(card), reserve(items))`".into(), span));
        }
        let mut tys = Vec::new();
        let mut inputs = Vec::new();
        for a in args {
            let before = self.steps.len();
            let v = self.value(&a.value, env);
            // a branch is one step (with its nested steps), not a value from outside
            if self.steps.len() == before {
                self.diags.push(("each branch of `parallel` is a call of a function or flow".into(), a.value.span));
            }
            tys.push(v.ty);
            inputs.push(v.src);
        }
        let ty = Ty::Tuple(tys);
        let idx = self.new_step("parallel", None, span, inputs, ty.clone(), None);
        Val { ty, src: StepInput::Step(idx) }
    }

    fn value(&mut self, e: &Expr, env: &[(String, Val)]) -> Val {
        match &e.kind {
            ExprKind::Name(n) => {
                if let Some(v) = self.lookup(env, n) {
                    return v;
                }
                // a bare function name as head: call with no args
                self.call_step(e, &[], None, env, e.span)
            }
            ExprKind::Call { func, args } => {
                if func.callee_name() == Some("parallel") && self.model.resolve_name(self.module, "parallel").is_none() {
                    return self.parallel(args, env, e.span);
                }
                // constructors are values, not steps
                if let Some(n) = func.callee_name() {
                    if let Some(node) = self.model.resolve_name(self.module, n).and_then(|id| self.model.nodes.get(id)) {
                        if node.sig().is_none() {
                            let mut typer = Typer::new(self.model, self.module);
                            let ty = typer.ty(e, &self.typer_env(env));
                            self.diags.extend(typer.diags);
                            return Val { ty, src: StepInput::Other };
                        }
                    }
                }
                self.call_step(func, args, None, env, e.span)
            }
            _ => {
                let mut typer = Typer::new(self.model, self.module);
                let ty = typer.ty(e, &self.typer_env(env));
                self.diags.extend(typer.diags);
                let src = match &e.kind {
                    ExprKind::Attr { base, .. } => match &base.kind {
                        ExprKind::Name(n) => self.lookup(env, n).map_or(StepInput::Other, |v| v.src),
                        _ => StepInput::Other,
                    },
                    _ => StepInput::Other,
                };
                Val { ty, src }
            }
        }
    }

    fn flow_expr(&mut self, fe: &FlowExpr, env: &[(String, Val)]) -> Val {
        let mut v = self.value(&fe.head, env);
        for step in &fe.steps {
            v = match &step.kind {
                ExprKind::Call { func, args } => self.call_step(func, args, Some(v), env, step.span),
                _ => self.call_step(step, &[], Some(v), env, step.span),
            };
        }
        v
    }

    fn stmts(&mut self, stmts: &[FlowStmt], env: &mut Vec<(String, Val)>) -> Option<Val> {
        let mut last = None;
        for s in stmts {
            last = None;
            match &s.kind {
                FlowStmtKind::Bind { targets, value } => {
                    let v = self.flow_expr(value, env);
                    if let StepInput::Step(i) = v.src {
                        self.steps[i].bindings = targets.iter().map(|t| t.name.clone()).collect();
                    }
                    if targets.len() == 1 {
                        env.push((targets[0].name.clone(), v));
                    } else {
                        let tys = match &v.ty {
                            Ty::Tuple(ts) if ts.len() == targets.len() => ts.clone(),
                            Ty::Unknown => vec![Ty::Unknown; targets.len()],
                            other => {
                                self.diags.push((format!("cannot unpack `{other}` into {} names", targets.len()), s.span));
                                vec![Ty::Unknown; targets.len()]
                            }
                        };
                        for (t, ty) in targets.iter().zip(tys) {
                            env.push((t.name.clone(), Val { ty, src: v.src.clone() }));
                        }
                    }
                }
                FlowStmtKind::Expr(fe) => last = Some(self.flow_expr(fe, env)),
                FlowStmtKind::Return(v) => {
                    return Some(match v {
                        Some(fe) => self.flow_expr(fe, env),
                        None => Val { ty: Ty::Unit, src: StepInput::Other },
                    });
                }
                FlowStmtKind::Match { scrutinee, arms } => {
                    let sv = self.value(scrutinee, env);
                    let idx = self.new_step("match", None, s.span, vec![sv.src.clone()], Ty::Unknown, None);
                    let en = self.model.enum_info(&sv.ty).cloned();
                    if en.is_none() && !sv.ty.is_unknown() {
                        self.diags.push((format!("`match` needs an enum, found `{}`", sv.ty), scrutinee.span));
                    }
                    let mut arm_ty: Option<Ty> = None;
                    let mut covered = BTreeSet::new();
                    for arm in arms {
                        let mut arm_env = env.clone();
                        let (vname, binds): (String, Vec<&Expr>) = match &arm.pattern.kind {
                            ExprKind::Name(n) => (n.clone(), vec![]),
                            ExprKind::Call { func, args } => {
                                (func.callee_name().unwrap_or("").to_string(), args.iter().map(|a| &a.value).collect())
                            }
                            _ => {
                                self.diags.push(("a case pattern is a variant, optionally with names: `Created(user)`".into(), arm.pattern.span));
                                continue;
                            }
                        };
                        covered.insert(vname.clone());
                        if let Some(en) = &en {
                            match en.variants.iter().find(|v| v.name == vname) {
                                Some(v) => {
                                    if binds.len() != v.items.len() {
                                        self.diags.push((format!("`{vname}` has {} item(s), the pattern binds {}", v.items.len(), binds.len()), arm.pattern.span));
                                    }
                                    for (b, item) in binds.iter().zip(&v.items) {
                                        if let ExprKind::Name(n) = &b.kind {
                                            arm_env.push((n.clone(), Val { ty: item.ty.clone(), src: StepInput::Step(idx) }));
                                        }
                                    }
                                }
                                None => self.diags.push((format!("`{vname}` is not a variant of `{}`", sv.ty), arm.pattern.span)),
                            }
                        }
                        let v = self.stmts(&arm.body, &mut arm_env);
                        if let Some(v) = v {
                            if arm_ty.is_none() {
                                arm_ty = Some(v.ty);
                            }
                        }
                    }
                    if let Some(en) = &en {
                        let missing: Vec<&str> =
                            en.variants.iter().map(|v| v.name.as_str()).filter(|v| !covered.contains(*v)).collect();
                        if !missing.is_empty() {
                            self.diags.push((format!("`match` does not handle: {}", missing.join(", ")), s.span));
                        }
                    }
                    let ty = arm_ty.unwrap_or(Ty::Unit);
                    self.steps[idx].output = ty.clone();
                    last = Some(Val { ty, src: StepInput::Step(idx) });
                }
            }
        }
        last
    }
}

fn analyze_flow(model: &Model, c: &mut Checker, file: usize, module: &str, flow_id: &str, f: &ast::Flow, sig: &SigInfo) -> FlowInfo {
    let mut ctx = FlowCtx {
        model,
        module,
        flow_id,
        sig,
        steps: Vec::new(),
        counts: BTreeMap::new(),
        diags: Vec::new(),
        caps: BTreeSet::new(),
        is_async: false,
    };
    let mut env: Vec<(String, Val)> =
        sig.params.iter().map(|p| (p.name.clone(), Val { ty: p.ty.clone(), src: StepInput::Param(p.name.clone()) })).collect();
    let result = ctx.stmts(&f.body, &mut env);
    if let Some(r) = &result {
        if sig.ret != Ty::Unit && !assignable(&sig.ret, &r.ty) {
            ctx.diags.push((format!("flow `{}` returns `{}`, but its last step produces `{}`", f.sig.name.name, sig.ret, r.ty), f.body_span));
        }
    } else if sig.ret != Ty::Unit {
        ctx.diags.push((format!("flow `{}` must end with a value of type `{}`", f.sig.name.name, sig.ret), f.body_span));
    }
    let mut out_sig = sig.clone();
    if f.sig.uses.is_empty() {
        out_sig.uses = ctx.caps.iter().cloned().collect();
    } else {
        for step in &ctx.steps {
            if let Some(callee) = &step.callee {
                for cap in model.uses_of(callee) {
                    if !sig.uses.contains(&cap) {
                        ctx.diags.push((format!("`{}` uses `{}`, which this flow does not declare in `uses`", step.label, short(&cap)), step.span));
                    }
                }
            }
        }
    }
    out_sig.is_async = ctx.is_async;
    let result_idx = result.and_then(|v| match v.src {
        StepInput::Step(i) => Some(i),
        _ => None,
    });
    c.push(file, std::mem::take(&mut ctx.diags));
    FlowInfo { sig: out_sig, steps: ctx.steps, result: result_idx }
}

fn resolve_provide(model: &Model, module: &str, p: &ast::Provide, diags: &mut Vec<(String, Span)>) -> ProvideInfo {
    let cap = match model.resolve_name(module, &p.capability.name) {
        Some(id) if matches!(model.nodes[id].kind, NodeKind::Capability(_)) => id.clone(),
        _ => {
            diags.push((format!("`{}` is not a capability", p.capability.name), p.capability.span));
            return ProvideInfo { capability: p.capability.name.clone(), adapter: None };
        }
    };
    let adapter = p.value.callee_name().and_then(|n| model.resolve_name(module, n)).cloned();
    match adapter.as_ref().map(|a| &model.nodes[a].kind) {
        Some(NodeKind::Adapter(a)) => {
            if !a.implements.contains(&cap) {
                diags.push((format!("adapter does not implement `{}`", p.capability.name), p.value.span));
            }
            let mut typer = Typer::new(model, module);
            typer.ty(&p.value, &Env::default());
            diags.extend(typer.diags);
        }
        _ => diags.push(("`provide` needs an adapter: `provide Db = MemoryDb()`".into(), p.value.span)),
    }
    ProvideInfo { capability: cap, adapter }
}

fn analyze_app(model: &Model, c: &mut Checker, node: &Node, app: &ast::App) -> AppInfo {
    let file = node.file;
    let text = &model.files[file].text;
    let mut d = Vec::new();
    let targets: Vec<String> = app.targets.iter().map(|t| text[t.span.range()].to_string()).collect();
    if targets.is_empty() {
        d.push(("an app needs a `target`, e.g. `target native(cli)`".into(), node.name_span));
    }
    let mut target = None;
    if let Some(extra) = app.targets.get(1) {
        d.push(("an app has one target; declare one app per target".into(), extra.span));
    }
    if let Some(e) = app.targets.first() {
        let kind = e.callee_name().unwrap_or("");
        let flavor = match &e.kind {
            ExprKind::Call { args, .. } => args.first().and_then(|a| a.value.callee_name()),
            _ => None,
        };
        match wh_targets::TargetProfile::parse(kind, flavor) {
            Ok(p) => target = Some(p),
            Err(m) => d.push((m, e.span)),
        }
    }
    let profile = target.clone().unwrap_or_else(wh_targets::TargetProfile::native_cli);
    for (k, v) in &app.log {
        let val = match &v.kind {
            ExprKind::Str { value, .. } => value.as_str(),
            _ => {
                d.push(("log settings take string values, e.g. `log level=\"info\"`".into(), v.span));
                continue;
            }
        };
        let ok = match k.name.as_str() {
            "level" | "release_level" => TRACE_LEVELS.contains(&val),
            "format" => LOG_FORMATS.contains(&val),
            _ => {
                d.push((format!("unknown log setting `{}`; use `level`, `release_level` or `format`", k.name), k.span));
                continue;
            }
        };
        if !ok {
            d.push((format!("invalid value `{val}` for `{}`", k.name), v.span));
        }
    }
    let provides: Vec<ProvideInfo> = app.provides.iter().map(|p| resolve_provide(model, &node.module, p, &mut d)).collect();
    for (p, ast_p) in provides.iter().zip(&app.provides) {
        let Some(a) = p.adapter.as_ref().and_then(|a| model.nodes.get(a)) else { continue };
        let in_memory = a.patterns.iter().any(|x| x == "InMemory");
        if !profile.adapter_available(&a.available_on, in_memory) {
            d.push((
                format!(
                    "adapter `{}` is not available on {}; provide an in-memory adapter or one marked `@target({})`",
                    a.name,
                    profile.written(),
                    profile.label()
                ),
                ast_p.value.span,
            ));
        }
    }
    let provided: BTreeSet<NodeId> = provides.iter().map(|p| p.capability.clone()).collect();
    let (routes, statuses, listen, exports) = analyze_target_sections(model, node, app, &profile, &provided, &mut d);
    c.push(file, d);
    if profile.kind != wh_targets::TargetKind::Native {
        if let Some(f) = &app.entry {
            let what = if profile.kind == wh_targets::TargetKind::Server { "`route`s" } else { "`export`s" };
            c.push(file, vec![(format!("a {} app has {what}, not an `entry`", profile.written()), f.sig.name.span)]);
        }
        return AppInfo { targets, provides, entry: None, target, routes, statuses, listen, exports };
    }
    let entry = app.entry.as_ref().map(|f| {
        let mut d = Vec::new();
        let sig = model.resolve_sig_pub(&node.module, &f.sig, &mut d);
        c.push(file, d);
        let fi = analyze_flow(model, c, file, &node.module, &node.id, f, &sig);
        let provided: BTreeSet<&NodeId> = provides.iter().map(|p| &p.capability).collect();
        let mut missing = Vec::new();
        for cap in &fi.sig.uses {
            if !provided.contains(cap) {
                missing.push((format!("the entry flow uses `{}`; add `provide {} = …`", short(cap), short(cap)), f.sig.name.span));
            }
        }
        c.push(file, missing);
        fi
    });
    if entry.is_none() {
        c.push(file, vec![("an app needs an `entry`".into(), node.name_span)]);
    }
    AppInfo { targets, provides, entry, target, routes, statuses, listen, exports }
}

pub const HTTP_METHODS: &[&str] = &["GET", "POST", "PUT", "PATCH", "DELETE"];

/// Why a type cannot cross an HTTP or wasm boundary (as JSON), if it cannot.
pub fn wire_type_problem(model: &Model, t: &Ty) -> Option<String> {
    match t {
        Ty::Fn(..) | Ty::Stream(_) | Ty::Instant | Ty::Param(_) => Some(format!("`{t}` cannot be sent as JSON")),
        Ty::List(i) | Ty::Set(i) | Ty::SortedSet(i) | Ty::Option(i) | Ty::Array(i, _) => wire_type_problem(model, i),
        Ty::Dict(k, v) | Ty::SortedDict(k, v) => wire_type_problem(model, k).or_else(|| wire_type_problem(model, v)),
        Ty::Tuple(ts) => ts.iter().find_map(|t| wire_type_problem(model, t)),
        Ty::Named(id, _) => match model.nodes.get(id).map(|n| &n.kind) {
            Some(NodeKind::Capability(_) | NodeKind::Adapter(_)) => Some(format!("`{t}` is not data and cannot be sent as JSON")),
            _ => None,
        },
        _ => None,
    }
}

/// Does a flow (or a flow it calls) block on threads or sleeps — a sync
/// `parallel`, or `retry` with a `backoff`?
fn blocking_step(model: &Model, id: &str, seen: &mut BTreeSet<NodeId>) -> Option<(String, String)> {
    if !seen.insert(id.to_string()) {
        return None;
    }
    let node = model.nodes.get(id)?;
    let NodeKind::Flow(f) = &node.kind else { return None };
    if !f.sig.is_async && f.steps.iter().any(|s| s.label == "parallel") {
        return Some((id.to_string(), "runs `parallel` on threads".into()));
    }
    if let ItemKind::Flow(ast_f) = &node.item.kind {
        let text = &model.files[node.file].text[ast_f.body_span.range()];
        if text.contains("retry(") && text.contains("backoff") {
            return Some((id.to_string(), "sleeps between `retry` attempts".into()));
        }
    }
    f.steps.iter().filter_map(|s| s.callee.as_ref()).find_map(|c| blocking_step(model, c, seen))
}

/// Can a value be written in a URL path segment or query parameter?
fn url_scalar(model: &Model, t: &Ty) -> bool {
    match t {
        Ty::Bool | Ty::Int(_) | Ty::Float32 | Ty::Float64 | Ty::Str | Ty::Char => true,
        Ty::Option(i) => url_scalar(model, i),
        Ty::Named(..) => {
            model.refined_base(t).is_some_and(|b| url_scalar(model, &b))
                || model.enum_info(t).is_some_and(|e| e.variants.iter().all(|v| v.items.is_empty()))
        }
        _ => false,
    }
}

type TargetSections = (Vec<RouteInfo>, Vec<StatusInfo>, Option<String>, Vec<NodeId>);

/// `route`, `status`, `listen` (server) and `export` (web) sections.
fn analyze_target_sections(
    model: &Model,
    node: &Node,
    app: &ast::App,
    profile: &wh_targets::TargetProfile,
    provided: &BTreeSet<NodeId>,
    d: &mut Vec<(String, Span)>,
) -> TargetSections {
    use wh_targets::TargetKind;
    let module = node.module.as_str();
    let resolve_target = |e: &Expr, d: &mut Vec<(String, Span)>| -> Option<NodeId> {
        let path = match &e.kind {
            ExprKind::Name(n) => n.clone(),
            ExprKind::Attr { base, attr } => match &base.kind {
                ExprKind::Name(b) => format!("{b}.{}", attr.name),
                _ => String::new(),
            },
            _ => String::new(),
        };
        match model.resolve_path(module, &path) {
            Some(id) if model.nodes[&id].sig().is_some() => Some(id),
            Some(id) => {
                d.push((format!("`{path}` is a {}, not a flow or def", model.nodes[&id].kind.label()), e.span));
                None
            }
            None => {
                d.push((format!("unknown flow or def `{path}`"), e.span));
                None
            }
        }
    };
    // what every routed or exported node needs
    let boundary = |id: &NodeId, span: Span, d: &mut Vec<(String, Span)>| {
        let n = &model.nodes[id];
        let sig = n.sig().unwrap();
        for cap in model.uses_of(id) {
            if !provided.contains(&cap) {
                d.push((format!("`{}` uses `{}`; add `provide {} = …`", n.name, short(&cap), short(&cap)), span));
            }
        }
        let tys = sig.params.iter().map(|p| &p.ty).chain([&sig.ret]).chain(sig.raises.iter());
        for t in tys {
            if let Some(problem) = wire_type_problem(model, t) {
                d.push((format!("`{}` cannot be called from outside: {problem}", n.name), span));
            }
        }
        if sig.is_async && !profile.supports_async() {
            d.push((format!("`{}` is async, which {} does not support yet", n.name, profile.written()), span));
        }
        if !profile.supports_threads() {
            if let Some((flow, why)) = blocking_step(model, id, &mut BTreeSet::new()) {
                d.push((format!("`{}` {why}, which needs threads; {} has none", short(&flow), profile.written()), span));
            }
        }
    };
    let mut routes = Vec::new();
    let mut seen = BTreeSet::new();
    for r in &app.routes {
        if profile.kind != TargetKind::Server {
            d.push((format!("`route` needs `target server(axum)`, not {}", profile.written()), r.span));
            continue;
        }
        if !HTTP_METHODS.contains(&r.method.name.as_str()) {
            d.push((format!("unknown HTTP method `{}`; use {}", r.method.name, HTTP_METHODS.join(", ")), r.method.span));
        }
        if !r.path.starts_with('/') {
            d.push(("a route path starts with `/`".into(), r.path_span));
        }
        if !seen.insert((r.method.name.clone(), r.path.clone())) {
            d.push((format!("route `{} {}` is declared twice", r.method.name, r.path), r.span));
        }
        let Some(id) = resolve_target(&r.target, d) else { continue };
        boundary(&id, r.span, d);
        let sig = model.nodes[&id].sig().unwrap();
        let segments: Vec<&str> = r.path.split('/').filter_map(|s| s.strip_prefix('{').and_then(|x| x.strip_suffix('}'))).collect();
        for s in &segments {
            if !sig.params.iter().any(|p| p.name == *s) {
                d.push((format!("path segment `{{{s}}}` is not a parameter of `{}`", model.nodes[&id].name), r.path_span));
            }
        }
        let bodyless = matches!(r.method.name.as_str(), "GET" | "DELETE");
        let params = sig
            .params
            .iter()
            .map(|p| {
                let src = if segments.contains(&p.name.as_str()) {
                    RouteSource::Path
                } else if bodyless {
                    RouteSource::Query
                } else {
                    RouteSource::Body
                };
                (p.name.clone(), src)
            })
            .collect();
        let params: Vec<(String, RouteSource)> = params;
        for (p, (_, src)) in sig.params.iter().zip(&params) {
            if *src != RouteSource::Body && !url_scalar(model, &p.ty) {
                let place = if *src == RouteSource::Path { "path" } else { "query string" };
                d.push((format!("`{}: {}` cannot be read from the {place}; use a body (`POST`) for structured values", p.name, p.ty), r.span));
            }
        }
        routes.push(RouteInfo { method: r.method.name.clone(), path: r.path.clone(), target: id, params, span: r.span });
    }
    let mut statuses = Vec::new();
    for st in &app.statuses {
        if profile.kind != TargetKind::Server {
            d.push((format!("`status` needs `target server(axum)`, not {}", profile.written()), st.span));
            continue;
        }
        let (error, variant) = model.resolve_error_ref(module, &st.error, d);
        let code = match &st.code.kind {
            ExprKind::Int(v) => v.parse::<u16>().ok().filter(|c| (100..=599).contains(c)),
            _ => None,
        };
        match code {
            Some(code) => statuses.push(StatusInfo { error, variant, code }),
            None => d.push(("an HTTP status is an integer from 100 to 599".into(), st.code.span)),
        }
    }
    let listen = app.listen.as_ref().and_then(|e| {
        if profile.kind != TargetKind::Server {
            d.push((format!("`listen` needs `target server(axum)`, not {}", profile.written()), e.span));
            return None;
        }
        match &e.kind {
            ExprKind::Str { value, .. } if value.rsplit_once(':').is_some_and(|(_, p)| p.parse::<u16>().is_ok()) => Some(value.clone()),
            _ => {
                d.push(("`listen` takes an address string such as \"127.0.0.1:3000\"".into(), e.span));
                None
            }
        }
    });
    let mut exports = Vec::new();
    for e in &app.exports {
        if profile.kind != TargetKind::Web {
            d.push((format!("`export` needs `target web(wasm)`, not {}", profile.written()), e.span));
            continue;
        }
        let Some(id) = resolve_target(e, d) else { continue };
        boundary(&id, e.span, d);
        if exports.contains(&id) {
            d.push((format!("`{}` is exported twice", model.nodes[&id].name), e.span));
        }
        exports.push(id);
    }
    match profile.kind {
        TargetKind::Server if app.routes.is_empty() => d.push(("a server app needs at least one `route`".into(), node.name_span)),
        TargetKind::Web if app.exports.is_empty() => d.push(("a web app needs at least one `export`".into(), node.name_span)),
        _ => {}
    }
    (routes, statuses, listen, exports)
}

impl Model {
    pub fn resolve_sig_pub(&self, module: &str, sig: &ast::Signature, diags: &mut Vec<(String, Span)>) -> SigInfo {
        let params = sig
            .params
            .iter()
            .map(|p| ParamInfo {
                name: p.name.name.clone(),
                mode: p.mode.clone(),
                ty: self.resolve_ty(module, &p.ty, &[], diags),
                has_default: p.default.is_some(),
                written: p.ty.to_string(),
                alias: self.written_alias(module, &p.ty),
                is_sensitive: p.is_sensitive,
            })
            .collect();
        let ret = sig.ret.as_ref().map_or(Ty::Unit, |t| self.resolve_ty(module, t, &[], diags));
        let raises = sig.raises.as_ref().map(|t| self.resolve_ty(module, t, &[], diags));
        SigInfo { params, ret, raises, uses: vec![], is_async: false }
    }
}
