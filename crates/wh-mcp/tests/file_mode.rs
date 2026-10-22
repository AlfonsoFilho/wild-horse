//! Every tool in file mode, against a copy of `examples/signup`.

mod common;

use std::path::Path;

use serde_json::json;
use wh_mcp::server::dispatch;
use wh_mcp::{Backend, FileBackend, Op, ProseEdit, ProseMode};

const SIGNUP: &str = "src/signup.wh.md";

fn read(root: &Path, rel: &str) -> String {
    std::fs::read_to_string(root.join(rel)).unwrap()
}

fn section_index(b: &FileBackend, title: &str) -> usize {
    let o = b.outline().unwrap();
    let f = o.files.iter().find(|f| f.path == SIGNUP).unwrap();
    f.sections.iter().find(|s| s.title == title).unwrap().index
}

/// The code of every `wh` block, in order.
fn code_blocks(text: &str) -> Vec<String> {
    let parsed = wh_syntax::parse_document(text);
    parsed.doc.blocks.iter().map(|b| text[b.content_span.range()].to_string()).collect()
}

#[test]
fn outline_lists_files_sections_and_definitions() {
    let root = common::fresh_copy("outline");
    let b = FileBackend::open(&root).unwrap();
    let o = b.outline().unwrap();
    assert_eq!(o.diagnostics.errors, 0, "{o:#?}");
    let paths: Vec<&str> = o.files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(paths, ["src/main.wh.md", SIGNUP]);
    let f = &o.files[1];
    assert_eq!(f.module, "signup");
    let titles: Vec<&str> = f.sections.iter().map(|s| s.title.as_str()).collect();
    assert_eq!(titles, ["Signup", "Domain", "Errors", "Validation and normalization", "Storage", "Registering"]);
    let domain = &f.sections[1];
    assert_eq!(domain.parent, Some(0));
    assert!(domain.nodes.iter().any(|n| n.id == "signup.User" && n.kind == "struct" && n.signature == "struct User"));
    let validation = &f.sections[3];
    let norm = validation.nodes.iter().find(|n| n.id == "signup.normalize_email").unwrap();
    assert_eq!(norm.kind, "def");
    assert_eq!(norm.signature, "def normalize_email(e: Email) -> Email");
    assert_eq!(norm.hole.as_deref(), Some("synthesized"));
    assert!(f.sections[5].nodes.iter().any(|n| n.id == "signup.register" && n.kind == "flow" && n.hole.is_none()));
    // through the tool dispatch, as JSON
    let v = dispatch(&b, "wh_outline", json!({})).unwrap();
    assert_eq!(v["files"][1]["sections"][3]["nodes"][1]["id"], "signup.normalize_email");
}

#[test]
fn get_node_resolves_signature_clauses_and_references() {
    let root = common::fresh_copy("get_node");
    let b = FileBackend::open(&root).unwrap();
    let n = b.get_node("signup.normalize_email").unwrap();
    assert_eq!(n.kind, "def");
    assert_eq!(n.path, SIGNUP);
    assert_eq!(n.section.as_ref().unwrap().title, "Validation and normalization");
    assert!(n.prose.as_deref().unwrap().contains("case-insensitively"), "{:?}", n.prose);
    assert!(n.source.starts_with("def normalize_email(e: Email) -> Email:"));
    let sig = n.resolved.as_ref().unwrap();
    assert_eq!(sig.returns, "Email");
    assert_eq!((sig.params[0].name.as_str(), sig.params[0].ty.as_str()), ("e", "Email"));
    let kinds: Vec<&str> = n.clauses.iter().map(|c| c.kind.as_str()).collect();
    assert_eq!(kinds, ["ensures", "example", "property", "pattern"]);
    assert_eq!(n.clauses[0].text, "ensures result == e.lower()");
    assert!(n.uses.contains(&"signup.Email".to_string()), "{:?}", n.uses);
    assert!(n.used_by.contains(&"signup.normalize_form".to_string()), "{:?}", n.used_by);
    assert!(n.diagnostics.is_empty());

    let user = b.get_node("signup.User").unwrap();
    let fields = user.details["fields"].as_array().unwrap();
    assert_eq!(fields[1]["name"], "email");
    assert_eq!(fields[1]["sensitive"], true);
    let flow = b.get_node("signup.register").unwrap();
    assert_eq!(flow.details["steps"].as_array().unwrap().len(), 4);
    let ve = b.get_node("signup.ValidationError").unwrap();
    assert!(ve.mentioned_in.iter().any(|m| m.starts_with("src/signup.wh.md:")), "{:?}", ve.mentioned_in);

    let err = b.get_node("signup.nope").unwrap_err();
    assert!(err.0.contains("unknown node"), "{err}");
    let err = b.get_node("normalize_email").unwrap_err();
    assert!(err.0.contains("did you mean signup.normalize_email"), "{err}");
}

