//! The synthesis pipeline with a scripted fake agent.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use wh_model::{Model, SourceInput};
use wh_synth::{AttemptOutcome, Driver, HoleState, Progress, SynthesisRequest, Synthesizer};

fn root() -> PathBuf {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../..")).canonicalize().unwrap()
}

fn fixture(rel: &str) -> String {
    std::fs::read_to_string(root().join("crates/wh-codegen/tests/fixtures/signup_impl").join(rel)).unwrap()
}

/// Plays back scripted attempts per node.
struct Scripted {
    log: Mutex<Vec<(String, usize, String)>>,
}

impl Driver for Scripted {
    fn id(&self) -> String {
        "scripted-test-agent".into()
    }

    fn attempt(&self, req: &SynthesisRequest) -> anyhow::Result<AttemptOutcome> {
        self.log.lock().unwrap().push((req.node.clone(), req.round, req.prompt.clone()));
        let code = match (req.node.as_str(), req.round) {
            // first attempt violates `ensures result == e.lower()`
            ("signup.normalize_email", 1) => {
                "use crate::prelude::*;\nuse crate::signup::*;\n\npub fn normalize_email(e: &Email) -> Email {\n    Email::parse(e.to_uppercase()).unwrap()\n}\n".to_string()
            }
            ("signup.normalize_email", _) => fixture("signup/normalize_email.rs"),
            ("signup.dedupe_emails", 1) => {
                // escapes the sandbox: must be discarded
                std::fs::write(req.workdir.join("gen/src/lib.rs"), "// tampered").unwrap();
                fixture("signup/dedupe_emails.rs")
            }
            ("signup.dedupe_emails", _) => fixture("signup/dedupe_emails.rs"),
            _ => anyhow::bail!("unexpected job {}", req.node),
        };
        std::fs::write(&req.hole_file, code)?;
        Ok(AttemptOutcome { transcript: format!("round {}", req.round) })
    }
}

#[test]
fn synthesis_repairs_verifies_and_locks() {
    let project = root().join("target/wh-synth-tests/signup");
    let _ = std::fs::remove_dir_all(project.join("impl"));
    let _ = std::fs::remove_dir_all(project.join(".wh/synth"));
    std::fs::create_dir_all(&project).unwrap();
    let src = root().join("examples/signup/src");
    let inputs: Vec<SourceInput> = ["signup.wh.md", "main.wh.md"]
        .iter()
        .map(|f| SourceInput { path: f.to_string(), text: std::fs::read_to_string(src.join(f)).unwrap() })
        .collect();
    let model = Model::build(&inputs);
    let opts = wh_codegen::GenOptions { package: "signup".into(), crate_name: "signup".into(), impl_root: project.join("impl"), contracts_in_release: false };
    let g = wh_codegen::generate(&model, &opts);
    wh_codegen::write(&g, &project.join(".wh/gen")).unwrap(); // creates stub holes

    let driver = Scripted { log: Mutex::new(vec![]) };
    let events = Mutex::new(Vec::new());
    let on_progress = |p: Progress| events.lock().unwrap().push(format!("{p:?}"));
    let target = root().join("target/wh-synth-tests/target");
    let synth = Synthesizer {
        model: &model,
        root: &project,
        package: "wh_synth_test",
        target_dir: &target,
        cache: None,
        max_rounds: 3,
        concurrency: 2,
        driver: &driver,
        on_progress: &on_progress,
    };

    let mut jobs = synth.plan(&g.manifest.holes, None, false);
    assert_eq!(jobs.len(), g.manifest.holes.len(), "all holes start missing");
    jobs.retain(|h| h.node == "signup.normalize_email" || h.node == "signup.dedupe_emails");
    let reports = synth.run(jobs);

    for r in &reports {
        assert!(r.accepted, "{} not accepted: {:#?}", r.node, r.rounds);
        assert_eq!(r.rounds.len(), 2, "{}: first round must be rejected", r.node);
    }
    let ne = reports.iter().find(|r| r.node == "signup.normalize_email").unwrap();
    let wh_synth::Verdict::Rejected(why) = &ne.rounds[0].verdict else { panic!() };
    assert!(why.contains("ensures violated"), "{why}");
    let dd = reports.iter().find(|r| r.node == "signup.dedupe_emails").unwrap();
    let wh_synth::Verdict::Rejected(why) = &dd.rounds[0].verdict else { panic!() };
    assert!(why.contains("other than the hole file"), "{why}");

    // the repair round saw the failure report
    let log = driver.log.lock().unwrap();
    let second = log.iter().find(|(n, r, _)| n == "signup.normalize_email" && *r == 2).unwrap();
    assert!(second.2.contains("previous attempt was rejected") && second.2.contains("ensures violated"));
    assert!(second.2.contains("Canonical form used for uniqueness checks"), "prompt carries the prose");
    drop(log);

    // accepted files are locked with their key; a second plan skips them
    let hole = g.manifest.holes.iter().find(|h| h.node == "signup.normalize_email").unwrap();
    let text = std::fs::read_to_string(&hole.path).unwrap();
    assert!(text.starts_with(wh_synth::KEY_PREFIX));
    assert_eq!(wh_synth::hole_state(hole, &synth.key(&hole.node)), HoleState::UpToDate);
    let again = synth.plan(&g.manifest.holes, None, false);
    assert!(!again.iter().any(|h| h.node == "signup.normalize_email" || h.node == "signup.dedupe_emails"));

    // changing the spec makes exactly that hole stale
    let edited: Vec<SourceInput> = inputs
        .iter()
        .map(|i| SourceInput { path: i.path.clone(), text: i.text.replace("ensures result == e.lower()", "ensures result == e.lower()\n    ensures len(result) > 0") })
        .collect();
    let model2 = Model::build(&edited);
    let synth2 = Synthesizer { model: &model2, ..synth };
    assert!(matches!(wh_synth::hole_state(hole, &synth2.key(&hole.node)), HoleState::Stale { .. }));
    let dd_hole = g.manifest.holes.iter().find(|h| h.node == "signup.dedupe_emails").unwrap();
    assert_eq!(wh_synth::hole_state(dd_hole, &synth2.key(&dd_hole.node)), HoleState::UpToDate);
    assert!(events.lock().unwrap().iter().any(|e| e.starts_with("Accepted")));
}

