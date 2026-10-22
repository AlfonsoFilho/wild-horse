//! End-to-end tests on a scratch copy of `examples/signup` (never the
//! example itself): auto-benches, gates, baselines and the optimization
//! tournament with a scripted agent.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::Result;
use wh_perf::{BaselinePolicy, BenchConfig, GateStatus, OptimizeOptions, PerfOptions, Stage, VariantVerdict};
use wh_project::Project;
use wh_synth::{AttemptOutcome, Driver, SynthesisRequest};

const NODE: &str = "signup.dedupe_emails";

fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

/// Dedicated target dir: keeps these builds away from the shared cache.
fn target_dir() -> PathBuf {
    workspace().join("target/wh-perf-tests/target")
}

/// Fresh copy of the signup example under `target/wh-perf-tests/<name>`.
fn scratch(name: &str) -> Project {
    let ws = workspace();
    let dir = ws.join("target/wh-perf-tests").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let ex = ws.join("examples/signup");
    wh_perf::sandbox::copy_tree(&ex.join("src"), &dir.join("src")).unwrap();
    wh_perf::sandbox::copy_tree(&ex.join("impl"), &dir.join("impl")).unwrap();
    std::fs::copy(ex.join("wildhorse.toml"), dir.join("wildhorse.toml")).unwrap();
    Project::load(&dir).unwrap()
}

fn hole_path(p: &Project) -> PathBuf {
    p.root.join("impl/signup/dedupe_emails.rs")
}

fn key_line(p: &Project) -> String {
    std::fs::read_to_string(hole_path(p)).unwrap().lines().next().unwrap().to_string()
}

const HEADER: &str = "use crate::prelude::*;\nuse crate::signup::*;\n\n";

/// Correct and linear, but re-validates every address with the regex and
/// clones it twice.
const SLOW_LINEAR: &str = r#"use std::collections::HashSet;

pub fn dedupe_emails(emails: &[Email]) -> Vec<Email> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut result = Vec::new();
    for email in emails {
        let checked = Email::parse(email.as_inner().clone()).expect("an Email is valid");
        if seen.insert(checked.as_inner().clone()) {
            result.push(checked);
        }
    }
    result
}
"#;

/// Correct, but quadratic.
const QUADRATIC: &str = r#"pub fn dedupe_emails(emails: &[Email]) -> Vec<Email> {
    let mut result: Vec<Email> = Vec::new();
    for email in emails {
        if !result.contains(email) {
            result.push(email.clone());
        }
    }
    result
}
"#;

/// Fast, but keeps duplicates.
const WRONG: &str = r#"pub fn dedupe_emails(emails: &[Email]) -> Vec<Email> {
    emails.to_vec()
}
"#;

/// Fast and correct, but traces every dropped address (a `sensitive` value).
const LEAKY: &str = r#"use std::collections::HashSet;

pub fn dedupe_emails(emails: &[Email]) -> Vec<Email> {
    let mut seen: HashSet<&Email> = HashSet::with_capacity(emails.len());
    let mut result = Vec::with_capacity(emails.len());
    for email in emails {
        if seen.insert(email) {
            result.push(email.clone());
        } else {
            tracing::trace!(email = %email, "dropping duplicate");
        }
    }
    result
}
"#;

/// Fast and correct.
const FAST: &str = r#"use std::collections::HashSet;

pub fn dedupe_emails(emails: &[Email]) -> Vec<Email> {
    let mut seen: HashSet<&Email> = HashSet::with_capacity(emails.len());
    let mut result = Vec::with_capacity(emails.len());
    for email in emails {
        if seen.insert(email) {
            result.push(email.clone());
        }
    }
    result
}
"#;

fn install(p: &Project, body: &str) {
    std::fs::write(hole_path(p), format!("{}\n{HEADER}{body}", key_line(p))).unwrap();
}

