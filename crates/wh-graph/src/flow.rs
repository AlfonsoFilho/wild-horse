//! The flow graph: definitions, flows and their step instances, wired by data
//! flow, plus capabilities, adapters and the app that binds them.

use std::collections::BTreeMap;

use wh_model::{FlowInfo, Model, Node, NodeKind, SigInfo, StepInput, Ty};

use crate::{EdgeKind, GNode, GNodeKind, Graph, PortDir, PortRef, Row};

#[derive(Debug, Clone, Default)]
pub struct FlowOptions {
    /// Only show this module's definitions; referenced definitions from other
    /// modules appear as stub nodes. `None` shows the whole project.
    pub module: Option<String>,
    /// Draw `calls` edges from step instances to the definitions they call.
    pub hide_calls: bool,
}

pub fn signature_text(sig: &SigInfo) -> String {
    let params: Vec<String> = sig.params.iter().map(|p| format!("{}: {}", p.name, p.ty)).collect();
    let mut s = format!("({})", params.join(", "));
    if sig.ret != Ty::Unit {
        s.push_str(&format!(" -> {}", sig.ret));
    }
    if let Some(r) = &sig.raises {
        s.push_str(&format!(" raises {r}"));
    }
    s
}

pub fn flow_graph(model: &Model, opts: &FlowOptions) -> Graph {
    let mut b = Builder { model, opts, g: Graph::default(), ids: BTreeMap::new() };
    let in_scope = |n: &Node| opts.module.as_deref().is_none_or(|m| n.module == m);
    // definitions first, in document order, so steps can point at them
    let mut nodes: Vec<&Node> = model.nodes.values().filter(|n| in_scope(n)).collect();
    nodes.sort_by_key(|n| (n.file, n.span.start));
    for n in &nodes {
        match &n.kind {
            NodeKind::Def(d) => {
                let mut rows: Vec<Row> = d.sig.params.iter().map(|p| Row::port(PortDir::In, &p.name, &p.ty)).collect();
                rows.push(ret_row(&d.sig));
                let mut badges: Vec<String> = n.patterns.clone();
                if d.extern_path.is_some() {
                    badges.push("extern".into());
                } else {
                    badges.push("hole".into());
                }
                let i = b.def_node(n, GNodeKind::Def, rows, signature_text(&d.sig), badges);
                b.g.nodes[i].raises = d.sig.raises.as_ref().map(Ty::to_string);
            }
            NodeKind::Flow(f) => {
                let i = b.def_node(n, GNodeKind::Flow, vec![result_row(&f.sig)], signature_text(&f.sig), n.patterns.clone());
                b.g.nodes[i].raises = f.sig.raises.as_ref().map(Ty::to_string);
            }
            NodeKind::App(a) => {
                let rows = match &a.entry {
                    Some(e) => vec![result_row(&e.sig)],
                    None => vec![],
                };
                let sub = a.entry.as_ref().map(|e| format!("entry main{}", signature_text(&e.sig))).unwrap_or_default();
                b.def_node(n, GNodeKind::App, rows, sub, a.targets.clone());
            }
            NodeKind::Capability(c) => {
                let rows = c.methods.iter().map(|m| Row::text(&m.name, signature_text(&m.sig))).collect();
                b.def_node(n, GNodeKind::Capability, rows, String::new(), n.patterns.clone());
            }
            NodeKind::Adapter(a) => {
                let rows = a.fields.iter().map(|f| Row::text(&f.name, f.ty.to_string())).collect();
                let sub = a.implements.iter().map(|c| short(c)).collect::<Vec<_>>().join(", ");
                b.def_node(n, GNodeKind::Adapter, rows, format!("implements {sub}"), n.patterns.clone());
            }
            _ => {}
        }
    }
    for n in &nodes {
        match &n.kind {
            NodeKind::Flow(f) => b.flow_body(n, f),
            NodeKind::App(a) => {
                if let Some(e) = &a.entry {
                    b.flow_body(n, e);
                }
                for p in &a.provides {
                    if let Some(ad) = &p.adapter {
                        let from = b.ids[&n.id];
                        let to = b.stub(ad);
                        b.g.edge(PortRef { node: from, row: None }, PortRef { node: to, row: None }, EdgeKind::Provides).label =
                            Some(short(&p.capability).to_string());
                    }
                }
            }
            _ => {}
        }
    }
    for n in &nodes {
        let Some(&from) = b.ids.get(&n.id) else { continue };
        let uses: Vec<String> = match &n.kind {
            NodeKind::Def(d) => d.sig.uses.clone(),
            NodeKind::Flow(f) => f.sig.uses.clone(),
            _ => vec![],
        };
        for u in uses {
            let to = b.stub(&u);
            b.g.edge(PortRef { node: from, row: None }, PortRef { node: to, row: None }, EdgeKind::Uses);
        }
        if let NodeKind::Adapter(a) = &n.kind {
            for c in &a.implements {
                let to = b.stub(c);
                b.g.edge(PortRef { node: from, row: None }, PortRef { node: to, row: None }, EdgeKind::Implements);
            }
        }
    }
    crate::annotate(model, &mut b.g);
    b.g
}