/// Always writes the reference implementation; counts calls.
struct Counting {
    calls: Mutex<usize>,
}

impl Driver for Counting {
    fn id(&self) -> String {
        "counting-test-agent".into()
    }

    fn attempt(&self, req: &SynthesisRequest) -> anyhow::Result<AttemptOutcome> {
        *self.calls.lock().unwrap() += 1;
        let file = req.node.strip_prefix("signup.").unwrap();
        std::fs::write(&req.hole_file, fixture(&format!("signup/{file}.rs")))?;
        Ok(AttemptOutcome::default())
    }
}

#[test]
fn synthesis_cache_reuses_verified_implementations() {
    let project = root().join("target/wh-synth-tests/cached");
    let _ = std::fs::remove_dir_all(&project);
    std::fs::create_dir_all(&project).unwrap();
    let src = root().join("examples/signup/src");
    let inputs: Vec<SourceInput> = ["signup.wh.md", "main.wh.md"]
        .iter()
        .map(|f| SourceInput { path: f.to_string(), text: std::fs::read_to_string(src.join(f)).unwrap() })
        .collect();
    let model = Model::build(&inputs);
    let opts = wh_codegen::GenOptions { package: "cached".into(), crate_name: "cached".into(), impl_root: project.join("impl"), contracts_in_release: false };
    let g = wh_codegen::generate(&model, &opts);
    wh_codegen::write(&g, &project.join(".wh/gen")).unwrap();

    let cache = wh_synth::cache::SynthCache::new(project.join("cache"));
    let driver = Counting { calls: Mutex::new(0) };
    let restored = Mutex::new(Vec::new());
    let on_progress = |p: Progress| {
        if let Progress::Restored { node } = p {
            restored.lock().unwrap().push(node);
        }
    };
    let target = root().join("target/wh-synth-tests/target");
    let synth = Synthesizer {
        model: &model,
        root: &project,
        package: "wh_synth_cache_test",
        target_dir: &target,
        cache: Some(&cache),
        max_rounds: 2,
        concurrency: 2,
        driver: &driver,
        on_progress: &on_progress,
    };
    let only = |n: &str| synth.plan(&g.manifest.holes, Some(n), false);

    // 1. a miss: the agent runs once and the result is cached
    let r = synth.run(only("signup.normalize_email"));
    assert!(r[0].accepted && !r[0].from_cache);
    assert_eq!(*driver.calls.lock().unwrap(), 1);

    // 2. a fresh clone (impl/ gone): restored from the cache, no agent call
    let hole = g.manifest.holes.iter().find(|h| h.node == "signup.normalize_email").unwrap();
    let synthesized = std::fs::read_to_string(&hole.path).unwrap();
    std::fs::remove_dir_all(project.join("impl")).unwrap();
    wh_codegen::write(&g, &project.join(".wh/gen")).unwrap(); // stubs again
    let r = synth.run(only("signup.normalize_email"));
    assert!(r[0].accepted && r[0].from_cache, "{r:#?}");
    assert_eq!(*driver.calls.lock().unwrap(), 1, "no agent call on a cache hit");
    assert_eq!(std::fs::read_to_string(&hole.path).unwrap(), synthesized);
    assert_eq!(*restored.lock().unwrap(), ["signup.normalize_email"]);

    // 3. a poisoned entry fails verification: evicted and re-synthesized
    let key = wh_synth::cache::SynthCache::key(&synth.key(&hole.node), hole);
    let bad = synthesized.replace("pub fn normalize_email(e: &Email) -> Email {", "pub fn normalize_email(e: &Email) -> Email {\n    return Email::parse(e.to_uppercase()).unwrap();");
    let meta = cache.meta(&key).unwrap();
    cache.put(&key, &bad, &meta).unwrap();
    std::fs::write(&hole.path, &hole.template).unwrap();
    let r = synth.run(only("signup.normalize_email"));
    assert!(r[0].accepted && !r[0].from_cache, "{r:#?}");
    assert_eq!(*driver.calls.lock().unwrap(), 2);
    assert_ne!(cache.get(&key).unwrap(), bad, "the bad entry was replaced");
}

