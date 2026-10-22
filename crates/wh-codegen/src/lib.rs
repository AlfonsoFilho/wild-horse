//! Deterministic Rust code generation from the semantic model (spec §12, §15).
//!
//! Produces a Cargo package under `.wh/gen/`: one Rust module per WH module
//! with types, capability traits, adapters, contract-checking and traced
//! wrappers around every hole, fully generated flows, tests, and one binary
//! per `app`. Hole bodies live in `impl/` and are linked with `#[path]`.

pub mod expr;
pub mod items;
pub mod rt;
pub mod rust;
pub mod targets;
pub mod tests;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use wh_model::{Model, NodeKind};
use wh_syntax::ast::ItemKind;

use crate::expr::{ExprGen, Scope};
use crate::rust::{Names, cap_var, module_ident};

/// Generator identity: crate version plus a hash of its source (build.rs).
pub const CODEGEN_VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), "+", env!("WH_CODEGEN_HASH"));

#[derive(Debug, Clone)]
pub struct GenOptions {
    /// Cargo package name: must be unique among packages sharing a target dir.
    pub package: String,
    /// Rust crate (library) name the generated binaries import (usually the project name).
    pub crate_name: String,
    /// Absolute directory holding hole implementations (`impl/`).
    pub impl_root: PathBuf,
    /// Keep `requires`/`ensures` checks in release builds.
    pub contracts_in_release: bool,
}

/// One hole the compiler agent (or a human) implements.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HoleSpec {
    pub node: String,
    /// Rust module name the hole file is mounted as (`h_normalize_email`).
    pub module_name: String,
    /// Absolute path of the hole file.
    pub path: PathBuf,
    /// Initial content when the file does not exist yet.
    pub template: String,
    /// The exact Rust signature the implementation must keep.
    pub signature: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Manifest {
    /// test function name → node id
    pub tests: BTreeMap<String, String>,
    pub holes: Vec<HoleSpec>,
    /// generated file (relative) → (first line, node id) markers
    pub regions: BTreeMap<String, Vec<(usize, String)>>,
    /// app node id → binary name
    pub bins: BTreeMap<String, String>,
    /// app node id → its target profile (spec §11)
    #[serde(default)]
    pub targets: BTreeMap<String, wh_targets::TargetProfile>,
}

impl Manifest {
    /// Node owning a line of a generated or hole file.
    pub fn node_for(&self, file: &str, line: usize) -> Option<&str> {
        if let Some(h) = self.holes.iter().find(|h| file.ends_with(&*h.path.to_string_lossy()) || h.path.ends_with(file)) {
            return Some(&h.node);
        }
        let (_, regions) = self.regions.iter().find(|(f, _)| file.ends_with(f.as_str()))?;
        regions.iter().rev().find(|(l, _)| *l <= line).map(|(_, n)| n.as_str())
    }

    pub fn node_for_test(&self, test: &str) -> Option<&str> {
        let short = test.rsplit("::").next().unwrap_or(test);
        self.tests.get(short).map(String::as_str)
    }
}

#[derive(Debug, Clone, Default)]
pub struct Generated {
    /// path relative to the generated crate root → content (empty when
    /// reused from disk because nothing changed; see [`up_to_date`])
    pub files: BTreeMap<String, String>,
    pub manifest: Manifest,
    /// Hash of everything the output depends on.
    pub fingerprint: String,
}

/// Hash of the generator inputs: sources, options and the generator itself.
pub fn fingerprint(model: &Model, opts: &GenOptions) -> String {
    let mut h = blake3::Hasher::new();
    h.update(CODEGEN_VERSION.as_bytes());
    for part in [&opts.package, &opts.crate_name, &opts.impl_root.to_string_lossy().to_string()] {
        h.update(b"\0");
        h.update(part.as_bytes());
    }
    h.update(&[u8::from(opts.contracts_in_release)]);
    let mut files: Vec<(&str, &str)> = model.files.iter().map(|f| (f.path.as_str(), f.text.as_str())).collect();
    files.sort();
    for (p, t) in files {
        h.update(b"\0");
        h.update(p.as_bytes());
        h.update(b"\0");
        h.update(t.as_bytes());
    }
    h.finalize().to_hex()[..32].to_string()
}

const FINGERPRINT_FILE: &str = ".wh-fingerprint";

/// The manifest of the crate already generated in `root`, if it was generated
/// from exactly these inputs (so generation can be skipped).
pub fn up_to_date(root: &Path, fingerprint: &str) -> Option<Manifest> {
    let stored = std::fs::read_to_string(root.join(FINGERPRINT_FILE)).ok()?;
    if stored.trim() != fingerprint {
        return None;
    }
    serde_json::from_str(&std::fs::read_to_string(root.join("wh-manifest.json")).ok()?).ok()
}

