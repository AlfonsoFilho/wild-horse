//! A heuristic check that an implementation does not put sensitive values
//! into traces: `tracing` macros must not format values whose type is
//! `@sensitive` or is the type of a `sensitive` field, `sensitive`
//! parameters, or `sensitive` fields read through `.field`.
//!
//! It is a text-level check, not a type checker: values are tracked from
//! the parameters through `for` loops, closures and `let` bindings; taking
//! `.len()`, `.is_empty()`, `.count()` or `.capacity()` of a value is fine.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use wh_model::{Model, NodeKind, Ty};

/// One sensitive value formatted into a trace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Leak {
    /// 1-based line in the hole file.
    pub line: usize,
    /// `trace!`, `debug!`, …
    pub macro_name: String,
    /// The offending expression (`email`, `form.email`).
    pub value: String,
    pub why: String,
}

impl Leak {
    pub fn message(&self) -> String {
        format!("line {}: `{}` records `{}`, {}", self.line, self.macro_name, self.value, self.why)
    }
}

const MACROS: &[&str] = &["trace", "debug", "info", "warn", "error", "event", "trace_span", "debug_span", "info_span", "warn_span", "error_span", "span"];
const SIZE_ACCESSORS: &[&str] = &[".len()", ".is_empty()", ".count()", ".capacity()"];

/// Named types whose values are sensitive: `@sensitive` types and the
/// (element) types of `sensitive` fields.
pub fn sensitive_types(model: &Model) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for n in model.nodes.values() {
        if n.is_sensitive {
            out.insert(n.id.clone());
        }
        let fields = match &n.kind {
            NodeKind::Struct(s) => &s.fields,
            NodeKind::Adapter(a) => &a.fields,
            _ => continue,
        };
        for f in fields.iter().filter(|f| f.is_sensitive) {
            f.ty.walk_named(&mut |id| {
                if !model.nodes.get(id).is_some_and(|n| matches!(n.kind, NodeKind::Struct(_) | NodeKind::Enum(_))) {
                    // structs and enums redact their own sensitive fields in `Debug`
                    out.insert(id.to_string());
                }
            });
        }
    }
    out
}

/// Names of `sensitive` fields (read as `.name`).
pub fn sensitive_fields(model: &Model) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for n in model.nodes.values() {
        let fields = match &n.kind {
            NodeKind::Struct(s) => &s.fields,
            NodeKind::Adapter(a) => &a.fields,
            _ => continue,
        };
        out.extend(fields.iter().filter(|f| f.is_sensitive).map(|f| f.name.clone()));
    }
    out
}

fn contains_sensitive(t: &Ty, types: &BTreeSet<String>) -> Option<String> {
    let mut hit = None;
    t.walk_named(&mut |id| {
        if hit.is_none() && types.contains(id) {
            hit = Some(id.to_string());
        }
    });
    hit
}

/// Blank out comments and string contents (keeping offsets), and return the
/// string literals' ranges.
fn mask(code: &str) -> (Vec<u8>, Vec<(usize, usize)>) {
    let b = code.as_bytes();
    let mut out = b.to_vec();
    let mut strings = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'/' && b.get(i + 1) == Some(&b'/') {
            while i < b.len() && b[i] != b'\n' {
                out[i] = b' ';
                i += 1;
            }
        } else if b[i] == b'/' && b.get(i + 1) == Some(&b'*') {
            while i < b.len() && !(b[i] == b'*' && b.get(i + 1) == Some(&b'/')) {
                out[i] = b' ';
                i += 1;
            }
            if i < b.len() {
                out[i] = b' ';
                out[i + 1] = b' ';
                i += 2;
            }
        } else if b[i] == b'"' {
            let start = i;
            i += 1;
            while i < b.len() && b[i] != b'"' {
                if b[i] == b'\\' {
                    out[i] = b' ';
                    i += 1;
                }
                if i < b.len() {
                    out[i] = b' ';
                    i += 1;
                }
            }
            strings.push((start + 1, i.min(b.len())));
            i += 1;
        } else if b[i] == b'\'' && b.get(i + 2) == Some(&b'\'') {
            // a char literal such as '"' must not open a string
            out[i + 1] = b' ';
            i += 3;
        } else {
            i += 1;
        }
    }
    (out, strings)
}

