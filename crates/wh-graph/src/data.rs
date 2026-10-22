//! The data-model graph: every data type of the project and how they relate,
//! ER/class-diagram style.

use std::collections::BTreeMap;

use wh_model::{Model, Node, NodeKind, Ty};
use wh_syntax::ast::{ItemKind, TypeArg, TypeExpr, TypeExprKind};

use crate::{Cardinality, EdgeKind, GNode, GNodeKind, Graph, PortDir, PortRef, Row};

#[derive(Debug, Clone, Default)]
pub struct DataOptions {
    /// Only this module's types (`None`: the whole project, the default).
    pub module: Option<String>,
    /// Add the defs, flows and capabilities that produce or consume each type.
    pub overlay: bool,
}

pub fn data_model_graph(model: &Model, opts: &DataOptions) -> Graph {
    let mut g = Graph::default();
    let mut ids: BTreeMap<String, usize> = BTreeMap::new();
    let mut types: Vec<&Node> = model
        .nodes
        .values()
        .filter(|n| n.kind.is_type() && opts.module.as_deref().is_none_or(|m| n.module == m))
        .collect();
    types.sort_by_key(|n| (n.file, n.span.start));

    for n in &types {
        let text = &model.files[n.file].text;
        let (kind, subtitle, rows) = match (&n.kind, &n.item.kind) {
            (NodeKind::Struct(s), ItemKind::Struct(ast)) => {
                let rows = s
                    .fields
                    .iter()
                    .zip(&ast.fields)
                    .map(|(f, af)| {
                        let mut label = String::new();
                        if f.is_key {
                            label.push_str("key ");
                        }
                        label.push_str(&f.name);
                        let mut r = Row::text(label, src(text, af.ty.span));
                        if f.has_default {
                            r.detail.push_str(" = …");
                        }
                        r.port = Some(PortDir::Out);
                        r.ty = Some(f.ty.clone());
                        r.field = Some(f.name.clone());
                        r
                    })
                    .collect::<Vec<_>>();
                let sub = if s.generics.is_empty() { String::new() } else { format!("[{}]", s.generics.join(", ")) };
                let mut rows = rows;
                if s.has_invariants {
                    for inv in &ast.invariants {
                        rows.push(Row::text("invariant", src(text, inv.span)));
                    }
                }
                (GNodeKind::Struct, sub, rows)
            }
            (NodeKind::Enum(e), ItemKind::Enum(ast)) => {
                let rows = ast
                    .variants
                    .iter()
                    .map(|v| {
                        let items: Vec<String> = v
                            .items
                            .iter()
                            .map(|it| {
                                let mut s = String::new();
                                if it.is_from {
                                    s.push_str("from ");
                                }
                                if let Some(nm) = &it.name {
                                    s.push_str(&format!("{}: ", nm.name));
                                }
                                s.push_str(src(text, it.ty.span));
                                s
                            })
                            .collect();
                        let mut r = Row::text(&v.name.name, if items.is_empty() { String::new() } else { format!("({})", items.join(", ")) });
                        if !items.is_empty() {
                            r.port = Some(PortDir::Out);
                        }
                        r
                    })
                    .collect();
                let sub = if e.generics.is_empty() { String::new() } else { format!("[{}]", e.generics.join(", ")) };
                (if n.is_error_type { GNodeKind::ErrorEnum } else { GNodeKind::Enum }, sub, rows)
            }
            (NodeKind::Refined { base, .. }, ItemKind::TypeDef(t)) => {
                let mut rows = vec![Row { port: Some(PortDir::Out), ..Row::port(PortDir::Out, "base", base) }];
                if let Some(p) = &t.refinement {
                    rows.push(Row::text("where", src(text, p.span)));
                }
                (GNodeKind::Refined, format!("= {}", src(text, t.base.span)), rows)
            }
            (NodeKind::Alias(ty), ItemKind::Alias(a)) => {
                (GNodeKind::Alias, format!("= {}", src(text, a.ty.span)), vec![Row::port(PortDir::Out, "=", ty)])
            }
            _ => continue,
        };
        let cluster = g.cluster_for(model, n.file, n.section);
        let mut badges: Vec<String> = n.derives.iter().map(|d| format!("derive {d}")).collect();
        badges.extend(n.patterns.iter().cloned());
        let i = g.push(GNode {
            id: n.id.clone(),
            kind,
            title: n.name.clone(),
            subtitle,
            rows,
            header_in: true,
            cluster,
            model_node: Some(n.id.clone()),
            callee: None,
            file: n.file,
            span: n.span,
            badges,
            raises: None,
            error: None,
        });
        ids.insert(n.id.clone(), i);
    }

    // relationships, from the written type expressions (so aliases are kept)
    for n in &types {
        let Some(&from) = ids.get(&n.id) else { continue };
        match &n.item.kind {
            ItemKind::Struct(s) => {
                for (ri, f) in s.fields.iter().enumerate() {
                    let mut targets = Vec::new();
                    walk(model, &n.module, &f.ty, Cardinality::One, &mut targets);
                    for (target, card) in targets {
                        let Some(&to) = ids.get(&target) else { continue };
                        // a field typed with another struct's key type is a foreign key
                        let (kind, to) = match references(model, &target) {
                            Some(owner) if !f.is_key => match ids.get(&owner) {
                                Some(&o) => (EdgeKind::References, o),
                                None => (EdgeKind::Has, to),
                            },
                            _ => (EdgeKind::Has, to),
                        };
                        let e = g.edge(PortRef { node: from, row: Some(ri) }, PortRef { node: to, row: None }, kind);
                        e.label = Some(f.name.name.clone());
                        e.cardinality = Some(card);
                        e.field = Some((n.id.clone(), f.name.name.clone()));
                    }
                }
            }
            ItemKind::Enum(en) => {
                for (ri, v) in en.variants.iter().enumerate() {
                    for it in &v.items {
                        let mut targets = Vec::new();
                        walk(model, &n.module, &it.ty, Cardinality::One, &mut targets);
                        for (target, card) in targets {
                            let Some(&to) = ids.get(&target) else { continue };
                            let kind = if it.is_from { EdgeKind::ConvertsFrom } else { EdgeKind::VariantOf };
                            let e = g.edge(PortRef { node: from, row: Some(ri) }, PortRef { node: to, row: None }, kind);
                            e.label = Some(v.name.name.clone());
                            if card != Cardinality::One {
                                e.cardinality = Some(card);
                            }
                        }
                    }
                }
            }
            ItemKind::TypeDef(t) => {
                let mut targets = Vec::new();
                walk(model, &n.module, &t.base, Cardinality::One, &mut targets);
                for (target, _) in targets {
                    if let Some(&to) = ids.get(&target) {
                        g.edge(PortRef { node: from, row: Some(0) }, PortRef { node: to, row: None }, EdgeKind::Refines);
                    }
                }
            }
            ItemKind::Alias(a) => {
                let mut targets = Vec::new();
                walk(model, &n.module, &a.ty, Cardinality::One, &mut targets);
                for (target, _) in targets {
                    if let Some(&to) = ids.get(&target) {
                        g.edge(PortRef { node: from, row: Some(0) }, PortRef { node: to, row: None }, EdgeKind::AliasOf);
                    }
                }
            }
            _ => {}
        }
    }

    if opts.overlay {
        overlay(model, &mut g, &ids);
    }
    crate::annotate(model, &mut g);
    g
}

