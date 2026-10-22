//! State machines (spec §8): state inference, transition typing, totality
//! (every `(state, event)` pair is handled or `ignore`d) and reachability.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use wh_syntax::Span;
use wh_syntax::ast::{self, Expr, ExprKind};

use crate::check::{Env, Typer, compatible};
use crate::model::*;
use crate::ty::Ty;

/// A state or event pattern: `Name`, `Name(a, b)` or `_`.
struct Pat {
    /// `None` for `_`
    name: Option<String>,
    binds: Vec<String>,
    span: Span,
}

fn pattern(e: &Expr, what: &str, diags: &mut Vec<(String, Span)>) -> Option<Pat> {
    match &e.kind {
        ExprKind::Name(n) if n == "_" => Some(Pat { name: None, binds: vec![], span: e.span }),
        ExprKind::Name(n) => Some(Pat { name: Some(n.clone()), binds: vec![], span: e.span }),
        ExprKind::Call { func, args } => {
            let ExprKind::Name(n) = &func.kind else {
                diags.push((format!("a {what} is a name, optionally with bound names: `Locked(code)`"), e.span));
                return None;
            };
            let mut binds = Vec::new();
            for a in args {
                match &a.value.kind {
                    ExprKind::Name(b) if a.name.is_none() => binds.push(b.clone()),
                    _ => {
                        diags.push((format!("a {what} binds plain names, e.g. `{n}(code)`"), a.value.span));
                        binds.push("_".into());
                    }
                }
            }
            Some(Pat { name: Some(n.clone()), binds, span: e.span })
        }
        _ => {
            diags.push((format!("a {what} is a name, optionally with bound names: `Locked(code)`"), e.span));
            None
        }
    }
}

/// A target state: `Name` or `Name(value, …)`.
fn target<'a>(e: &'a Expr, diags: &mut Vec<(String, Span)>) -> Option<(String, Vec<&'a Expr>)> {
    match &e.kind {
        ExprKind::Name(n) if n != "_" => Some((n.clone(), vec![])),
        ExprKind::Call { func, args } => match &func.kind {
            ExprKind::Name(n) => Some((n.clone(), args.iter().map(|a| &a.value).collect())),
            _ => {
                diags.push(("a target state is a state name, optionally with values: `Locked(code)`".into(), e.span));
                None
            }
        },
        _ => {
            diags.push(("a target state is a state name, optionally with values: `Locked(code)`".into(), e.span));
            None
        }
    }
}

struct State {
    tys: Vec<Ty>,
    span: Span,
}

