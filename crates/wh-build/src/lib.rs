//! Runs cargo on a generated crate and maps compiler diagnostics and test
//! results back to Wild Horse nodes.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::Serialize;
use wh_codegen::Manifest;

#[derive(Debug, Clone, Serialize)]
pub struct NodeDiagnostic {
    pub node: Option<String>,
    pub level: String,
    pub message: String,
    pub file: Option<String>,
    pub line: Option<usize>,
    pub column: Option<usize>,
    pub rendered: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct TestOutcome {
    pub name: String,
    pub node: Option<String>,
    pub passed: bool,
    /// Captured output of a failed test (panic message and trace).
    pub output: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct BuildResult {
    pub success: bool,
    pub diagnostics: Vec<NodeDiagnostic>,
    pub tests: Vec<TestOutcome>,
    /// Built binaries (for `build`).
    pub artifacts: Vec<PathBuf>,
}

impl BuildResult {
    pub fn errors(&self) -> impl Iterator<Item = &NodeDiagnostic> {
        self.diagnostics.iter().filter(|d| d.level == "error")
    }

    pub fn failed_tests(&self) -> impl Iterator<Item = &TestOutcome> {
        self.tests.iter().filter(|t| !t.passed)
    }
}

pub struct Cargo<'a> {
    pub crate_dir: &'a Path,
    pub target_dir: &'a Path,
    pub manifest: &'a Manifest,
}

impl<'a> Cargo<'a> {
    fn command(&self, args: &[&str]) -> Command {
        let mut c = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()));
        c.args(args).current_dir(self.crate_dir).env("CARGO_TARGET_DIR", self.target_dir);
        c
    }

    /// `cargo check` (including tests) with node-mapped diagnostics.
    pub fn check(&self) -> std::io::Result<BuildResult> {
        self.compile(&["check", "--tests", "--bins", "--message-format=json"])
    }

    /// `cargo clippy` with the synthesis lint gates (they are enabled on hole modules by codegen).
    pub fn clippy(&self) -> std::io::Result<BuildResult> {
        self.compile(&["clippy", "--tests", "--bins", "--message-format=json"])
    }

    pub fn build(&self, release: bool) -> std::io::Result<BuildResult> {
        let mut args = vec!["build", "--bins", "--message-format=json"];
        if release {
            args.push("--release");
        }
        self.compile(&args)
    }

    fn compile(&self, args: &[&str]) -> std::io::Result<BuildResult> {
        let mut child = self.command(args).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
        let stdout = child.stdout.take().unwrap();
        let mut result = BuildResult::default();
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { continue };
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else { continue };
            match v["reason"].as_str() {
                Some("compiler-message") => {
                    if let Some(d) = self.diagnostic(&v["message"]) {
                        result.diagnostics.push(d);
                    }
                }
                Some("compiler-artifact") => {
                    if let Some(exe) = v["executable"].as_str() {
                        result.artifacts.push(PathBuf::from(exe));
                    }
                }
                _ => {}
            }
        }
        let stderr = child.stderr.take().map(|s| std::io::read_to_string(s).unwrap_or_default()).unwrap_or_default();
        let status = child.wait()?;
        result.success = status.success();
        if !status.success() && result.errors().next().is_none() {
            // cargo itself failed (manifest, dependency resolution, …)
            result.diagnostics.push(NodeDiagnostic {
                node: None,
                level: "error".into(),
                message: stderr.lines().find(|l| l.starts_with("error")).unwrap_or("cargo failed").to_string(),
                file: None,
                line: None,
                column: None,
                rendered: stderr,
            });
        }
        Ok(result)
    }

    fn diagnostic(&self, m: &serde_json::Value) -> Option<NodeDiagnostic> {
        let level = m["level"].as_str()?.to_string();
        if level == "failure-note" {
            return None;
        }
        let message = m["message"].as_str().unwrap_or("").to_string();
        if message.starts_with("aborting due to") || message.contains("warnings emitted") || message.contains("warning emitted") {
            return None;
        }
        let spans = m["spans"].as_array().cloned().unwrap_or_default();
        let primary = spans.iter().find(|s| s["is_primary"].as_bool() == Some(true)).or(spans.first());
        let file = primary.and_then(|s| s["file_name"].as_str()).map(String::from);
        let line = primary.and_then(|s| s["line_start"].as_u64()).map(|l| l as usize);
        let column = primary.and_then(|s| s["column_start"].as_u64()).map(|c| c as usize);
        let node = match (&file, line) {
            (Some(f), Some(l)) => self.manifest.node_for(f, l).map(String::from),
            _ => None,
        };
        Some(NodeDiagnostic { node, level, message, file, line, column, rendered: m["rendered"].as_str().unwrap_or("").to_string() })
    }

    /// `cargo test`, optionally filtered to a node's tests.
    pub fn test(&self, node: Option<&str>) -> std::io::Result<BuildResult> {
        let compiled = self.compile(&["test", "--no-run", "--message-format=json"])?;
        if !compiled.success {
            return Ok(compiled);
        }
        let filter = node.map(wh_codegen::rust::test_prefix);
        // libtest must not be quiet: per-test lines carry the names we map to nodes
        let mut args = vec!["test", "--"];
        if let Some(f) = &filter {
            args.push(f);
        }
        args.push("--color=never");
        let out = self.command(&args).env("NO_COLOR", "1").output()?;
        let text = String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
        let mut result = compiled;
        result.tests = parse_libtest(&text, self.manifest);
        result.success = out.status.success();
        Ok(result)
    }
}

