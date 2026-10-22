//! Synthesis keys: a stable hash of everything an implementation depends on.
//!
//! The key covers the node's own definition (signature and clauses, without
//! docstrings, comments or formatting) plus the definitions of every type and
//! capability its signature mentions, transitively. Prose is deliberately
//! excluded: a prose-only edit makes a hole *soft-stale*, never stale.

use std::collections::BTreeSet;

use wh_syntax::lexer::{TokenKind, lex};

use crate::model::{Model, NodeKind};
use crate::ty::{NodeId, Ty};

/// Source of a node with comments, docstrings and whitespace normalized away.
pub fn normalized_source(model: &Model, id: &str) -> String {
    let node = &model.nodes[id];
    let text = &model.files[node.file].text[node.span.range()];
    let (toks, _) = lex(text, 0);
    let mut out = String::new();
    for t in toks {
        match t.kind {
            TokenKind::Whitespace | TokenKind::Comment | TokenKind::TripleStr => {}
            TokenKind::Newline => out.push('\n'),
            _ => {
                out.push_str(&text[t.span.range()]);
                out.push(' ');
            }
        }
    }
    out.lines().filter(|l| !l.trim().is_empty()).collect::<Vec<_>>().join("\n")
}

/// Every type or capability node a node depends on, transitively.
pub fn dependencies(model: &Model, id: &str) -> BTreeSet<NodeId> {
    let mut seen = BTreeSet::new();
    let mut stack: Vec<Ty> = Vec::new();
    let mut caps: Vec<NodeId> = Vec::new();
    if let Some(node) = model.nodes.get(id) {
        match &node.kind {
            NodeKind::Def(d) => {
                stack.extend(d.sig.params.iter().map(|p| p.ty.clone()));
                stack.push(d.sig.ret.clone());
                stack.extend(d.sig.raises.clone());
                caps.extend(d.sig.uses.iter().cloned());
            }
            NodeKind::Adapter(a) => {
                caps.extend(a.implements.iter().cloned());
                stack.extend(a.fields.iter().map(|f| f.ty.clone()));
            }
            _ => {}
        }
    }
    for c in caps {
        if seen.insert(c.clone()) {
            if let Some(NodeKind::Capability(ci)) = model.nodes.get(&c).map(|n| &n.kind) {
                for m in &ci.methods {
                    stack.extend(m.sig.params.iter().map(|p| p.ty.clone()));
                    stack.push(m.sig.ret.clone());
                    stack.extend(m.sig.raises.clone());
                }
            }
        }
    }
    while let Some(t) = stack.pop() {
        let mut named = Vec::new();
        t.walk_named(&mut |n| named.push(n.to_string()));
        for n in named {
            if !seen.insert(n.clone()) {
                continue;
            }
            let Some(node) = model.nodes.get(&n) else { continue };
            match &node.kind {
                NodeKind::Struct(s) => stack.extend(s.fields.iter().map(|f| f.ty.clone())),
                NodeKind::Enum(e) => stack.extend(e.variants.iter().flat_map(|v| v.items.iter().map(|i| i.ty.clone()))),
                NodeKind::Refined { base, .. } => stack.push(base.clone()),
                NodeKind::Machine(m) => {
                    stack.extend(m.states.variants.iter().flat_map(|v| v.items.iter().map(|i| i.ty.clone())));
                    stack.push(m.event.clone());
                    stack.push(m.effect.clone());
                }
                _ => {}
            }
        }
    }
    seen.remove(id);
    seen
}

/// Hex digest identifying the spec a hole implementation was synthesized for.
pub fn spec_key(model: &Model, id: &str, salt: &str) -> String {
    let mut h = blake3::Hasher::new();
    h.update(salt.as_bytes());
    h.update(b"\0");
    h.update(normalized_source(model, id).as_bytes());
    for dep in dependencies(model, id) {
        if model.nodes.contains_key(&dep) {
            h.update(b"\0");
            h.update(dep.as_bytes());
            h.update(b"=");
            h.update(normalized_source(model, &dep).as_bytes());
        }
    }
    h.finalize().to_hex()[..16].to_string()
}