/// Infer and check a machine. Returns its info and the diagnostics.
pub fn analyze_machine(model: &Model, node: &Node, m: &ast::Machine) -> (MachineInfo, Vec<(String, Span)>) {
    let module = node.module.as_str();
    let mut d: Vec<(String, Span)> = Vec::new();
    let event_ty = model.resolve_ty(module, &m.param.ty, &[], &mut d);
    let event_enum = match &event_ty {
        Ty::Named(id, _) => match &model.nodes[id].kind {
            NodeKind::Enum(e) => Some(e.clone()),
            _ => None,
        },
        _ => None,
    };
    if event_enum.is_none() && !event_ty.is_unknown() {
        d.push((format!("the event of a machine must be an enum, found `{event_ty}`"), m.param.ty.span));
    }
    let effect = m.ret.as_ref().map_or(Ty::Unit, |t| model.resolve_ty(module, t, &[], &mut d));
    let event_variant = |name: &str| event_enum.as_ref().and_then(|e| e.variants.iter().find(|v| v.name == name));

    // ---- states, in order of first appearance
    let mut order: Vec<String> = Vec::new();
    let mut states: BTreeMap<String, State> = BTreeMap::new();
    let mut add = |name: &str, arity: usize, span: Span, d: &mut Vec<(String, Span)>| match states.get(name) {
        Some(s) if s.tys.len() != arity => {
            d.push((format!("state `{name}` has {} item(s) elsewhere, but {arity} here", s.tys.len()), span));
        }
        Some(_) => {}
        None => {
            if !is_pascal(name) {
                d.push((format!("state names are PascalCase: `{name}`"), span));
            }
            order.push(name.to_string());
            states.insert(name.to_string(), State { tys: vec![Ty::Unknown; arity], span });
        }
    };
    struct T<'a> {
        from: Pat,
        event: Pat,
        to: (String, Vec<&'a Expr>),
        ast: &'a ast::Transition,
    }
    let mut ts: Vec<T> = Vec::new();
    for t in &m.transitions {
        let from = pattern(&t.from, "source state", &mut d);
        let event = pattern(&t.event, "event pattern", &mut d);
        let to = target(&t.to, &mut d);
        let (Some(from), Some(event), Some(to)) = (from, event, to) else { continue };
        if let Some(n) = &from.name {
            add(n, from.binds.len(), from.span, &mut d);
        }
        add(&to.0, to.1.len(), t.to.span, &mut d);
        match &event.name {
            Some(n) => match event_variant(n) {
                Some(v) if v.items.len() != event.binds.len() && !event.binds.is_empty() => d.push((
                    format!("event `{n}` has {} item(s), the pattern binds {}", v.items.len(), event.binds.len()),
                    event.span,
                )),
                Some(_) => {}
                None if event_enum.is_some() => d.push((format!("`{n}` is not an event of `{event_ty}`"), event.span)),
                None => {}
            },
            None if !event.binds.is_empty() => d.push(("`_` matches any event and binds nothing".into(), event.span)),
            None => {}
        }
        let mut seen = BTreeSet::new();
        for b in from.binds.iter().chain(&event.binds) {
            if b != "_" && !seen.insert(b.clone()) {
                d.push((format!("`{b}` is bound twice in this transition"), t.span));
            }
        }
        ts.push(T { from, event, to, ast: t });
    }
    let initial = match &m.initial {
        Some(e) => target(e, &mut d).map(|(n, args)| {
            add(&n, args.len(), e.span, &mut d);
            (n, args, e.span)
        }),
        None => {
            d.push(("a machine needs an `initial` state: `initial Closed`".into(), node.name_span));
            None
        }
    };

    // ---- infer payload types: a state's items get the types of the values
    // that transitions (or `initial`) build it from
    let env_for = |t: &T, states: &BTreeMap<String, State>| -> Env {
        let mut env = Env::default();
        if let Some(st) = t.from.name.as_ref().and_then(|n| states.get(n)) {
            for (b, ty) in t.from.binds.iter().zip(&st.tys) {
                env = env.with(b, ty.clone());
            }
        }
        if let Some(v) = t.event.name.as_ref().and_then(|n| event_variant(n)) {
            for (b, item) in t.event.binds.iter().zip(&v.items) {
                env = env.with(b, item.ty.clone());
            }
        }
        env
    };
    for _ in 0..=order.len() + 1 {
        let mut changed = false;
        let mut builds: Vec<(&str, Vec<&Expr>, Env)> =
            ts.iter().map(|t| (t.to.0.as_str(), t.to.1.clone(), env_for(t, &states))).collect();
        if let Some((n, args, _)) = &initial {
            builds.push((n.as_str(), args.clone(), Env::default()));
        }
        for (name, args, env) in builds {
            for (i, a) in args.iter().enumerate() {
                if states.get(name).and_then(|s| s.tys.get(i)).is_some_and(|t| t.is_unknown()) {
                    let ty = Typer::new(model, module).ty(a, &env);
                    if !ty.is_unknown() && !matches!(&ty, Ty::Option(inner) if inner.is_unknown()) {
                        states.get_mut(name).unwrap().tys[i] = ty;
                        changed = true;
                    }
                }
            }
        }
        if !changed {
            break;
        }
    }
    for name in &order {
        let st = &states[name];
        for (i, t) in st.tys.iter().enumerate() {
            if t.is_unknown() {
                d.push((
                    format!("cannot infer the type of item {} of state `{name}`; build it from an event's payload or a typed value", i + 1),
                    st.span,
                ));
            }
        }
    }

    // ---- type guards, target values and effects
    let mut typer = Typer::new(model, module);
    for t in &ts {
        let env = env_for(t, &states);
        if let Some(g) = &t.ast.guard {
            typer.expect_bool(g, &env, "a transition guard");
        }
        if let Some(st) = states.get(&t.to.0) {
            for (a, want) in t.to.1.iter().zip(&st.tys) {
                let got = typer.ty(a, &env);
                if !compatible(model, want, &got) {
                    typer.diags.push((format!("state `{}` holds `{want}`, found `{got}`", t.to.0), a.span));
                }
            }
        }
        if let Some(e) = &t.ast.emit {
            if m.ret.is_none() {
                typer.diags.push((format!("machine `{}` emits nothing; declare an effect type: `-> Effect`", node.name), e.span));
            } else {
                let got = typer.ty(e, &env);
                if !compatible(model, &effect, &got) {
                    typer.diags.push((format!("a transition emits `{effect}`, found `{got}`"), e.span));
                }
            }
        }
    }
    if let Some((n, args, _)) = &initial {
        if let Some(st) = states.get(n) {
            for (a, want) in args.iter().zip(&st.tys) {
                let got = typer.ty(a, &Env::default());
                if !compatible(model, want, &got) {
                    typer.diags.push((format!("state `{n}` holds `{want}`, found `{got}`"), a.span));
                }
            }
        }
    }
    for (id, span) in &typer.impure_calls {
        typer.diags.push((format!("guards and effects must be pure, but `{id}` uses capabilities"), *span));
    }
    d.extend(typer.diags);

    // ---- ignore rules
    let mut ignores = Vec::new();
    for ig in &m.ignores {
        let (Some(s), Some(e)) = (pattern(&ig.state, "state", &mut d), pattern(&ig.event, "event", &mut d)) else { continue };
        if let Some(n) = &s.name {
            if !states.contains_key(n) {
                d.push((format!("unknown state `{n}`"), s.span));
            }
        }
        if let Some(n) = &e.name {
            if event_enum.is_some() && event_variant(n).is_none() {
                d.push((format!("`{n}` is not an event of `{event_ty}`"), e.span));
            }
        }
        ignores.push(IgnoreInfo { state: s.name, event: e.name, span: ig.span });
    }

    // ---- transition ids
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    let transitions: Vec<TransitionInfo> = ts
        .iter()
        .map(|t| {
            let pair = format!("{}.{}", t.from.name.as_deref().unwrap_or("_"), t.event.name.as_deref().unwrap_or("_"));
            let k = counts.entry(pair.clone()).or_insert(0);
            let id = format!("{}#{pair}[{k}]", node.id);
            *k += 1;
            TransitionInfo {
                id,
                from: t.from.name.clone(),
                from_binds: t.from.binds.clone(),
                event: t.event.name.clone(),
                event_binds: t.event.binds.clone(),
                to: t.to.0.clone(),
                has_guard: t.ast.guard.is_some(),
                has_emit: t.ast.emit.is_some(),
                span: t.ast.span,
            }
        })
        .collect();

    // ---- reachability: shortest witness paths, unguarded transitions first
    let mut paths: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    if let Some((init, _, _)) = &initial {
        let mut best: BTreeMap<String, (usize, Vec<usize>)> = BTreeMap::new();
        best.insert(init.clone(), (0, vec![]));
        let mut queue: VecDeque<String> = VecDeque::from([init.clone()]);
        while let Some(s) = queue.pop_front() {
            let (guards, path) = best[&s].clone();
            for (i, t) in transitions.iter().enumerate() {
                if t.from.as_ref().is_some_and(|f| f != &s) {
                    continue;
                }
                let cand = (guards + usize::from(t.has_guard), path.len() + 1);
                let better = best.get(&t.to).is_none_or(|(g, p)| cand < (*g, p.len()));
                if better {
                    let mut p = path.clone();
                    p.push(i);
                    best.insert(t.to.clone(), (cand.0, p));
                    queue.push_back(t.to.clone());
                }
            }
        }
        paths = best.into_iter().map(|(k, (_, p))| (k, p)).collect();
        for name in &order {
            if !paths.contains_key(name) {
                d.push((format!("state `{name}` is unreachable from the initial state `{init}`"), states[name].span));
            }
        }
    }

    // ---- totality: every reachable (state, event) pair is handled or ignored
    if let Some(en) = &event_enum {
        for name in order.iter().filter(|n| paths.contains_key(*n) || initial.is_none()) {
            let covers = |s: &Option<String>, e: &Option<String>, ev: &str| {
                s.as_ref().is_none_or(|s| s == name) && e.as_ref().is_none_or(|e| e == ev)
            };
            let missing: Vec<&str> = en
                .variants
                .iter()
                .map(|v| v.name.as_str())
                .filter(|ev| {
                    !transitions.iter().any(|t| covers(&t.from, &t.event, ev))
                        && !ignores.iter().any(|i| covers(&i.state, &i.event, ev))
                })
                .collect();
            if !missing.is_empty() {
                d.push((
                    format!(
                        "state `{name}` does not handle {}: {}; add a transition or `ignore {name} on {}`",
                        if missing.len() == 1 { "event" } else { "events" },
                        missing.join(", "),
                        missing[0]
                    ),
                    states[name].span,
                ));
            }
        }
    }

    let variants = order
        .iter()
        .map(|n| VariantInfo {
            name: n.clone(),
            items: states[n].tys.iter().map(|t| VariantItemInfo { name: None, ty: t.clone(), is_from: false }).collect(),
        })
        .collect();
    let info = MachineInfo {
        event_param: m.param.name.name.clone(),
        event: event_ty,
        effect,
        states: EnumInfo { generics: vec![], variants },
        initial: initial.map(|(n, _, _)| n),
        transitions,
        ignores,
        paths,
    };
    (info, d)
}
