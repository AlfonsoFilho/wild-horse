//! Generation of the probe crate: one Rust function per probe (an example,
//! the `ensures` samples of a def, a property, a scenario test), plus the
//! metadata that maps what the probe prints back to clauses and spans.

use std::fmt::Write as _;

use serde::{Deserialize, Serialize};
use wh_codegen::expr::{ExprGen, Scope, Var};
use wh_codegen::rust::{module_ident, rust_keyword_safe};
use wh_model::{Model, NodeKind};
use wh_perf::inputs::InputGen;
use wh_syntax::ast::{Arg, ClauseKind, CmpOp, Expr, ExprKind, ParamMode};

/// What a live value belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ItemKind {
    /// `example …` of a def (clause index among the def's clauses).
    Example,
    /// `ensures …` of a def, checked on sampled inputs.
    Ensures,
    /// `property name(…): …` of a def.
    Property,
    /// `expect …` of a scenario `test` (index among the test's expects).
    Expect,
}

/// A byte range in a source file, with its 1-based start line and column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceSpan {
    pub start: u32,
    pub end: u32,
    pub line: u32,
    pub col: u32,
}

impl SourceSpan {
    pub fn of(text: &str, span: wh_syntax::span::Span) -> Self {
        let start = (span.start as usize).min(text.len());
        let before = &text[..start];
        let line = before.matches('\n').count() as u32 + 1;
        let col = before.rsplit('\n').next().map_or(0, |l| l.chars().count()) as u32 + 1;
        SourceSpan {
            start: span.start,
            end: span.end,
            line,
            col,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SlotMeta {
    /// `left`, `right`, `result`, a parameter name, …
    pub label: String,
    /// The sub-expression the value belongs to, if it has one.
    pub span: Option<SourceSpan>,
}

/// One clause a probe reports on.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ItemMeta {
    pub node: String,
    pub kind: ItemKind,
    pub clause_index: usize,
    /// Source file (project-relative) and span of the clause.
    pub file: String,
    pub span: SourceSpan,
    pub source: String,
    pub slots: Vec<SlotMeta>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProbeMeta {
    pub id: String,
    /// The node whose clauses it runs (`module#test:name` for scenario tests).
    pub node: String,
    pub items: Vec<ItemMeta>,
    /// Lines of its function in `src/probes.rs` (to attribute compile errors).
    pub lines: (usize, usize),
}

/// A clause that cannot be probed, and why.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Unprobed {
    pub item: ItemMeta,
    pub reason: String,
}

pub struct Generated {
    pub code: String,
    pub probes: Vec<ProbeMeta>,
    pub unprobed: Vec<Unprobed>,
}

#[derive(Debug, Clone)]
pub struct GenSettings {
    /// Inputs sampled per def for its `ensures` clauses.
    pub samples: usize,
    /// Cases per property.
    pub cases: u32,
    /// Probe ids to leave out (they failed to compile).
    pub exclude: Vec<String>,
}

fn fn_name(id: &str) -> String {
    let s: String = id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    format!("p_{s}")
}

fn name_expr(name: &str, span: wh_syntax::span::Span) -> Expr {
    Expr {
        kind: ExprKind::Name(name.into()),
        span,
    }
}

/// A call to a def that raises: (func, args).
fn raising_call<'e>(model: &Model, module: &str, e: &'e Expr) -> Option<(&'e Expr, &'e [Arg])> {
    let ExprKind::Call { func, args } = &e.kind else {
        return None;
    };
    let ExprKind::Name(name) = &func.kind else {
        return None;
    };
    let id = model.resolve_name(module, name)?;
    model.nodes.get(id)?.sig()?.raises.as_ref()?;
    Some((func, args))
}

/// Comparison operators whose operands are shown separately.
fn split_op(op: CmpOp) -> bool {
    matches!(
        op,
        CmpOp::Eq
            | CmpOp::NotEq
            | CmpOp::Lt
            | CmpOp::Le
            | CmpOp::Gt
            | CmpOp::Ge
            | CmpOp::In
            | CmpOp::NotIn
    )
}

struct Ctx<'m> {
    model: &'m Model,
    lib: String,
}

impl<'m> Ctx<'m> {
    fn fix(&self, code: String) -> String {
        code.replace("crate::", &format!("{}::", self.lib))
    }