/// Writes the todo example's reference implementations; logs every attempt.
struct TodoAgent {
    log: Mutex<Vec<String>>,
}

impl Driver for TodoAgent {
    fn id(&self) -> String {
        "todo-test-agent".into()
    }

    fn attempt(&self, req: &SynthesisRequest) -> anyhow::Result<AttemptOutcome> {
        self.log.lock().unwrap().push(req.node.clone());
        let (module, name) = req.node.split_once('.').unwrap();
        let code = std::fs::read_to_string(root().join("examples/todo/reference_impl").join(module).join(format!("{name}.rs")))?;
        std::fs::write(&req.hole_file, code)?;
        Ok(AttemptOutcome::default())
    }
}

fn todo_project(name: &str) -> (PathBuf, Model, wh_codegen::Generated) {
    let project = root().join("target/wh-synth-tests").join(name);
    let _ = std::fs::remove_dir_all(&project);
    std::fs::create_dir_all(&project).unwrap();
    let src = root().join("examples/todo/src");
    let inputs: Vec<SourceInput> = std::fs::read_dir(&src)
        .unwrap()
        .flatten()
        .map(|e| SourceInput { path: e.file_name().to_string_lossy().into(), text: std::fs::read_to_string(e.path()).unwrap() })
        .collect();
    let model = Model::build(&inputs);
    let opts = wh_codegen::GenOptions { package: name.into(), crate_name: "todo".into(), impl_root: project.join("impl"), contracts_in_release: false };
    let g = wh_codegen::generate(&model, &opts);
    wh_codegen::write(&g, &project.join(".wh/gen")).unwrap();
    (project, model, g)
}

#[test]
fn spec_dependencies_follow_calls_in_specs() {
    let (_, model, _) = todo_project("todo-deps");
    let deps = wh_synth::spec_dependencies(&model, "tasks.add_task");
    assert!(deps.contains("tasks.new_list"), "{deps:?}");
    let deps = wh_synth::spec_dependencies(&model, "tasks.remove");
    assert!(deps.contains("tasks.new_list") && deps.contains("tasks.add_task"), "{deps:?}");
    assert!(!wh_synth::spec_dependencies(&model, "tasks.new_list").contains("tasks.new_list"));
}

