//! Target profiles (language spec §11): everything that differs between the
//! kinds of program an `app` can become — the generated crate's shape, its
//! dependencies, the entry point, the cargo invocations that build and test
//! it, and how it is run — so the checker, the code generator and the build
//! tools agree on one description per target.

use std::process::Command;

use serde::{Deserialize, Serialize};

/// The kind of program an app is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TargetKind {
    /// `native(cli)`: a binary whose `entry` flow is `main`.
    Native,
    /// `server(axum)`: an HTTP server whose `route`s call flows.
    Server,
    /// `web(wasm)`: a `wasm32-unknown-unknown` library exporting flows/defs.
    Web,
}

/// A Cargo dependency a target adds to the generated crate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Dependency {
    pub name: &'static str,
    pub version: &'static str,
    pub features: &'static [&'static str],
}

impl Dependency {
    /// The `Cargo.toml` line: `axum = "0.8"` or `tokio = { version = …, features = […] }`.
    pub fn toml(&self) -> String {
        if self.features.is_empty() {
            format!("{} = {:?}\n", self.name, self.version)
        } else {
            format!("{} = {{ version = {:?}, features = {:?} }}\n", self.name, self.version, self.features)
        }
    }
}

/// What the generated crate's entry point is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum EntryPoint {
    /// `fn main` calls the app's `entry` flow with the command line.
    Main,
    /// `#[tokio::main] async fn main` serves the app's router until shutdown.
    Server,
    /// No `main`: `#[wasm_bindgen]` exports in the library.
    WasmExports,
}

/// How a built app is run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum Runner {
    /// Run the binary with the user's arguments.
    Exec,
    /// Run the binary; it listens on `$addr_env` (default `default_addr`)
    /// and prints `listening on http://<addr>` on stderr once it accepts
    /// connections. It stops gracefully on Ctrl-C or SIGTERM.
    Serve { addr_env: &'static str, default_addr: &'static str },
    /// Build the library for `rust_target`, then (when the `wasm-bindgen` CLI
    /// is installed) generate JS bindings into `out_dir` for `web/index.html`.
    WasmBindgen { out_dir: &'static str },
}

/// One target an app can be built for.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TargetProfile {
    pub kind: TargetKind,
    /// The flavour inside the parentheses: `cli`, `axum`, `wasm`.
    pub flavor: String,
}

/// Every supported `target kind(flavor)`.
pub const SUPPORTED: &[(&str, &str)] = &[("native", "cli"), ("server", "axum"), ("web", "wasm")];

/// Labels usable in `@target(...)` on adapters.
pub const LABELS: &[&str] = &["native", "server", "web"];

impl TargetProfile {
    /// `native(cli)`: today's default.
    pub fn native_cli() -> Self {
        TargetProfile { kind: TargetKind::Native, flavor: "cli".into() }
    }

    /// Parse `target <kind>(<flavor>)` as written in an app.
    pub fn parse(kind: &str, flavor: Option<&str>) -> Result<Self, String> {
        let found = SUPPORTED.iter().find(|(k, f)| *k == kind && flavor.is_none_or(|x| x == *f));
        match found {
            Some((k, f)) => Ok(TargetProfile {
                kind: match *k {
                    "native" => TargetKind::Native,
                    "server" => TargetKind::Server,
                    _ => TargetKind::Web,
                },
                flavor: f.to_string(),
            }),
            None => Err(format!(
                "unknown target `{kind}{}`; supported: {}",
                flavor.map_or(String::new(), |f| format!("({f})")),
                SUPPORTED.iter().map(|(k, f)| format!("{k}({f})")).collect::<Vec<_>>().join(", ")
            )),
        }
    }