fn short(id: &str) -> &str {
    id.rsplit('.').next().unwrap_or(id)
}

fn ret_row(sig: &SigInfo) -> Row {
    Row::port(PortDir::Out, "out", &sig.ret)
}

fn result_row(sig: &SigInfo) -> Row {
    Row::port(PortDir::In, "result", &sig.ret)
}

struct Builder<'a> {
    model: &'a Model,
    opts: &'a FlowOptions,
    g: Graph,
    ids: BTreeMap<String, usize>,
}

impl Builder<'_> {
    fn def_node(&mut self, n: &Node, kind: GNodeKind, rows: Vec<Row>, subtitle: String, badges: Vec<String>) -> usize {
        let cluster = self.g.cluster_for(self.model, n.file, n.section);
        let i = self.g.push(GNode {
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
        self.ids.insert(n.id.clone(), i);
        i
    }

    /// The node for a definition, adding an out-of-scope stub if needed.
    fn stub(&mut self, id: &str) -> usize {
        if let Some(&i) = self.ids.get(id) {
            return i;
        }
        let (kind, file, span, title) = match self.model.nodes.get(id) {
            Some(n) => (
                match &n.kind {
                    NodeKind::Flow(_) => GNodeKind::Flow,
                    NodeKind::Capability(_) => GNodeKind::Capability,
                    NodeKind::Adapter(_) => GNodeKind::Adapter,
                    NodeKind::App(_) => GNodeKind::App,
                    _ => GNodeKind::Def,
                },
                n.file,
                n.span,
                n.name.clone(),
            ),
            None => (GNodeKind::Def, 0, Default::default(), short(id).to_string()),
        };
        let i = self.g.push(GNode {
            id: id.to_string(),
            kind,
            title,
            subtitle: format!("from {}", id.rsplit_once('.').map_or("", |(m, _)| m)),
            rows: vec![],
            header_in: true,
            cluster: None,
            model_node: Some(id.to_string()),
            callee: None,
            file,
            span,
            badges: vec!["external".into()],
            raises: None,
            error: None,
        });
        self.ids.insert(id.to_string(), i);
        i
    }

    fn flow_body(&mut self, n: &Node, f: &FlowInfo) {
        let cluster = self.g.cluster_for(self.model, n.file, n.section);
        let mut params = BTreeMap::new();
        for p in &f.sig.params {
            let i = self.g.push(GNode {
                id: format!("{}#in:{}", n.id, p.name),
                kind: GNodeKind::Param,
                title: p.name.clone(),
                subtitle: format!("{} input", n.name),
                rows: vec![Row::port(PortDir::Out, &p.name, &p.ty)],
                header_in: false,
                cluster,
                model_node: Some(n.id.clone()),
                callee: None,
                file: n.file,
                span: n.span,
                badges: vec![],
                raises: None,
                error: None,
            });
            params.insert(p.name.clone(), i);
        }
        let mut steps = Vec::new();
        for (si, s) in f.steps.iter().enumerate() {
            let callee = s.callee.as_ref().and_then(|c| self.model.nodes.get(c));
            let callee_sig = callee.and_then(|c| c.sig());
            let mut rows: Vec<Row> = Vec::new();
            let n_in = s.inputs.len().max(callee_sig.map_or(0, |sig| sig.params.len()));
            for k in 0..n_in {
                rows.push(match callee_sig.and_then(|sig| sig.params.get(k)) {
                    Some(p) => Row::port(PortDir::In, &p.name, &p.ty),
                    None => Row::port(PortDir::In, format!("in{k}"), &Ty::Unknown),
                });
            }
            rows.push(Row::port(PortDir::Out, if s.bindings.is_empty() { "out".into() } else { s.bindings.join(", ") }, &s.output));
            let i = self.g.push(GNode {
                id: s.id.clone(),
                kind: GNodeKind::Step,
                title: s.label.clone(),
                subtitle: format!("step {} of {}", si + 1, n.name),
                rows,
                header_in: true,
                cluster,
                model_node: Some(n.id.clone()),
                callee: s.callee.clone(),
                file: n.file,
                span: s.span,
                badges: vec![],
                raises: s.raises.as_ref().map(Ty::to_string),
                error: None,
            });
            steps.push(i);
        }
        for (si, s) in f.steps.iter().enumerate() {
            let to_node = steps[si];
            for (k, input) in s.inputs.iter().enumerate() {
                let from = match input {
                    StepInput::Param(p) => params.get(p).map(|&i| PortRef { node: i, row: Some(0) }),
                    StepInput::Step(j) => steps.get(*j).map(|&i| PortRef { node: i, row: Some(self.g.nodes[i].rows.len() - 1) }),
                    StepInput::Other => None,
                };
                if let Some(from) = from {
                    self.g.edge(from, PortRef { node: to_node, row: Some(k) }, EdgeKind::Data).step = Some((n.id.clone(), si));
                }
            }
            if !self.opts.hide_calls {
                if let Some(c) = &s.callee {
                    let to = self.stub(c);
                    self.g.edge(PortRef { node: to_node, row: None }, PortRef { node: to, row: None }, EdgeKind::Calls);
                }
            }
        }
        self.unresolved_steps(n, f, cluster, &params, &steps);
        if let Some(r) = f.result.and_then(|r| steps.get(r).copied()) {
            let owner = self.ids[&n.id];
            let from = PortRef { node: r, row: Some(self.g.nodes[r].rows.len() - 1) };
            self.g.edge(from, PortRef { node: owner, row: Some(0) }, EdgeKind::Result);
        }
    }
}

impl Builder<'_> {
    /// Steps whose callee does not resolve are dropped by the model; show
    /// each as an error node in place, between the value it was fed and the
    /// step that lost its input.
    fn unresolved_steps(&mut self, n: &Node, f: &FlowInfo, cluster: Option<usize>, params: &BTreeMap<String, usize>, steps: &[usize]) {
        let mut found: Vec<(wh_syntax::Span, String)> = self
            .model
            .diagnostics
            .iter()
            .filter(|d| d.file == n.file && d.diag.span.start >= n.span.start && d.diag.span.end <= n.span.end)
            .filter_map(|d| Some((d.diag.span, d.diag.message.strip_prefix("unknown function `")?.strip_suffix('`')?.to_string())))
            .collect();
        found.sort_by_key(|(s, _)| s.start);
        let consumed = |j: usize| f.steps.iter().any(|s| s.inputs.iter().any(|i| *i == StepInput::Step(j)));
        let param_used = |p: &str| f.steps.iter().any(|s| s.inputs.iter().any(|i| matches!(i, StepInput::Param(q) if q == p)));
        let mut taken_succ = Vec::new();
        for (span, name) in found {
            let suggestion = did_you_mean(self.model, &n.module, &name);
            let hint = suggestion.as_ref().map(|s| format!(" — did you mean `{s}`?")).unwrap_or_default();
            // the rows carry the message, so the node is sized for it
            let mut rows = vec![Row::port(PortDir::In, "unknown function", &Ty::Unknown), Row::port(PortDir::Out, "", &Ty::Unknown)];
            rows[0].detail = String::new();
            rows[1].label = suggestion.map_or("no definition by that name".into(), |s| format!("did you mean `{s}`?"));
            rows[1].detail = String::new();
            let i = self.g.push(GNode {
                id: format!("{}#?{}@{}", n.id, name, span.start),
                kind: GNodeKind::Step,
                title: name.clone(),
                subtitle: "unresolved".into(),
                rows,
                header_in: true,
                cluster,
                model_node: Some(n.id.clone()),
                callee: None,
                file: n.file,
                span,
                badges: vec![],
                raises: None,
                error: Some(format!("unknown function `{name}`{hint}")),
            });
            // fed by the closest earlier step (or parameter) whose value nobody uses
            let pred = (0..f.steps.len()).rev().find(|&j| f.steps[j].span.end <= span.start && !consumed(j)).map(|j| (steps[j], self.g.nodes[steps[j]].rows.len() - 1));
            let pred = pred.or_else(|| f.sig.params.iter().find(|p| !param_used(&p.name)).and_then(|p| params.get(&p.name)).map(|&i| (i, 0)));
            if let Some((from, row)) = pred {
                self.g.edge(PortRef { node: from, row: Some(row) }, PortRef { node: i, row: Some(0) }, EdgeKind::Data);
            }
            // feeding the first later step that lost an input
            let succ = (0..f.steps.len()).find(|&j| {
                f.steps[j].span.start >= span.end && !taken_succ.contains(&j) && f.steps[j].inputs.iter().any(|x| *x == StepInput::Other)
            });
            if let Some(j) = succ {
                taken_succ.push(j);
                let k = f.steps[j].inputs.iter().position(|x| *x == StepInput::Other).unwrap_or(0);
                self.g.edge(PortRef { node: i, row: Some(1) }, PortRef { node: steps[j], row: Some(k) }, EdgeKind::Data);
            }
        }
    }
}

