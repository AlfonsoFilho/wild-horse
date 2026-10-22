use std::path::Path;

use wh_edit::{Cardinality, EditError, Op, apply_to, pipeline_position, plan};
use wh_model::{Model, NodeKind, SourceInput, Ty};

fn root() -> &'static Path {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
}

fn inputs() -> Vec<SourceInput> {
    let dir = root().join("examples/signup/src");
    ["signup.wh.md", "main.wh.md"]
        .iter()
        .map(|f| SourceInput { path: f.to_string(), text: std::fs::read_to_string(dir.join(f)).unwrap() })
        .collect()
}

/// Apply an op; return the new model and the edited texts.
fn run(op: Op) -> (Model, Vec<SourceInput>) {
    let mut inputs = inputs();
    let model = Model::build(&inputs);
    let edits = plan(&model, &op).unwrap_or_else(|e| panic!("{e}"));
    assert!(!edits.is_empty());
    for fe in &edits {
        let input = inputs.iter_mut().find(|i| i.path == fe.path).unwrap();
        let before = input.text.clone();
        input.text = apply_to(&input.text, &fe.edits);
        // nothing outside the edited ranges changes
        let mut cursor = 0;
        let mut sorted = fe.edits.clone();
        sorted.sort_by_key(|e| e.range.start);
        for e in &sorted {
            assert!(input.text.contains(&before[cursor..e.range.start]));
            cursor = e.range.end;
        }
    }
    let m = Model::build(&inputs);
    (m, inputs)
}

fn errors(m: &Model) -> Vec<String> {
    m.diagnostics
        .iter()
        .filter(|d| d.diag.severity == wh_syntax::Severity::Error)
        .map(|d| d.diag.render(&m.files[d.file].path, &m.files[d.file].text))
        .collect()
}

fn signup_text(i: &[SourceInput]) -> &str {
    &i.iter().find(|i| i.path == "signup.wh.md").unwrap().text
}

#[test]
fn insert_flow_step_multiline() {
    let (m, i) = run(Op::InsertFlowStep { flow: "signup.register".into(), stmt: 0, position: 2, callee: "normalize_form".into() });
    assert!(signup_text(&i).contains("    |> normalize_form\n    |> normalize_form\n    |> check_age"));
    let NodeKind::Flow(f) = &m.nodes["signup.register"].kind else { panic!() };
    assert_eq!(f.steps.len(), 5);
    assert!(errors(&m).is_empty(), "{:?}", errors(&m));
}

#[test]
fn remove_flow_step() {
    let (m, i) = run(Op::RemoveFlowStep { flow: "signup.register".into(), stmt: 0, position: 1 });
    assert!(signup_text(&i).contains("    |> validate\n    |> check_age"));
    let NodeKind::Flow(f) = &m.nodes["signup.register"].kind else { panic!() };
    assert_eq!(f.steps.len(), 3);
}

#[test]
fn inline_pipeline_step_and_position() {
    let (m, i) = run(Op::InsertFlowStep { flow: "main.signup_cli".into(), stmt: 0, position: 3, callee: "report".into() });
    let main = &i.iter().find(|i| i.path == "main.wh.md").unwrap().text;
    assert!(main.contains("args |> parse_cli |> register |> report |> report"), "{main}");
    // and the first step maps back to its position
    let model = Model::build(&inputs());
    assert_eq!(pipeline_position(&model, "signup.register", 0), Some((0, Some(0))));
    assert_eq!(pipeline_position(&model, "signup.register", 3), Some((0, Some(3))));
    // the extra report makes the entry ill-typed (unit piped into report): reported on that step
    assert!(errors(&m).iter().any(|e| e.contains("piped value is `unit`")));
}

#[test]
fn cardinality_rewrites_field_type() {
    let (m, i) = run(Op::SetCardinality { owner: "signup.User".into(), field: "email".into(), cardinality: Cardinality::Many });
    assert!(signup_text(&i).contains("    sensitive email: list[Email]\n"));
    let NodeKind::Struct(s) = &m.nodes["signup.User"].kind else { panic!() };
    assert!(matches!(&s.fields[1].ty, Ty::List(_)));
    // optional → one also drops the `= None` default
    let (_, i) = run(Op::SetCardinality { owner: "signup.User".into(), field: "guardian".into(), cardinality: Cardinality::One });
    assert!(signup_text(&i).contains("    guardian: UserId\n"));
}