#[test]
fn dependent_holes_are_scheduled_after_their_dependencies() {
    let (project, model, g) = todo_project("todo-order");
    let driver = TodoAgent { log: Mutex::new(vec![]) };
    let target = root().join("target/wh-synth-tests/target");
    let on_progress = |_: Progress| {};
    let synth = Synthesizer {
        model: &model,
        root: &project,
        package: "wh_synth_todo_order",
        target_dir: &target,
        cache: None,
        max_rounds: 3,
        concurrency: 3,
        driver: &driver,
        on_progress: &on_progress,
    };
    let jobs: Vec<_> = synth
        .plan(&g.manifest.holes, None, false)
        .into_iter()
        .filter(|h| ["tasks.remove", "tasks.add_task", "tasks.new_list"].contains(&h.node.as_str()))
        .collect();
    let reports = synth.run(jobs);
    for r in &reports {
        assert!(r.accepted, "{} failed: {:#?}", r.node, r.rounds);
        assert_eq!(r.rounds.len(), 1, "{}: no wasted rounds", r.node);
    }
    let log = driver.log.lock().unwrap();
    let pos = |n: &str| log.iter().position(|x| x == n).unwrap();
    assert!(pos("tasks.new_list") < pos("tasks.add_task") && pos("tasks.add_task") < pos("tasks.remove"), "{log:?}");
}

#[test]
fn missing_dependencies_block_instead_of_burning_rounds() {
    let (project, model, g) = todo_project("todo-blocked");
    let driver = TodoAgent { log: Mutex::new(vec![]) };
    let target = root().join("target/wh-synth-tests/target");
    let on_progress = |_: Progress| {};
    let synth = Synthesizer {
        model: &model,
        root: &project,
        package: "wh_synth_todo_blocked",
        target_dir: &target,
        cache: None,
        max_rounds: 4,
        concurrency: 1,
        driver: &driver,
        on_progress: &on_progress,
    };
    // add_task alone: its examples call new_list, which stays a stub
    let reports = synth.run(synth.plan(&g.manifest.holes, Some("tasks.add_task"), false));
    let r = &reports[0];
    assert!(!r.accepted);
    assert_eq!(driver.log.lock().unwrap().len(), 1, "one agent attempt, no repair rounds");
    assert!(r.notes.iter().any(|n| n.contains("blocked") && n.contains("tasks.new_list")), "{:?}", r.notes);
    assert!(matches!(r.rounds.last().map(|x| &x.verdict), Some(wh_synth::Verdict::Blocked(_))));
}

/// Writes an example's reference implementations.
struct ReferenceAgent {
    example: &'static str,
    log: Mutex<Vec<String>>,
}

impl Driver for ReferenceAgent {
    fn id(&self) -> String {
        format!("reference-{}", self.example)
    }

    fn attempt(&self, req: &SynthesisRequest) -> anyhow::Result<AttemptOutcome> {
        self.log.lock().unwrap().push(req.node.clone());
        let (module, name) = req.node.split_once('.').unwrap();
        let path = root().join("examples").join(self.example).join("reference_impl").join(module).join(format!("{name}.rs"));
        std::fs::write(&req.hole_file, std::fs::read_to_string(path)?)?;
        Ok(AttemptOutcome::default())
    }
}