/// Parse libtest's human output into outcomes (failure output attached).
pub fn parse_libtest(text: &str, manifest: &Manifest) -> Vec<TestOutcome> {
    let mut tests: Vec<TestOutcome> = Vec::new();
    let mut failures_section: Vec<String> = Vec::new();
    let mut in_failures = false;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("test ") {
            if let Some((name, status)) = rest.rsplit_once(" ... ") {
                let passed = status.trim() == "ok";
                if status.trim() == "ignored" {
                    continue;
                }
                tests.push(TestOutcome {
                    node: manifest.node_for_test(name).map(String::from),
                    name: name.trim().to_string(),
                    passed,
                    output: String::new(),
                });
                continue;
            }
        }
        if line == "failures:" {
            in_failures = true;
        }
        if in_failures {
            failures_section.push(line.to_string());
        }
    }
    // quiet mode prints dots instead of names: recover failures from the summary
    let mut current: Option<String> = None;
    let mut buf = String::new();
    let flush = |name: &Option<String>, buf: &mut String, tests: &mut Vec<TestOutcome>| {
        if let Some(n) = name {
            match tests.iter_mut().find(|t| &t.name == n) {
                Some(t) => {
                    t.passed = false;
                    t.output = std::mem::take(buf);
                }
                None => tests.push(TestOutcome {
                    node: manifest.node_for_test(n).map(String::from),
                    name: n.clone(),
                    passed: false,
                    output: std::mem::take(buf),
                }),
            }
        }
        buf.clear();
    };
    for line in &failures_section {
        if let Some(name) = line.strip_prefix("---- ").and_then(|l| l.strip_suffix(" stdout ----")) {
            flush(&current, &mut buf, &mut tests);
            current = Some(name.to_string());
        } else if line == "failures:" && current.is_some() {
            flush(&current, &mut buf, &mut tests);
            current = None;
        } else if current.is_some() {
            buf.push_str(line);
            buf.push('\n');
        }
    }
    flush(&current, &mut buf, &mut tests);
    // names listed under the final "failures:" without output sections
    let mut in_list = false;
    let mut listed = Vec::new();
    for line in text.lines() {
        if line == "failures:" {
            in_list = true;
            listed.clear();
            continue;
        }
        if in_list {
            match line.strip_prefix("    ") {
                Some(n) if !n.contains(' ') => listed.push(n.to_string()),
                _ if line.starts_with("test result:") => in_list = false,
                _ => {}
            }
        }
    }
    for n in listed {
        if !tests.iter().any(|t| t.name == n) {
            tests.push(TestOutcome { node: manifest.node_for_test(&n).map(String::from), name: n, passed: false, output: String::new() });
        }
    }
    tests
}

/// Holes whose implementation is still the generated `todo!()` stub.
pub fn unimplemented_holes(manifest: &Manifest) -> Vec<&wh_codegen::HoleSpec> {
    manifest
        .holes
        .iter()
        .filter(|h| std::fs::read_to_string(&h.path).map_or(true, |s| s.contains("todo!(\"synthesize ")))
        .collect()
}