#[test]
fn fields_and_variants() {
    let (m, i) = run(Op::AddField { owner: "signup.User".into(), name: "nickname".into(), ty: "str | None".into() });
    assert!(signup_text(&i).contains("    guardian: UserId | None = None\n    nickname: str | None\n    invariant"));
    assert!(errors(&m).is_empty());
    let (_, i) = run(Op::RenameField { owner: "signup.SignupForm".into(), field: "age".into(), new_name: "years".into() });
    assert!(signup_text(&i).contains("    years: int\n"));
    let (m, _) = run(Op::RemoveField { owner: "signup.ValidForm".into(), field: "guardian".into() });
    let NodeKind::Struct(s) = &m.nodes["signup.ValidForm"].kind else { panic!() };
    assert_eq!(s.fields.len(), 2);
    let (m, _) = run(Op::AddVariant { owner: "signup.DbError".into(), variant: "Timeout".into() });
    let NodeKind::Enum(e) = &m.nodes["signup.DbError"].kind else { panic!() };
    assert_eq!(e.variants.last().unwrap().name, "Timeout");
}

#[test]
fn clauses_and_patterns() {
    let (m, i) = run(Op::AddClause { node: "signup.dedupe_emails".into(), clause: "alloc bounded(1)".into() });
    assert!(signup_text(&i).contains("    optimize speed\n    alloc bounded(1)\n"));
    assert!(errors(&m).is_empty());
    // a def without a body gets one
    let (_, i) = run(Op::AddClause { node: "main.report".into(), clause: "pure".into() });
    let _ = i;
    let (_, i) = run(Op::SetPattern { node: "signup.check_age".into(), pattern: Some("Transform".into()) });
    assert!(signup_text(&i).contains("example check_age(ValidForm(email=\"a@x.io\", age=40)).age == 40\n    pattern Transform"));
    let (_, i) = run(Op::SetPattern { node: "signup.normalize_email".into(), pattern: None });
    assert!(!signup_text(&i).contains("        normalize_email(normalize_email(x)) == normalize_email(x)\n    pattern Transform"));
    let (m, _) = run(Op::ReplaceClause { node: "signup.normalize_email".into(), index: 0, clause: "ensures result == e.upper()".into() });
    assert!(errors(&m).is_empty());
}

#[test]
fn rename_updates_references_and_prose() {
    let (m, i) = run(Op::Rename { node: "signup.register".into(), new_name: "sign_up".into() });
    let main = &i.iter().find(|i| i.path == "main.wh.md").unwrap().text;
    assert!(main.contains("from signup import sign_up,"));
    assert!(main.contains("args |> parse_cli |> sign_up |> report"));
    assert!(main.contains("[[signup.sign_up]]"));
    assert!(signup_text(&i).contains("[[sign_up]]"));
    assert!(signup_text(&i).contains("expect sign_up(SignupForm"));
    assert!(m.nodes.contains_key("signup.sign_up"));
    assert!(errors(&m).is_empty(), "{:?}", errors(&m));

    let (m, i) = run(Op::Rename { node: "signup.Email".into(), new_name: "EmailAddress".into() });
    assert!(signup_text(&i).contains("    sensitive email: EmailAddress\n"));
    // the field *named* email is untouched
    assert!(signup_text(&i).contains("form.email"));
    assert!(errors(&m).is_empty(), "{:?}", errors(&m));
}

#[test]
fn add_and_remove_nodes() {
    let model = Model::build(&inputs());
    let storage = model.files[0].doc.sections.iter().position(|s| s.title == "Storage").unwrap();
    let (m, i) = run(Op::AddDef {
        file: "signup.wh.md".into(),
        section: Some(storage),
        signature: "count_users() -> int uses Db".into(),
        clauses: vec!["ensures result >= 0".into()],
    });
    let t = signup_text(&i);
    let pos_new = t.find("def count_users").unwrap();
    assert!(pos_new < t.find("## Registering").unwrap() && pos_new > t.find("## Storage").unwrap());
    assert!(m.nodes["signup.count_users"].prose.as_deref().unwrap().starts_with("TODO"));
    assert!(errors(&m).is_empty(), "{:?}", errors(&m));

    let (m, _) = run(Op::AddStruct { file: "signup.wh.md".into(), section: None, name: "Invite".into(), fields: vec![("code".into(), "str".into())] });
    assert!(m.nodes.contains_key("signup.Invite"));

    let (m, i) = run(Op::RemoveNode { node: "signup.dedupe_emails".into() });
    assert!(!m.nodes.contains_key("signup.dedupe_emails"));
    assert!(signup_text(&i).contains("must stay linear, because imports can be large."), "prose is kept");
    let (m, _) = run(Op::RemoveNode { node: "signup.ValidForm".into() });
    assert!(m.nodes.contains_key("signup.SignupForm"));
}

#[test]
fn invalid_ops_are_rejected() {
    let model = Model::build(&inputs());
    let r = plan(&model, &Op::EditSignature { node: "signup.validate".into(), signature: "validate(form: ) ->".into() });
    assert!(matches!(r, Err(EditError::WouldBreak(_))), "{r:?}");
    let r = plan(&model, &Op::Rename { node: "signup.User".into(), new_name: "user".into() });
    assert!(matches!(r, Err(EditError::Invalid(_))));
    let r = plan(&model, &Op::Rename { node: "signup.validate".into(), new_name: "check_age".into() });
    assert!(matches!(r, Err(EditError::Invalid(_))));
    let r = plan(&model, &Op::AddField { owner: "signup.User".into(), name: "email".into(), ty: "str".into() });
    assert!(matches!(r, Err(EditError::Invalid(_))));
}

