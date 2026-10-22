//! A content-addressed store of accepted implementations.
//!
//! Entries are keyed by the synthesis key (spec, referenced types, pattern,
//! agent) plus the exact Rust signature the hole must keep, so a cached body
//! is only ever offered to a hole with the same spec *and* the same Rust
//! interface. Reverting a spec edit, switching branches, re-cloning a
//! project, or writing the same definition in another project therefore
//! reuses earlier agent work instead of calling the agent again. Entries are
//! still verified before they are accepted (see `Synthesizer::run`).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use wh_codegen::HoleSpec;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntryMeta {
    pub node: String,
    pub spec_key: String,
    pub signature: String,
    pub agent: String,
    pub rounds: usize,
    /// Unix seconds.
    pub created: u64,
}

#[derive(Debug, Clone)]
pub struct SynthCache {
    dir: PathBuf,
}

impl SynthCache {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Cache key of a hole under a synthesis key.
    pub fn key(spec_key: &str, hole: &HoleSpec) -> String {
        let mut h = blake3::Hasher::new();
        h.update(spec_key.as_bytes());
        h.update(b"\0");
        h.update(hole.signature.as_bytes());
        h.finalize().to_hex()[..32].to_string()
    }

    fn paths(&self, key: &str) -> (PathBuf, PathBuf) {
        let shard = self.dir.join(&key[..2]);
        (shard.join(format!("{key}.rs")), shard.join(format!("{key}.json")))
    }

    /// The cached implementation file (including its `// wh:key=` header).
    pub fn get(&self, key: &str) -> Option<String> {
        std::fs::read_to_string(self.paths(key).0).ok()
    }

    pub fn meta(&self, key: &str) -> Option<EntryMeta> {
        serde_json::from_str(&std::fs::read_to_string(self.paths(key).1).ok()?).ok()
    }

    /// Store an accepted implementation (written atomically: temp file + rename).
    pub fn put(&self, key: &str, content: &str, meta: &EntryMeta) -> std::io::Result<()> {
        let (code, json) = self.paths(key);
        std::fs::create_dir_all(code.parent().unwrap())?;
        let write = |path: &Path, data: &[u8]| -> std::io::Result<()> {
            let tmp = path.with_extension(format!("tmp{}", std::process::id()));
            std::fs::write(&tmp, data)?;
            std::fs::rename(&tmp, path)
        };
        write(&code, content.as_bytes())?;
        write(&json, serde_json::to_string_pretty(meta).unwrap_or_default().as_bytes())
    }

    /// Drop an entry that turned out not to verify any more.
    pub fn evict(&self, key: &str) {
        let (code, json) = self.paths(key);
        let _ = std::fs::remove_file(code);
        let _ = std::fs::remove_file(json);
    }
}

pub fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}