    /// Code evaluating a boolean check once, showing the operands of a
    /// comparison: slots `base` (left) and `base + 1` (right), or none.
    /// Ends with `out.pass(…)`, or `out.fail(…)` when a raising call errs.
    fn check(
        &self,
        module: &str,
        e: &Expr,
        scope: &Scope,
        base: usize,
        label: &str,
        indent: &str,
    ) -> (String, Vec<SlotMeta>, bool) {
        let eg = ExprGen::new(self.model, module);
        let mut code = String::new();
        if let ExprKind::Compare { first, rest } = &e.kind
            && let [(op, rhs)] = rest.as_slice()
            && split_op(*op)
        {
            {
                {
                    let mut sc = scope.clone();
                    for (side, var, slot) in
                        [(first.as_ref(), "wh_l", base), (rhs, "wh_r", base + 1)]
                    {
                        let ty = eg.ty(side, &sc);
                        match raising_call(self.model, module, side) {
                            Some((func, args)) => {
                                let call = self.fix(eg.call(func, args, &sc, false));
                                let _ = write!(
                                    code,
                                    "{indent}let {var}_r = {call};\n{indent}out.res({slot}, &{var}_r);\n{indent}let {var} = match {var}_r {{\n{indent}    Ok(v) => v,\n{indent}    Err(e) => {{\n{indent}        out.fail(format!(\"raised {{e:?}}\"));\n{indent}        break '{label};\n{indent}    }}\n{indent}}};\n"
                                );
                            }
                            None => {
                                let v = self.fix(eg.emit(side, &sc));
                                let _ = write!(
                                    code,
                                    "{indent}let {var} = {v};\n{indent}out.val({slot}, &{var});\n"
                                );
                            }
                        }
                        sc.var(var, var, ty, false);
                    }
                    let synthetic = Expr {
                        kind: ExprKind::Compare {
                            first: Box::new(name_expr("wh_l", first.span)),
                            rest: vec![(*op, name_expr("wh_r", rhs.span))],
                        },
                        span: e.span,
                    };
                    let _ = writeln!(
                        code,
                        "{indent}out.pass({});",
                        self.fix(eg.emit_bool(&synthetic, &sc))
                    );
                    return (code, vec![], true);
                }
            }
        }
        let _ = writeln!(
            code,
            "{indent}out.pass({});",
            self.fix(eg.emit_bool(e, scope))
        );
        (code, vec![], false)
    }
}

