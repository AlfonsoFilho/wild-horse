//! A Wild Horse project on disk: `wildhorse.toml`, literate sources, and
//! the derived directories (`impl/`, `.wh/gen`, `.wh/target`).

pub mod live;
pub mod scaffold;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;
use wh_model::{Model, SourceInput};

#[derive(Debug, Clone, Deserialize, Default)]
pub struct ProjectSection {
    pub name: String,
    #[serde(default = "default_sources")]
    pub sources: Vec<String>,
}

fn default_sources() -> Vec<String> {
    vec!["src".into()]
}

#[derive(Debug, Clone, Deserialize)]
pub struct AgentSection {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SynthesisSection {
    #[serde(default = "default_mode")]
    pub mode: String,
    #[serde(default = "default_debounce")]
    pub debounce_ms: u64,
    #[serde(default = "default_concurrency")]
    pub concurrency: usize,
    #[serde(default = "default_rounds")]
    pub max_rounds: usize,
    /// Stop calling the agent after this much reported cost (USD) per run.
    #[serde(default)]
    pub budget_usd: Option<f64>,
    /// Stop calling the agent after this many attempts per run.
    #[serde(default)]
    pub max_attempts: Option<usize>,
}

fn default_mode() -> String {
    "auto".into()
}
fn default_debounce() -> u64 {
    3000
}
fn default_concurrency() -> usize {
    4
}
fn default_rounds() -> usize {
    4
}

impl Default for SynthesisSection {
    fn default() -> Self {
        Self {
            mode: default_mode(),
            debounce_ms: default_debounce(),
            concurrency: default_concurrency(),
            max_rounds: default_rounds(),
            budget_usd: None,
            max_attempts: None,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct BuildSection {
    /// Keep `requires`/`ensures` checks in release builds.
    #[serde(default)]
    pub contracts_in_release: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub project: ProjectSection,
    #[serde(default)]
    pub agent: BTreeMap<String, AgentSection>,
    #[serde(default)]
    pub synthesis: SynthesisSection,
    #[serde(default)]
    pub build: BuildSection,
}

#[derive(Debug, Clone)]
pub struct Project {
    pub root: PathBuf,
    pub config: Config,
}

impl Project {
    pub fn load(dir: impl AsRef<Path>) -> Result<Self> {
        let root = dir.as_ref().canonicalize().with_context(|| format!("no project at {}", dir.as_ref().display()))?;
        let cfg_path = root.join("wildhorse.toml");
        let text = std::fs::read_to_string(&cfg_path).with_context(|| format!("reading {}", cfg_path.display()))?;
        let config: Config = toml::from_str(&text).with_context(|| format!("parsing {}", cfg_path.display()))?;
        Ok(Self { root, config })
    }

    pub fn name(&self) -> &str {
        &self.config.project.name
    }

    pub fn impl_dir(&self) -> PathBuf {
        self.root.join("impl")
    }

    pub fn gen_dir(&self) -> PathBuf {
        self.root.join(".wh/gen")
    }

    /// The cargo target dir, shared by every project and synthesis job of
    /// this user so dependencies compile once (`WH_TARGET_DIR` overrides).
    pub fn target_dir(&self) -> PathBuf {
        shared_target_dir()
    }

    /// Where built binaries are installed for this project.
    pub fn bin_dir(&self) -> PathBuf {
        self.root.join(".wh/bin")
    }

    /// Short stable hash of the project's location.
    pub fn id(&self) -> String {
        blake3::hash(self.root.to_string_lossy().as_bytes()).to_hex()[..10].to_string()
    }

    /// Rust crate name of the generated library (the project name).
    pub fn crate_name(&self) -> String {
        sanitize(self.name())
    }

    /// Copy built binaries out of the shared target dir into `.wh/bin`.
    pub fn install_bins(&self, artifacts: &[PathBuf]) -> Result<Vec<PathBuf>> {
        let dir = self.bin_dir();
        std::fs::create_dir_all(&dir)?;
        let mut out = Vec::new();
        for a in artifacts {
            let Some(name) = a.file_name() else { continue };
            let to = dir.join(name);
            std::fs::copy(a, &to).with_context(|| format!("installing {}", a.display()))?;
            out.push(to);
        }
        Ok(out)
    }

    /// All `.wh.md` files under the configured source directories, as paths
    /// relative to the project root.
    pub fn source_paths(&self) -> Vec<String> {
        let mut out = Vec::new();
        for s in &self.config.project.sources {
            collect(&self.root.join(s), &mut out);
        }
        out.sort();
        out.into_iter()
            .map(|p| p.strip_prefix(&self.root).unwrap_or(&p).to_string_lossy().replace('\\', "/"))
            .collect()
    }

    pub fn read_sources(&self) -> Result<Vec<SourceInput>> {
        self.source_paths()
            .into_iter()
            .map(|p| {
                let text = std::fs::read_to_string(self.root.join(&p)).with_context(|| format!("reading {p}"))?;
                Ok(SourceInput { path: p, text })
            })
            .collect()
    }

    pub fn model(&self) -> Result<Model> {
        Ok(Model::build(&self.read_sources()?))
    }

    pub fn gen_options(&self) -> wh_codegen::GenOptions {
        wh_codegen::GenOptions {
            // unique per project location: packages share one target dir, and cargo
            // would otherwise mix up two root packages with the same name
            package: format!("wh_{}_{}", self.crate_name(), self.id()),
            crate_name: self.crate_name(),
            impl_root: self.impl_dir(),
            contracts_in_release: self.config.build.contracts_in_release,
        }
    }

    /// Generate the crate for `model` into `.wh/gen` (and stub missing holes),
    /// skipping generation when nothing it depends on changed.
    pub fn generate(&self, model: &Model) -> Result<wh_codegen::Generated> {
        Ok(wh_codegen::generate_cached(model, &self.gen_options(), &self.gen_dir())?.0)
    }
}

/// Per-user cache root: `WH_CACHE_DIR`, else `$XDG_CACHE_HOME/wild-horse`, else `~/.cache/wild-horse`.
pub fn cache_root() -> PathBuf {
    if let Some(d) = std::env::var_os("WH_CACHE_DIR") {
        return PathBuf::from(d);
    }
    if let Some(d) = std::env::var_os("XDG_CACHE_HOME") {
        return PathBuf::from(d).join("wild-horse");
    }
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    home.join(".cache/wild-horse")
}

/// The shared cargo target dir (`WH_TARGET_DIR`, else `<cache>/target`).
pub fn shared_target_dir() -> PathBuf {
    std::env::var_os("WH_TARGET_DIR").map(PathBuf::from).unwrap_or_else(|| cache_root().join("target"))
}

/// The per-user cache of synthesized implementations.
pub fn synth_cache_dir() -> PathBuf {
    cache_root().join("synth")
}

fn sanitize(name: &str) -> String {
    let s: String = name.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '_' }).collect();
    if s.chars().next().is_none_or(|c| c.is_ascii_digit()) { format!("p_{s}") } else { s }
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect(&p, out);
        } else if p.to_string_lossy().ends_with(".wh.md") {
            out.push(p);
        }
    }
}
