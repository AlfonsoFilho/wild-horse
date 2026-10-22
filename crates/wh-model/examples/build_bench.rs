//! `cargo run --release -p wh-model --example build_bench`: time
//! `Model::build` on the examples and on a synthetic 50-module project.

use std::time::Instant;

use wh_model::{Model, SourceInput};

fn time(label: &str, runs: usize, f: impl Fn()) {
    f(); // warm up
    let t = Instant::now();
    for _ in 0..runs {
        f();
    }
    println!("{label:<44} {:>9.2} ms", t.elapsed().as_secs_f64() * 1000.0 / runs as f64);
}

fn example(dir: &str) -> Vec<SourceInput> {
    let root = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples")).join(dir).join("src");
    let mut out = Vec::new();
    for e in std::fs::read_dir(&root).unwrap().flatten() {
        let p = e.path();
        if p.to_string_lossy().ends_with(".wh.md") {
            out.push(SourceInput { path: p.file_name().unwrap().to_string_lossy().into(), text: std::fs::read_to_string(&p).unwrap() });
        }
    }
    out
}

fn main() {
    for ex in ["signup", "todo", "pong"] {
        let inputs = example(ex);
        time(&format!("full build: examples/{ex}"), 50, || {
            Model::build(&inputs);
        });
    }
    let synth = wh_model::synthetic::project(50, 8);
    let defs: usize = Model::build(&synth).nodes.len();
    time(&format!("full build: synthetic 50 modules, {defs} definitions"), 5, || {
        Model::build(&synth);
    });
    // a one-block edit in the middle module
    let mut edited = synth.clone();
    edited[25] = wh_model::synthetic::module(25, 8, 7);
    time("full rebuild after a one-block edit", 5, || {
        Model::build(&edited);
    });
    let _ = &edited;
    if std::env::var("WH_PROF").is_ok() {
        Model::build(&synth);
    }
}