#[test]
fn apply_ops_is_atomic_and_writes_the_file() {
    let root = common::fresh_copy("apply_ops");
    let b = FileBackend::open(&root).unwrap();
    let before = read(&root, SIGNUP);
    let section = section_index(&b, "Validation and normalization");

    // the second op edits what the first one created
    let ops: Vec<Op> = serde_json::from_value(json!([
        { "op": "add_def", "file": SIGNUP, "section": section, "signature": "is_adult(age: Age) -> bool",
          "clauses": ["ensures result == (age >= 18)"] },
        { "op": "add_clause", "node": "signup.is_adult", "clause": "example is_adult(20)" },
        { "op": "add_field", "owner": "signup.User", "name": "nickname", "ty": "str | None" },
    ]))
    .unwrap();
    let r = b.apply_ops(&ops).unwrap();
    assert_eq!(r.applied.len(), 3);
    assert_eq!(r.counts.errors, 0, "{:#?}", r.diagnostics);
    assert!(r.introduced.is_empty(), "{:#?}", r.introduced);
    let after = read(&root, SIGNUP);
    assert_eq!(b.text(SIGNUP).unwrap(), after, "the file on disk is the store's text");
    assert!(after.contains("def is_adult(age: Age) -> bool:\n    ensures result == (age >= 18)\n    example is_adult(20)\n"), "{after}");
    assert!(after.contains("    nickname: str | None\n"));
    // the new def landed inside its section, before `## Storage`
    assert!(after.find("def is_adult").unwrap() < after.find("## Storage").unwrap());
    assert!(r.edited.iter().any(|e| e.text.contains("is_adult") && e.path == SIGNUP), "{:#?}", r.edited);
    assert!(r.edited.iter().any(|e| e.text.contains("nickname")));
    // nothing else changed: removing the additions gives back the original
    let n = b.get_node("signup.is_adult").unwrap();
    assert_eq!(n.clauses.len(), 2);
    assert!(before.lines().all(|l| after.contains(l)));

    // a failing op applies nothing, even after successful ones
    let ops: Vec<Op> = serde_json::from_value(json!([
        { "op": "add_clause", "node": "signup.is_adult", "clause": "example is_adult(3) == false" },
        { "op": "rename", "node": "signup.missing", "new_name": "x" },
    ]))
    .unwrap();
    let err = b.apply_ops(&ops).unwrap_err();
    assert!(err.0.starts_with("op 1 (rename to x): unknown node"), "{err}");
    assert_eq!(read(&root, SIGNUP), after);

    // an op that would break parsing is refused
    let err = b.apply_ops(&[Op::AddClause { node: "signup.is_adult".into(), clause: "requires (".into() }]).unwrap_err();
    assert!(err.0.contains("would break"), "{err}");
    assert_eq!(read(&root, SIGNUP), after);

    // a type error is applied and reported as introduced
    let r = b.apply_ops(&[Op::AddClause { node: "signup.is_adult".into(), clause: "ensures result == undefined_thing".into() }]).unwrap();
    assert!(r.counts.errors > 0);
    assert!(r.introduced.iter().any(|d| d.node.as_deref() == Some("signup.is_adult") && d.severity == "error"), "{:#?}", r.introduced);
    // and wh_check sees it
    let c = b.check(false).unwrap();
    assert_eq!(c.counts.errors, r.counts.errors);
    assert!(c.cargo.is_none());
}

#[test]
fn apply_ops_takes_mcp_json() {
    let root = common::fresh_copy("apply_ops_json");
    let b = FileBackend::open(&root).unwrap();
    let r = dispatch(&b, "wh_apply_ops", json!({ "ops": [{ "op": "rename", "node": "signup.dedupe_emails", "new_name": "unique_emails" }] })).unwrap();
    assert_eq!(r["applied"][0], "rename to unique_emails");
    let text = read(&root, SIGNUP);
    assert!(text.contains("def unique_emails(") && text.contains("example unique_emails(") && !text.contains("dedupe_emails"));
    let err = dispatch(&b, "wh_apply_ops", json!({ "ops": [{ "op": "rename" }] })).unwrap_err();
    assert!(err.0.starts_with("op 0:"), "{err}");
}

