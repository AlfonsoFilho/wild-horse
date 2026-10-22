//! Test generation: examples → `#[test]`, properties → `proptest`, scenario
//! tests with capability substitution, and proptest strategies per type.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use std::collections::{BTreeMap, BTreeSet as Set};

use wh_model::{IntTy, Model, Node, NodeKind, ParamInfo, SigInfo, StructInfo, Ty};
use wh_syntax::ast::{self, BinOp, ClauseKind, CmpOp, Expr, ExprKind, ItemKind, ParamMode};

use crate::expr::{ExprGen, Scope, Var};
use crate::rust::{Names, cap_var, rust_keyword_safe, test_prefix};

pub(crate) fn esc(s: &str) -> String {
    format!("{s:?}")
}

pub struct TestGen<'m> {
    pub model: &'m Model,
    pub module: &'m str,
    pub out: String,
    /// test function name → node id
    pub tests: Vec<(String, String)>,
    strategies: BTreeSet<String>,
    strategy_code: String,
    /// Generate small values (integers and floats within ±1000): used by the
    /// tests patterns add, which check behaviour, not performance.
    pub(crate) small: bool,
}

pub(crate) const PROPTEST_HEADER: &str = "    proptest::proptest! {\n        #![proptest_config(proptest::prelude::ProptestConfig::with_cases(64))]\n        #[test]";

impl<'m> TestGen<'m> {
    pub fn new(model: &'m Model, module: &'m str) -> Self {
        Self {
            model,
            module,
            out: String::new(),
            tests: vec![],
            strategies: BTreeSet::new(),
            strategy_code: String::new(),
            small: false,
        }
    }