#[test]
fn holes_whose_specs_call_each_other_are_verified_together() {
    let project = root().join("target/wh-synth-tests/pong-cycle");
    let _ = std::fs::remove_dir_all(&project);
    std::fs::create_dir_all(&project).unwrap();
    let src = root().join("examples/pong/src");
    let inputs: Vec<SourceInput> = std::fs::read_dir(&src)
        .unwrap()
        .flatten()
        .map(|e| SourceInput { path: e.file_name().to_string_lossy().into(), text: std::fs::read_to_string(e.path()).unwrap() })
        .collect();
    let model = Model::build(&inputs);
    // step's properties call simulate; simulate's *implementation* calls step:
    // a cycle that only shows at run time, as a stub panic
    assert!(wh_synth::spec_dependencies(&model, "game.step").contains("game.simulate"));
    assert!(!wh_synth::spec_dependencies(&model, "game.simulate").contains("game.step"));
    let opts = wh_codegen::GenOptions { package: "pong_cycle".into(), crate_name: "pong".into(), impl_root: project.join("impl"), contracts_in_release: false };
    let g = wh_codegen::generate(&model, &opts);
    wh_codegen::write(&g, &project.join(".wh/gen")).unwrap();
    let driver = ReferenceAgent { example: "pong", log: Mutex::new(vec![]) };
    let target = root().join("target/wh-synth-tests/target");
    let on_progress = |_: Progress| {};
    let synth = Synthesizer {
        model: &model,
        root: &project,
        package: "wh_synth_pong_cycle",
        target_dir: &target,
        cache: None,
        max_rounds: 3,
        concurrency: 2,
        driver: &driver,
        on_progress: &on_progress,
    };
    let jobs: Vec<_> = synth
        .plan(&g.manifest.holes, None, false)
        .into_iter()
        .filter(|h| ["game.step", "game.simulate", "game.new_game"].contains(&h.node.as_str()))
        .collect();
    let reports = synth.run(jobs);
    let summary: Vec<String> = reports.iter().map(|r| format!("{} accepted={} notes={:?} verdicts={:?}", r.node, r.accepted, r.notes, r.rounds.iter().map(|x| format!("{:?}", x.verdict).chars().take(300).collect::<String>()).collect::<Vec<_>>())).collect();
    for r in &reports {
        assert!(r.accepted, "{}\nlog: {:?}", summary.join("\n"), driver.log.lock().unwrap());
    }
    // one agent attempt per hole: the cycle is resolved by re-verification, not repair rounds
    assert_eq!(driver.log.lock().unwrap().len(), 3, "{:?}", driver.log.lock().unwrap());
}

#[test]
fn acceptance_hook_rejects_with_feedback() {
    let project = root().join("target/wh-synth-tests/hook");
    let _ = std::fs::remove_dir_all(&project);
    std::fs::create_dir_all(&project).unwrap();
    let src = root().join("examples/signup/src");
    let inputs: Vec<SourceInput> = ["signup.wh.md", "main.wh.md"]
        .iter()
        .map(|f| SourceInput { path: f.to_string(), text: std::fs::read_to_string(src.join(f)).unwrap() })
        .collect();
    let model = Model::build(&inputs);
    let opts = wh_codegen::GenOptions { package: "hook".into(), crate_name: "hook".into(), impl_root: project.join("impl"), contracts_in_release: false };
    let g = wh_codegen::generate(&model, &opts);
    wh_codegen::write(&g, &project.join(".wh/gen")).unwrap();
    let driver = Counting { calls: Mutex::new(0) };
    let target = root().join("target/wh-synth-tests/target");
    let on_progress = |_: Progress| {};
    let synth = Synthesizer {
        model: &model,
        root: &project,
        package: "wh_synth_hook_test",
        target_dir: &target,
        cache: None,
        max_rounds: 3,
        concurrency: 1,
        driver: &driver,
        on_progress: &on_progress,
    };
    let checks = Mutex::new(0);
    let hook = |_: &wh_codegen::HoleSpec, impl_root: &Path| -> anyhow::Result<Vec<String>> {
        assert!(impl_root.ends_with("impl"), "the hook sees the scratch impl root");
        let mut n = checks.lock().unwrap();
        *n += 1;
        Ok(if *n == 1 { vec!["auto/n=1024: 1.40× slower than the baseline".into()] } else { vec![] })
    };
    let r = synth.run_with(synth.plan(&g.manifest.holes, Some("signup.normalize_email"), false), Some(&hook));
    assert!(r[0].accepted, "{:#?}", r[0].rounds);
    assert_eq!(r[0].rounds.len(), 2);
    let wh_synth::Verdict::Rejected(why) = &r[0].rounds[0].verdict else { panic!("{:?}", r[0].rounds[0].verdict) };
    assert!(why.contains("fails the performance gate") && why.contains("1.40× slower"), "{why}");
    assert_eq!(*driver.calls.lock().unwrap(), 2);
}