/// Generate the probes for every def and scenario test of the model.
pub fn generate(model: &Model, lib: &str, settings: &GenSettings) -> Generated {
    let cx = Ctx {
        model,
        lib: lib.to_string(),
    };
    let mut code = String::new();
    let mut probes: Vec<ProbeMeta> = Vec::new();
    let mut unprobed = Vec::new();
    let mut table = String::new();
    let mut emit_probe =
        |code: &mut String, id: String, node: String, items: Vec<ItemMeta>, body: String| {
            if settings.exclude.contains(&id) {
                return;
            }
            let f = fn_name(&id);
            let start = code.lines().count() + 1;
            let _ = write!(code, "fn {f}(out: &mut rt::Out) {{\n{body}}}\n\n");
            let end = code.lines().count();
            let _ = writeln!(table, "    ({id:?}, {f}),");
            probes.push(ProbeMeta {
                id,
                node,
                items,
                lines: (start, end),
            });
        };

    for n in model.nodes.values() {
        let NodeKind::Def(info) = &n.kind else {
            continue;
        };
        let Some(def) = n.def() else { continue };
        let file = &model.files[n.file];
        let text = &file.text;
        let item = |kind: ItemKind, i: usize, span: wh_syntax::span::Span, slots: Vec<SlotMeta>| {
            ItemMeta {
                node: n.id.clone(),
                kind,
                clause_index: i,
                file: file.path.clone(),
                span: SourceSpan::of(text, span),
                source: text.get(span.range()).unwrap_or("").trim().to_string(),
                slots,
            }
        };
        let use_mod = format!("    use {lib}::{}::*;\n", module_ident(&n.module));
        let eg = ExprGen::new(model, &n.module);
        let generic = !def.sig.generics.is_empty();

        // examples
        for (i, c) in def.clauses.iter().enumerate() {
            let ClauseKind::Example { expr, raises } = &c.kind else {
                continue;
            };
            let id = format!("{}/example/{i}", n.id);
            let mut body = use_mod.clone();
            body.push_str("    'p: {\n");
            let slots = match raises {
                Some(r) => {
                    let mut d = Vec::new();
                    let (err, variant) = model.resolve_error_ref(&n.module, r, &mut d);
                    let call = match &expr.kind {
                        ExprKind::Call { func, args } => {
                            eg.call(func, args, &Scope::default(), false)
                        }
                        _ => eg.emit(expr, &Scope::default()),
                    };
                    let pat = cx.fix(eg.err_pattern(&err, &variant));
                    let _ = write!(
                        body,
                        "        let wh_v = {};\n        out.res(0, &wh_v);\n        out.pass(matches!(&wh_v, {pat}));\n",
                        cx.fix(call)
                    );
                    vec![SlotMeta {
                        label: "result".into(),
                        span: Some(SourceSpan::of(text, expr.span)),
                    }]
                }
                None => {
                    let (check, _, split) =
                        cx.check(&n.module, expr, &Scope::default(), 0, "p", "        ");
                    body.push_str(&check);
                    match &expr.kind {
                        ExprKind::Compare { first, rest } if split => vec![
                            SlotMeta {
                                label: "left".into(),
                                span: Some(SourceSpan::of(text, first.span)),
                            },
                            SlotMeta {
                                label: "right".into(),
                                span: Some(SourceSpan::of(text, rest[0].1.span)),
                            },
                        ],
                        _ => vec![],
                    }
                }
            };
            body.push_str("    }\n");
            emit_probe(
                &mut code,
                id,
                n.id.clone(),
                vec![item(ItemKind::Example, i, c.span, slots)],
                body,
            );
        }

        // properties
        for (i, c) in def.clauses.iter().enumerate() {
            let ClauseKind::Property {
                params,
                given,
                body: pbody,
                ..
            } = &c.kind
            else {
                continue;
            };
            let slots: Vec<SlotMeta> = params
                .iter()
                .map(|p| SlotMeta {
                    label: p.name.name.clone(),
                    span: Some(SourceSpan::of(text, p.span)),
                })
                .chain(
                    [("left", 0usize), ("right", 1)]
                        .iter()
                        .map(|(l, _)| SlotMeta {
                            label: (*l).into(),
                            span: None,
                        }),
                )
                .collect();
            let it = item(ItemKind::Property, i, c.span, slots);
            let mut ig = InputGen::new(model, lib);
            let mut strats = Vec::new();
            let mut scope = Scope::default();
            let mut failed = None;
            let mut d = Vec::new();
            for p in params {
                let ty = model.resolve_ty(&n.module, &p.ty, &[], &mut d);
                let s = match given.iter().find(|(g, _)| g.name == p.name.name) {
                    Some((_, g)) => Ok(cx.fix(eg.emit(g, &Scope::default()))),
                    None => ig.strategy(&ty, false),
                };
                match s {
                    Ok(s) => strats.push(s),
                    Err(e) => failed = Some(e),
                }
                scope.var(&p.name.name, &rust_keyword_safe(&p.name.name), ty, false);
            }
            if let Some(reason) = failed {
                unprobed.push(Unprobed { item: it, reason });
                continue;
            }
            let pat = format!(
                "({},)",
                params
                    .iter()
                    .map(|p| rust_keyword_safe(&p.name.name))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            let cond = cx.fix(eg.emit_bool(pbody, &scope));
            let mut sides = String::new();
            if let ExprKind::Compare { first, rest } = &pbody.kind
                && let [(_, rhs)] = rest.as_slice()
            {
                {
                    let k = params.len();
                    let _ = write!(
                        sides,
                        "                out.text({k}, rt::try_show(|| {}));\n                out.text({}, rt::try_show(|| {}));\n",
                        cx.fix(eg.emit(first, &scope)),
                        k + 1,
                        cx.fix(eg.emit(rhs, &scope))
                    );
                }
            }
            let shown: String = params
                .iter()
                .enumerate()
                .map(|(k, p)| {
                    let ty = model.resolve_ty(&n.module, &p.ty, &[], &mut Vec::new());
                    if p.is_sensitive || model.type_is_sensitive(&ty) {
                        format!("                out.text({k}, \"<redacted>\");\n")
                    } else {
                        format!(
                            "                out.val({k}, &{});\n",
                            rust_keyword_safe(&p.name.name)
                        )
                    }
                })
                .collect();
            let body = format!(
                "{use_mod}    use proptest::strategy::Strategy;\n    out.capture(false);\n    let mut wh_runner = rt::runner({cases});\n    let wh_strategy = ({},);\n    let wh_r = wh_runner.run(&wh_strategy, |{pat}| match rt::try_bool(|| {cond}) {{\n        Some(true) => Ok(()),\n        Some(false) => Err(proptest::test_runner::TestCaseError::fail(\"the property does not hold\")),\n        None => {{\n            let m = rt::panic_message();\n            if m.contains(\"requires violated\") {{\n                Err(proptest::test_runner::TestCaseError::reject(m))\n            }} else {{\n                Err(proptest::test_runner::TestCaseError::fail(m))\n            }}\n        }}\n    }});\n    out.capture(true);\n    match wh_r {{\n        Ok(()) => {{\n            out.pass(true);\n            out.cases({cases});\n        }}\n        Err(proptest::test_runner::TestError::Fail(why, {pat})) => {{\n            out.fail(why.to_string());\n{shown}{sides}        }}\n        Err(proptest::test_runner::TestError::Abort(why)) => out.error(why.to_string()),\n    }}\n",
                strats.join(", "),
                cases = settings.cases,
            );
            emit_probe(
                &mut code,
                format!("{}/property/{i}", n.id),
                n.id.clone(),
                vec![it],
                body,
            );
        }

        // ensures, on sampled inputs
        let ensures: Vec<(usize, &Expr, wh_syntax::span::Span)> = def
            .clauses
            .iter()
            .enumerate()
            .filter_map(|(i, c)| match &c.kind {
                ClauseKind::Ensures(e) => Some((i, e, c.span)),
                _ => None,
            })
            .collect();
        if ensures.is_empty() || info.extern_path.is_some() {
            continue;
        }
        let sig = &info.sig;
        let slots: Vec<SlotMeta> = sig
            .params
            .iter()
            .map(|p| SlotMeta {
                label: p.name.clone(),
                span: None,
            })
            .chain(std::iter::once(SlotMeta {
                label: "result".into(),
                span: None,
            }))
            .collect();
        let items: Vec<ItemMeta> = ensures
            .iter()
            .map(|(i, _, sp)| item(ItemKind::Ensures, *i, *sp, slots.clone()))
            .collect();
        let reason = if generic {
            Some("generic defs are not sampled".to_string())
        } else if !sig.uses.is_empty() {
            Some("defs that use capabilities are not sampled".into())
        } else if sig.is_async {
            Some("async defs are not sampled".into())
        } else if sig.params.iter().any(|p| p.mode == ParamMode::Mut) {
            Some("defs with `mut` parameters are not sampled".into())
        } else {
            None
        };
        let mut ig = InputGen::new(model, lib);
        let strats: Result<Vec<String>, String> = sig
            .params
            .iter()
            .map(|p| ig.strategy(&p.ty, false))
            .collect();
        let strats = match (reason, strats) {
            (Some(r), _) | (None, Err(r)) => {
                unprobed.extend(items.into_iter().map(|item| Unprobed {
                    item,
                    reason: r.clone(),
                }));
                continue;
            }
            (None, Ok(s)) => s,
        };
        let names = wh_codegen::rust::Names::new(model, &n.module);
        let mut body = format!(
            "{use_mod}    let mut wh_runner = rt::runner(1);\n    let (mut wh_k, mut wh_tries) = (0usize, 0usize);\n    while wh_k < {} && wh_tries < {} {{\n        wh_tries += 1;\n",
            settings.samples,
            settings.samples * 10
        );
        let mut args = Vec::new();
        let mut post = Scope::default();
        for (k, (p, s)) in sig.params.iter().zip(&strats).enumerate() {
            let var = format!("a{k}");
            let _ = writeln!(
                body,
                "        let Some({var}) = rt::draw(&mut wh_runner, {s}) else {{ continue }};"
            );
            args.push(match p.mode {
                ParamMode::Own => format!("{var}.clone()"),
                ParamMode::Borrow if names.is_copy(&p.ty) => var.clone(),
                _ => format!("&{var}"),
            });
        }
        body.push_str("        out.begin(wh_k);\n");
        for (k, p) in sig.params.iter().enumerate() {
            if p.is_sensitive || model.type_is_sensitive(&p.ty) {
                let _ = writeln!(body, "        out.text({k}, \"<redacted>\");");
            } else {
                let _ = writeln!(body, "        out.val({k}, &a{k});");
            }
        }
        let path = format!(
            "{lib}::{}::{}",
            module_ident(&n.module),
            rust_keyword_safe(&n.name)
        );
        let rslot = sig.params.len();
        let _ = write!(
            body,
            "        let wh_res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {path}({})));\n        let result = match wh_res {{\n            Ok(r) => r,\n            Err(_) => {{\n                let m = rt::panic_message();\n                if m.contains(\"requires violated\") {{\n                    out.discard();\n                    continue;\n                }}\n                out.fail(m);\n                wh_k += 1;\n                continue;\n            }}\n        }};\n",
            args.join(", ")
        );
        for (k, p) in sig.params.iter().enumerate() {
            let _ = writeln!(body, "        let {} = a{k};", rust_keyword_safe(&p.name));
            post.var(&p.name, &rust_keyword_safe(&p.name), p.ty.clone(), false);
        }
        let raises = sig.raises.is_some();
        if raises {
            let _ = writeln!(
                body,
                "        out.res({rslot}, &result);\n        if let Ok(result) = &result {{"
            );
        } else {
            let _ = writeln!(body, "        out.val({rslot}, &result);\n        {{");
        }
        post.result = Some(Var {
            name: "result".into(),
            rust: "result".into(),
            ty: sig.ret.clone(),
            is_ref: raises,
        });
        for (j, (_, e, _)) in ensures.iter().enumerate() {
            let _ = writeln!(
                body,
                "            out.check({j}, rt::try_bool(|| {}));",
                cx.fix(eg.emit_bool(e, &post))
            );
        }
        body.push_str("        }\n        wh_k += 1;\n    }\n");
        emit_probe(
            &mut code,
            format!("{}/ensures", n.id),
            n.id.clone(),
            items,
            body,
        );
    }

    // scenario tests
    for t in &model.tests {
        let file = &model.files[t.file];
        let text = &file.text;
        let node = format!("{}#test:{}", t.module, t.test.name);
        let eg = ExprGen::new(model, &t.module);
        let mut scope = Scope::default();
        let mut body = format!("    use {lib}::{}::*;\n", module_ident(&t.module));
        for (g, ast_g) in t.givens.iter().zip(&t.test.givens) {
            let v = wh_codegen::rust::cap_var(&g.capability);
            let _ = writeln!(
                body,
                "    let {v} = {};",
                cx.fix(eg.emit(&ast_g.value, &Scope::default()))
            );
            scope.caps.push((g.capability.clone(), format!("&{v}")));
        }
        let mut items = Vec::new();
        for (j, e) in t.test.expects.iter().enumerate() {
            let label = format!("e{j}");
            let _ = writeln!(body, "    out.begin({j});\n    '{label}: {{");
            let slots = match &e.raises {
                Some(r) => {
                    let mut d = Vec::new();
                    let (err, variant) = model.resolve_error_ref(&t.module, r, &mut d);
                    let call = match &e.expr.kind {
                        ExprKind::Call { func, args } => eg.call(func, args, &scope, false),
                        _ => eg.emit(&e.expr, &scope),
                    };
                    let pat = cx.fix(eg.err_pattern(&err, &variant));
                    let _ = write!(
                        body,
                        "        let wh_v = {};\n        out.res(0, &wh_v);\n        out.pass(matches!(&wh_v, {pat}));\n",
                        cx.fix(call)
                    );
                    vec![SlotMeta {
                        label: "result".into(),
                        span: Some(SourceSpan::of(text, e.expr.span)),
                    }]
                }
                None => {
                    let (check, _, split) =
                        cx.check(&t.module, &e.expr, &scope, 0, &label, "        ");
                    body.push_str(&check);
                    match &e.expr.kind {
                        ExprKind::Compare { first, rest } if split => vec![
                            SlotMeta {
                                label: "left".into(),
                                span: Some(SourceSpan::of(text, first.span)),
                            },
                            SlotMeta {
                                label: "right".into(),
                                span: Some(SourceSpan::of(text, rest[0].1.span)),
                            },
                        ],
                        _ => vec![],
                    }
                }
            };
            body.push_str("    }\n");
            items.push(ItemMeta {
                node: node.clone(),
                kind: ItemKind::Expect,
                clause_index: j,
                file: file.path.clone(),
                span: SourceSpan::of(text, e.span),
                source: text.get(e.span.range()).unwrap_or("").trim().to_string(),
                slots,
            });
        }
        emit_probe(&mut code, node.clone(), node, items, body);
    }

    let code = format!(
        "//! Generated by Wild Horse (wh-live). Do not edit.\n#![allow(unused_imports, unused_variables, unused_mut, unused_parens, unused_labels, unreachable_code, non_snake_case, clippy::all)]\n\nuse crate::rt;\n#[allow(unused_imports)]\nuse proptest::strategy::Strategy as _;\n\n{code}pub static PROBES: &[(&str, fn(&mut rt::Out))] = &[\n{table}];\n"
    );
    Generated {
        code,
        probes,
        unprobed,
    }
}
