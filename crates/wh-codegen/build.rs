//! Fingerprints the generator's own source, so cached generated code is
//! invalidated whenever the code generator changes (not just its version).

use std::path::Path;

fn walk(dir: &Path, files: &mut Vec<std::path::PathBuf>) {
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(&p, files);
        } else if p.extension().is_some_and(|x| x == "rs") {
            files.push(p);
        }
    }
}

fn main() {
    let mut files = Vec::new();
    walk(Path::new("src"), &mut files);
    files.sort();
    // FNV-1a: no dependencies needed in a build script
    let mut h: u64 = 0xcbf29ce484222325;
    for f in &files {
        println!("cargo:rerun-if-changed={}", f.display());
        for b in f.to_string_lossy().bytes().chain(std::fs::read(f).unwrap_or_default()) {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x100000001b3);
        }
    }
    println!("cargo:rustc-env=WH_CODEGEN_HASH={h:016x}");
}