    pub(crate) fn eg(&self) -> ExprGen<'m> {
        ExprGen::new(self.model, self.module)
    }

    pub(crate) fn names(&self) -> Names<'m> {
        Names::new(self.model, self.module)
    }

    pub fn generate(&mut self) -> String {
        let mut body = String::new();
        let nodes: Vec<_> = self.model.nodes.values().filter(|n| n.module == self.module).collect();
        for n in nodes {
            let Some(def) = n.def() else { continue };
            let text = &self.model.files[n.file].text;
            let prefix = test_prefix(&n.id);
            let mut ex = 0;
            for c in &def.clauses {
                match &c.kind {
                    ClauseKind::Example { expr, raises } => {
                        let name = format!("{prefix}__example_{ex}");
                        ex += 1;
                        let src = text[expr.span.range()].to_string();
                        let scope = Scope::default();
                        let check = match raises {
                            Some(r) => {
                                let mut d = Vec::new();
                                let (err, variant) = self.model.resolve_error_ref(&n.module, r, &mut d);
                                let call = match &expr.kind {
                                    ExprKind::Call { func, args } => self.eg().call(func, args, &scope, false),
                                    _ => self.eg().emit(expr, &scope),
                                };
                                let pat = self.eg().err_pattern(&err, &variant);
                                format!(
                                    "let r = {call};\n    assert!(matches!(&r, {pat}), \"example `{}` should raise {}, got {{r:?}}\");",
                                    src.replace('\\', "\\\\").replace('"', "\\\"").replace('{', "{{").replace('}', "}}"),
                                    text[r.span.range()].replace('\\', "\\\\").replace('"', "\\\"").replace('{', "{{").replace('}', "}}")
                                )
                            }
                            None => format!(
                                "assert!({}, \"example failed: {}\");",
                                self.eg().emit_bool(expr, &scope),
                                src.replace('\\', "\\\\").replace('"', "\\\"").replace('{', "{{").replace('}', "}}")
                            ),
                        };
                        let _ = writeln!(body, "    #[test]\n    fn {name}() {{\n        crate::wh_rt::init_test_tracing();\n        {}\n    }}\n", check.replace('\n', "\n    "));
                        self.tests.push((name, n.id.clone()));
                    }
                    ClauseKind::Property { name: pname, params, given, body: pbody } => {
                        let name = format!("{prefix}__property_{}", pname.name);
                        let mut scope = Scope::default();
                        let mut strategies = Vec::new();
                        let mut d = Vec::new();
                        for p in params {
                            let ty = self.model.resolve_ty(&n.module, &p.ty, &[], &mut d);
                            let strat = match given.iter().find(|(g, _)| g.name == p.name.name) {
                                Some((_, g)) => self.eg().emit(g, &Scope::default()),
                                None => self.strategy(&ty),
                            };
                            strategies.push(format!("{} in {strat}", p.name.name));
                            scope.var(&p.name.name, &p.name.name, ty, false);
                        }
                        let assume = self.requires_assumptions(n, pbody, &scope);
                        let _ = writeln!(
                            body,
                            "    proptest::proptest! {{\n        #![proptest_config(proptest::prelude::ProptestConfig::with_cases(64))]\n        #[test]\n        fn {name}({}) {{\n            crate::wh_rt::init_test_tracing();\n{assume}            proptest::prop_assert!({}, \"property `{}` failed\");\n        }}\n    }}\n",
                            strategies.join(", "),
                            self.eg().emit_bool(pbody, &scope),
                            pname.name
                        );
                        self.tests.push((name, n.id.clone()));
                    }
                    _ => {}
                }
            }
        }
        // tests added by patterns and machines
        let nodes: Vec<&'m Node> = self.model.nodes.values().filter(|n| n.module == self.module).collect();
        for n in nodes {
            match &n.kind {
                NodeKind::Def(d) if n.patterns.iter().any(|p| p == "Transform") => self.transform_total(&mut body, n, &d.sig),
                NodeKind::Machine(_) => self.machine_tests(&mut body, n),
                NodeKind::App(a) if a.profile().kind == wh_targets::TargetKind::Server => self.server_tests(&mut body, n, a),
                NodeKind::App(a) if a.profile().kind == wh_targets::TargetKind::Web => self.web_tests(&mut body, n, a),
                NodeKind::Adapter(_) => {
                    self.repository_tests(&mut body, n);
                    if n.patterns.iter().any(|p| p == "Adapter" || p == "InMemory") {
                        self.contract_tests(&mut body, n);
                    }
                }
                _ => {}
            }
        }
        // scenario tests
        for t in self.model.tests.iter().filter(|t| t.module == self.module) {
            let slug: String = t.test.name.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '_' }).collect();
            let name = format!("{}__test__{}", test_prefix(self.module), slug.trim_matches('_'));
            let mut scope = Scope::default();
            let mut setup = String::new();
            for (g, ast_g) in t.givens.iter().zip(&t.test.givens) {
                let v = cap_var(&g.capability);
                let _ = writeln!(setup, "        let {v} = {};", self.eg().emit(&ast_g.value, &Scope::default()));
                scope.caps.push((g.capability.clone(), format!("&{v}")));
            }
            let text = &self.model.files[t.file].text;
            let mut checks = String::new();
            for e in &t.test.expects {
                let src = text[e.expr.span.range()].replace('\\', "\\\\").replace('"', "\\\"").replace('{', "{{").replace('}', "}}");
                match &e.raises {
                    Some(r) => {
                        let mut d = Vec::new();
                        let (err, variant) = self.model.resolve_error_ref(&t.module, r, &mut d);
                        let call = match &e.expr.kind {
                            ExprKind::Call { func, args } => self.eg().call(func, args, &scope, false),
                            _ => self.eg().emit(&e.expr, &scope),
                        };
                        let pat = self.eg().err_pattern(&err, &variant);
                        let _ = writeln!(checks, "        let r = {call};\n        assert!(matches!(&r, {pat}), \"expected `{src}` to raise, got {{r:?}}\");");
                    }
                    None => {
                        let _ = writeln!(checks, "        assert!({}, \"expectation failed: {src}\");", self.eg().emit_bool(&e.expr, &scope));
                    }
                }
            }
            let _ = writeln!(body, "    #[test]\n    fn {name}() {{\n        crate::wh_rt::init_test_tracing();\n{setup}{checks}    }}\n");
            self.tests.push((name, format!("{}#test:{}", t.module, t.test.name)));
        }
        if body.is_empty() {
            return String::new();
        }
        format!(
            "\n#[cfg(test)]\nmod wh_tests {{\n    use super::*;\n    #[allow(unused_imports)]\n    use proptest::prelude::*;\n\n{body}{}}}\n",
            self.strategy_code
        )
    }

    /// A proptest strategy expression for a type (helpers are emitted once).
    pub fn strategy(&mut self, t: &Ty) -> String {
        match t {
            Ty::Bool => "proptest::prelude::any::<bool>()".into(),
            Ty::Int(i) if self.small => {
                let (lo, hi) = small_range(*i, None, None);
                format!("({lo}{t}..={hi}{t})", t = i.rust())
            }
            Ty::Int(i) => format!("proptest::prelude::any::<{}>()", i.rust()),
            Ty::Float64 if self.small => "(-1000.0f64..1000.0f64)".into(),
            Ty::Float32 if self.small => "(-1000.0f32..1000.0f32)".into(),
            Ty::Float64 => "(-1.0e9f64..1.0e9f64)".into(),
            Ty::Float32 => "(-1.0e6f32..1.0e6f32)".into(),
            Ty::Str => "\"[a-zA-Z0-9 @._-]{0,16}\"".into(),
            Ty::Char => "proptest::prelude::any::<char>()".into(),
            Ty::Bytes => "proptest::collection::vec(proptest::prelude::any::<u8>(), 0..16)".into(),
            Ty::Unit => "proptest::strategy::Just(())".into(),
            Ty::Duration => "(0u64..1000).prop_map(std::time::Duration::from_millis)".into(),
            Ty::List(e) => format!("proptest::collection::vec({}, 0..8)", self.strategy(e)),
            Ty::Set(e) => format!("proptest::collection::hash_set({}, 0..8)", self.strategy(e)),
            Ty::SortedSet(e) => format!("proptest::collection::btree_set({}, 0..8)", self.strategy(e)),
            Ty::Dict(k, v) => format!("proptest::collection::hash_map({}, {}, 0..8)", self.strategy(k), self.strategy(v)),
            Ty::SortedDict(k, v) => format!("proptest::collection::btree_map({}, {}, 0..8)", self.strategy(k), self.strategy(v)),
            Ty::Option(e) => format!("proptest::option::of({})", self.strategy(e)),
            Ty::Tuple(ts) => {
                let s: Vec<String> = ts.iter().map(|t| self.strategy(t)).collect();
                format!("({},)", s.join(", "))
            }
            Ty::Named(id, _) => {
                let f = self.strategy_fn(id);
                if self.strategies.insert(f.clone()) {
                    let code = self.named_strategy(id);
                    self.strategy_code.push_str(&code);
                }
                format!("{f}()")
            }
            _ => "proptest::strategy::Just(Default::default())".into(),
        }
    }

    fn strategy_fn(&self, id: &str) -> String {
        format!("wh_{}strategy_{}", if self.small { "small_" } else { "" }, id.replace('.', "_"))
    }

    fn named_strategy(&mut self, id: &str) -> String {
        let node = &self.model.nodes[id];
        let rust_ty = self.names().ty(&Ty::Named(id.to_string(), vec![]));
        let f = self.strategy_fn(id);
        let body = match (&node.kind, &node.item.kind) {
            (NodeKind::Refined { base, .. }, ItemKind::TypeDef(td)) => {
                // a regex refinement generates matching strings directly
                let regex = td.refinement.as_ref().and_then(|r| find_regex(r));
                let base_s = match (&regex, base) {
                    (Some(re), Ty::Str) => {
                        let re = re.trim_start_matches('^').trim_end_matches('$');
                        format!("proptest::string::string_regex({}).expect(\"valid regex\")", esc(re))
                    }
                    _ => match (base, td.refinement.as_ref().map(int_bounds)) {
                        // a numeric range refinement maps to a range strategy (no mass rejection)
                        (Ty::Int(i), Some((lo, hi))) if self.small => {
                            let (lo, hi) = small_range(*i, lo, hi);
                            format!("({lo}{t}..={hi}{t})", t = i.rust())
                        }
                        (Ty::Int(i), Some((lo, hi))) if lo.is_some() || hi.is_some() => {
                            let t = i.rust();
                            let lo = lo.map_or(format!("{t}::MIN"), |v| format!("{v}{t}"));
                            let hi = hi.map_or(format!("{t}::MAX"), |v| format!("{v}{t}"));
                            format!("({lo}..={hi})")
                        }
                        _ => self.strategy(base),
                    },
                };
                format!("{base_s}.prop_filter_map(\"refinement\", |v| {rust_ty}::parse(v).ok()).boxed()")
            }
            (NodeKind::Struct(s), ItemKind::Struct(ast_s)) => self.struct_strategy(id, s, ast_s, &rust_ty),
            (NodeKind::Enum(_) | NodeKind::Machine(_), _) => {
                let variants = node.kind.enum_like().unwrap().variants.clone();
                let mut arms = Vec::new();
                for v in &variants {
                    if v.items.is_empty() {
                        arms.push(format!("proptest::strategy::Just({rust_ty}::{}).boxed()", v.name));
                    } else {
                        let strats: Vec<String> = v.items.iter().map(|i| self.strategy(&i.ty)).collect();
                        let vars: Vec<String> = (0..v.items.len()).map(|i| format!("a{i}")).collect();
                        let ctor = if v.items.iter().all(|i| i.name.is_some()) {
                            let fs: Vec<String> = v.items.iter().zip(&vars).map(|(i, a)| format!("{}: {a}", i.name.as_ref().unwrap())).collect();
                            format!("{rust_ty}::{} {{ {} }}", v.name, fs.join(", "))
                        } else {
                            format!("{rust_ty}::{}({})", v.name, vars.join(", "))
                        };
                        arms.push(format!("{}.prop_map(|{}| {ctor}).boxed()", nest_tuple(&strats), nest_tuple(&vars)));
                    }
                }
                format!("proptest::strategy::Union::new(vec![{}]).boxed()", arms.join(", "))
            }
            (NodeKind::Alias(t), _) => format!("{}.boxed()", self.strategy(&t.clone())),
            _ => "proptest::strategy::Just(unimplemented!()).boxed()".into(),
        };
        format!("    #[allow(dead_code)]\n    fn {f}() -> proptest::strategy::BoxedStrategy<{rust_ty}> {{\n        {body}\n    }}\n\n")
    }
}

