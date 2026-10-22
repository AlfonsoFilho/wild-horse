use std::path::Path;

use wh_syntax::ast::ItemKind;
use wh_syntax::{Severity, parse_document};

fn root() -> &'static Path {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
}

fn assert_clean(name: &str, src: &str) {
    let parsed = parse_document(src);
    let errors: Vec<String> = parsed
        .diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .map(|d| d.render(name, src))
        .collect();
    assert!(errors.is_empty(), "{}", errors.join("\n"));
    assert!(parsed.doc.blocks.iter().all(|b| b.items.iter().all(|i| i.kind != ItemKind::Error)));
}

#[test]
fn spec_examples_parse() {
    let src = std::fs::read_to_string(root().join("docs/language-spec.md")).unwrap();
    assert_clean("language-spec.md", &src);
    let parsed = parse_document(&src);
    assert!(parsed.doc.blocks.len() > 10, "found {} wh blocks", parsed.doc.blocks.len());
}

#[test]
fn signup_example_parses() {
    for f in ["signup.wh.md", "main.wh.md"] {
        let src = std::fs::read_to_string(root().join("examples/signup/src").join(f)).unwrap();
        assert_clean(f, &src);
    }
}

#[test]
fn signup_structure() {
    let src = std::fs::read_to_string(root().join("examples/signup/src/signup.wh.md")).unwrap();
    let doc = parse_document(&src).doc;
    assert_eq!(doc.front_matter.unwrap().module.as_deref(), Some("signup"));
    let names: Vec<String> =
        doc.blocks.iter().flat_map(|b| b.items.iter().filter_map(|i| i.name().map(|n| n.name.clone()))).collect();
    for n in ["Email", "User", "register", "MemoryDb", "dedupe_emails"] {
        assert!(names.contains(&n.to_string()), "missing {n} in {names:?}");
    }
    assert!(doc.refs.iter().any(|r| r.target == "register"));
    // the block defining Email is in section "Domain", with prose attached
    let email_block = doc.blocks.iter().find(|b| b.items.iter().any(|i| i.name().is_some_and(|n| n.name == "Email"))).unwrap();
    assert_eq!(doc.sections[email_block.section.unwrap()].title, "Domain");
    assert!(doc.prose[email_block.prose.unwrap()].text.starts_with("An email address"));
    // the register flow has 4 pipeline steps
    let flow = doc.blocks.iter().flat_map(|b| &b.items).find_map(|i| match &i.kind {
        ItemKind::Flow(f) => Some(f),
        _ => None,
    }).unwrap();
    match &flow.body[0].kind {
        wh_syntax::ast::FlowStmtKind::Expr(fe) => assert_eq!(fe.steps.len(), 4),
        k => panic!("{k:?}"),
    }
}

#[test]
fn spans_point_at_source() {
    let src = std::fs::read_to_string(root().join("examples/signup/src/signup.wh.md")).unwrap();
    let doc = parse_document(&src).doc;
    for b in &doc.blocks {
        for i in &b.items {
            if let Some(n) = i.name() {
                assert_eq!(&src[n.span.range()], n.name);
            }
        }
    }
}

fn errors_of(src: &str) -> Vec<String> {
    parse_document(src).diagnostics.into_iter().map(|d| d.message).collect()
}

#[test]
fn errors_recover_per_line() {
    let src = "```wh\ndef f(x: int) -> int:\n    ensures result >\n    example f(1) == 2\n\nstruct S:\n    a: int\n```\n";
    let parsed = parse_document(src);
    assert_eq!(parsed.diagnostics.len(), 1, "{:?}", parsed.diagnostics);
    let items = &parsed.doc.blocks[0].items;
    assert_eq!(items.len(), 2);
    match &items[0].kind {
        ItemKind::Def(d) => assert_eq!(d.clauses.len(), 1),
        k => panic!("{k:?}"),
    }
}