/// Which struct a type identifies, when `ty_id` names a struct's `key` type
/// (`UserId` → `User`).
fn references(model: &Model, ty_id: &str) -> Option<String> {
    let n = model.nodes.get(ty_id)?;
    let ty = match &n.kind {
        NodeKind::Alias(t) => t.clone(),
        _ => Ty::Named(ty_id.to_string(), vec![]),
    };
    let owner = model.key_owner(&ty)?;
    // aliases expand, so check the owner really declared its key with this name
    if let NodeKind::Alias(_) = n.kind {
        let ItemKind::Struct(s) = &owner.item.kind else { return None };
        let named = s.fields.iter().filter(|f| f.is_key).any(|f| type_names(&f.ty).iter().any(|p| p == &n.name));
        if !named {
            return None;
        }
    }
    Some(owner.id.clone())
}

fn type_names(t: &TypeExpr) -> Vec<String> {
    match &t.kind {
        TypeExprKind::Named { path, .. } => vec![path.last().unwrap().name.clone()],
        TypeExprKind::Optional(i) => type_names(i),
        _ => vec![],
    }
}

fn src(text: &str, span: wh_syntax::Span) -> &str {
    text.get(span.range()).unwrap_or("").trim()
}

/// Collect the user types a type expression mentions, with the cardinality
/// implied by the containers around them.
fn walk(model: &Model, module: &str, t: &TypeExpr, card: Cardinality, out: &mut Vec<(String, Cardinality)>) {
    match &t.kind {
        TypeExprKind::Optional(inner) => {
            let c = if card == Cardinality::One { Cardinality::ZeroOrOne } else { card };
            walk(model, module, inner, c, out);
        }
        TypeExprKind::Fn { .. } | TypeExprKind::Error => {}
        TypeExprKind::Named { path, args } => {
            let tys: Vec<&TypeExpr> = args.iter().filter_map(|a| if let TypeArg::Type(t) = a { Some(t) } else { None }).collect();
            let name = path.last().unwrap().name.as_str();
            if path.len() == 1 {
                let inner = match name {
                    "list" | "set" | "sorted_set" | "Stream" => Some(Cardinality::Many),
                    "tuple" => Some(card.clone()),
                    "dict" | "sorted_dict" => {
                        if let [k, v] = tys.as_slice() {
                            walk(model, module, k, Cardinality::Many, out);
                            let key = model.files.iter().find(|f| f.module == module).map_or_else(String::new, |f| src(&f.text, k.span).to_string());
                            walk(model, module, v, Cardinality::Keyed(key), out);
                        }
                        return;
                    }
                    "array" => {
                        let n = args.iter().find_map(|a| match a {
                            TypeArg::Const(e) => match &e.kind {
                                wh_syntax::ast::ExprKind::Int(v) => v.replace('_', "").parse::<u64>().ok(),
                                _ => None,
                            },
                            _ => None,
                        });
                        Some(Cardinality::Exactly(n.unwrap_or(0)))
                    }
                    _ => None,
                };
                if let Some(c) = inner {
                    for a in tys {
                        walk(model, module, a, c.clone(), out);
                    }
                    return;
                }
            }
            let full = path.iter().map(|i| i.name.as_str()).collect::<Vec<_>>().join(".");
            if let Some(id) = model.resolve_path(module, &full) {
                // `NonEmpty[T]`-style refined lists hold 1..* of their argument
                let refined_list = matches!(&model.nodes[&id].kind, NodeKind::Refined { base: Ty::List(_), .. });
                for a in tys {
                    walk(model, module, a, if refined_list { Cardinality::OneOrMore } else { card.clone() }, out);
                }
                out.push((id, card));
            }
        }
    }
}

