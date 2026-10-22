//! Scratch-copy helpers, mirroring `wh-synth`'s sandbox: the agent works in
//! a copy of `impl/` where only the hole file may change, which the
//! compiler checks by hashing the tree before and after each attempt.

use std::path::{Path, PathBuf};

pub fn slug(node: &str) -> String {
    node.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '_' }).collect()
}

pub fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    let Ok(rd) = std::fs::read_dir(from) else { return Ok(()) };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            copy_tree(&p, &to.join(e.file_name()))?;
        } else {
            std::fs::copy(&p, to.join(e.file_name()))?;
        }
    }
    Ok(())
}

/// Digest of every file under `dir` except `skip`, `target` directories and
/// `Cargo.lock` files (written by verification builds, not by the agent).
pub fn tree_digest(dir: &Path, skip: &Path) -> String {
    fn walk(d: &Path, skip: &Path, out: &mut Vec<(PathBuf, Vec<u8>)>) {
        let Ok(rd) = std::fs::read_dir(d) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if p == skip || p.file_name().is_some_and(|n| n == "target" || n == "Cargo.lock") {
                continue;
            }
            if p.is_dir() {
                walk(&p, skip, out);
            } else {
                out.push((p.clone(), std::fs::read(&p).unwrap_or_default()));
            }
        }
    }
    let mut files = Vec::new();
    walk(dir, skip, &mut files);
    files.sort();
    let mut h = blake3::Hasher::new();
    for (p, c) in files {
        h.update(p.to_string_lossy().as_bytes());
        h.update(&c);
    }
    h.finalize().to_hex().to_string()
}

/// Whitespace-insensitive form of code, for signature checks.
pub fn squash(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The hole file's code without its `// wh:key=` header.
pub fn strip_key(code: &str) -> String {
    code.lines().skip_while(|l| l.starts_with(wh_synth::KEY_PREFIX)).collect::<Vec<_>>().join("\n")
}