/// Inclusive integer bounds on `self` stated by a refinement, e.g.
/// `1 <= self <= 1000`, `self >= 1`, `self < 10 and ...`.
fn int_bounds(e: &wh_syntax::ast::Expr) -> (Option<i128>, Option<i128>) {
    use wh_syntax::ast::{BinOp, CmpOp};
    fn lit(e: &wh_syntax::ast::Expr) -> Option<i128> {
        match &e.kind {
            ExprKind::Int(v) => v.parse().ok(),
            ExprKind::Unary { op: wh_syntax::ast::UnaryOp::Neg, expr } => lit(expr).map(|v| -v),
            _ => None,
        }
    }
    fn is_self(e: &wh_syntax::ast::Expr) -> bool {
        matches!(&e.kind, ExprKind::Name(n) if n == "self")
    }
    let mut lo: Option<i128> = None;
    let mut hi: Option<i128> = None;
    let mut tighten = |l: Option<i128>, h: Option<i128>| {
        if let Some(l) = l {
            lo = Some(lo.map_or(l, |x| x.max(l)));
        }
        if let Some(h) = h {
            hi = Some(hi.map_or(h, |x| x.min(h)));
        }
    };
    let mut stack = vec![e];
    while let Some(e) = stack.pop() {
        match &e.kind {
            ExprKind::Binary { op: BinOp::And, lhs, rhs } => {
                stack.push(lhs);
                stack.push(rhs);
            }
            ExprKind::Compare { first, rest } => {
                let mut prev: &wh_syntax::ast::Expr = first;
                for (op, next) in rest {
                    // normalise to `a op b`
                    match (is_self(prev), lit(prev), is_self(next), lit(next)) {
                        (true, _, _, Some(v)) => match op {
                            CmpOp::Lt => tighten(None, Some(v - 1)),
                            CmpOp::Le => tighten(None, Some(v)),
                            CmpOp::Gt => tighten(Some(v + 1), None),
                            CmpOp::Ge => tighten(Some(v), None),
                            _ => {}
                        },
                        (_, Some(v), true, _) => match op {
                            CmpOp::Lt => tighten(Some(v + 1), None),
                            CmpOp::Le => tighten(Some(v), None),
                            CmpOp::Gt => tighten(None, Some(v - 1)),
                            CmpOp::Ge => tighten(None, Some(v)),
                            _ => {}
                        },
                        _ => {}
                    }
                    prev = next;
                }
            }
            _ => {}
        }
    }
    (lo, hi)
}

fn nest_tuple(items: &[String]) -> String {
    if items.len() == 1 {
        return format!("({},)", items[0]);
    }
    if items.len() <= 10 {
        return format!("({})", items.join(", "));
    }
    let (a, b) = items.split_at(10);
    format!("({}, {})", a.join(", "), nest_tuple(b))
}

fn find_regex(e: &wh_syntax::ast::Expr) -> Option<String> {
    match &e.kind {
        ExprKind::Call { func, args } if func.callee_name() == Some("matches") && args.len() == 2 => match &args[1].value.kind {
            ExprKind::Str { value, .. } => Some(value.clone()),
            _ => None,
        },
        ExprKind::Binary { op: wh_syntax::ast::BinOp::And, lhs, rhs } => find_regex(lhs).or_else(|| find_regex(rhs)),
        _ => None,
    }
}