/// Generate into `root` unless it already holds output for these inputs;
/// missing hole stubs are (re)created either way.
pub fn generate_cached(model: &Model, opts: &GenOptions, root: &Path) -> std::io::Result<(Generated, bool)> {
    let fp = fingerprint(model, opts);
    if let Some(manifest) = up_to_date(root, &fp) {
        let g = Generated { files: BTreeMap::new(), manifest, fingerprint: fp };
        write(&g, root)?;
        return Ok((g, true));
    }
    let g = generate(model, opts);
    write(&g, root)?;
    Ok((g, false))
}

pub(crate) fn contracts_cfg(opts: &GenOptions) -> &'static str {
    if opts.contracts_in_release { "any(debug_assertions, not(debug_assertions))" } else { "any(debug_assertions, feature = \"wh-contracts\")" }
}

fn uses_regex(model: &Model) -> bool {
    model.files.iter().any(|f| f.text.contains("matches("))
}

pub fn generate(model: &Model, opts: &GenOptions) -> Generated {
    let mut g = Generated::default();
    let mut modules: Vec<String> = model.files.iter().map(|f| f.module.clone()).collect();
    modules.sort();
    modules.dedup();

    let mut lib = String::from("//! Generated by Wild Horse. Do not edit.\n#![deny(unsafe_code)]\n#![allow(clippy::all)]\n\npub mod wh_rt;\npub mod prelude;\n");
    for m in &modules {
        lib.push_str(&format!("#[path = \"modules/{0}.rs\"]\npub mod {0};\n", module_ident(m)));
    }
    g.files.insert("src/lib.rs".into(), lib);
    g.files.insert("src/wh_rt.rs".into(), rt::WH_RT.to_string() + BLOCK_ON);
    g.files.insert(
        "src/prelude.rs".into(),
        "//! Generated by Wild Horse. Do not edit.\npub use crate::wh_rt::InvalidValue;\npub use tracing::{debug, error, info, trace, warn};\n".into(),
    );

    for m in &modules {
        let mut mg = items::ModuleGen { model, module: m, opts, out: String::new(), holes: vec![], regions: vec![] };
        mg.generate();
        let mut tg = tests::TestGen::new(model, m);
        let tests = tg.generate();
        let mut code = mg.out;
        code.push_str(&tests);
        let rel = format!("src/modules/{}.rs", module_ident(m));
        g.manifest.regions.insert(rel.clone(), mg.regions);
        g.manifest.holes.extend(mg.holes);
        g.manifest.tests.extend(tg.tests);
        g.files.insert(rel, code);
    }

    // binaries (and the web app's page)
    for n in model.nodes.values() {
        let (NodeKind::App(app), ItemKind::App(ast_app)) = (&n.kind, &n.item.kind) else { continue };
        let profile = app.profile();
        g.manifest.targets.insert(n.id.clone(), profile.clone());
        let lib_name = opts.crate_name.replace('-', "_");
        let log_setting = |k: &str, d: &str| {
            ast_app
                .log
                .iter()
                .find(|(key, _)| key.name == k)
                .and_then(|(_, v)| match &v.kind {
                    wh_syntax::ast::ExprKind::Str { value, .. } => Some(value.clone()),
                    _ => None,
                })
                .unwrap_or_else(|| d.to_string())
        };
        match profile.kind {
            wh_targets::TargetKind::Server => {
                let main = targets::server_main(n, app, &lib_name, &log_setting("level", "info"), &log_setting("format", "pretty"));
                g.files.insert(format!("src/bin/{}.rs", n.name), main);
                g.manifest.bins.insert(n.id.clone(), n.name.clone());
                continue;
            }
            wh_targets::TargetKind::Web => {
                g.files.insert("web/index.html".into(), targets::web_index(n, app, &lib_name));
                continue;
            }
            wh_targets::TargetKind::Native => {}
        }
        let Some(entry) = &app.entry else { continue };
        let names = Names::new(model, &n.module);
        let eg = ExprGen::new(model, &n.module);
        let lib_name = opts.crate_name.replace('-', "_");
        let setting = |k: &str, d: &str| {
            ast_app
                .log
                .iter()
                .find(|(key, _)| key.name == k)
                .and_then(|(_, v)| match &v.kind {
                    wh_syntax::ast::ExprKind::Str { value, .. } => Some(value.clone()),
                    _ => None,
                })
                .unwrap_or_else(|| d.to_string())
        };
        let mut main = format!(
            "//! Generated by Wild Horse from app `{}`. Do not edit.\nuse {lib_name}::{}::*;\n\nfn main() -> std::process::ExitCode {{\n    {lib_name}::wh_rt::init_tracing({:?}, {:?});\n",
            n.id,
            module_ident(&n.module),
            setting("level", "info"),
            setting("format", "pretty"),
        );
        for (p, ast_p) in app.provides.iter().zip(&ast_app.provides) {
            let code = eg.emit(&ast_p.value, &Scope::default()).replace("crate::", &format!("{lib_name}::"));
            main.push_str(&format!("    let {} = {code};\n", cap_var(&p.capability)));
        }
        let mut args: Vec<String> = entry.sig.uses.iter().map(|c| format!("&{}", cap_var(c))).collect();
        for p in &entry.sig.params {
            match &p.ty {
                wh_model::Ty::List(inner) if **inner == wh_model::Ty::Str => {
                    main.push_str(&format!("    let {}: Vec<String> = std::env::args().collect();\n", p.name));
                    args.push(format!("&{}", p.name));
                }
                other => {
                    main.push_str(&format!("    let {}: {} = Default::default();\n", p.name, names.ty(other)));
                    args.push(format!("&{}", p.name));
                }
            }
        }
        let call = format!("{}_entry({})", n.name, args.join(", "));
        let call = if entry.sig.is_async { format!("{lib_name}::wh_rt::block_on({call})") } else { call };
        if entry.sig.raises.is_some() {
            main.push_str(&format!(
                "    match {call} {{\n        Ok(_) => std::process::ExitCode::SUCCESS,\n        Err(e) => {{\n            tracing::debug!(wh.node = {:?}, error = %e, \"app failed\");\n            eprintln!(\"error: {{e}}\");\n            std::process::ExitCode::FAILURE\n        }}\n    }}\n}}\n",
                n.id
            ));
        } else {
            main.push_str(&format!("    let _ = {call};\n    std::process::ExitCode::SUCCESS\n}}\n"));
        }
        g.files.insert(format!("src/bin/{}.rs", n.name), main);
        g.manifest.bins.insert(n.id.clone(), n.name.clone());
    }

    // Cargo.toml: the base crate, then what the apps' target profiles add
    let mut deps = String::from(
        "tracing = \"0.1\"\ntracing-subscriber = { version = \"0.3\", features = [\"env-filter\", \"fmt\", \"json\"] }\n",
    );
    let mut declared: Vec<String> = vec!["tracing".into(), "tracing-subscriber".into()];
    if uses_regex(model) {
        deps.push_str("regex = \"1\"\n");
        declared.push("regex".into());
    }
    let profiles = targets::profiles(model);
    if model.nodes.values().any(|n| n.derives.iter().any(|d| d == "Serialize" || d == "Deserialize"))
        || profiles.iter().any(|p| p.dependencies().iter().any(|d| d.name == "serde"))
    {
        deps.push_str("serde = { version = \"1\", features = [\"derive\"] }\n");
        declared.push("serde".into());
    }
    for d in &model.depends {
        if declared.contains(&d.crate_name) {
            continue;
        }
        declared.push(d.crate_name.clone());
        if d.features.is_empty() {
            deps.push_str(&format!("{} = {:?}\n", d.crate_name, d.version));
        } else {
            deps.push_str(&format!("{} = {{ version = {:?}, features = {:?} }}\n", d.crate_name, d.version, d.features));
        }
    }
    let mut dev_deps = String::from("proptest = \"1\"\n");
    for p in &profiles {
        for d in p.dependencies() {
            if !declared.iter().any(|x| x == d.name) {
                declared.push(d.name.to_string());
                deps.push_str(&d.toml());
            }
        }
        for d in p.dev_dependencies() {
            if !declared.iter().any(|x| x == d.name) {
                declared.push(d.name.to_string());
                dev_deps.push_str(&d.toml());
            }
        }
    }
    let crate_type = profiles
        .iter()
        .find_map(|p| p.lib_crate_types())
        .map(|ts| format!("crate-type = {ts:?}\n"))
        .unwrap_or_default();
    let release_level = model
        .nodes
        .values()
        .filter_map(|n| match &n.item.kind {
            ItemKind::App(a) => a.log.iter().find(|(k, _)| k.name == "release_level").and_then(|(_, v)| match &v.kind {
                wh_syntax::ast::ExprKind::Str { value, .. } => Some(value.clone()),
                _ => None,
            }),
            _ => None,
        })
        .next();
    let tracing_features = match release_level.as_deref() {
        Some(l @ ("error" | "warn" | "info" | "debug" | "trace")) => format!("\"tracing/release_max_level_{l}\""),
        Some("off") => "\"tracing/release_max_level_off\"".into(),
        _ => String::new(),
    };
    let bins: String = g
        .manifest
        .bins
        .values()
        .map(|b| format!("[[bin]]\nname = {b:?}\npath = \"src/bin/{b}.rs\"\n\n"))
        .collect();
    let cargo = format!(
        "# Generated by Wild Horse. Do not edit.\n[package]\nname = {:?}\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\nautobins = false\nautoexamples = false\nautotests = false\nautobenches = false\n\n[lib]\nname = {:?}\npath = \"src/lib.rs\"\n{crate_type}\n{bins}[workspace]\n\n[features]\ndefault = [{tracing_features}]\n# keep requires/ensures checks in release builds\nwh-contracts = []\n\n[dependencies]\n{deps}\n[dev-dependencies]\n{dev_deps}\n[profile.release]\nopt-level = 3\nlto = \"fat\"\ncodegen-units = 1\n",
        opts.package,
        opts.crate_name.replace('-', "_"),
    );
    g.files.insert("Cargo.toml".into(), cargo);
    g.files.insert(".gitignore".into(), "*\n".into());
    g.files.insert("wh-manifest.json".into(), serde_json::to_string_pretty(&g.manifest).unwrap_or_default());
    g.fingerprint = fingerprint(model, opts);
    g
}