    /// `native`, `server` or `web`, as used by `@target(...)`.
    pub fn label(&self) -> &'static str {
        match self.kind {
            TargetKind::Native => "native",
            TargetKind::Server => "server",
            TargetKind::Web => "web",
        }
    }

    pub fn written(&self) -> String {
        format!("{}({})", self.label(), self.flavor)
    }

    /// The Rust compilation target, when it is not the host.
    pub fn rust_target(&self) -> Option<&'static str> {
        match self.kind {
            TargetKind::Web => Some("wasm32-unknown-unknown"),
            _ => None,
        }
    }

    pub fn entry(&self) -> EntryPoint {
        match self.kind {
            TargetKind::Native => EntryPoint::Main,
            TargetKind::Server => EntryPoint::Server,
            TargetKind::Web => EntryPoint::WasmExports,
        }
    }

    /// Does the app become a `[[bin]]` of the generated crate?
    pub fn has_binary(&self) -> bool {
        self.kind != TargetKind::Web
    }

    /// `crate-type` of the generated library (`None` = cargo's default).
    pub fn lib_crate_types(&self) -> Option<&'static [&'static str]> {
        match self.kind {
            TargetKind::Web => Some(&["cdylib", "rlib"]),
            _ => None,
        }
    }

    /// Dependencies the generated crate needs for this target.
    pub fn dependencies(&self) -> Vec<Dependency> {
        match self.kind {
            TargetKind::Native => vec![],
            TargetKind::Server => vec![
                Dependency { name: "axum", version: "0.8", features: &[] },
                Dependency { name: "tokio", version: "1", features: &["rt-multi-thread", "macros", "net", "signal"] },
                Dependency { name: "serde", version: "1", features: &["derive"] },
                Dependency { name: "serde_json", version: "1", features: &[] },
            ],
            TargetKind::Web => vec![
                Dependency { name: "wasm-bindgen", version: "0.2", features: &[] },
                Dependency { name: "serde", version: "1", features: &["derive"] },
                Dependency { name: "serde_json", version: "1", features: &[] },
            ],
        }
    }

    /// Dependencies of the generated tests.
    pub fn dev_dependencies(&self) -> Vec<Dependency> {
        match self.kind {
            TargetKind::Server => vec![
                Dependency { name: "tower", version: "0.5", features: &["util"] },
                Dependency { name: "serde_urlencoded", version: "0.7", features: &[] },
            ],
            _ => vec![],
        }
    }

    /// Arguments of `cargo` that build the app.
    pub fn cargo_build_args(&self, release: bool) -> Vec<String> {
        let mut args = vec!["build".to_string()];
        if let Some(t) = self.rust_target() {
            args.extend(["--lib".into(), "--target".into(), t.into()]);
        }
        if release {
            args.push("--release".into());
        }
        args
    }

    /// Arguments of `cargo` that run the generated tests. Tests always run on
    /// the host: web exports are tested through their Rust entry points.
    pub fn cargo_test_args(&self) -> Vec<String> {
        vec!["test".into()]
    }

    pub fn runner(&self) -> Runner {
        match self.kind {
            TargetKind::Native => Runner::Exec,
            TargetKind::Server => Runner::Serve { addr_env: "WH_ADDR", default_addr: "127.0.0.1:3000" },
            TargetKind::Web => Runner::WasmBindgen { out_dir: "web/pkg" },
        }
    }

    /// Can generated code use OS threads and blocking sleeps (sync
    /// `parallel`, `retry … backoff`) on this target?
    pub fn supports_threads(&self) -> bool {
        self.kind != TargetKind::Web
    }

    /// Can exported or routed flows be `async`?
    pub fn supports_async(&self) -> bool {
        self.kind != TargetKind::Web
    }

    /// Is an adapter usable on this target? `@target(...)` labels restrict
    /// an adapter explicitly; without them, in-memory adapters run anywhere
    /// and other adapters only natively (they may use files, sockets,
    /// threads, terminals).
    pub fn adapter_available(&self, labels: &[String], in_memory: bool) -> bool {
        if !labels.is_empty() {
            return labels.iter().any(|l| l == self.label());
        }
        in_memory || self.kind != TargetKind::Web
    }
}

/// Rust targets installed with rustup (`None` when rustup is unavailable).
pub fn installed_rust_targets() -> Option<Vec<String>> {
    let out = Command::new("rustup").args(["target", "list", "--installed"]).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).lines().map(|l| l.trim().to_string()).collect())
}

pub fn rust_target_installed(target: &str) -> bool {
    installed_rust_targets().is_some_and(|ts| ts.iter().any(|t| t == target))
}

/// The installed `wasm-bindgen` CLI's version, if any. Detection only: the
/// toolchain never installs it.
pub fn wasm_bindgen_cli() -> Option<String> {
    let out = Command::new("wasm-bindgen").arg("--version").output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// `wasm-bindgen --target web --out-dir <out> <wasm>`: JS bindings for a
/// built web app (run only when [`wasm_bindgen_cli`] finds the CLI).
pub fn wasm_bindgen_command(wasm: &std::path::Path, out_dir: &std::path::Path) -> Command {
    let mut c = Command::new("wasm-bindgen");
    c.args(["--target", "web", "--no-typescript", "--out-dir"]).arg(out_dir).arg(wasm);
    c
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_describe() {
        assert_eq!(TargetProfile::parse("native", Some("cli")), Ok(TargetProfile::native_cli()));
        let s = TargetProfile::parse("server", Some("axum")).unwrap();
        assert_eq!((s.written(), s.entry(), s.has_binary()), ("server(axum)".to_string(), EntryPoint::Server, true));
        assert!(s.dependencies().iter().any(|d| d.name == "axum"));
        let w = TargetProfile::parse("web", None).unwrap();
        assert_eq!(w.rust_target(), Some("wasm32-unknown-unknown"));
        assert_eq!(w.cargo_build_args(true), ["build", "--lib", "--target", "wasm32-unknown-unknown", "--release"]);
        assert_eq!(w.lib_crate_types(), Some(&["cdylib", "rlib"][..]));
        assert!(!w.supports_threads());
        let e = TargetProfile::parse("embedded", Some("thumbv7em")).unwrap_err();
        assert!(e.contains("supported: native(cli), server(axum), web(wasm)"), "{e}");
    }

    #[test]
    fn adapter_availability() {
        let web = TargetProfile::parse("web", Some("wasm")).unwrap();
        let native = TargetProfile::native_cli();
        assert!(web.adapter_available(&[], true));
        assert!(!web.adapter_available(&[], false));
        assert!(native.adapter_available(&[], false));
        assert!(web.adapter_available(&["web".into()], false));
        assert!(!native.adapter_available(&["web".into()], true));
    }

    #[test]
    fn dependency_lines() {
        let d = Dependency { name: "tokio", version: "1", features: &["net"] };
        assert_eq!(d.toml(), "tokio = { version = \"1\", features = [\"net\"] }\n");
    }
}