/// The closest function or flow name visible from `module`, for a name that does not resolve.
pub fn did_you_mean(model: &Model, module: &str, name: &str) -> Option<String> {
    let mut best: Option<(usize, String)> = None;
    for d in model.nodes.values().filter(|d| d.sig().is_some() && matches!(d.kind, NodeKind::Def(_) | NodeKind::Flow(_))) {
        let written = if d.module == module {
            d.name.clone()
        } else {
            match model.scopes.get(module).and_then(|s| s.module_aliases.iter().find(|(_, m)| **m == d.module)) {
                Some((alias, _)) => format!("{alias}.{}", d.name),
                None => continue,
            }
        };
        let dist = edit_distance(name, &written);
        if dist <= (name.chars().count() / 3).max(2) && best.as_ref().is_none_or(|(b, _)| dist < *b) {
            best = Some((dist, written));
        }
    }
    best.map(|(_, w)| w)
}

fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut cur = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            cur.push((prev[j] + usize::from(ca != *cb)).min(prev[j + 1] + 1).min(cur[j] + 1));
        }
        prev = cur;
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::signup;

    #[test]
    fn register_pipeline_is_a_chain() {
        let m = signup();
        let g = flow_graph(&m, &FlowOptions::default());
        let param = g.index_of("signup.register#in:form").expect("param node");
        let steps: Vec<usize> =
            g.nodes.iter().enumerate().filter(|(_, n)| n.kind == GNodeKind::Step && n.model_node.as_deref() == Some("signup.register")).map(|(i, _)| i).collect();
        let labels: Vec<&str> = steps.iter().map(|&i| g.nodes[i].title.as_str()).collect();
        assert_eq!(labels, ["validate", "normalize_form", "check_age", "save_user"]);
        // param → validate → normalize_form → check_age → save_user → register.result
        let data: Vec<(usize, usize)> = g.edges_of_kind(EdgeKind::Data).map(|e| (e.from.node, e.to.node)).collect();
        assert!(data.contains(&(param, steps[0])));
        for w in steps.windows(2) {
            assert!(data.contains(&(w[0], w[1])), "missing edge {:?}", w);
        }
        let flow = g.index_of("signup.register").unwrap();
        assert!(g.edges_of_kind(EdgeKind::Result).any(|e| e.from.node == steps[3] && e.to.node == flow));
        // step ids come from the model
        assert!(g.nodes[steps[0]].id.starts_with("signup.register#"));
        // the step's input port is typed by the callee's parameter
        assert_eq!(g.nodes[steps[0]].rows[0].detail, "SignupForm");
    }

    #[test]
    fn capabilities_adapters_and_clusters() {
        let m = signup();
        let g = flow_graph(&m, &FlowOptions::default());
        let save = g.index_of("signup.save_user").unwrap();
        let db = g.index_of("signup.Db").unwrap();
        let mem = g.index_of("signup.MemoryDb").unwrap();
        assert!(g.edges_of_kind(EdgeKind::Uses).any(|e| e.from.node == save && e.to.node == db));
        assert!(g.edges_of_kind(EdgeKind::Implements).any(|e| e.from.node == mem && e.to.node == db));
        let app = g.index_of("main.signup_cli").unwrap();
        assert!(g.edges_of_kind(EdgeKind::Provides).any(|e| e.from.node == app && e.to.node == mem));
        // clusters follow document sections
        let c = g.nodes[save].cluster.expect("save_user is in a section");
        assert_eq!(g.clusters[c].title, "Storage");
        let parent = g.clusters[c].parent.expect("nested under the module heading");
        assert_eq!(g.clusters[parent].title, "Signup");
    }

    #[test]
    fn module_scope_uses_stubs() {
        let m = signup();
        let g = flow_graph(&m, &FlowOptions { module: Some("main".into()), hide_calls: false });
        assert!(g.node("signup.validate").is_none());
        let reg = g.node("signup.register").expect("stub for the called flow");
        assert!(reg.badges.contains(&"external".to_string()));
        let entry_steps: Vec<&str> =
            g.nodes.iter().filter(|n| n.kind == GNodeKind::Step).map(|n| n.title.as_str()).collect();
        assert_eq!(entry_steps, ["parse_cli", "register", "report"]);
    }

    #[test]
    fn unresolved_step_is_an_error_node() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples/signup/src");
        let mut inputs = Vec::new();
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.to_string_lossy().ends_with(".wh.md") {
                let text = std::fs::read_to_string(&p).unwrap().replace("|> validate\n", "|> validatexx\n");
                inputs.push(wh_model::SourceInput { path: p.to_string_lossy().into_owned(), text });
            }
        }
        let m = Model::build(&inputs);
        let g = flow_graph(&m, &FlowOptions { module: Some("signup".into()), hide_calls: true });
        let bad = g.nodes.iter().position(|n| n.title == "validatexx").expect("error node for the unresolved step");
        let err = g.nodes[bad].error.as_deref().unwrap();
        assert!(err.contains("did you mean `validate`"), "{err}");
        // still in the chain: form → validatexx → normalize_form
        let param = g.index_of("signup.register#in:form").unwrap();
        let norm = g.nodes.iter().position(|n| n.title == "normalize_form" && n.kind == GNodeKind::Step).unwrap();
        let data: Vec<(usize, usize)> = g.edges_of_kind(EdgeKind::Data).map(|e| (e.from.node, e.to.node)).collect();
        assert!(data.contains(&(param, bad)), "{data:?}");
        assert!(data.contains(&(bad, norm)), "{data:?}");
    }

    #[test]
    fn ports_carry_type_families() {
        let m = signup();
        let g = flow_graph(&m, &FlowOptions::default());
        let save = g.node("signup.save_user").unwrap();
        assert!(save.raises.is_some(), "save_user raises");
        assert!(save.rows.iter().all(|r| r.ty.is_none() || !r.detail.contains(" ! ")));
        let param = g.node("signup.register#in:form").unwrap();
        assert_eq!(param.rows[0].family, crate::TypeFamily::Record);
    }
}