/// A fake compiler agent that writes the next scripted variant.
struct Scripted {
    variants: Vec<&'static str>,
    prompts: Mutex<Vec<String>>,
}

impl Driver for Scripted {
    fn id(&self) -> String {
        "scripted".into()
    }

    fn attempt(&self, req: &SynthesisRequest) -> Result<AttemptOutcome> {
        let mut prompts = self.prompts.lock().unwrap();
        let body = self.variants[prompts.len() % self.variants.len()];
        prompts.push(req.prompt.clone());
        std::fs::write(&req.hole_file, format!("{HEADER}{body}"))?;
        Ok(AttemptOutcome { transcript: format!("wrote variant {}", prompts.len()) })
    }
}

#[test]
fn auto_bench_gates_and_baselines() {
    let p = scratch("bench");
    let model = p.model().unwrap();
    let opts = PerfOptions {
        bench: BenchConfig::quick(),
        target_dir: Some(target_dir()),
        nodes: Some(vec![NODE.into(), "signup.normalize_email".into(), "signup.save_user".into()]),
        baselines: BaselinePolicy::IfMissing,
        ..Default::default()
    };
    let report = wh_perf::bench_project(&p, &model, &opts).unwrap();
    assert!(report.skipped.iter().any(|s| s.node == "signup.save_user"), "{:?}", report.skipped);
    let node = report.nodes.iter().find(|n| n.node == NODE).expect("dedupe_emails measured");
    let sizes: Vec<u64> = node.measurements.iter().map(|m| m.size).collect();
    assert_eq!(sizes, vec![64, 256, 1024, 4096]);
    for m in &node.measurements {
        assert!(m.mean.lower <= m.mean.point && m.mean.point <= m.mean.upper, "{m:?}");
        assert!(m.samples.len() >= 10);
        // one clone per address plus the set and the result vector
        assert!(m.allocs.unwrap() >= m.size, "{m:?}");
    }
    let gate = node.gates.iter().find(|g| g.gate.starts_with("complexity")).expect("complexity gate");
    assert_eq!(gate.status, GateStatus::Pass, "{gate:?} {:?}", node.complexity);
    assert!(node.baseline_recorded && node.baseline.is_none());
    let normalize = report.nodes.iter().find(|n| n.node == "signup.normalize_email").expect("normalize_email measured");
    assert_eq!(normalize.measurements.len(), 1, "a def without a size parameter runs at one size");
    assert!(wh_perf::baseline::baseline_path(&p.root, NODE).exists());
    let json = serde_json::to_string(&report).unwrap();
    let back: wh_perf::PerfReport = serde_json::from_str(&json).unwrap();
    assert_eq!(back.nodes.len(), report.nodes.len());

    // the synthesis-gate check: a quadratic candidate in a scratch impl root fails
    let cand = workspace().join("target/wh-perf-tests/bench-candidate");
    let _ = std::fs::remove_dir_all(&cand);
    wh_perf::sandbox::copy_tree(&p.impl_dir(), &cand).unwrap();
    std::fs::write(cand.join("signup/dedupe_emails.rs"), format!("{}\n{HEADER}{QUADRATIC}", key_line(&p))).unwrap();
    let failures = wh_perf::check_against_baseline(&p, &model, NODE, &cand, &opts).unwrap().expect("dedupe_emails has a baseline");
    assert!(failures.iter().any(|f| f.contains("complexity O(n)")), "{failures:?}");
    assert!(failures.iter().any(|f| f.contains("regression")), "{failures:?}");
    assert_eq!(wh_perf::check_against_baseline(&p, &model, "signup.validate", &cand, &opts).unwrap(), None, "no baseline: nothing to check");

    // a quadratic implementation fails the complexity gate and regresses against the baseline
    install(&p, QUADRATIC);
    let report = wh_perf::bench_project(&p, &model, &PerfOptions { nodes: Some(vec![NODE.into()]), ..opts }).unwrap();
    let node = &report.nodes[0];
    let gate = node.gates.iter().find(|g| g.gate.starts_with("complexity")).unwrap();
    assert_eq!(gate.status, GateStatus::Fail, "{gate:?}");
    let base = node.baseline.as_ref().expect("compared with the baseline");
    assert!(base.regressed, "{}", base.summary);
    assert!(!report.passed());
}