/// Calls `name(...)` in an expression, innermost first, outside
/// comprehensions, quantifiers and lambdas (whose variables are not in scope).
fn calls_of<'e>(e: &'e Expr, name: &str, out: &mut Vec<&'e [ast::Arg]>) {
    match &e.kind {
        ExprKind::Call { func, args } => {
            for a in args {
                calls_of(&a.value, name, out);
            }
            calls_of(func, name, out);
            if matches!(&func.kind, ExprKind::Name(n) if n == name) {
                out.push(args);
            }
        }
        ExprKind::Attr { base, .. } => calls_of(base, name, out),
        ExprKind::Index { base, index } => {
            calls_of(base, name, out);
            calls_of(index, name, out);
        }
        ExprKind::Slice { base, lo, hi } => {
            calls_of(base, name, out);
            for x in [lo, hi].into_iter().flatten() {
                calls_of(x, name, out);
            }
        }
        ExprKind::Unary { expr, .. } => calls_of(expr, name, out),
        ExprKind::Binary { lhs, rhs, .. } => {
            calls_of(lhs, name, out);
            calls_of(rhs, name, out);
        }
        ExprKind::Compare { first, rest } => {
            calls_of(first, name, out);
            for (_, r) in rest {
                calls_of(r, name, out);
            }
        }
        ExprKind::IfElse { then, cond, otherwise } => {
            calls_of(cond, name, out);
            calls_of(then, name, out);
            calls_of(otherwise, name, out);
        }
        ExprKind::List(xs) | ExprKind::Tuple(xs) => xs.iter().for_each(|x| calls_of(x, name, out)),
        _ => {}
    }
}

/// Integer bounds for small-value generation: the type's range and the
/// refinement's bounds, narrowed to about ±1000 where possible.
fn small_range(i: IntTy, lo: Option<i128>, hi: Option<i128>) -> (i128, i128) {
    let (tmin, tmax) = int_type_range(i);
    let base_lo = lo.map_or(tmin, |l| l.max(tmin));
    let base_hi = hi.map_or(tmax, |h| h.min(tmax));
    let mut a = base_lo.max(-1000);
    let mut b = base_hi.min(1000);
    if a > b {
        if base_lo > 1000 {
            a = base_lo;
            b = base_hi.min(base_lo.saturating_add(1000));
        } else {
            b = base_hi;
            a = base_lo.max(base_hi.saturating_sub(1000));
        }
    }
    (a, b)
}

fn int_type_range(i: IntTy) -> (i128, i128) {
    match i {
        IntTy::I8 => (i8::MIN as i128, i8::MAX as i128),
        IntTy::I16 => (i16::MIN as i128, i16::MAX as i128),
        IntTy::I32 => (i32::MIN as i128, i32::MAX as i128),
        IntTy::I64 | IntTy::Isize => (i64::MIN as i128, i64::MAX as i128),
        IntTy::I128 => (i128::MIN, i128::MAX),
        IntTy::U8 => (0, u8::MAX as i128),
        IntTy::U16 => (0, u16::MAX as i128),
        IntTy::U32 => (0, u32::MAX as i128),
        IntTy::U64 | IntTy::Usize => (0, u64::MAX as i128),
        IntTy::U128 => (0, i128::MAX),
    }
}

/// `self.a` or `self.a.b`
fn self_path(e: &Expr) -> Option<Vec<String>> {
    match &e.kind {
        ExprKind::Attr { base, attr } => match &base.kind {
            ExprKind::Name(n) if n == "self" => Some(vec![attr.name.clone()]),
            ExprKind::Attr { base: inner, attr: a } if matches!(&inner.kind, ExprKind::Name(n) if n == "self") => {
                Some(vec![a.name.clone(), attr.name.clone()])
            }
            _ => None,
        },
        _ => None,
    }
}

/// Every `self.a(.b)` path an expression reads; `None` if it uses `self`
/// in any other way (e.g. a quantifier over `self.items`, or `self` itself).
fn self_paths(e: &Expr) -> Option<Vec<Vec<String>>> {
    fn walk(e: &Expr, out: &mut Vec<Vec<String>>) -> bool {
        if let Some(p) = self_path(e) {
            out.push(p);
            return true;
        }
        match &e.kind {
            ExprKind::Name(n) => n != "self",
            ExprKind::Int(_) | ExprKind::Float(_) | ExprKind::Bool(_) | ExprKind::Str { .. } | ExprKind::Duration { .. } => true,
            ExprKind::Unary { expr, .. } => walk(expr, out),
            ExprKind::Binary { lhs, rhs, .. } => walk(lhs, out) && walk(rhs, out),
            ExprKind::Call { func, args } => {
                matches!(func.kind, ExprKind::Name(_)) && args.iter().all(|a| walk(&a.value, out))
            }
            ExprKind::Attr { base, .. } => walk(base, out),
            ExprKind::IfElse { then, cond, otherwise } => walk(then, out) && walk(cond, out) && walk(otherwise, out),
            _ => false,
        }
    }
    let mut out = Vec::new();
    walk(e, &mut out).then_some(out)
}

/// `a op b and c op d op e` → pairwise comparisons.
fn comparisons<'e>(e: &'e Expr, out: &mut Vec<(&'e Expr, CmpOp, &'e Expr)>) {
    match &e.kind {
        ExprKind::Binary { op: BinOp::And, lhs, rhs } => {
            comparisons(lhs, out);
            comparisons(rhs, out);
        }
        ExprKind::Compare { first, rest } => {
            let mut prev: &Expr = first;
            for (op, next) in rest {
                if matches!(op, CmpOp::Lt | CmpOp::Le | CmpOp::Gt | CmpOp::Ge | CmpOp::Eq) {
                    out.push((prev, *op, next));
                }
                prev = next;
            }
        }
        _ => {}
    }
}

fn flip(op: CmpOp) -> CmpOp {
    match op {
        CmpOp::Lt => CmpOp::Gt,
        CmpOp::Le => CmpOp::Ge,
        CmpOp::Gt => CmpOp::Lt,
        CmpOp::Ge => CmpOp::Le,
        other => other,
    }
}

impl<'m> TestGen<'m> {
    /// Type of a `self.a(.b)` path of a struct.
    fn path_ty(&self, owner: &Ty, path: &[String]) -> Option<Ty> {
        let mut t = owner.clone();
        for p in path {
            t = self.model.struct_fields(&t)?.into_iter().find(|f| &f.name == p)?.ty;
        }
        Some(t)
    }

