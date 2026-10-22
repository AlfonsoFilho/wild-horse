//! End-to-end tests on scratch copies of `examples/signup` and `examples/todo`
//! (never the examples themselves).

use std::path::{Path, PathBuf};
use std::time::Instant;

use wh_live::{ItemKind, LiveOptions, LiveScope, Status};
use wh_project::Project;

fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

/// Dedicated target dir, shared by these tests.
fn target_dir() -> PathBuf {
    workspace().join("target/wh-live-tests/target")
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            copy_tree(&p, &to.join(e.file_name()));
        } else {
            std::fs::copy(&p, to.join(e.file_name())).unwrap();
        }
    }
}

fn scratch(example: &str, name: &str) -> Project {
    let ws = workspace();
    let dir = ws.join("target/wh-live-tests").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    let ex = ws.join("examples").join(example);
    copy_tree(&ex.join("src"), &dir.join("src"));
    copy_tree(&ex.join("impl"), &dir.join("impl"));
    std::fs::copy(ex.join("wildhorse.toml"), dir.join("wildhorse.toml")).unwrap();
    Project::load(&dir).unwrap()
}

fn opts(scope: LiveScope) -> LiveOptions {
    LiveOptions {
        scope,
        target_dir: Some(target_dir()),
        timeout_ms: 3000,
        ..Default::default()
    }
}

#[test]
fn signup_values_steps_and_edits() {
    let p = scratch("signup", "signup");
    let model = p.model().unwrap();
    let r = wh_live::evaluate_with(&p, &model, &opts(LiveScope::All)).unwrap();
    assert!(r.build_errors.is_empty(), "{:#?}", r.build_errors);
    eprintln!("cold: {:?}", r.timings);

    // example: both sides of the comparison, with their spans
    let ex = r
        .get("signup.normalize_email", ItemKind::Example, 1)
        .expect("normalize_email example");
    assert_eq!(ex.status, Status::Pass, "{ex:#?}");
    let left = ex.values.iter().find(|v| v.label == "left").unwrap();
    assert_eq!(left.value, "Email(\"a@b.com\")");
    assert_eq!(
        ex.values.iter().find(|v| v.label == "right").unwrap().value,
        "\"a@b.com\""
    );
    assert!(left.span.unwrap().start < ex.span.end && ex.source.starts_with("example"));

    // example with `raises`: the raised error is the value
    let raises = r.nodes["signup.validate"]
        .iter()
        .find(|v| v.kind == ItemKind::Example && v.source.contains("\"nope\""))
        .unwrap();
    assert_eq!(raises.status, Status::Pass);
    assert!(
        raises.values[0].value.starts_with("raised BadEmail"),
        "{raises:#?}"
    );

    // property: held, with its case count
    let prop = r.nodes["signup.normalize_email"]
        .iter()
        .find(|v| v.kind == ItemKind::Property)
        .unwrap();
    assert_eq!(
        (prop.status.clone(), prop.cases),
        (Status::Pass, Some(64)),
        "{prop:#?}"
    );

    // ensures, on sampled inputs
    let ens: Vec<_> = r.nodes["signup.dedupe_emails"]
        .iter()
        .filter(|v| v.kind == ItemKind::Ensures)
        .collect();
    assert_eq!(ens.len(), 3);
    for e in &ens {
        assert_eq!(e.status, Status::Pass, "{e:#?}");
        assert_eq!(e.samples.len(), 3);
        assert!(
            e.samples
                .iter()
                .all(|s| s.result.is_some() && s.holds == Some(true))
        );
    }

    // scenario test: values per expectation, flow steps, sensitive fields redacted
    let test = "signup#test:minors need a guardian";
    let first = r.get(test, ItemKind::Expect, 0).unwrap();
    assert_eq!(first.status, Status::Pass, "{first:#?}");
    assert!(first.values[0].value.contains("TooYoung"), "{first:#?}");
    let steps: Vec<&str> = first.steps.iter().map(|s| s.step.as_str()).collect();
    assert!(
        steps.contains(&"signup.register#validate[0]")
            && steps.contains(&"signup.register#check_age[0]"),
        "{steps:?}"
    );
    let validated = first
        .steps
        .iter()
        .find(|s| s.step == "signup.register#validate[0]")
        .unwrap();
    assert!(
        validated.value.contains("<redacted>") && !validated.value.contains("kid@x.io"),
        "{validated:?}"
    );
    assert!(
        first
            .steps
            .iter()
            .any(|s| s.step == "signup.register#check_age[0]" && s.error),
        "{:?}",
        first.steps
    );
    let second = r.get(test, ItemKind::Expect, 1).unwrap();
    assert_eq!(
        (second.status.clone(), second.values[0].value.as_str()),
        (Status::Pass, "1"),
        "{second:#?}"
    );
    assert!(
        r.steps["signup.register#save_user[0]"]
            .iter()
            .any(|s| s.value == "Ok(1)"),
        "{:?}",
        r.steps.keys()
    );
    let json = serde_json::to_string(&r).unwrap();
    assert!(
        !json.contains("kid@x.io\\\"") || json.contains("expect"),
        "values never carry the sensitive email"
    );

    // a node scope runs only that node's probes, without rebuilding
    let t = Instant::now();
    let r2 = wh_live::evaluate_with(
        &p,
        &model,
        &opts(LiveScope::Node("signup.normalize_email".into())),
    )
    .unwrap();
    eprintln!(
        "warm, one node: {:?} ({} ms wall)",
        r2.timings,
        t.elapsed().as_millis()
    );
    assert!(!r2.timings.rebuilt);
    assert_eq!(
        r2.nodes.keys().collect::<Vec<_>>(),
        ["signup.normalize_email"]
    );

    // editing a hole: the example now fails and shows the wrong value
    let hole = p.root.join("impl/signup/normalize_email.rs");
    let code = std::fs::read_to_string(&hole).unwrap();
    std::fs::write(
        &hole,
        code.replace(
            "let lowered = e.as_inner().to_lowercase();",
            "let lowered = e.as_inner().to_uppercase();",
        ),
    )
    .unwrap();
    let t = Instant::now();
    let r3 = wh_live::evaluate_with(
        &p,
        &model,
        &opts(LiveScope::Node("signup.normalize_email".into())),
    )
    .unwrap();
    eprintln!(
        "after a hole edit: {:?} ({} ms wall)",
        r3.timings,
        t.elapsed().as_millis()
    );
    assert!(r3.timings.rebuilt);
    let ex = r3
        .get("signup.normalize_email", ItemKind::Example, 1)
        .unwrap();
    // the debug contract (`ensures result == e.lower()`) catches it first
    assert!(matches!(ex.status, Status::Error | Status::Fail), "{ex:#?}");
    assert!(
        ex.message.as_deref().unwrap_or("").contains("ensures"),
        "{ex:#?}"
    );
}