const BLOCK_ON: &str = r#"
/// Run two futures concurrently on the current task (`parallel` in async flows).
pub async fn join<A: std::future::Future, B: std::future::Future>(a: A, b: B) -> (A::Output, B::Output) {
    use std::task::Poll;
    let mut a = std::pin::pin!(a);
    let mut b = std::pin::pin!(b);
    let (mut ra, mut rb) = (None, None);
    std::future::poll_fn(|cx| {
        if ra.is_none() {
            if let Poll::Ready(v) = a.as_mut().poll(cx) {
                ra = Some(v);
            }
        }
        if rb.is_none() {
            if let Poll::Ready(v) = b.as_mut().poll(cx) {
                rb = Some(v);
            }
        }
        if ra.is_some() && rb.is_some() { Poll::Ready((ra.take().unwrap(), rb.take().unwrap())) } else { Poll::Pending }
    })
    .await
}

/// Minimal executor for async entry points and async calls in tests.
pub fn block_on<F: std::future::Future>(fut: F) -> F::Output {
    use std::sync::Arc;
    use std::task::{Context, Poll, Wake, Waker};
    struct ThreadWaker(std::thread::Thread);
    impl Wake for ThreadWaker {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }
    let waker = Waker::from(Arc::new(ThreadWaker(std::thread::current())));
    let mut cx = Context::from_waker(&waker);
    let mut fut = std::pin::pin!(fut);
    loop {
        match fut.as_mut().poll(&mut cx) {
            Poll::Ready(v) => return v,
            Poll::Pending => std::thread::park(),
        }
    }
}
"#;