fn is_ident(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

/// Identifiers in a pattern (`(i, mut e)` → `i`, `e`).
fn pattern_idents(p: &str) -> Vec<String> {
    p.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .filter(|w| !w.is_empty() && !matches!(*w, "mut" | "ref" | "_") && !w.chars().next().is_some_and(|c| c.is_ascii_digit() || c.is_ascii_uppercase()))
        .map(String::from)
        .collect()
}

/// Occurrences of `name` as a value in `s` (masked text): whole word, not a
/// field or path segment, not a field name before `=`, not followed by a
/// size accessor.
fn value_uses(s: &[u8], name: &str) -> Vec<usize> {
    let n = name.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i + n.len() <= s.len() {
        if &s[i..i + n.len()] == n
            && (i == 0 || !is_ident(s[i - 1]) && s[i - 1] != b'.' && s[i - 1] != b':')
            && s.get(i + n.len()).is_none_or(|c| !is_ident(*c))
        {
            let rest = String::from_utf8_lossy(&s[i + n.len()..]);
            let rest = rest.trim_start();
            let named_field = rest.starts_with('=') && !rest.starts_with("==") && !rest.starts_with("=>");
            let sized = SIZE_ACCESSORS.iter().any(|a| rest.starts_with(a));
            if !named_field && !sized && !rest.starts_with("::") {
                out.push(i);
            }
            i += n.len();
        } else {
            i += 1;
        }
    }
    out
}

/// Top-level comma-separated arguments of a macro call: (offset, text).
fn segments(args: &[u8]) -> Vec<(usize, &[u8])> {
    let mut out = Vec::new();
    let (mut depth, mut start) = (0i32, 0);
    for (i, &c) in args.iter().enumerate() {
        match c {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            b',' if depth == 0 => {
                out.push((start, &args[start..i]));
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push((start, &args[start..]));
    out
}

/// A field value that is a comparison or a predicate records a `bool`,
/// not the value: `changed = a != b`, `known = set.contains(e)`.
fn is_predicate(seg: &[u8]) -> bool {
    let s = String::from_utf8_lossy(seg);
    let value = match s.find('=') {
        Some(i) if !s[i..].starts_with("==") && !s[..i].ends_with(['!', '<', '>']) => &s[i + 1..],
        _ => &s[..],
    };
    ["==", "!=", "<=", ">=", " < ", " > ", ".is_", ".contains(", ".starts_with(", ".ends_with(", ".eq(", ".ne("]
        .iter()
        .any(|op| value.contains(op))
}

fn line_of(code: &str, at: usize) -> usize {
    code[..at.min(code.len())].matches('\n').count() + 1
}

/// Sensitive values formatted into `tracing` macros in the implementation
/// of `node`.
pub fn sensitive_logging(model: &Model, node: &str, code: &str) -> Vec<Leak> {
    let types = sensitive_types(model);
    let fields = sensitive_fields(model);
    let (masked, strings) = mask(code);
    // tainted identifier → why
    let mut tainted: Vec<(String, String)> = Vec::new();
    if let Some(sig) = model.nodes.get(node).and_then(|n| n.sig()) {
        for p in &sig.params {
            if p.is_sensitive {
                tainted.push((p.name.clone(), format!("the `sensitive` parameter `{}`", p.name)));
            } else if let Some(t) = contains_sensitive(&p.ty, &types) {
                tainted.push((p.name.clone(), format!("a value of the sensitive type `{t}`")));
            }
        }
    }
    // propagate through `for … in`, closures and `let` (a few passes)
    let text = String::from_utf8_lossy(&masked).to_string();
    let statements: Vec<(usize, &str)> = {
        let mut v = Vec::new();
        let mut start = 0;
        for (i, c) in text.char_indices() {
            if matches!(c, ';' | '{' | '}') {
                v.push((start, &text[start..i]));
                start = i + 1;
            }
        }
        v.push((start, &text[start..]));
        v
    };
    for _ in 0..3 {
        let mut added = Vec::new();
        for (_, st) in &statements {
            let taint = |expr: &str| tainted.iter().find(|(n, _)| !value_uses(expr.as_bytes(), n).is_empty()).map(|(_, w)| w.clone());
            let st_trim = st.trim_start();
            if let Some(rest) = st_trim.strip_prefix("for ").or_else(|| st.find(" for ").map(|i| &st[i + 5..]))
                && let Some((pat, expr)) = rest.split_once(" in ")
                && let Some(w) = taint(expr)
            {
                added.extend(pattern_idents(pat).into_iter().map(|p| (p, w.clone())));
            }
            if let Some(rest) = st_trim.strip_prefix("let ").or_else(|| st.find(" let ").map(|i| &st[i + 5..]))
                && let Some((pat, expr)) = rest.split_once('=')
                && !expr.starts_with('=')
                && let Some(w) = taint(expr)
            {
                let pat = pat.split(':').next().unwrap_or(pat);
                added.extend(pattern_idents(pat).into_iter().map(|p| (p, w.clone())));
            }
            // collections filled with tainted values: `out.push(email.clone())`
            for method in [".push(", ".insert(", ".extend(", ".push_str(", ".push_back(", ".entry("] {
                let mut k = 0;
                while let Some(i) = st[k..].find(method) {
                    let at = k + i;
                    k = at + method.len();
                    let target = &st[..at];
                    let start = target.rfind(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).map_or(0, |s| s + 1);
                    let owner = &target[start..];
                    let args_end = st[k..].find(')').map_or(st.len(), |e| k + e);
                    if let Some(w) = taint(&st[k..args_end]).filter(|_| !owner.is_empty()) {
                        added.push((owner.to_string(), w));
                    }
                }
            }
            // closures in a chain over a tainted value: `xs.iter().map(|x| …)`
            if let Some(w) = taint(st) {
                let mut rest = *st;
                while let Some(a) = rest.find('|') {
                    let Some(b) = rest[a + 1..].find('|') else { break };
                    let params = &rest[a + 1..a + 1 + b];
                    if !params.contains('|') && !params.trim().is_empty() {
                        added.extend(pattern_idents(params).into_iter().map(|p| (p, w.clone())));
                    }
                    rest = &rest[a + 1 + b + 1..];
                }
            }
        }
        let before = tainted.len();
        for (n, w) in added {
            if !tainted.iter().any(|(t, _)| *t == n) {
                tainted.push((n, w));
            }
        }
        if tainted.len() == before {
            break;
        }
    }

    let mut leaks = Vec::new();
    let b = &masked;
    for m in MACROS {
        let pat = format!("{m}!");
        let mut from = 0;
        while let Some(off) = text[from..].find(&pat) {
            let at = from + off;
            from = at + pat.len();
            if at > 0 && is_ident(b[at - 1]) {
                continue;
            }
            let Some(open) = text[from..].find(|c: char| !c.is_whitespace()).map(|o| from + o).filter(|&o| b[o] == b'(') else { continue };
            let mut depth = 0;
            let mut close = open;
            for (i, &c) in b.iter().enumerate().skip(open) {
                match c {
                    b'(' | b'[' | b'{' => depth += 1,
                    b')' | b']' | b'}' => {
                        depth -= 1;
                        if depth == 0 {
                            close = i;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            let args = &b[open + 1..close];
            let base = open + 1;
            let mut found: Vec<(usize, String, String)> = Vec::new();
            for (name, why) in &tainted {
                for (seg_start, seg) in segments(args) {
                    if is_predicate(seg) {
                        continue;
                    }
                    for u in value_uses(seg, name) {
                        found.push((base + seg_start + u, name.clone(), why.clone()));
                    }
                }
                // inline format arguments: "{email}" / "{email:?}"
                for &(s, e) in strings.iter().filter(|(s, e)| *s >= open && *e <= close) {
                    let lit = &code[s..e];
                    for brace in [format!("{{{name}}}"), format!("{{{name}:")] {
                        if let Some(i) = lit.find(&brace) {
                            found.push((s + i, name.clone(), why.clone()));
                        }
                    }
                }
            }
            for (seg_start, seg) in segments(args) {
                if is_predicate(seg) {
                    continue;
                }
                let a = String::from_utf8_lossy(seg);
                for f in &fields {
                    let needle = format!(".{f}");
                    let mut k = 0;
                    while let Some(i) = a[k..].find(&needle) {
                        let p = k + i;
                        k = p + needle.len();
                        let rest = a[k..].trim_start();
                        if a[k..].starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_') || SIZE_ACCESSORS.iter().any(|s| rest.starts_with(s)) {
                            continue;
                        }
                        let start = a[..p].rfind(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '.')).map_or(0, |s| s + 1);
                        found.push((base + seg_start + start, a[start..k].to_string(), format!("the `sensitive` field `{f}`")));
                    }
                }
            }
            found.sort();
            found.dedup_by(|x, y| x.1 == y.1);
            for (pos, value, why) in found {
                leaks.push(Leak { line: line_of(code, pos), macro_name: pat.clone(), value, why });
            }
        }
    }
    leaks.sort_by(|a, b| (a.line, &a.value).cmp(&(b.line, &b.value)));
    leaks.dedup();
    leaks
}

#[cfg(test)]
mod tests {
    use super::*;
    use wh_model::SourceInput;

    fn model() -> Model {
        let src = r#"---
module: m
---
```wh
type Email = str where matches(self, r"^[^@\s]+@[^@\s]+$")

struct Form:
    sensitive email: str
    age: int

struct User:
    sensitive email: Email
    id: u64

def dedupe(emails: list[Email]) -> list[Email]:
    note "x"

def check(form: Form) -> bool:
    note "x"
```
"#;
        Model::build(&[SourceInput { path: "m.wh.md".into(), text: src.into() }])
    }

    #[test]
    fn finds_leaks_through_loops_and_fields() {
        let m = model();
        let code = r#"
pub fn dedupe(emails: &[Email]) -> Vec<Email> {
    let mut seen = HashSet::with_capacity(emails.len());
    let mut out = Vec::new();
    for email in emails {
        if seen.insert(email) { out.push(email.clone()); } else {
            trace!(email = %email, "dropping duplicate"); // leaks
        }
    }
    debug!(input = emails.len(), kept = out.len(), "deduplicated");
    emails.iter().for_each(|e| tracing::warn!("saw {e:?}"));
    debug!("first is {}", out[0]);
    out
}
"#;
        let leaks = sensitive_logging(&m, "m.dedupe", code);
        let values: Vec<(&str, usize)> = leaks.iter().map(|l| (l.value.as_str(), l.line)).collect();
        assert_eq!(values, [("email", 7), ("e", 11), ("out", 12)], "{leaks:#?}");
        let form = r#"pub fn check(form: &Form) -> bool {
    debug!(age = form.age, "checking");
    info!(email = %form.email, "checking");
    debug!(len = form.email.len());
    trace!(same = form.email == "x", known = form.email.contains('@'));
    true
}"#;
        let leaks = sensitive_logging(&m, "m.check", form);
        assert_eq!(leaks.len(), 1, "{leaks:#?}");
        assert_eq!((leaks[0].line, leaks[0].value.as_str()), (3, "form.email"));
    }
}