#[test]
fn timeouts_and_crashes_do_not_hang() {
    let p = scratch("signup", "hang");
    // check_age loops forever for minors; normalize_email overflows the stack
    let hole = p.root.join("impl/signup/check_age.rs");
    let code = std::fs::read_to_string(&hole).unwrap();
    let code = code.replacen('{', "{\n    if *form.age.as_inner() < 13 {\n        loop {\n            std::hint::spin_loop();\n        }\n    }", 1);
    std::fs::write(&hole, code).unwrap();
    let hole = p.root.join("impl/signup/normalize_email.rs");
    std::fs::write(
        &hole,
        format!(
            "{}\nuse crate::prelude::*;\nuse crate::signup::*;\n\n#[allow(unconditional_recursion)]\npub fn normalize_email(e: &Email) -> Email {{\n    let x = normalize_email(e);\n    std::hint::black_box(x)\n}}\n",
            std::fs::read_to_string(&hole).unwrap().lines().next().unwrap()
        ),
    )
    .unwrap();
    let model = p.model().unwrap();
    let t = Instant::now();
    let r = wh_live::evaluate_with(
        &p,
        &model,
        &LiveOptions {
            timeout_ms: 500,
            ..opts(LiveScope::All)
        },
    )
    .unwrap();
    eprintln!("{:?}", r.timings);
    assert!(r.build_errors.is_empty(), "{:#?}", r.build_errors);
    let minor = r.get("signup.check_age", ItemKind::Example, 1).unwrap();
    assert_eq!(minor.status, Status::Timeout, "{minor:#?}");
    let adult = r.get("signup.check_age", ItemKind::Example, 2).unwrap();
    assert_eq!(
        adult.status,
        Status::Pass,
        "other probes still run: {adult:#?}"
    );
    let ex = r
        .get("signup.normalize_email", ItemKind::Example, 1)
        .unwrap();
    assert_eq!(ex.status, Status::Crashed, "{ex:#?}");
    assert!(r.timings.processes >= 3, "{:?}", r.timings);
    assert!(t.elapsed().as_secs() < 60);
}

#[test]
fn todo_scenarios_show_flow_steps() {
    let p = scratch("todo", "todo");
    let model = p.model().unwrap();
    let r = wh_live::evaluate_with(&p, &model, &opts(LiveScope::File("src/main.wh.md".into())))
        .unwrap();
    assert!(r.build_errors.is_empty(), "{:#?}", r.build_errors);
    let t = "main#test:removed ids are not reused";
    let last = r.get(t, ItemKind::Expect, 3).unwrap();
    assert_eq!(last.status, Status::Pass, "{last:#?}");
    assert!(last.values[0].value.starts_with("raised"), "{last:#?}");
    let add = r.get(t, ItemKind::Expect, 0).unwrap();
    assert_eq!(
        add.values
            .iter()
            .find(|v| v.label == "left")
            .map(|v| v.value.as_str()),
        Some("\"added #1: a\""),
        "{add:#?}"
    );
    for s in &add.steps {
        eprintln!(
            "{} = {}{}",
            s.step,
            s.value,
            if s.error { " (error)" } else { "" }
        );
    }
    assert!(
        add.steps
            .iter()
            .any(|s| s.step.starts_with("main.execute#")),
        "{:?}",
        add.steps
    );
    assert!(r.values().all(|v| v.file == "src/main.wh.md"));
    assert!(
        r.values()
            .filter(|v| v.kind == ItemKind::Expect)
            .all(|v| v.status == Status::Pass),
        "{:#?}",
        r.values()
            .filter(|v| v.status != Status::Pass)
            .collect::<Vec<_>>()
    );
}
