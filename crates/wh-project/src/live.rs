//! The live pipeline: everything the IDE runs in the background on the
//! current (possibly unsaved) model — generate, check, build, run with trace
//! capture — with results mapped to nodes.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Serialize;
use wh_model::Model;

use crate::Project;

/// One event from a running app's `WH_TRACE_FILE`, mapped to its node.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct TraceRecord {
    pub level: String,
    /// `wh.node` of the innermost span carrying one.
    pub node: Option<String>,
    pub message: String,
    /// `time.busy` for span-close events.
    pub duration: Option<Duration>,
}

fn parse_duration(s: &str) -> Option<Duration> {
    let s = s.trim();
    let (num, unit) = s.split_at(s.find(|c: char| !(c.is_ascii_digit() || c == '.'))?);
    let v: f64 = num.parse().ok()?;
    let secs = match unit {
        "ns" => v / 1e9,
        "µs" | "us" => v / 1e6,
        "ms" => v / 1e3,
        "s" => v,
        _ => return None,
    };
    Some(Duration::from_secs_f64(secs))
}

/// Parse one JSON line written by `tracing-subscriber`'s JSON formatter.
pub fn parse_trace_line(line: &str) -> Option<TraceRecord> {
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    let level = v["level"].as_str().unwrap_or("INFO").to_string();
    let fields = &v["fields"];
    let message = fields["message"].as_str().unwrap_or("").to_string();
    let node = fields["wh.node"]
        .as_str()
        .or_else(|| v["span"]["wh.node"].as_str())
        .or_else(|| v["spans"].as_array()?.iter().rev().find_map(|s| s["wh.node"].as_str()))
        .map(String::from);
    let duration = fields["time.busy"].as_str().and_then(parse_duration);
    let message = if message == "close" {
        let name = v["span"]["name"].as_str().unwrap_or("span");
        format!("{name} finished")
    } else {
        message
    };
    Some(TraceRecord { level, node, message, duration })
}

/// Generate the crate for the live model into `.wh/gen`.
pub fn generate(project: &Project, model: &Model) -> Result<wh_codegen::Generated> {
    project.generate(model)
}

/// Generate and `cargo check` the live model.
pub fn check(project: &Project, model: &Model) -> Result<(wh_codegen::Generated, wh_build::BuildResult)> {
    let g = generate(project, model)?;
    let (dir, target) = (project.gen_dir(), project.target_dir());
    let cargo = wh_build::Cargo { crate_dir: &dir, target_dir: &target, manifest: &g.manifest };
    let r = cargo.check()?;
    Ok((g, r))
}

/// Generate and run the specs of one node (or all).
pub fn test(project: &Project, model: &Model, node: Option<&str>) -> Result<wh_build::BuildResult> {
    let g = generate(project, model)?;
    let (dir, target) = (project.gen_dir(), project.target_dir());
    let cargo = wh_build::Cargo { crate_dir: &dir, target_dir: &target, manifest: &g.manifest };
    Ok(cargo.test(node)?)
}

#[derive(Debug, Clone)]
pub struct RunOutput {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
    pub trace: Vec<TraceRecord>,
}

/// Build the app's binary from the live model and run it with trace capture.
pub fn run_app(project: &Project, model: &Model, app: Option<&str>, args: &[String]) -> Result<RunOutput> {
    let g = generate(project, model)?;
    let missing = wh_build::unimplemented_holes(&g.manifest);
    if !missing.is_empty() {
        bail!(
            "{} hole(s) are not implemented yet ({}); synthesize them first",
            missing.len(),
            missing.iter().map(|h| h.node.as_str()).collect::<Vec<_>>().join(", ")
        );
    }
    let (dir, target) = (project.gen_dir(), project.target_dir());
    let cargo = wh_build::Cargo { crate_dir: &dir, target_dir: &target, manifest: &g.manifest };
    let built = cargo.build(false)?;
    if !built.success {
        let first = built.errors().next().map(|d| d.rendered.clone()).unwrap_or_default();
        bail!("the app does not build:\n{first}");
    }
    let bin_name = match app {
        Some(id) => g.manifest.bins.get(id).cloned().context("no such app")?,
        None => g.manifest.bins.values().next().cloned().context("the project has no `app`")?,
    };
    // binaries live in the shared target dir; run this project's installed copy
    let installed = project.install_bins(&built.artifacts)?;
    let exe: PathBuf = installed
        .into_iter()
        .find(|p| p.file_name().is_some_and(|n| n.to_string_lossy() == bin_name))
        .unwrap_or_else(|| target.join("debug").join(&bin_name));
    let trace_file = project.root.join(".wh/trace.jsonl");
    let _ = std::fs::remove_file(&trace_file);
    let out = Command::new(&exe)
        .args(args)
        .current_dir(&project.root)
        .env("WH_TRACE_FILE", &trace_file)
        .output()
        .with_context(|| format!("running {}", exe.display()))?;
    let trace = read_trace(&trace_file);
    Ok(RunOutput {
        success: out.status.success(),
        stdout: String::from_utf8_lossy(&out.stdout).into(),
        stderr: String::from_utf8_lossy(&out.stderr).into(),
        trace,
    })
}