    /// Strategy for a struct. Fields are generated independently and the
    /// invariants filter the result; when invariants bound a numeric field
    /// by other fields (`0.0 <= self.ball.x < self.width`,
    /// `self.score <= self.target`), that field is computed from the fields
    /// it depends on instead, so related fields are generated together
    /// rather than mostly rejected.
    fn struct_strategy(&mut self, id: &str, s: &StructInfo, ast_s: &ast::Struct, rust_ty: &str) -> String {
        let fields = s.fields.clone();
        let strats: Vec<String> = fields.iter().map(|f| self.strategy(&f.ty)).collect();
        let names: Vec<String> = fields.iter().map(|f| rust_keyword_safe(&f.name)).collect();
        if !s.has_invariants {
            return format!("{}.prop_map(|{}| {rust_ty} {{ {} }}).boxed()", nest_tuple(&strats), nest_tuple(&names), names.join(", "));
        }
        let owner = Ty::Named(id.to_string(), vec![]);
        // bounds per path: (op with the path on the left, other side)
        let mut bounds: BTreeMap<Vec<String>, Vec<(CmpOp, &Expr)>> = BTreeMap::new();
        let mut deps: BTreeMap<Vec<String>, Set<Vec<String>>> = BTreeMap::new();
        let numeric = |t: &Ty| matches!(t, Ty::Int(_) | Ty::Float32 | Ty::Float64);
        fn reaches(deps: &BTreeMap<Vec<String>, Set<Vec<String>>>, from: &Vec<String>, to: &Vec<String>) -> bool {
            let mut stack = vec![from.clone()];
            let mut seen = Set::new();
            while let Some(p) = stack.pop() {
                if &p == to {
                    return true;
                }
                if seen.insert(p.clone()) {
                    stack.extend(deps.get(&p).into_iter().flatten().cloned());
                }
            }
            false
        }
        let mut atoms = Vec::new();
        for inv in &ast_s.invariants {
            comparisons(inv, &mut atoms);
        }
        for (l, op, r) in atoms {
            let candidates = [(self_path(l), op, r), (self_path(r), flip(op), l)];
            for (path, op, other) in candidates {
                let Some(path) = path else { continue };
                if !self.path_ty(&owner, &path).is_some_and(|t| numeric(&t)) {
                    continue;
                }
                let Some(reads) = self_paths(other) else { continue };
                // depending on a path's prefix or on itself would be circular
                if reads.iter().any(|q| q.starts_with(&path) || path.starts_with(q)) {
                    continue;
                }
                if reads.iter().any(|q| reaches(&deps, q, &path)) {
                    continue;
                }
                deps.entry(path.clone()).or_default().extend(reads);
                bounds.entry(path).or_default().push((op, other));
                break;
            }
        }
        let check = ".prop_filter(\"invariant\", |v| v.wh_check().is_ok())";
        if bounds.is_empty() {
            return format!("{}.prop_map(|{}| {rust_ty} {{ {} }}){check}.boxed()", nest_tuple(&strats), nest_tuple(&names), names.join(", "));
        }
        // bounded paths in dependency order
        let mut order: Vec<Vec<String>> = Vec::new();
        while order.len() < bounds.len() {
            let next = bounds
                .keys()
                .find(|p| !order.contains(p) && deps[*p].iter().all(|q| !bounds.contains_key(q) || order.contains(q)))
                .cloned();
            match next {
                Some(p) => order.push(p),
                None => break,
            }
        }
        let eg = ExprGen::new(self.model, self.module);
        let mut scope = Scope::default();
        scope.self_var = Some(Var { name: "self".into(), rust: "v".into(), ty: owner.clone(), is_ref: false });
        let span = if self.small { "1000.0" } else { "1.0e6" };
        let mut all_strats = strats.clone();
        let mut all_names = names.clone();
        let mut assigns = String::new();
        for (k, path) in order.iter().enumerate() {
            let seed = format!("wh_seed_{k}");
            all_strats.push("(0.0f64..1.0f64)".into());
            all_names.push(seed.clone());
            let ty = self.path_ty(&owner, path).unwrap();
            let place = format!("v.{}", path.iter().map(|p| rust_keyword_safe(p)).collect::<Vec<_>>().join("."));
            let mut lo = vec!["f64::NEG_INFINITY".to_string()];
            let mut hi = vec!["f64::INFINITY".to_string()];
            let int = matches!(ty, Ty::Int(_));
            for (op, other) in &bounds[path] {
                let e = format!("(({}) as f64)", eg.emit_as(other, &Ty::Float64, &scope));
                let (l, h) = match (op, int) {
                    (CmpOp::Ge, true) => (Some(format!("{e}.ceil()")), None),
                    (CmpOp::Gt, true) => (Some(format!("({e}.floor() + 1.0)")), None),
                    (CmpOp::Le, true) => (None, Some(format!("{e}.floor()"))),
                    (CmpOp::Lt, true) => (None, Some(format!("({e}.ceil() - 1.0)"))),
                    (CmpOp::Ge | CmpOp::Gt, false) => (Some(e), None),
                    (CmpOp::Le | CmpOp::Lt, false) => (None, Some(e)),
                    (CmpOp::Eq, _) => (Some(e.clone()), Some(e)),
                    _ => (None, None),
                };
                lo.extend(l);
                hi.extend(h);
            }
            let (rust, tmin, tmax) = match &ty {
                Ty::Int(i) => {
                    let (a, b) = if self.small { small_range(*i, None, None) } else { int_type_range(*i) };
                    (i.rust().to_string(), format!("{a}f64"), format!("{b}f64"))
                }
                Ty::Float32 => ("f32".to_string(), "f64::NEG_INFINITY".into(), "f64::INFINITY".into()),
                _ => ("f64".to_string(), "f64::NEG_INFINITY".into(), "f64::INFINITY".into()),
            };
            let fold = |xs: &[String], f: &str| xs[1..].iter().fold(xs[0].clone(), |acc, x| format!("{acc}.{f}({x})"));
            let value = if int {
                format!("(lo + {seed} * (hi - lo + 1.0)).floor().clamp(lo, hi) as i128 as {rust}")
            } else {
                format!("(if lo.is_finite() && hi.is_finite() {{ lo + {seed} * (hi - lo) }} else if lo.is_finite() {{ lo + {seed} * {span} }} else {{ hi - {seed} * {span} }}) as {rust}")
            };
            let _ = write!(
                assigns,
                "\n            let wh_x = {{\n                let lo: f64 = {}.max({tmin});\n                let hi: f64 = {}.min({tmax});\n                if lo <= hi && (lo.is_finite() || hi.is_finite()) {{ {value} }} else {{ {place} }}\n            }};\n            {place} = wh_x;",
                fold(&lo, "max"),
                fold(&hi, "min"),
            );
        }
        format!(
            "{}.prop_map(|{}| {{\n            let mut v = {rust_ty} {{ {} }};{assigns}\n            v\n        }}){check}.boxed()",
            nest_tuple(&all_strats),
            nest_tuple(&all_names),
            names.join(", ")
        )
    }

