//! The `mutate score >= x` gate, using `cargo-mutants` scoped to one hole
//! file. `cargo-mutants` is never installed automatically: without it the
//! gate reports [`MutationStatus::NotInstalled`] and is skipped.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use wh_codegen::GenOptions;
use wh_model::Model;
use wh_project::Project;

use crate::gates::{GateResult, GateStatus};
use crate::measure::{run_with_timeout, tail};
use crate::sandbox::{copy_tree, slug};
use crate::spec::PerfSpec;

pub const INSTALL_HINT: &str = "cargo install --locked cargo-mutants";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum MutationStatus {
    /// `cargo mutants` is not available; install it with [`INSTALL_HINT`].
    NotInstalled { hint: String },
    /// cargo-mutants generated no mutants for the hole file.
    NoMutants { detail: String },
    Scored {
        caught: u64,
        missed: u64,
        timeout: u64,
        unviable: u64,
        /// (caught + timeout) / (caught + missed + timeout)
        score: f64,
        /// Descriptions of the mutants the tests did not catch.
        missed_mutants: Vec<String>,
    },
    Failed { reason: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MutationReport {
    pub node: String,
    pub status: MutationStatus,
    /// The node's `mutate` clause checked against the score, if it has one.
    pub gate: Option<GateResult>,
}

impl MutationReport {
    pub fn score(&self) -> Option<f64> {
        match &self.status {
            MutationStatus::Scored { score, .. } => Some(*score),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct MutationOptions {
    pub target_dir: Option<PathBuf>,
    pub timeout_secs: u64,
    /// Test this code instead of the locked implementation (used for
    /// optimization candidates).
    pub code: Option<String>,
}

impl Default for MutationOptions {
    fn default() -> Self {
        Self { target_dir: None, timeout_secs: 1800, code: None }
    }
}

/// Is `cargo mutants` available?
pub fn cargo_mutants_version() -> Option<String> {
    let out = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into())).args(["mutants", "--version"]).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Mutation score of a node's implementation, checked against its `mutate` clause.
pub fn mutation_score(project: &Project, node: &str) -> Result<MutationReport> {
    let model = project.model()?;
    mutation_score_with(project, &model, node, &MutationOptions::default())
}

pub fn mutation_score_with(project: &Project, model: &Model, node: &str, opts: &MutationOptions) -> Result<MutationReport> {
    let status = if cargo_mutants_version().is_none() {
        MutationStatus::NotInstalled { hint: INSTALL_HINT.into() }
    } else {
        run(project, model, node, opts).unwrap_or_else(|e| MutationStatus::Failed { reason: format!("{e:#}") })
    };
    let gate = PerfSpec::of(model, node).and_then(|s| s.mutate).map(|g| match &status {
        MutationStatus::Scored { score, .. } => GateResult {
            gate: g.text.clone(),
            status: if g.passes(*score) { GateStatus::Pass } else { GateStatus::Fail },
            detail: format!("mutation score {score:.2}"),
        },
        MutationStatus::NotInstalled { hint } => {
            GateResult { gate: g.text.clone(), status: GateStatus::Skipped, detail: format!("cargo-mutants is not installed ({hint})") }
        }
        MutationStatus::NoMutants { detail } | MutationStatus::Failed { reason: detail } => {
            GateResult { gate: g.text.clone(), status: GateStatus::Skipped, detail: detail.clone() }
        }
    });
    Ok(MutationReport { node: node.to_string(), status, gate })
}

fn run(project: &Project, model: &Model, node: &str, opts: &MutationOptions) -> Result<MutationStatus> {
    let base = project.gen_options();
    let hole = wh_codegen::generate(model, &base).manifest.holes.into_iter().find(|h| h.node == node).context("not a hole")?;
    let rel = hole.path.strip_prefix(project.impl_dir()).context("hole outside impl/")?.to_path_buf();
    let work = project.root.join(".wh/mutants").join(slug(node));
    let _ = std::fs::remove_dir_all(&work);
    // hole files live inside the crate tree, so cargo-mutants can find them
    let crate_dir = work.join("gen");
    let impl_root = crate_dir.join("wh_impl");
    copy_tree(&project.impl_dir(), &impl_root)?;
    if let Some(code) = &opts.code {
        std::fs::write(impl_root.join(&rel), code)?;
    }
    let gopts = GenOptions {
        package: format!("{}_mut_{}", base.package, slug(node)),
        crate_name: base.crate_name.clone(),
        // relative: cargo-mutants rejects absolute `#[path]`s ("invalid filesystem traversal")
        impl_root: PathBuf::from("wh_impl"),
        contracts_in_release: false,
    };
    wh_codegen::write(&wh_codegen::generate(model, &gopts), &crate_dir)?;
    let target = opts.target_dir.clone().unwrap_or_else(|| project.target_dir());
    // cargo-mutants names the file by its unnormalized module path
    // (`src/modules/../../wh_impl/…`), so match it with a glob
    let file = Path::new("**/wh_impl").join(&rel);
    let mut c = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()));
    c.current_dir(&crate_dir)
        .env("CARGO_TARGET_DIR", &target)
        .args(["mutants", "--in-place"])
        .arg("--dir")
        .arg(&crate_dir)
        .arg("--file")
        .arg(&file)
        .arg("--output")
        .arg(&work);
    // exit codes 2/3 mean missed mutants/timeouts: the outcomes file tells
    let (_, stdout, stderr) = run_with_timeout(c, Duration::from_secs(opts.timeout_secs))?;
    let out = work.join("mutants.out");
    if stdout.contains("Found 0 mutants") || stderr.contains("No mutants found") {
        return Ok(MutationStatus::NoMutants { detail: format!("cargo-mutants found no mutants matching `--file {}`", file.display()) });
    }
    let Ok(text) = std::fs::read_to_string(out.join("outcomes.json")) else {
        return Ok(MutationStatus::Failed { reason: format!("cargo-mutants produced no outcomes:\n{}\n{}", tail(&stdout, 10), tail(&stderr, 20)) });
    };
    let missed_list: Vec<String> =
        std::fs::read_to_string(out.join("missed.txt")).unwrap_or_default().lines().map(str::to_string).filter(|l| !l.is_empty()).collect();
    Ok(parse_outcomes(&text, missed_list))
}

/// Interpret cargo-mutants' `outcomes.json`.
pub fn parse_outcomes(json: &str, missed_mutants: Vec<String>) -> MutationStatus {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else {
        return MutationStatus::Failed { reason: "unreadable outcomes.json".into() };
    };
    let count = |summary: &str| -> u64 {
        v["outcomes"].as_array().map_or(0, |a| a.iter().filter(|o| o["summary"].as_str() == Some(summary)).count() as u64)
    };
    let (caught, missed, timeout, unviable) = (
        v["caught"].as_u64().unwrap_or_else(|| count("CaughtMutant")),
        v["missed"].as_u64().unwrap_or_else(|| count("MissedMutant")),
        v["timeout"].as_u64().unwrap_or_else(|| count("Timeout")),
        v["unviable"].as_u64().unwrap_or_else(|| count("Unviable")),
    );
    let baseline_failed = v["outcomes"]
        .as_array()
        .is_some_and(|a| a.iter().any(|o| o["scenario"].as_str() == Some("Baseline") && o["summary"].as_str() != Some("Success")));
    if baseline_failed {
        return MutationStatus::Failed { reason: "the unmutated tests fail (cargo-mutants baseline)".into() };
    }
    let viable = caught + missed + timeout;
    if viable == 0 {
        return MutationStatus::NoMutants { detail: format!("no viable mutants ({unviable} unviable)") };
    }
    MutationStatus::Scored { caught, missed, timeout, unviable, score: (caught + timeout) as f64 / viable as f64, missed_mutants }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_outcomes() {
        let json = r#"{"outcomes":[{"scenario":"Baseline","summary":"Success"},{"scenario":{"Mutant":{}},"summary":"CaughtMutant"},{"scenario":{"Mutant":{}},"summary":"CaughtMutant"},{"scenario":{"Mutant":{}},"summary":"CaughtMutant"},{"scenario":{"Mutant":{}},"summary":"MissedMutant"},{"scenario":{"Mutant":{}},"summary":"Unviable"}]}"#;
        match parse_outcomes(json, vec!["x.rs:3:5: replace f -> Vec<Email> with vec![]".into()]) {
            MutationStatus::Scored { caught, missed, unviable, score, missed_mutants, .. } => {
                assert_eq!((caught, missed, unviable), (3, 1, 1));
                assert!((score - 0.75).abs() < 1e-9);
                assert_eq!(missed_mutants.len(), 1);
            }
            other => panic!("{other:?}"),
        }
        let failed = r#"{"outcomes":[{"scenario":"Baseline","summary":"Failure"}]}"#;
        assert!(matches!(parse_outcomes(failed, vec![]), MutationStatus::Failed { .. }));
    }
}