#[test]
fn move_flow_step_reorders_pipeline() {
    let (_, i) = run(Op::MoveFlowStep { flow: "signup.register".into(), stmt: 0, from: 1, to: 0 });
    assert!(signup_text(&i).contains("    |> normalize_form\n    |> validate\n    |> check_age\n    |> save_user"), "{}", signup_text(&i));
    let (m, i) = run(Op::MoveFlowStep { flow: "signup.register".into(), stmt: 0, from: 0, to: 3 });
    assert!(signup_text(&i).contains("    |> normalize_form\n    |> check_age\n    |> save_user\n    |> validate"));
    let NodeKind::Flow(f) = &m.nodes["signup.register"].kind else { panic!() };
    let labels: Vec<&str> = f.steps.iter().map(|s| s.label.as_str()).collect();
    assert_eq!(labels, ["normalize_form", "check_age", "save_user", "validate"]);
    // inline pipelines keep their separators
    let (_, i) = run(Op::MoveFlowStep { flow: "main.signup_cli".into(), stmt: 0, from: 2, to: 0 });
    let main = &i.iter().find(|i| i.path == "main.wh.md").unwrap().text;
    assert!(main.contains("args |> report |> parse_cli |> register"), "{main}");
    let model = Model::build(&inputs());
    let r = plan(&model, &Op::MoveFlowStep { flow: "signup.register".into(), stmt: 0, from: 0, to: 9 });
    assert!(matches!(r, Err(EditError::Invalid(_))));
    assert!(plan(&model, &Op::MoveFlowStep { flow: "signup.register".into(), stmt: 0, from: 2, to: 2 }).unwrap().is_empty());
}

const DAG: &str = "---\nmodule: m\n---\n# M\n\n```wh\ndef f(x: int) -> int\n\ndef g(x: int) -> int\n\nflow h(x: int) -> int:\n    a = f(x)\n    return a\n```\n";

#[test]
fn insert_flow_binding_in_dag_form() {
    let mut inputs = vec![SourceInput { path: "m.wh.md".into(), text: DAG.into() }];
    let model = Model::build(&inputs);
    assert!(errors(&model).is_empty(), "{:?}", errors(&model));
    let op = Op::InsertFlowBinding { flow: "m.h".into(), index: 1, name: "b".into(), value: "g(a)".into() };
    let edits = plan(&model, &op).unwrap();
    inputs[0].text = apply_to(&inputs[0].text, &edits[0].edits);
    assert!(inputs[0].text.contains("    a = f(x)\n    b = g(a)\n    return a\n"), "{}", inputs[0].text);
    let m = Model::build(&inputs);
    let NodeKind::Flow(f) = &m.nodes["m.h"].kind else { panic!() };
    let g = f.steps.iter().find(|s| s.label == "g").expect("a step for the new binding");
    assert_eq!(g.bindings, ["b"]);
    assert!(matches!(g.inputs.as_slice(), [wh_model::StepInput::Step(0)]), "{:?}", g.inputs);
    assert_eq!(wh_edit::step_site(&m, "m.h", 0), Some((0, None, vec!["a".to_string()], true)));
    assert_eq!(wh_edit::flow_len(&m, "m.h"), Some(3));
    // at the start, fed by a parameter; and at the end of the body
    let edits = plan(&model, &Op::InsertFlowBinding { flow: "m.h".into(), index: 0, name: "y".into(), value: "g(x)".into() }).unwrap();
    assert!(apply_to(DAG, &edits[0].edits).contains("flow h(x: int) -> int:\n    y = g(x)\n    a = f(x)\n"));
    let edits = plan(&model, &Op::InsertFlowBinding { flow: "m.h".into(), index: 2, name: "z".into(), value: "g(a)".into() }).unwrap();
    assert!(apply_to(DAG, &edits[0].edits).contains("    return a\n    z = g(a)\n```"));
    // names are checked
    let r = plan(&model, &Op::InsertFlowBinding { flow: "m.h".into(), index: 1, name: "a".into(), value: "g(a)".into() });
    assert!(matches!(r, Err(EditError::Invalid(_))));
    let r = plan(&model, &Op::InsertFlowBinding { flow: "m.h".into(), index: 1, name: "Bad".into(), value: "g(a)".into() });
    assert!(matches!(r, Err(EditError::Invalid(_))));
    let r = plan(&model, &Op::InsertFlowBinding { flow: "m.h".into(), index: 1, name: "c".into(), value: "g(".into() });
    assert!(matches!(r, Err(EditError::WouldBreak(_))), "{r:?}");
}