    /// `prop_assume!` the node's `requires` for every call of the node in a
    /// property body: inputs outside the precondition are not test cases.
    fn requires_assumptions(&self, n: &Node, body: &Expr, scope: &Scope) -> String {
        let (Some(def), NodeKind::Def(info)) = (n.def(), &n.kind) else { return String::new() };
        let requires: Vec<&Expr> = def
            .clauses
            .iter()
            .filter_map(|c| match &c.kind {
                ClauseKind::Requires(e) => Some(e),
                _ => None,
            })
            .collect();
        if requires.is_empty() {
            return String::new();
        }
        let mut calls = Vec::new();
        calls_of(body, &n.name, &mut calls);
        let eg = self.eg();
        let mut out = String::new();
        for (k, args) in calls.into_iter().enumerate() {
            let mut inner = scope.clone();
            let mut lets = String::new();
            let mut positional = args.iter().filter(|a| a.name.is_none());
            for (p, ap) in info.sig.params.iter().zip(&def.sig.params) {
                let given = args.iter().find(|a| a.name.as_ref().is_some_and(|x| x.name == p.name)).or_else(|| positional.next());
                let code = match (given, &ap.default) {
                    (Some(a), _) => eg.emit_as(&a.value, &p.ty, scope),
                    (None, Some(d)) => eg.emit_as(d, &p.ty, &Scope::default()),
                    (None, None) => continue,
                };
                let v = format!("wh_req{k}_{}", p.name);
                let _ = write!(lets, "let {v} = {code}; ");
                inner.var(&p.name, &v, p.ty.clone(), false);
            }
            for e in &requires {
                let _ = writeln!(out, "            {{ {lets}proptest::prop_assume!({}); }}", eg.emit_bool(e, &inner));
            }
        }
        out
    }

    /// The test-owned variable for a parameter, passed by its mode.
    pub(crate) fn pass_owned(&self, var: &str, p: &ParamInfo) -> String {
        let names = self.names();
        match p.mode {
            ParamMode::Own => {
                if names.is_copy(&p.ty) {
                    var.to_string()
                } else {
                    format!("{var}.clone()")
                }
            }
            ParamMode::Mut => format!("&mut {var}"),
            ParamMode::Borrow if names.is_copy(&p.ty) => var.to_string(),
            ParamMode::Borrow => format!("&{var}"),
        }
    }

    /// Can tests generate values of this type?
    pub(crate) fn generable(&self, t: &Ty) -> bool {
        let mut ok = !matches!(t, Ty::Fn(..) | Ty::Stream(_) | Ty::Param(_) | Ty::Unknown | Ty::Instant);
        t.walk_named(&mut |id| {
            ok &= matches!(
                self.model.nodes.get(id).map(|n| &n.kind),
                Some(NodeKind::Struct(_) | NodeKind::Enum(_) | NodeKind::Refined { .. } | NodeKind::Alias(_) | NodeKind::Machine(_))
            );
        });
        match t {
            Ty::List(e) | Ty::Set(e) | Ty::SortedSet(e) | Ty::Option(e) | Ty::Array(e, _) => ok && self.generable(e),
            Ty::Dict(k, v) | Ty::SortedDict(k, v) => ok && self.generable(k) && self.generable(v),
            Ty::Tuple(ts) => ok && ts.iter().all(|t| self.generable(t)),
            _ => ok,
        }
    }

    /// `pattern Transform`: the def is total, i.e. it returns (a value or one
    /// of its declared errors) for every valid input, without panicking and
    /// while keeping its contracts (checked by the wrapper it is called through).
    fn transform_total(&mut self, body: &mut String, n: &Node, sig: &SigInfo) {
        let [p] = sig.params.as_slice() else { return };
        if !sig.uses.is_empty() || !self.generable(&p.ty) {
            return;
        }
        let Some(def) = n.def() else { return };
        self.small = true;
        let strat = self.strategy(&p.ty);
        self.small = false;
        let eg = self.eg();
        let var = rust_keyword_safe(&p.name);
        let mut scope = Scope::default();
        scope.var(&p.name, &var, p.ty.clone(), false);
        let mut assume = String::new();
        for c in &def.clauses {
            if let ClauseKind::Requires(e) = &c.kind {
                let _ = writeln!(assume, "            proptest::prop_assume!({});", eg.emit_bool(e, &scope));
            }
        }
        let mut call = format!("{}({})", self.names().path(&n.id), self.pass_owned(&var, p));
        if sig.is_async {
            call = format!("crate::wh_rt::block_on({call})");
        }
        let name = format!("{}__transform_total", test_prefix(&n.id));
        let mutable = if p.mode == ParamMode::Mut { "mut " } else { "" };
        let _ = writeln!(
            body,
            "{PROPTEST_HEADER}\n        fn {name}({mutable}{var} in {strat}) {{\n            crate::wh_rt::init_test_tracing();\n{assume}            let _ = {call};\n        }}\n    }}\n"
        );
        self.tests.push((name, n.id.clone()));
    }