#[test]
fn edit_prose_keeps_code_blocks() {
    let root = common::fresh_copy("edit_prose");
    let b = FileBackend::open(&root).unwrap();
    let before = read(&root, SIGNUP);
    let code = code_blocks(&before);

    // a definition's prose
    let r = b
        .edit_prose(&ProseEdit {
            target: "signup.check_age".into(),
            text: "Minors need a guardian who already has an account.".into(),
            mode: ProseMode::Replace,
            heading: None,
        })
        .unwrap();
    assert_eq!(r.edited.len(), 1);
    let text = read(&root, SIGNUP);
    assert!(text.contains("\nMinors need a guardian who already has an account.\n\n```wh\ndef check_age("), "{text}");
    assert!(!text.contains("Minors need a guardian.\n"));
    assert_eq!(b.get_node("signup.check_age").unwrap().prose.as_deref(), Some("Minors need a guardian who already has an account."));

    // a section without an introduction gets one under its heading
    b.edit_prose(&ProseEdit { target: "signup#Storage".into(), text: "Where users live.".into(), mode: ProseMode::Replace, heading: None })
        .unwrap();
    let text = read(&root, SIGNUP);
    assert!(text.contains("## Storage\n\nWhere users live.\n\n```wh\ncapability Db:"), "{text}");

    // appending to a section, and starting a subsection
    b.edit_prose(&ProseEdit { target: format!("{SIGNUP}#2"), text: "All errors derive `Debug`.".into(), mode: ProseMode::Append, heading: None })
        .unwrap();
    b.edit_prose(&ProseEdit {
        target: "signup#Registering".into(),
        text: "Nothing yet.".into(),
        mode: ProseMode::Append,
        heading: Some("Open questions".into()),
    })
    .unwrap();
    let text = read(&root, SIGNUP);
    assert!(text.contains("```\n\nAll errors derive `Debug`.\n\n## Validation"), "{text}");
    assert!(text.ends_with("```\n\n### Open questions\n\nNothing yet.\n"), "{text}");

    // a whole-file introduction (after the front matter)
    b.edit_prose(&ProseEdit { target: "signup".into(), text: "Generated docs live elsewhere.".into(), mode: ProseMode::Prepend, heading: None })
        .unwrap();
    let text = read(&root, SIGNUP);
    assert!(text.starts_with("---\nmodule: signup\n---\n\nGenerated docs live elsewhere.\n\n# Signup"), "{text}");

    assert_eq!(code_blocks(&text), code, "code blocks are untouched");
    assert_eq!(b.check(false).unwrap().counts.errors, 0);

    // refused: fences, headings, unknown targets
    for (target, t) in [("signup.check_age", "```wh\ndef x() -> int\n```"), ("signup#Domain", "# New"), ("nowhere", "text")] {
        let err = b.edit_prose(&ProseEdit { target: target.into(), text: t.into(), mode: ProseMode::Replace, heading: None });
        assert!(err.is_err(), "{target}: {t}");
    }
    assert_eq!(read(&root, SIGNUP), text);
    // through dispatch with the default mode
    dispatch(&b, "wh_edit_prose", json!({ "target": "signup.save_user", "text": "Stores a checked form." })).unwrap();
    assert!(read(&root, SIGNUP).contains("Stores a checked form.\n\n```wh\ndef save_user("));
}

#[test]
fn disk_changes_are_merged_before_each_call() {
    let root = common::fresh_copy("disk_merge");
    let b = FileBackend::open(&root).unwrap();
    // someone edits the file behind the server's back
    let mut text = read(&root, SIGNUP);
    text = text.replace("Minors need a guardian.", "Children need a guardian.");
    std::fs::write(root.join(SIGNUP), &text).unwrap();
    b.apply_ops(&[Op::AddVariant { owner: "signup.DbError".into(), variant: "Timeout".into() }]).unwrap();
    let after = read(&root, SIGNUP);
    assert!(after.contains("Children need a guardian.") && after.contains("    Unavailable\n    Timeout\n"), "{after}");
}

#[test]
fn spec_is_the_language_specification() {
    let root = common::fresh_copy("spec");
    let b = FileBackend::open(&root).unwrap();
    let s = b.spec().unwrap();
    assert!(s.starts_with('#') && s.contains("Wild Horse"));
    assert_eq!(dispatch(&b, "wh_spec", json!(null)).unwrap(), json!(s));
}

/// `wh_check` with cargo and `wh_run_specs` run cargo on the generated crate
/// (slow the first time: dependencies build into the shared target dir).
#[test]
fn cargo_check_and_run_specs() {
    let root = common::fresh_copy("cargo");
    let b = FileBackend::open(&root).unwrap();
    let c = b.check(true).unwrap();
    let cargo = c.cargo.expect("cargo report");
    assert!(cargo.success, "{cargo:#?}");

    let r = b.run_specs(Some("signup.normalize_email")).unwrap();
    assert!(r.success && r.failed == 0 && r.passed > 0, "{r:#?}");
    assert!(r.tests.iter().all(|t| t.node.as_deref() == Some("signup.normalize_email")), "{r:#?}");

    let err = b.run_specs(Some("signup.nope")).unwrap_err();
    assert!(err.0.contains("unknown node"));
}
