//! The `wh` MCP server (design doc, Core idea 2, Role A): semantic tools that
//! let the author agent read and change a Wild Horse program through the same
//! structured operations the graph and inspector use, instead of rewriting
//! `.wh.md` files with its own file tools.
//!
//! Tools: `wh_outline`, `wh_get_node`, `wh_apply_ops`, `wh_edit_prose`,
//! `wh_check`, `wh_run_specs`, `wh_spec` (see [`server`]).
//!
//! Every tool is answered by a [`Backend`]:
//! - [`FileBackend`] (file mode, for the CLI) loads a project, keeps the
//!   sources in a `DocStore` and writes the files after every successful edit;
//! - in bridge mode (for the IDE) the stdio server is a thin shim that
//!   forwards each call over a Unix socket ([`bridge`]) to the IDE, which
//!   implements [`Backend`] on its own document store and serves it with
//!   [`bridge::serve`].
//!
//! [`author`] has what an author session needs to attach the server.

pub mod author;
pub mod bridge;
mod file;
pub mod server;
pub mod view;

use std::process::ExitCode;
use std::sync::Arc;

pub use file::FileBackend;
pub use view::{
    ApplyResult, CheckResult, DiagCounts, DiagView, EditedRange, NodeView, Outline, ProseEdit, ProseMode, SpecRun,
};
pub use wh_edit::Op;

/// The language specification, served by `wh_spec`.
pub const SPEC: &str = include_str!("../../../docs/language-spec.md");

/// A tool failure the agent should read (returned as an `isError` tool result).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolError(pub String);

impl ToolError {
    pub fn new(msg: impl Into<String>) -> Self {
        Self(msg.into())
    }
}

impl std::fmt::Display for ToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ToolError {}

impl From<wh_edit::EditError> for ToolError {
    fn from(e: wh_edit::EditError) -> Self {
        Self(e.to_string())
    }
}

impl From<wh_doc::DocError> for ToolError {
    fn from(e: wh_doc::DocError) -> Self {
        Self(e.to_string())
    }
}

impl From<anyhow::Error> for ToolError {
    fn from(e: anyhow::Error) -> Self {
        Self(format!("{e:#}"))
    }
}

impl From<std::io::Error> for ToolError {
    fn from(e: std::io::Error) -> Self {
        Self(e.to_string())
    }
}

pub type ToolResult<T> = Result<T, ToolError>;

/// What answers the tools: the CLI's [`FileBackend`], or the IDE's live
/// document store behind [`bridge::serve`]. Calls may come from any thread
/// and may block (cargo runs for `check(true)` and `run_specs`).
///
/// [`view`] has the model-level pieces an implementation needs: `outline`,
/// `node_view`, `plan_ops`, `plan_prose`, `apply_to_store` and
/// `apply_result`.
pub trait Backend: Send + Sync {
    fn outline(&self) -> ToolResult<Outline>;
    fn get_node(&self, id: &str) -> ToolResult<NodeView>;
    /// Plan all ops against the evolving model and apply them as one
    /// transaction, or nothing.
    fn apply_ops(&self, ops: &[Op]) -> ToolResult<ApplyResult>;
    fn edit_prose(&self, edit: &ProseEdit) -> ToolResult<ApplyResult>;
    /// Model diagnostics, plus `cargo check` of the generated crate when `cargo`.
    fn check(&self, cargo: bool) -> ToolResult<CheckResult>;
    /// Run the specs (examples, properties, contracts) of one node, or all.
    fn run_specs(&self, id: Option<&str>) -> ToolResult<SpecRun>;
    fn spec(&self) -> ToolResult<String> {
        Ok(SPEC.to_string())
    }
}

/// Where the stdio server sends its tool calls.
#[derive(Debug, Clone)]
pub enum Target {
    /// `--project <dir>` (default: the current directory).
    Project(std::path::PathBuf),
    /// `--bridge <socket>`.
    Bridge(std::path::PathBuf),
}

impl Target {
    pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Self, String> {
        let mut args = args.into_iter();
        let mut target = None;
        while let Some(a) = args.next() {
            match a.as_str() {
                "--project" | "-p" => target = Some(Target::Project(args.next().ok_or("--project needs a directory")?.into())),
                "--bridge" => target = Some(Target::Bridge(args.next().ok_or("--bridge needs a socket path")?.into())),
                "-h" | "--help" => return Err(USAGE.into()),
                other if !other.starts_with('-') && target.is_none() => target = Some(Target::Project(other.into())),
                other => return Err(format!("unexpected argument `{other}`\n{USAGE}")),
            }
        }
        Ok(target.unwrap_or_else(|| Target::Project(".".into())))
    }
}

pub const USAGE: &str = "usage: wh-mcp [--project <dir>] | --bridge <socket>";

/// Serve MCP on stdin/stdout until the client hangs up.
pub fn run_stdio(target: Target) -> anyhow::Result<()> {
    let tools: Arc<dyn server::ToolCaller> = match target {
        Target::Project(dir) => Arc::new(server::Local(Arc::new(FileBackend::open(&dir)?))),
        Target::Bridge(socket) => Arc::new(bridge::BridgeClient::new(socket)),
    };
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    rt.block_on(server::serve_stdio(tools))
}

/// Entry point shared by the `wh-mcp` binary and `wh mcp`: `args` are the
/// arguments after the command name. Logs go to stderr (stdout is the protocol).
pub fn cli_main(args: impl IntoIterator<Item = String>) -> ExitCode {
    let target = match Target::parse(args) {
        Ok(t) => t,
        Err(msg) => {
            eprintln!("{msg}");
            return ExitCode::from(2);
        }
    };
    match run_stdio(target) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("wh-mcp: {e:#}");
            ExitCode::FAILURE
        }
    }
}