    /// Machines: `step` is total and agrees with the transition table, and
    /// every state is reachable from `initial` along its witness path.
    fn machine_tests(&mut self, body: &mut String, n: &Node) {
        let NodeKind::Machine(info) = &n.kind else { return };
        let ty = Ty::Named(n.id.clone(), vec![]);
        if !self.generable(&info.event) || !self.generable(&ty) {
            return;
        }
        let path = self.names().path(&n.id);
        self.small = true;
        let states = self.strategy(&ty);
        let events = self.strategy(&info.event);
        self.small = false;
        let prefix = test_prefix(&n.id);
        let name = format!("{prefix}__totality");
        let _ = writeln!(
            body,
            "{PROPTEST_HEADER}\n        fn {name}(state in {states}, event in {events}) {{\n            crate::wh_rt::init_test_tracing();\n            let (from, ev) = (state.name(), {path}::event_name(&event));\n            let handled = {path}::TRANSITIONS.iter().any(|t| t.matches(from, ev))\n                || {path}::IGNORED.iter().any(|(s, e)| (*s == \"_\" || *s == from) && (*e == \"_\" || *e == ev));\n            proptest::prop_assert!(handled, \"({{from}}, {{ev}}) is not handled\");\n            let (next, effect) = {path}::step(state.clone(), event);\n            let fired = {path}::TRANSITIONS.iter().any(|t| t.matches(from, ev) && t.to == next.name());\n            proptest::prop_assert!(fired || (next == state && effect.is_none()), \"step({{from}}, {{ev}}) went to {{}} outside the table\", next.name());\n            proptest::prop_assert!({path}::STATES.contains(&next.name()));\n        }}\n    }}\n"
        );
        self.tests.push((name, n.id.clone()));
        let witnesses: Vec<String> = info
            .paths
            .iter()
            .map(|(s, p)| format!("({}, &[{}][..])", esc(s), p.iter().map(|i| format!("{i}usize")).collect::<Vec<_>>().join(", ")))
            .collect();
        let name = format!("{prefix}__reachability");
        let _ = writeln!(
            body,
            "    #[test]\n    fn {name}() {{\n        crate::wh_rt::init_test_tracing();\n        let mut runner = proptest::test_runner::TestRunner::deterministic();\n        let events = {events};\n        for (target, path) in [{}] {{\n            let mut state = {path}::initial();\n            for &i in path {{\n                let t = &{path}::TRANSITIONS[i];\n                let mut moved = false;\n                for _ in 0..512 {{\n                    let ev = proptest::strategy::ValueTree::current(&proptest::strategy::Strategy::new_tree(&events, &mut runner).unwrap());\n                    if t.event != \"_\" && {path}::event_name(&ev) != t.event {{\n                        continue;\n                    }}\n                    let (next, _) = {path}::step(state.clone(), ev);\n                    if next.name() == t.to {{\n                        state = next;\n                        moved = true;\n                        break;\n                    }}\n                }}\n                assert!(moved, \"could not take {{}} on the way to `{{target}}`\", t.id);\n            }}\n            assert_eq!(state.name(), target);\n        }}\n    }}\n",
            witnesses.join(", ")
        );
        self.tests.push((name, n.id.clone()));
    }

    /// `Adapter::new(...)` from the adapter's field defaults; `None` when a
    /// field has no default.
    pub(crate) fn adapter_ctor(&self, n: &Node) -> Option<String> {
        let (ItemKind::Adapter(a), NodeKind::Adapter(info)) = (&n.item.kind, &n.kind) else { return None };
        let eg = self.eg();
        let mut args = Vec::new();
        for (f, fi) in a.fields.iter().zip(&info.fields) {
            args.push(eg.emit_as(f.default.as_ref()?, &fi.ty, &Scope::default()));
        }
        Some(format!("{}::new({})", self.names().path(&n.id), args.join(", ")))
    }

    fn method_call(&self, recv: &str, name: &str, sig: &SigInfo, args: &[String]) -> String {
        let call = format!("{recv}.{}({})", rust_keyword_safe(name), args.join(", "));
        if sig.is_async { format!("crate::wh_rt::block_on({call})") } else { call }
    }

    /// `pattern Repository` (on the adapter or its capability): the stored
    /// value comes back unchanged, `find(insert(x)) == x`.
    fn repository_tests(&mut self, body: &mut String, n: &Node) {
        let NodeKind::Adapter(a) = &n.kind else { return };
        let Some(ctor) = self.adapter_ctor(n) else { return };
        for cap in &a.implements {
            let cap_node = &self.model.nodes[cap];
            let tagged = |x: &Node| x.patterns.iter().any(|p| p == "Repository");
            let item = if tagged(n) {
                self.model.pattern_arg(n, "Repository")
            } else if tagged(cap_node) {
                self.model.pattern_arg(cap_node, "Repository")
            } else {
                continue;
            };
            let Some(shape) = self.model.repository_shape(cap, item.as_ref()) else { continue };
            let NodeKind::Capability(c) = &cap_node.kind else { continue };
            let ins = c.methods.iter().find(|m| m.name == shape.insert).unwrap();
            let find = c.methods.iter().find(|m| m.name == shape.find).unwrap();
            if !self.generable(&shape.item) {
                continue;
            }
            self.small = true;
            let strat = self.strategy(&shape.item);
            self.small = false;
            let fail = |what: &str| format!(".map_err(|e| proptest::test_runner::TestCaseError::fail(format!(\"`{what}` raised {{e:?}}\")))?");
            let q_ins = if ins.sig.raises.is_some() { fail(&shape.insert) } else { String::new() };
            let q_find = if find.sig.raises.is_some() { fail(&shape.find) } else { String::new() };
            let insert = self.method_call("repo", &shape.insert, &ins.sig, &[self.pass_owned("x", &ins.sig.params[0])]);
            let key = match &shape.key_field {
                None => format!("            let key = {insert}{q_ins};\n"),
                Some(f) => format!("            {insert}{q_ins};\n            let key = x.{}.clone();\n", rust_keyword_safe(f)),
            };
            let found = self.method_call("repo", &shape.find, &find.sig, &[self.pass_owned("key", &find.sig.params[0])]);
            let name = format!("{}__repository_roundtrip", test_prefix(&n.id));
            let _ = writeln!(
                body,
                "{PROPTEST_HEADER}\n        fn {name}(x in {strat}) {{\n            crate::wh_rt::init_test_tracing();\n            let repo = {ctor};\n{key}            let found = {found}{q_find};\n            proptest::prop_assert_eq!(found, Some(x), \"find(insert(x)) == x\");\n        }}\n    }}\n"
            );
            self.tests.push((name, n.id.clone()));
        }
    }