#[test]
fn tournament_accepts_only_the_fast_correct_variant() {
    let p = scratch("optimize");
    install(&p, SLOW_LINEAR);
    let key = key_line(&p);
    let model = p.model().unwrap();
    let driver = Scripted { variants: vec![QUADRATIC, WRONG, LEAKY, FAST], prompts: Mutex::new(vec![]) };
    let opts = OptimizeOptions {
        variants: 4,
        rounds: 1,
        bench: BenchConfig::quick(),
        target_dir: Some(target_dir()),
        ..Default::default()
    };
    let report = wh_perf::optimize(&p, &model, NODE, &driver, &opts).unwrap();
    let verdicts: Vec<(&str, &Option<VariantVerdict>)> = report.candidates.iter().map(|c| (c.label.as_str(), &c.verdict)).collect();
    assert_eq!(report.candidates.len(), 4, "{verdicts:?}");
    let stage = |i: usize| match &report.candidates[i].verdict {
        Some(VariantVerdict::Rejected { stage, .. }) => Some(*stage),
        _ => None,
    };
    assert_eq!(stage(0), Some(Stage::Gates), "the quadratic variant fails `complexity O(n)`: {verdicts:?}");
    assert_eq!(stage(1), Some(Stage::Tests), "the wrong variant fails the specs: {verdicts:?}");
    assert_eq!(stage(2), Some(Stage::Privacy), "the leaking variant traces emails: {verdicts:?}");
    assert_eq!(report.candidates[3].verdict, Some(VariantVerdict::Winner), "{verdicts:?}");
    assert_eq!(report.accepted.as_deref(), Some("r1v4"));
    assert!(report.speedup.unwrap() > 1.03, "{:?}", report.notes);

    // the winner is locked with the key header, and the baseline updated
    let locked = std::fs::read_to_string(hole_path(&p)).unwrap();
    assert!(locked.starts_with(&format!("{key}\n")), "{locked}");
    assert!(locked.contains("HashSet<&Email>"), "{locked}");
    let base = wh_perf::baseline::load(&p.root, NODE, &report.fingerprint).expect("baseline recorded");
    assert_eq!(base.impl_hash, wh_perf::baseline::impl_hash(&locked));

    // the prompts carry the spec, the current code, the numbers and the target
    let prompts = driver.prompts.lock().unwrap();
    assert!(prompts[0].contains("complexity O(n)") && prompts[0].contains("Email::parse") && prompts[0].contains("| auto | 4096 |"));
    assert!(prompts[0].contains("speed (mean time per call)"));
    assert!(prompts[0].contains("Never log, trace or format into errors the values of `sensitive` fields"));
    assert!(prompts[2].contains("r1v2: rejected at Tests"), "{}", prompts[2]);
}

#[test]
fn mutation_gate_scores_a_hole() {
    let p = scratch("mutants");
    if wh_perf::mutation::cargo_mutants_version().is_none() {
        let r = wh_perf::mutation_score(&p, NODE).unwrap();
        assert!(matches!(r.status, wh_perf::MutationStatus::NotInstalled { .. }), "{r:?}");
        assert!(r.gate.is_none(), "dedupe_emails has no `mutate` clause");
        return;
    }
    // give `validate` a mutation gate
    let src = p.root.join("src/signup.wh.md");
    let text = std::fs::read_to_string(&src).unwrap();
    let anchor = "    raises ValidationError when not (0 <= form.age <= 150)\n";
    assert!(text.contains(anchor));
    std::fs::write(&src, text.replacen(anchor, &format!("{anchor}    mutate score >= 0.5\n"), 1)).unwrap();
    let model = p.model().unwrap();
    let opts = wh_perf::mutation::MutationOptions { target_dir: Some(target_dir()), ..Default::default() };
    let r = wh_perf::mutation_score_with(&p, &model, "signup.validate", &opts).unwrap();
    // regression: a `--file` filter that matches nothing silently yields no mutants
    let wh_perf::MutationStatus::Scored { caught, missed, timeout, score, .. } = &r.status else { panic!("{r:?}") };
    assert!(caught + missed + timeout > 0, "{r:?}");
    assert!(*score >= 0.5, "{r:?}");
    let gate = r.gate.as_ref().expect("the `mutate` clause is checked");
    assert_eq!(gate.status, GateStatus::Pass, "{gate:?}");
}