pub fn read_trace(path: &Path) -> Vec<TraceRecord> {
    std::fs::read_to_string(path).unwrap_or_default().lines().filter_map(parse_trace_line).collect()
}

/// In-memory generation, memoized on the generator fingerprint (the inspector
/// asks for one node at a time; generate once per change, not once per node).
fn generate_memo(project: &Project, model: &Model) -> std::sync::Arc<wh_codegen::Generated> {
    static MEMO: std::sync::Mutex<Option<std::sync::Arc<wh_codegen::Generated>>> = std::sync::Mutex::new(None);
    let opts = project.gen_options();
    let fp = wh_codegen::fingerprint(model, &opts);
    let mut memo = MEMO.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(g) = memo.as_ref().filter(|g| g.fingerprint == fp) {
        return g.clone();
    }
    let g = std::sync::Arc::new(wh_codegen::generate(model, &opts));
    *memo = Some(g.clone());
    g
}

/// The generated Rust of one node (its region in the generated module file).
pub fn generated_rust(project: &Project, model: &Model, node: &str) -> Option<String> {
    let g = generate_memo(project, model);
    let node_module = model.nodes.get(node).map(|n| n.module.clone()).or_else(|| {
        // flow steps: `module.flow#…`
        let base = node.split('#').next()?;
        model.nodes.get(base).map(|n| n.module.clone())
    })?;
    let file = format!("src/modules/{}.rs", wh_codegen::rust::module_ident(&node_module));
    let code = g.files.get(&file)?;
    let regions = g.manifest.regions.get(&file)?;
    let base = node.split('#').next().unwrap_or(node);
    let i = regions.iter().position(|(_, n)| n == base)?;
    let start = regions[i].0;
    let end = regions.get(i + 1).map_or(usize::MAX, |r| r.0);
    let lines: Vec<&str> = code.lines().collect();
    let mut s = lines.get(start..end.min(lines.len()))?.join("\n");
    if let Some(cut) = s.find("\npub(crate) mod imp {") {
        s.truncate(cut);
    }
    if let Some(cut) = s.find("\n#[cfg(test)]") {
        s.truncate(cut);
    }
    Some(s.trim_end().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trace_lines_map_to_nodes() {
        let line = r#"{"timestamp":"t","level":"DEBUG","fields":{"message":"close","time.busy":"1.62µs","time.idle":"3µs"},"target":"signup::main","span":{"name":"main.parse_cli","wh.node":"main.parse_cli"},"spans":[{"name":"main.signup_cli","wh.node":"main.signup_cli"},{"name":"main.parse_cli","wh.node":"main.parse_cli"}]}"#;
        let r = parse_trace_line(line).unwrap();
        assert_eq!(r.node.as_deref(), Some("main.parse_cli"));
        assert_eq!(r.message, "main.parse_cli finished");
        assert_eq!(r.duration, Some(Duration::from_nanos(1620)));
        let warn = r#"{"level":"WARN","fields":{"message":"invalid value","wh.node":"signup.Email"},"spans":[]}"#;
        assert_eq!(parse_trace_line(warn).unwrap().node.as_deref(), Some("signup.Email"));
    }

    #[test]
    fn generated_rust_of_a_node() {
        let root = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples/signup"));
        let p = Project::load(root).unwrap();
        let m = p.model().unwrap();
        let code = generated_rust(&p, &m, "signup.normalize_email").unwrap();
        assert!(code.contains("pub fn normalize_email(e: &Email) -> Email"), "{code}");
        assert!(code.contains("contract_violation"));
        let flow = generated_rust(&p, &m, "signup.register#validate[0]").unwrap();
        assert!(flow.contains("pub fn register("));
    }
}