/// Defs, flows and capability methods that produce (return/raise) or consume
/// (take as parameter) each type.
fn overlay(model: &Model, g: &mut Graph, ids: &BTreeMap<String, usize>) {
    let mut users: Vec<&Node> = model
        .nodes
        .values()
        .filter(|n| matches!(n.kind, NodeKind::Def(_) | NodeKind::Flow(_) | NodeKind::Capability(_)))
        .collect();
    users.sort_by_key(|n| (n.file, n.span.start));
    for n in users {
        let sigs: Vec<&wh_model::SigInfo> = match &n.kind {
            NodeKind::Def(d) => vec![&d.sig],
            NodeKind::Flow(f) => vec![&f.sig],
            NodeKind::Capability(c) => c.methods.iter().map(|m| &m.sig).collect(),
            _ => continue,
        };
        let mut consumes = Vec::new();
        let mut produces = Vec::new();
        for s in &sigs {
            for p in &s.params {
                p.ty.walk_named(&mut |id| consumes.push(id.to_string()));
            }
            s.ret.walk_named(&mut |id| produces.push(id.to_string()));
            if let Some(r) = &s.raises {
                r.walk_named(&mut |id| produces.push(id.to_string()));
            }
        }
        consumes.retain(|id| ids.contains_key(id));
        produces.retain(|id| ids.contains_key(id));
        consumes.dedup();
        produces.dedup();
        if consumes.is_empty() && produces.is_empty() {
            continue;
        }
        let kind = match n.kind {
            NodeKind::Flow(_) => GNodeKind::Flow,
            NodeKind::Capability(_) => GNodeKind::Capability,
            _ => GNodeKind::Def,
        };
        let cluster = g.cluster_for(model, n.file, n.section);
        let i = g.push(GNode {
            id: n.id.clone(),
            kind,
            title: n.name.clone(),
            subtitle: n.kind.label().to_string(),
            rows: vec![],
            header_in: true,
            cluster,
            model_node: Some(n.id.clone()),
            callee: None,
            file: n.file,
            span: n.span,
            badges: vec![],
            raises: None,
            error: None,
        });
        for t in consumes {
            g.edge(PortRef { node: ids[&t], row: None }, PortRef { node: i, row: None }, EdgeKind::Consumes);
        }
        for t in produces {
            g.edge(PortRef { node: i, row: None }, PortRef { node: ids[&t], row: None }, EdgeKind::Produces);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::signup;

    fn edge<'g>(g: &'g Graph, from: &str, to: &str, kind: EdgeKind) -> Option<&'g crate::GEdge> {
        let f = g.index_of(from)?;
        let t = g.index_of(to)?;
        g.edges.iter().find(|e| e.from.node == f && e.to.node == t && e.kind == kind)
    }

    #[test]
    fn signup_types_and_relationships() {
        let m = signup();
        let g = data_model_graph(&m, &DataOptions::default());
        for id in ["signup.Email", "signup.Age", "signup.UserId", "signup.User", "signup.SignupForm", "signup.RegisterError"] {
            assert!(g.node(id).is_some(), "missing {id}");
        }
        assert!(g.node("signup.validate").is_none(), "no defs without the overlay");
        assert_eq!(g.node("signup.RegisterError").unwrap().kind, GNodeKind::ErrorEnum);
        assert_eq!(g.node("signup.Email").unwrap().kind, GNodeKind::Refined);
        assert_eq!(g.node("signup.UserId").unwrap().kind, GNodeKind::Alias);

        let e = edge(&g, "signup.User", "signup.Email", EdgeKind::Has).expect("User has Email");
        assert_eq!(e.cardinality, Some(Cardinality::One));
        assert_eq!(e.field, Some(("signup.User".into(), "email".into())));
        assert!(edge(&g, "signup.User", "signup.Age", EdgeKind::Has).is_some());

        // `guardian: UserId | None` is an optional foreign key to User
        let r = edge(&g, "signup.User", "signup.User", EdgeKind::References).expect("guardian references User");
        assert_eq!(r.cardinality, Some(Cardinality::ZeroOrOne));
        assert_eq!(r.label.as_deref(), Some("guardian"));
        // the key field itself is not a reference
        assert!(!g.edges.iter().any(|e| e.field == Some(("signup.User".into(), "id".into())) && e.kind == EdgeKind::References));
        assert!(edge(&g, "signup.SignupForm", "signup.User", EdgeKind::References).is_some());

        assert!(edge(&g, "signup.RegisterError", "signup.ValidationError", EdgeKind::ConvertsFrom).is_some());
        assert!(edge(&g, "signup.RegisterError", "signup.DbError", EdgeKind::ConvertsFrom).is_some());
        assert!(edge(&g, "signup.DbError", "signup.Email", EdgeKind::VariantOf).is_some());
        assert!(edge(&g, "main.AppError", "signup.RegisterError", EdgeKind::ConvertsFrom).is_some());
    }

    #[test]
    fn cardinalities_from_field_types() {
        let src = "---\nmodule: m\n---\n# M\n\n```wh\nstruct A:\n    x: int\n\nstruct B:\n    one: A\n    opt: A | None\n    many: list[A]\n    keyed: dict[str, A]\n    fixed: array[A, 3]\n    set_of: set[A]\n```\n";
        let m = wh_model::Model::build(&[wh_model::SourceInput { path: "m.wh.md".into(), text: src.into() }]);
        let g = data_model_graph(&m, &DataOptions::default());
        let card = |f: &str| g.edges.iter().find(|e| e.label.as_deref() == Some(f)).and_then(|e| e.cardinality.clone()).unwrap();
        assert_eq!(card("one"), Cardinality::One);
        assert_eq!(card("opt"), Cardinality::ZeroOrOne);
        assert_eq!(card("many"), Cardinality::Many);
        assert_eq!(card("keyed"), Cardinality::Keyed("str".into()));
        assert_eq!(card("fixed"), Cardinality::Exactly(3));
        assert_eq!(card("set_of"), Cardinality::Many);
    }

    #[test]
    fn overlay_links_defs() {
        let m = signup();
        let g = data_model_graph(&m, &DataOptions { module: None, overlay: true });
        assert!(edge(&g, "signup.SignupForm", "signup.validate", EdgeKind::Consumes).is_some());
        assert!(edge(&g, "signup.validate", "signup.ValidForm", EdgeKind::Produces).is_some());
        assert!(edge(&g, "signup.validate", "signup.ValidationError", EdgeKind::Produces).is_some());
    }
}