#[test]
fn error_messages() {
    let e = errors_of("```wh\nfoo bar\n```\n");
    assert!(e[0].starts_with("expected a definition"), "{e:?}");
    let e = errors_of("```wh\ndef f(x: int):\n    frobnicate\n```\n");
    assert!(e[0].contains("unknown clause"), "{e:?}");
    let e = errors_of("```wh\nstruct S:\n\ta: int\n```\n");
    assert!(e.iter().any(|m| m.contains("tabs")), "{e:?}");
    let e = errors_of("```wh\nstruct S:\n    a: int\n      b: int\n```\n");
    assert!(e.iter().any(|m| m.contains("unexpected indentation")), "{e:?}");
}

#[test]
fn machine_clauses() {
    let src = "```wh\nmachine Door(event: DoorEvent) -> DoorEffect:\n    \"\"\"A door.\"\"\"\n    Open on Close -> Closed\n    Locked(code) on Unlock(c) if c == code -> Closed emit Unlocked\n    ignore Open on _\n    ignore Locked(_) on Lock\n    trace info\n    initial Closed\n```\n";
    assert_clean("machine", src);
    let doc = parse_document(src).doc;
    let ItemKind::Machine(m) = &doc.blocks[0].items[0].kind else { panic!() };
    assert_eq!(m.doc.as_deref(), Some("A door."));
    assert_eq!(m.transitions.len(), 2);
    assert!(m.transitions[1].guard.is_some() && m.transitions[1].emit.is_some());
    assert_eq!(m.ignores.len(), 2);
    assert_eq!(&src[m.ignores[1].span.range()], "ignore Locked(_) on Lock");
    assert_eq!(m.trace.as_ref().map(|t| t.name.as_str()), Some("info"));
    assert!(m.initial.is_some());
    assert!(src[m.body_span.range()].starts_with("\"\"\"A door.\"\"\""));
}

#[test]
fn type_exprs_display_as_written() {
    let src = "```wh\nstruct S:\n    a: UserId |   None\n    b: dict[str, list[accounts.User]]\n    c: array[u8, 32]\n    d: (int, str) -> bool\n```\n";
    let doc = parse_document(src).doc;
    let ItemKind::Struct(s) = &doc.blocks[0].items[0].kind else { panic!() };
    let shown: Vec<String> = s.fields.iter().map(|f| f.ty.to_string()).collect();
    assert_eq!(shown, ["UserId | None", "dict[str, list[accounts.User]]", "array[u8, 32]", "(int, str) -> bool"]);
}

#[test]
fn sensitive_parameters() {
    let src = "```wh\ndef f(sensitive a: str, own sensitive b: str, sensitive: int) -> int\n```\n";
    assert_clean("sensitive", src);
    let doc = parse_document(src).doc;
    let ItemKind::Def(d) = &doc.blocks[0].items[0].kind else { panic!() };
    let ps: Vec<(&str, bool)> = d.sig.params.iter().map(|p| (p.name.name.as_str(), p.is_sensitive)).collect();
    assert_eq!(ps, [("a", true), ("b", true), ("sensitive", false)]);
}

#[test]
fn app_target_sections() {
    let src = "```wh\napp api:\n    target server(axum)\n    listen \"0.0.0.0:80\"\n    route GET \"/users/{id}\" -> find_user\n    status DbError.Missing = 404\n    export a, b\n    export c\n```\n";
    assert_clean("app", src);
    let doc = parse_document(src).doc;
    let ItemKind::App(a) = &doc.blocks[0].items[0].kind else { panic!() };
    assert_eq!((a.routes[0].method.name.as_str(), a.routes[0].path.as_str()), ("GET", "/users/{id}"));
    assert_eq!(&src[a.routes[0].span.range()], "route GET \"/users/{id}\" -> find_user");
    assert_eq!(a.statuses[0].error.to_string(), "DbError.Missing");
    assert_eq!(a.exports.len(), 3);
    assert!(a.listen.is_some());
}