/// Write generated files under `root`, touching only files whose content
/// changed (keeps cargo's incremental cache warm), and create missing hole
/// files from their templates. Returns the files written.
pub fn write(generated: &Generated, root: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut written = Vec::new();
    if generated.files.is_empty() {
        // reused output: only make sure every hole file exists
        for h in &generated.manifest.holes {
            let path = hole_file(root, h);
            if !path.exists() {
                if let Some(dir) = path.parent() {
                    std::fs::create_dir_all(dir)?;
                }
                std::fs::write(&path, &h.template)?;
                written.push(path);
            }
        }
        return Ok(written);
    }
    for (rel, content) in &generated.files {
        let path = root.join(rel);
        if std::fs::read_to_string(&path).ok().as_deref() == Some(content.as_str()) {
            continue;
        }
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&path, content)?;
        written.push(path);
    }
    // remove generated sources that no longer exist
    let src = root.join("src");
    if src.exists() {
        for entry in walk(&src) {
            let rel = entry.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
            if rel.ends_with(".rs") && !generated.files.contains_key(&rel) {
                let _ = std::fs::remove_file(&entry);
            }
        }
    }
    for h in &generated.manifest.holes {
        let path = hole_file(root, h);
        if !path.exists() {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            std::fs::write(&path, &h.template)?;
            written.push(path);
        }
    }
    // written last: a crash mid-write never leaves a matching fingerprint
    if !generated.fingerprint.is_empty() {
        std::fs::write(root.join(FINGERPRINT_FILE), &generated.fingerprint)?;
    }
    Ok(written)
}

/// Where a hole file lives: a relative path is relative to the generated crate root.
pub fn hole_file(root: &Path, h: &HoleSpec) -> PathBuf {
    if h.path.is_absolute() { h.path.clone() } else { root.join(&h.path) }
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                out.extend(walk(&p));
            } else {
                out.push(p);
            }
        }
    }
    out
}
