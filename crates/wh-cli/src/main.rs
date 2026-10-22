//! `wh`: the headless Wild Horse toolchain.

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;
use clap::{Parser, Subcommand};
use wh_project::Project;
use wh_syntax::Severity;

#[derive(Parser)]
#[command(name = "wh", about = "Wild Horse: literate, contract-first Rust with AI synthesis")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create a new project.
    New {
        /// Folder to create.
        dir: PathBuf,
        /// Project name (snake_case); defaults to the folder name.
        #[arg(long)]
        name: Option<String>,
    },
    /// Parse and check the project; print diagnostics.
    Check {
        #[arg(default_value = ".")]
        project: PathBuf,
        /// Also print lints.
        #[arg(long)]
        lints: bool,
    },
    /// Generate the Rust crate into `.wh/gen` (stubbing missing holes).
    Gen {
        #[arg(default_value = ".")]
        project: PathBuf,
        /// Use hole implementations from this directory instead of `impl/`.
        #[arg(long)]
        impl_root: Option<PathBuf>,
        /// Write the crate here instead of `.wh/gen`.
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Generate and build the project's binaries, synthesizing missing or
    /// stale holes with the compiler agent first.
    Build {
        #[arg(default_value = ".")]
        project: PathBuf,
        #[arg(long)]
        release: bool,
        /// Never call an agent: build only from the implementations in `impl/` (CI).
        #[arg(long)]
        frozen: bool,
        /// Build even if some holes are still `todo!()` stubs.
        #[arg(long)]
        allow_holes: bool,
        /// Don't reuse implementations from the synthesis cache.
        #[arg(long)]
        no_cache: bool,
        /// With --release: shorter benchmarks for the performance gates.
        #[arg(long)]
        quick: bool,
        /// With --release: record the current measurements as the new baselines.
        #[arg(long)]
        update_baselines: bool,
        /// With --release: profile-guided optimization of the app (needs `cargo-pgo`).
        #[arg(long)]
        pgo: bool,
    },
    /// Make holes faster: the compiler agent proposes variants, which must pass
    /// every spec and beat the current implementation measurably.
    Optimize {
        #[arg(default_value = ".")]
        project: PathBuf,
        /// Only this hole (default: every hole with an `optimize` clause).
        #[arg(long)]
        node: Option<String>,
        /// Shorter benchmarks (noisier, for trying things out).
        #[arg(long)]
        quick: bool,
        /// Variants per round.
        #[arg(long, default_value_t = 3)]
        variants: usize,
        /// Rounds (stops early when a round brings no improvement).
        #[arg(long, default_value_t = 2)]
        rounds: usize,
        /// Skip the mutation gate.
        #[arg(long)]
        no_mutation: bool,
    },
    /// Synthesize hole implementations with the compiler agent.
    Synth {
        #[arg(default_value = ".")]
        project: PathBuf,
        /// Only this node (e.g. `signup.normalize_email`).
        #[arg(long)]
        node: Option<String>,
        /// Re-synthesize even if the implementation is up to date (needs --node).
        #[arg(long)]
        force: bool,
        /// Don't reuse implementations from the synthesis cache.
        #[arg(long)]
        no_cache: bool,
    },
    /// Ask the author agent to change the program (edits the `.wh.md` sources).
    Ask {
        instruction: String,
        #[arg(long, default_value = ".")]
        project: PathBuf,
        /// Give the agent this definition as the current selection.
        #[arg(long)]
        node: Option<String>,
        /// Let the agent use its own file-edit tools instead of the `wh` MCP server.
        #[arg(long)]
        raw_edits: bool,
    },
    /// Show live values: every example, ensures sample and property evaluated
    /// with the current implementations, with the values of its parts.
    Live {
        #[arg(default_value = ".")]
        project: PathBuf,
        /// Only this definition (e.g. `signup.check_age`).
        #[arg(long)]
        node: Option<String>,
        /// Also print the values seen on each flow step.
        #[arg(long)]
        steps: bool,
        /// Print the report as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Serve the `wh` MCP server on stdio (spawned by author agents).
    Mcp {
        /// Project directory (file mode).
        #[arg(long, default_value = ".")]
        project: PathBuf,
        /// Forward to a running IDE over this socket instead (bridge mode).
        #[arg(long)]
        bridge: Option<PathBuf>,
    },
    /// Show each hole's state: missing, up to date, stale, pinned or manual.
    Status {
        #[arg(default_value = ".")]
        project: PathBuf,
    },
    /// Generate and run the specs (examples, properties, scenario tests).
    Test {
        #[arg(default_value = ".")]
        project: PathBuf,
        /// Only the specs of this node, e.g. `signup.normalize_email`.
        #[arg(long)]
        node: Option<String>,
    },
}

fn print_diagnostics(model: &wh_model::Model, lints: bool) -> bool {
    let mut errors = 0;
    for d in &model.diagnostics {
        if d.diag.severity == Severity::Lint && !lints {
            continue;
        }
        if d.diag.severity == Severity::Error {
            errors += 1;
        }
        let f = &model.files[d.file];
        eprintln!("{}", d.diag.render(&f.path, &f.text));
    }
    errors == 0
}

fn print_build(result: &wh_build::BuildResult) {
    for d in result.diagnostics.iter().filter(|d| d.level == "error" || d.level == "warning") {
        let node = d.node.as_deref().unwrap_or("?");
        eprintln!("[{node}] {}", d.rendered.trim_end());
    }
}

/// Load, check and generate; `None` if the spec has errors.
fn prepare(project: &PathBuf) -> Result<Option<(Project, wh_codegen::Generated)>> {
    let p = Project::load(project)?;
    let model = p.model()?;
    if !print_diagnostics(&model, false) {
        return Ok(None);
    }
    let g = p.generate(&model)?;
    Ok(Some((p, g)))
}

/// Format a criterion estimate (nanoseconds).
fn fmt_time(nanos: f64) -> String {
    let secs = nanos / 1e9;
    match secs {
        s if s < 1e-6 => format!("{:.1} ns", s * 1e9),
        s if s < 1e-3 => format!("{:.2} µs", s * 1e6),
        s if s < 1.0 => format!("{:.2} ms", s * 1e3),
        s => format!("{s:.2} s"),
    }
}

/// `wh build --release`: performance gates (complexity, budget, alloc, baselines) and mutation gates.
fn perf_gates(p: &Project, model: &wh_model::Model, quick: bool, update_baselines: bool) -> Result<bool> {
    let nodes = wh_perf::perf_nodes(p, model);
    let mut ok = true;
    if !nodes.is_empty() {
        eprintln!("performance gates for {} hole(s)…", nodes.len());
        let opts = wh_perf::PerfOptions {
            bench: if quick { wh_perf::BenchConfig::quick() } else { wh_perf::BenchConfig::default() },
            nodes: Some(nodes),
            baselines: if update_baselines { wh_perf::BaselinePolicy::Always } else { wh_perf::BaselinePolicy::IfMissing },
            ..Default::default()
        };
        let report = wh_perf::bench_project(p, model, &opts)?;
        for s in &report.skipped {
            eprintln!("skipped {}: {}", s.node, s.reason);
        }
        for n in &report.nodes {
            let largest = n.measurements.iter().max_by_key(|m| m.size);
            let at = largest.map(|m| format!("{} at n={}", fmt_time(m.mean.point), m.size)).unwrap_or_default();
            let class = n.complexity.as_ref().map(|c| format!(", fits {:?} (exponent {:.2})", c.class, c.exponent)).unwrap_or_default();
            let mark = if n.failures().is_empty() { "✓" } else { "✗" };
            eprintln!("{mark} {}: {at}{class}{}", n.node, if n.baseline_recorded { ", baseline recorded" } else { "" });
        }
        for (node, msg) in report.failures() {
            eprintln!("  ✗ {node}: {msg}");
        }
        ok &= report.passed();
    }
    for n in model.nodes.values().filter(|n| n.is_hole()) {
        if wh_perf::PerfSpec::of(model, &n.id).is_none_or(|s| s.mutate.is_none()) {
            continue;
        }
        let r = wh_perf::mutation_score_with(p, model, &n.id, &Default::default())?;
        match (&r.status, &r.gate) {
            (wh_perf::MutationStatus::NotInstalled { hint }, _) => {
                eprintln!("note: {} declares `mutate`, but cargo-mutants is not installed ({hint})", n.id);
            }
            (status, Some(g)) if g.status == wh_perf::gates::GateStatus::Fail => {
                eprintln!("✗ {}: mutation gate failed: {}", n.id, g.detail);
                if let wh_perf::MutationStatus::Scored { missed_mutants, .. } = status {
                    for m in missed_mutants.iter().take(5) {
                        eprintln!("    survived: {m}  (no spec fails when this change is made: add an example or property that does)");
                    }
                }
                ok = false;
            }
            (wh_perf::MutationStatus::Scored { caught, missed, score, missed_mutants, .. }, _) => {
                eprintln!("✓ {}: mutation score {:.0}% ({caught} caught, {missed} missed)", n.id, score * 100.0);
                for m in missed_mutants.iter().take(5) {
                    eprintln!("    survived: {m}");
                }
            }
            (wh_perf::MutationStatus::NoMutants { detail }, _) => eprintln!("note: {}: no mutants to test ({detail})", n.id),
            (wh_perf::MutationStatus::Failed { reason }, _) => {
                eprintln!("✗ {}: mutation testing failed: {}", n.id, reason.lines().take(3).collect::<Vec<_>>().join(" | "));
                ok = false;
            }
        }
    }
    if !ok {
        eprintln!("performance gates failed (a regression in --release is a build failure)");
    }
    Ok(ok)
}

/// `wh build --release --pgo`: instrumented build, training run, optimized build.
fn pgo_build(p: &Project, model: &wh_model::Model) -> Result<bool> {
    eprintln!("profile-guided optimization…");
    let r = wh_perf::pgo_build(p, model, None)?;
    match &r.status {
        wh_perf::PgoStatus::NotInstalled { hint } => {
            eprintln!("note: cargo-pgo is not installed; skipping PGO ({hint})");
            return Ok(true);
        }
        wh_perf::PgoStatus::Failed { step, reason } => {
            eprintln!("✗ PGO failed at {step}: {}", reason.lines().next().unwrap_or(""));
            return Ok(false);
        }
        wh_perf::PgoStatus::Done => {}
    }
    eprintln!("trained on: {}", r.training.join("; "));
    if let Some(c) = &r.comparison {
        eprintln!("PGO vs plain release: {}", c.summary);
    }
    for b in p.install_bins(&r.binaries)? {
        eprintln!("built {} (PGO)", b.display());
    }
    for n in &r.notes {
        eprintln!("note: {n}");
    }
    Ok(true)
}

/// `wh optimize`: the variant tournament for one or every `optimize` hole.
#[allow(clippy::too_many_arguments)]
fn optimize(p: &Project, model: &wh_model::Model, g: &wh_codegen::Generated, node: Option<&str>, quick: bool, variants: usize, rounds: usize, mutation: bool) -> Result<bool> {
    let nodes: Vec<String> = match node {
        Some(n) => {
            let id = if model.nodes.contains_key(n) {
                n.to_string()
            } else {
                model.nodes.values().find(|x| x.name == n).map(|x| x.id.clone()).ok_or_else(|| anyhow::anyhow!("no hole `{n}`"))?
            };
            vec![id]
        }
        None => {
            let mut wanted: Vec<String> = model
                .nodes
                .values()
                .filter(|n| n.is_hole() && wh_perf::PerfSpec::of(model, &n.id).is_some_and(|s| s.optimize.is_some()))
                .map(|n| n.id.clone())
                .collect();
            // hottest first, by measured time in the app's scenario tests
            match wh_perf::profile::hot_holes(p, model, None) {
                Ok(profile) => {
                    let rank = |id: &str| profile.holes.iter().position(|h| h.node == id).unwrap_or(usize::MAX);
                    wanted.sort_by_key(|id| rank(id));
                    for h in profile.holes.iter().filter(|h| wanted.contains(&h.node)) {
                        eprintln!("hot: {} ({:.1}% of traced time, {} calls)", h.node, h.share * 100.0, h.calls);
                    }
                }
                Err(e) => eprintln!("note: no profile ({e:#}); optimizing in declaration order"),
            }
            wanted
        }
    };
    if nodes.is_empty() {
        eprintln!("no hole declares `optimize`; pass --node to optimize one anyway");
        return Ok(true);
    }
    let budget = wh_synth::Budget { max_usd: p.config.synthesis.budget_usd, max_attempts: p.config.synthesis.max_attempts };
    let driver = wh_synth::Budgeted::new(wh_synth::acp::AcpDriver::new(compiler_agent(p)), budget);
    let opts = wh_perf::OptimizeOptions {
        variants,
        rounds,
        bench: if quick { wh_perf::BenchConfig::quick() } else { wh_perf::BenchConfig::default() },
        mutation,
        on_progress: Some(std::sync::Arc::new(|ev| match ev {
            wh_perf::OptimizeProgress::Measuring { label, .. } => eprintln!("… measuring {label}"),
            wh_perf::OptimizeProgress::Attempt { round, variant, .. } => eprintln!("… round {round}, variant {variant}: asking the agent"),
            wh_perf::OptimizeProgress::Verdict { label, accepted, reason, .. } => {
                eprintln!("{} {label}: {}", if accepted { "✓" } else { "✗" }, reason.lines().next().unwrap_or(""))
            }
            _ => {}
        })),
        ..Default::default()
    };
    let cache = wh_synth::cache::SynthCache::new(wh_project::synth_cache_dir());
    let mut ok = true;
    for n in nodes {
        eprintln!("optimizing {n}");
        let r = wh_perf::optimize(p, model, &n, &driver, &opts)?;
        for m in &r.incumbent.measurements {
            let allocs = m.allocs.map(|a| format!(", {a} allocs")).unwrap_or_default();
            eprintln!("  incumbent n={:<6} {}{allocs}", m.size, fmt_time(m.mean.point));
        }
        for c in &r.candidates {
            let verdict = match &c.verdict {
                Some(wh_perf::VariantVerdict::Winner) => "winner".to_string(),
                Some(wh_perf::VariantVerdict::RunnerUp) => "runner-up".to_string(),
                Some(wh_perf::VariantVerdict::Rejected { stage, reason }) => format!("rejected at {stage:?}: {}", reason.lines().next().unwrap_or("")),
                None => "-".to_string(),
            };
            let cmp = c.comparison.as_ref().map(|x| format!(" ({})", x.summary)).unwrap_or_default();
            eprintln!("  {:<9} {verdict}{cmp}", c.label);
        }
        match (&r.accepted, r.speedup) {
            (Some(label), Some(s)) => eprintln!("✓ {n}: accepted {label}, {s:.2}× faster"),
            (Some(label), None) => eprintln!("✓ {n}: accepted {label}"),
            (None, _) => eprintln!("= {n}: no variant beat the current implementation"),
        }
        for note in &r.notes {
            eprintln!("note: {note}");
        }
        // the optimized code replaces the cached implementation for this spec
        if r.accepted.is_some() {
            if let Some(h) = g.manifest.holes.iter().find(|h| h.node == n) {
                if let Ok(text) = std::fs::read_to_string(&h.path) {
                    if let Some(key) = wh_synth::read_key(&text) {
                        let meta = wh_synth::cache::EntryMeta {
                            node: n.clone(),
                            spec_key: key.to_string(),
                            signature: h.signature.clone(),
                            agent: wh_synth::Driver::id(&driver),
                            rounds: 0,
                            created: wh_synth::cache::now(),
                        };
                        let _ = cache.put(&wh_synth::cache::SynthCache::key(key, h), &text, &meta);
                    }
                }
            }
        }
        ok &= r.accepted.is_some() || r.candidates.iter().all(|c| !matches!(c.verdict, Some(wh_perf::VariantVerdict::Winner)));
    }
    let spent = wh_synth::Driver::spent_usd(&driver).map(|c| format!(", ${c:.2} reported by the agent")).unwrap_or_default();
    eprintln!("{} agent attempt(s){spent}", driver.attempts());
    Ok(ok)
}

fn compiler_agent(p: &Project) -> wh_agent::AgentConfig {
    agent_config(p, "compiler")
}

fn agent_config(p: &Project, role: &str) -> wh_agent::AgentConfig {
    match p.config.agent.get(role) {
        Some(a) => {
            let mut c = wh_agent::AgentConfig::new(&a.command);
            for x in &a.args {
                c = c.arg(x);
            }
            for (k, v) in &a.env {
                c = c.env(k, v);
            }
            c
        }
        None => wh_agent::AgentConfig::claude_agent_acp(),
    }
}

/// Synthesize missing/stale holes; true when every planned job was accepted.
fn synthesize(p: &Project, model: &wh_model::Model, g: &wh_codegen::Generated, node: Option<&str>, force: bool, use_cache: bool) -> Result<bool> {
    let budget = wh_synth::Budget { max_usd: p.config.synthesis.budget_usd, max_attempts: p.config.synthesis.max_attempts };
    let driver = wh_synth::Budgeted::new(wh_synth::acp::AcpDriver::new(compiler_agent(p)), budget);
    let on_progress = |ev: wh_synth::Progress| match ev {
        wh_synth::Progress::Attempt { node, round } => eprintln!("… {node}: attempt {round}"),
        wh_synth::Progress::Accepted { node, rounds } => eprintln!("✓ {node}: accepted after {rounds} round(s)"),
        wh_synth::Progress::Failed { node, reason } => eprintln!("✗ {node}: {reason}"),
        wh_synth::Progress::Restored { node } => eprintln!("♻ {node}: reused from the synthesis cache"),
        _ => {}
    };
    let cache = wh_synth::cache::SynthCache::new(wh_project::synth_cache_dir());
    let (package, target) = (p.gen_options().package, p.target_dir());
    let synth = wh_synth::Synthesizer {
        model,
        root: &p.root,
        package: &package,
        target_dir: &target,
        cache: use_cache.then_some(&cache),
        max_rounds: p.config.synthesis.max_rounds,
        concurrency: p.config.synthesis.concurrency,
        driver: &driver,
        on_progress: &on_progress,
    };
    let jobs = synth.plan(&g.manifest.holes, node, force);
    if jobs.is_empty() {
        eprintln!("all holes are up to date");
        return Ok(true);
    }
    eprintln!("{} hole(s) need an implementation (cache first, then `{}`)", jobs.len(), wh_synth::Driver::id(&driver));
    // a re-synthesized hole with a committed baseline must not get slower
    let perf_check = |hole: &wh_codegen::HoleSpec, impl_root: &std::path::Path| -> Result<Vec<String>> {
        let opts = wh_perf::PerfOptions { bench: wh_perf::BenchConfig::quick(), ..Default::default() };
        Ok(wh_perf::check_against_baseline(p, model, &hole.node, impl_root, &opts)?.unwrap_or_default())
    };
    let reports = synth.run_with(jobs, Some(&perf_check));
    let failed: Vec<_> = reports.iter().filter(|r| !r.accepted).collect();
    for r in &failed {
        if let Some(last) = r.rounds.last() {
            if let wh_synth::Verdict::Rejected(why) = &last.verdict {
                eprintln!("--- {} (last rejection)\n{}", r.node, why.lines().take(20).collect::<Vec<_>>().join("\n"));
            }
        }
    }
    for note in reports.iter().flat_map(|r| &r.notes) {
        eprintln!("note: {note}");
    }
    let cached = reports.iter().filter(|r| r.from_cache).count();
    let spent = wh_synth::Driver::spent_usd(&driver).map(|c| format!(", ${c:.2} reported by the agent")).unwrap_or_default();
    eprintln!(
        "{} accepted ({cached} from cache), {} failed; {} agent attempt(s){spent}",
        reports.len() - failed.len(),
        failed.len(),
        driver.attempts()
    );
    Ok(failed.is_empty())
}

fn run(cli: Cli) -> Result<bool> {
    match cli.command {
        Command::New { dir, name } => {
            let name = name.unwrap_or_else(|| dir.file_name().map(|n| n.to_string_lossy().replace('-', "_")).unwrap_or_default());
            wh_project::scaffold::new_project(&dir, &name)?;
            eprintln!("created project `{name}` in {}", dir.display());
            eprintln!("next: `wh check {0}`, then `wh build {0}` (the compiler agent implements the 3 holes)", dir.display());
            Ok(true)
        }
        Command::Check { project, lints } => {
            let p = Project::load(&project)?;
            let model = p.model()?;
            let ok = print_diagnostics(&model, lints);
            let holes = model.nodes.values().filter(|n| n.is_hole()).count();
            eprintln!("{} module(s), {} definition(s), {holes} hole(s)", model.files.len(), model.nodes.len());
            Ok(ok)
        }
        Command::Gen { project, impl_root, out } => {
            let p = Project::load(&project)?;
            let model = p.model()?;
            if !print_diagnostics(&model, false) {
                return Ok(false);
            }
            let mut opts = p.gen_options();
            if let Some(r) = impl_root {
                std::fs::create_dir_all(&r)?;
                opts.impl_root = r.canonicalize().map_err(|e| anyhow::anyhow!("{}: {e}", r.display()))?;
            }
            let out = out.unwrap_or_else(|| p.gen_dir());
            let g = wh_codegen::generate(&model, &opts);
            wh_codegen::write(&g, &out)?;
            eprintln!("generated {} file(s) in {}; {} hole(s)", g.files.len(), out.display(), g.manifest.holes.len());
            Ok(true)
        }
        Command::Synth { project, node, force, no_cache } => {
            let Some((p, g)) = prepare(&project)? else { return Ok(false) };
            let model = p.model()?;
            synthesize(&p, &model, &g, node.as_deref(), force, !no_cache)
        }
        Command::Live { project, node, steps, json } => {
            let Some((p, _)) = prepare(&project)? else { return Ok(false) };
            let model = p.model()?;
            let scope = match node {
                Some(n) => wh_live::LiveScope::Node(
                    if model.nodes.contains_key(&n) { n } else { model.nodes.values().find(|x| x.name == n).map(|x| x.id.clone()).unwrap_or(n) },
                ),
                None => wh_live::LiveScope::All,
            };
            let r = wh_live::evaluate(&p, &model, scope)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&r)?);
                return Ok(r.build_errors.is_empty());
            }
            for e in &r.build_errors {
                eprintln!("build error: {e}");
            }
            let mut all_ok = true;
            for (node, values) in &r.nodes {
                println!("{node}");
                for v in values {
                    let mark = match v.status {
                        wh_live::Status::Pass => "✓",
                        wh_live::Status::Fail => "✗",
                        _ => "!",
                    };
                    all_ok &= matches!(v.status, wh_live::Status::Pass);
                    let parts: Vec<String> = v.values.iter().map(|x| format!("{} = {}", x.label, x.value)).collect();
                    let extra = v.message.as_deref().map(|m| format!("  ({m})")).unwrap_or_default();
                    println!("  {mark} {:<58} {}{extra}", v.source.lines().next().unwrap_or(""), parts.join(", "));
                }
            }
            if steps {
                for (step, samples) in &r.steps {
                    for s in samples.iter().take(3) {
                        println!("  ↳ {step}: {}{}", s.value, if s.error { "  (raised)" } else { "" });
                    }
                }
            }
            eprintln!("{} value(s) in {} ms (rebuilt: {})", r.values().count(), r.timings.total_ms, r.timings.rebuilt);
            Ok(all_ok)
        }
        Command::Mcp { project, bridge } => {
            let target = match bridge {
                Some(s) => wh_mcp::Target::Bridge(s),
                None => wh_mcp::Target::Project(project),
            };
            wh_mcp::run_stdio(target)?;
            Ok(true)
        }
        Command::Ask { instruction, project, node, raw_edits } => {
            let p = Project::load(&project)?;
            let model = p.model()?;
            let buffers: std::collections::BTreeMap<String, String> =
                model.files.iter().map(|f| (f.path.clone(), f.text.clone())).collect();
            let selection = node.as_ref().and_then(|id| {
                let n = model.nodes.get(id)?;
                Some((id.clone(), model.files[n.file].text[n.span.range()].to_string()))
            });
            let author = wh_author::Author::new(p.root.clone(), agent_config(&p, "author"));
            let edits = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
            let seen = edits.clone();
            // in CLI mode the agent writes the files itself (the policy allowed the edit); just report
            // by default the agent edits through the `wh` MCP server (`wh mcp --project <root>`)
            let server = (!raw_edits)
                .then(|| std::env::current_exe().map(|exe| wh_mcp::author::file_mode_server(exe, &["mcp"], &p.root)))
                .transpose()?;
            let semantic = server.is_some();
            author.ask_with(&instruction, buffers, selection.as_ref().map(|(a, b)| (a.as_str(), b.as_str())), server, move |e| match e {
                wh_author::AuthorEvent::Text(t) => eprint!("{t}"),
                wh_author::AuthorEvent::Tool(t) => eprintln!("\n→ {t}"),
                wh_author::AuthorEvent::Edit(e) => {
                    eprintln!("✎ {}: {}", e.path, e.label);
                    seen.lock().unwrap().push(e.path);
                }
                wh_author::AuthorEvent::Error(m) => eprintln!("\nerror: {m}"),
                wh_author::AuthorEvent::Finished => eprintln!(),
            })?;
            if semantic {
                eprintln!("(edits were applied through the `wh` MCP tools)");
            } else {
                eprintln!("{} edit(s) to the sources", edits.lock().unwrap().len());
            }
            let model = p.model()?;
            let ok = print_diagnostics(&model, false);
            eprintln!("check: {}", if ok { "clean" } else { "errors (see above)" });
            Ok(ok)
        }
        Command::Status { project } => {
            let Some((p, g)) = prepare(&project)? else { return Ok(false) };
            let model = p.model()?;
            let driver = wh_synth::acp::AcpDriver::new(compiler_agent(&p));
            let cache = wh_synth::cache::SynthCache::new(wh_project::synth_cache_dir());
            let seeded = wh_synth::seed_cache(&model, &g.manifest.holes, &wh_synth::Driver::id(&driver), &cache);
            if seeded > 0 {
                eprintln!("added {seeded} up-to-date implementation(s) to the synthesis cache");
            }
            for (node, st) in wh_synth::states(&model, &g.manifest.holes, &wh_synth::Driver::id(&driver)) {
                eprintln!("{:<12} {node}", format!("{st:?}").split([' ', '{']).next().unwrap_or(""));
            }
            Ok(true)
        }
        Command::Optimize { project, node, quick, variants, rounds, no_mutation } => {
            let Some((p, g)) = prepare(&project)? else { return Ok(false) };
            let model = p.model()?;
            optimize(&p, &model, &g, node.as_deref(), quick, variants, rounds, !no_mutation)
        }
        Command::Build { project, release, frozen, allow_holes, no_cache, quick, update_baselines, pgo } => {
            let Some((p, g)) = prepare(&project)? else { return Ok(false) };
            if !frozen {
                let model = p.model()?;
                if !synthesize(&p, &model, &g, None, false, !no_cache)? && !allow_holes {
                    return Ok(false);
                }
            }
            let missing = wh_build::unimplemented_holes(&g.manifest);
            if !missing.is_empty() && !allow_holes {
                for h in &missing {
                    eprintln!("hole not implemented: {} ({})", h.node, h.path.display());
                }
                eprintln!("{} hole(s) are not implemented; run `wh synth` or pass --allow-holes", missing.len());
                return Ok(false);
            }
            let (gen_dir, target) = (p.gen_dir(), p.target_dir());
            let cargo = wh_build::Cargo { crate_dir: &gen_dir, target_dir: &target, manifest: &g.manifest };
            let r = cargo.build(release)?;
            print_build(&r);
            for a in p.install_bins(&r.artifacts)? {
                eprintln!("built {}", a.display());
            }
            if release && r.success {
                let model = p.model()?;
                let mut ok = perf_gates(&p, &model, quick, update_baselines)?;
                if pgo {
                    ok &= pgo_build(&p, &model)?;
                }
                return Ok(ok);
            }
            Ok(r.success)
        }
        Command::Test { project, node } => {
            let Some((p, g)) = prepare(&project)? else { return Ok(false) };
            let (gen_dir, target) = (p.gen_dir(), p.target_dir());
            let cargo = wh_build::Cargo { crate_dir: &gen_dir, target_dir: &target, manifest: &g.manifest };
            let r = cargo.test(node.as_deref())?;
            print_build(&r);
            let mut by_node: std::collections::BTreeMap<&str, (usize, usize)> = Default::default();
            for t in &r.tests {
                let e = by_node.entry(t.node.as_deref().unwrap_or("?")).or_default();
                if t.passed { e.0 += 1 } else { e.1 += 1 }
            }
            for (n, (ok, bad)) in &by_node {
                let mark = if *bad == 0 { "✓" } else { "✗" };
                eprintln!("{mark} {n}: {ok} passed, {bad} failed");
            }
            for t in r.failed_tests() {
                let first = t.output.lines().find(|l| l.contains("panicked") || l.contains("wh:")).unwrap_or("");
                let msg = t.output.lines().skip_while(|l| !l.contains("panicked")).nth(1).unwrap_or(first);
                eprintln!("  ✗ {} [{}] {}", t.name, t.node.as_deref().unwrap_or("?"), msg.trim());
            }
            Ok(r.success)
        }
    }
}

fn main() -> ExitCode {
    let _ = tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_env("WH_LOG").unwrap_or_else(|_| "warn".into()))
        .try_init();
    match run(Cli::parse()) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}
