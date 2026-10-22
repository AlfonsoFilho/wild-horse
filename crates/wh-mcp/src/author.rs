//! Attaching the server to an author session (`wh_agent::SessionOptions`).
//!
//! ```ignore
//! let server = wh_mcp::author::file_mode_server(&wh_exe, &["mcp"], &project.root);
//! let options = wh_mcp::author::session_options(server);
//! agent.new_session_with(&project.root, options, policy).await?;
//! ```

use std::path::Path;

use wh_agent::{McpServerConfig, SessionOptions, acp};

use crate::server::TOOL_NAMES;

/// The MCP server name; Claude Code exposes its tools as `mcp__wh__<tool>`.
pub const SERVER_NAME: &str = "wh";

/// Claude Code tools an author session must not have, so that sources change
/// only through `wh_apply_ops`/`wh_edit_prose`: file writes, shell, notebooks,
/// sub-agents (which would get their own Write/Edit) and the web.
pub const DISALLOWED_TOOLS: &[&str] = &[
    "Write",
    "Edit",
    "MultiEdit",
    "NotebookEdit",
    "Bash",
    "BashOutput",
    "KillShell",
    "Task",
    "Agent",
    "WebFetch",
    "WebSearch",
];

/// The `wh` tools as Claude Code names them (`mcp__wh__wh_outline`, …).
pub fn qualified_tool_names() -> Vec<String> {
    TOOL_NAMES.iter().map(|t| format!("mcp__{SERVER_NAME}__{t}")).collect()
}

/// A stdio server config running `program args…` (e.g. `wh-mcp --project <dir>`,
/// or `wh mcp --bridge <socket>`).
pub fn server_config(program: impl AsRef<Path>, args: impl IntoIterator<Item = impl Into<String>>) -> McpServerConfig {
    McpServerConfig::Stdio(
        acp::McpServerStdio::new(SERVER_NAME, program.as_ref()).args(args.into_iter().map(Into::into).collect()),
    )
}

/// File mode: `program <prefix…> --project <root>`.
pub fn file_mode_server(program: impl AsRef<Path>, prefix: &[&str], root: impl AsRef<Path>) -> McpServerConfig {
    let mut args: Vec<String> = prefix.iter().map(|s| s.to_string()).collect();
    args.extend(["--project".to_string(), root.as_ref().to_string_lossy().into_owned()]);
    server_config(program, args)
}

/// Bridge mode: `program <prefix…> --bridge <socket>`.
pub fn bridge_mode_server(program: impl AsRef<Path>, prefix: &[&str], socket: impl AsRef<Path>) -> McpServerConfig {
    let mut args: Vec<String> = prefix.iter().map(|s| s.to_string()).collect();
    args.extend(["--bridge".to_string(), socket.as_ref().to_string_lossy().into_owned()]);
    server_config(program, args)
}

/// `_meta.claudeCode.options` for an author session with the `wh` server:
/// file-changing tools disallowed, the `wh` tools auto-approved, and user
/// and project settings ignored (so their `allow` rules cannot re-enable edits).
pub fn claude_code_options() -> serde_json::Value {
    serde_json::json!({
        "disallowedTools": DISALLOWED_TOOLS,
        "allowedTools": qualified_tool_names(),
        "settingSources": [],
    })
}

/// Session options for an author session that edits only through `server`.
pub fn session_options(server: McpServerConfig) -> SessionOptions {
    SessionOptions {
        mcp_servers: vec![server],
        meta: Some(wh_agent::claude_code_meta(claude_code_options())),
        ..Default::default()
    }
}

/// Whether a permission request is for one of the `wh` tools (the author
/// policy should allow these; `allowedTools` normally makes asking unnecessary).
pub fn is_wh_tool_request(req: &wh_agent::PermissionRequest) -> bool {
    let names = qualified_tool_names();
    let title = req.title.as_deref().unwrap_or("");
    names.iter().any(|n| title.contains(n.as_str())) || TOOL_NAMES.contains(&title)
}

/// What to tell the agent about editing (append to the author prompt).
pub const PROMPT_RULES: &str = "Change the program only with the `wh` MCP tools: read it with wh_outline and wh_get_node, \
change definitions with wh_apply_ops (structured ops; several related ops in one call) and prose with wh_edit_prose, then \
run wh_check and fix any diagnostics you introduced. Read wh_spec before writing new definitions. File writing tools are disabled.";