#[test]
fn profile_ranks_holes_by_traced_time() {
    let p = scratch("profile");
    let toml = p.root.join("wildhorse.toml");
    let text = std::fs::read_to_string(&toml).unwrap();
    std::fs::write(&toml, format!("{text}\n[perf]\nworkload = [[\"a@b.io\", \"30\"], [\"kid@x.io\", \"12\"]]\n")).unwrap();
    let model = p.model().unwrap();
    let mut opts = wh_perf::ProfileOptions::from_project(&p);
    assert_eq!(opts.app_runs.len(), 2);
    opts.target_dir = Some(target_dir());
    let profile = wh_perf::hot_holes_with(&p, &model, None, &opts).unwrap();
    assert_eq!(profile.workload, ["scenario tests", "signup_cli a@b.io 30", "signup_cli kid@x.io 12"], "{:?}", profile.notes);
    for h in &profile.holes {
        eprintln!("{:>6.2}%  {:>5} calls  {}", h.share * 100.0, h.calls, h.node);
    }
    let names: Vec<&str> = profile.holes.iter().map(|h| h.node.as_str()).collect();
    for n in ["signup.validate", "signup.normalize_email", "main.parse_cli", "signup.MemoryDb"] {
        assert!(names.contains(&n), "{n} missing from {names:?}");
    }
    assert!(profile.holes.windows(2).all(|w| w[0].self_ns >= w[1].self_ns));
    let total_share: f64 = profile.holes.iter().map(|h| h.share).sum();
    assert!(total_share > 0.0 && total_share <= 1.0 + 1e-9, "{total_share}");
    assert_eq!(wh_perf::optimization_queue(&model, Some(&profile)), [NODE], "only dedupe_emails says `optimize`");
}

#[test]
fn pgo_reports_missing_tool() {
    let tools = wh_perf::pgo::cargo_pgo_version().is_some() && wh_perf::pgo::llvm_profdata().is_some();
    if tools {
        return; // installed: exercised by `wh build --release --pgo`
    }
    let p = scratch("pgo");
    let model = p.model().unwrap();
    let r = wh_perf::pgo_build(&p, &model, None).unwrap();
    let wh_perf::PgoStatus::NotInstalled { hint } = &r.status else { panic!("{r:?}") };
    if wh_perf::pgo::cargo_pgo_version().is_some() {
        assert!(hint.contains("llvm-tools-preview") && !hint.contains("cargo install"), "{hint}");
    }
}

#[test]
fn privacy_heuristic_on_signup_impls() {
    let p = scratch("privacy");
    let model = p.model().unwrap();
    let holes = wh_codegen::generate(&model, &p.gen_options()).manifest.holes;
    let mut flagged = Vec::new();
    for h in &holes {
        let code = std::fs::read_to_string(&h.path).unwrap();
        for l in wh_perf::privacy::sensitive_logging(&model, &h.node, &code) {
            eprintln!("{}: {}", h.node, l.message());
            flagged.push((h.node.clone(), l.value.clone()));
        }
    }
    // the example's dedupe_emails traces each dropped address
    assert!(flagged.iter().any(|(n, v)| n == NODE && v == "email"), "{flagged:?}");
}