    /// `pattern Adapter` / `InMemory`: the contracts of the capability's
    /// methods (`requires` as assumptions, `raises … when`, `ensures`) are
    /// checked against this adapter, so every adapter of a capability is
    /// held to the same tests.
    fn contract_tests(&mut self, body: &mut String, n: &Node) {
        let NodeKind::Adapter(a) = &n.kind else { return };
        let Some(ctor) = self.adapter_ctor(n) else { return };
        for cap in &a.implements {
            let cap_node = &self.model.nodes[cap];
            let (NodeKind::Capability(c), ItemKind::Capability(ast_c)) = (&cap_node.kind, &cap_node.item.kind) else { continue };
            for (m, am) in c.methods.iter().zip(&ast_c.methods) {
                let contracts: Vec<&ast::Clause> = am
                    .clauses
                    .iter()
                    .filter(|cl| matches!(cl.kind, ClauseKind::Requires(_) | ClauseKind::Ensures(_) | ClauseKind::RaisesWhen { .. }))
                    .collect();
                if contracts.is_empty() || !m.sig.params.iter().all(|p| self.generable(&p.ty)) {
                    continue;
                }
                // the contracts are resolved in the capability's module, but emitted here
                let mut eg = ExprGen::new(self.model, &cap_node.module);
                eg.names.qualified = cap_node.module != self.module;
                let text = &self.model.files[cap_node.file].text;
                self.small = true;
                let strats: Vec<String> = m
                    .sig
                    .params
                    .iter()
                    .map(|p| format!("{}{} in {}", if p.mode == ParamMode::Mut { "mut " } else { "" }, rust_keyword_safe(&p.name), self.strategy(&p.ty)))
                    .collect();
                self.small = false;
                let mut scope = Scope::default();
                for p in &m.sig.params {
                    scope.var(&p.name, &rust_keyword_safe(&p.name), p.ty.clone(), false);
                }
                let mut pre = String::new();
                for p in m.sig.params.iter().filter(|p| p.mode == ParamMode::Mut) {
                    let v = format!("wh_pre_{}", p.name);
                    let _ = writeln!(pre, "            let {v} = {}.clone();", rust_keyword_safe(&p.name));
                    scope.old.push((p.name.clone(), v));
                }
                let mut checks = String::new();
                let mut raises = Vec::new();
                for cl in &contracts {
                    match &cl.kind {
                        ClauseKind::Requires(e) => {
                            let _ = writeln!(pre, "            proptest::prop_assume!({});", eg.emit_bool(e, &scope));
                        }
                        ClauseKind::RaisesWhen { error, when } => {
                            let v = format!("wh_raise_{}", raises.len());
                            let _ = writeln!(pre, "            let {v} = {};", eg.emit_bool(when, &scope));
                            let (err, variant) = self.model.resolve_error_ref(&cap_node.module, error, &mut Vec::new());
                            raises.push((v, eg.err_pattern(&err, &variant), text[cl.span.range()].to_string()));
                        }
                        _ => {}
                    }
                }
                for (v, pat, src) in &raises {
                    let _ = writeln!(checks, "            proptest::prop_assert!(!{v} || matches!(&result, {pat}), \"{}\");", src.replace('\\', "\\\\").replace('"', "\\\"").replace('{', "{{").replace('}', "}}"));
                }
                let is_result = m.sig.raises.is_some();
                let mut es = scope.clone();
                es.result = Some(Var { name: "result".into(), rust: "result".into(), ty: m.sig.ret.clone(), is_ref: is_result });
                let mut ensures = String::new();
                for cl in &contracts {
                    if let ClauseKind::Ensures(e) = &cl.kind {
                        let src = text[e.span.range()].replace('\\', "\\\\").replace('"', "\\\"").replace('{', "{{").replace('}', "}}");
                        let _ = writeln!(ensures, "                proptest::prop_assert!({}, \"ensures {src}\");", eg.emit_bool(e, &es));
                    }
                }
                if !ensures.is_empty() {
                    if is_result {
                        let _ = writeln!(checks, "            if let Ok(result) = &result {{\n{ensures}            }}");
                    } else {
                        let _ = writeln!(checks, "            {{\n{ensures}            }}");
                    }
                }
                let args: Vec<String> = m.sig.params.iter().map(|p| self.pass_owned(&rust_keyword_safe(&p.name), p)).collect();
                let call = self.method_call("adapter", &m.name, &m.sig, &args);
                let name = format!("{}__contract__{}", test_prefix(&n.id), m.name);
                if strats.is_empty() {
                    // nothing to generate: run the checks once
                    let _ = writeln!(
                        body,
                        "    #[test]\n    fn {name}() {{\n        crate::wh_rt::init_test_tracing();\n        let check = || -> Result<(), proptest::test_runner::TestCaseError> {{\n            let adapter = {ctor};\n{pre}            let result = {call};\n{checks}            Ok(())\n        }};\n        if let Err(e) = check() {{\n            panic!(\"{{e}}\");\n        }}\n    }}\n"
                    );
                } else {
                    let _ = writeln!(
                        body,
                        "{PROPTEST_HEADER}\n        fn {name}({}) {{\n            crate::wh_rt::init_test_tracing();\n            let adapter = {ctor};\n{pre}            let result = {call};\n{checks}        }}\n    }}\n",
                        strats.join(", ")
                    );
                }
                self.tests.push((name, n.id.clone()));
            }
        }
    }
}
