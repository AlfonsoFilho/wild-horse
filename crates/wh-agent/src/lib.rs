//! ACP client infrastructure shared by the author and compiler roles.
//!
//! [`AgentHandle`] spawns an agent subprocess, initializes it (advertising
//! `fs` read/write, no terminal) and opens sessions. Each [`SessionHandle`]
//! carries a [`SessionPolicy`] that answers the agent's file reads, file
//! writes and permission requests; [`SessionHandle::prompt`] returns the
//! turn's [`SessionEvent`]s as a stream. The connection runs on its own
//! tokio thread, and the API is usable from any executor or plain threads.
//!
//! This crate holds no synthesis or authoring logic.

mod config;
mod connection;
mod event;
mod policy;
mod process;

pub use agent_client_protocol::schema::v1 as acp;

pub use config::AgentConfig;
pub use connection::{
    AgentHandle, Error, McpServerConfig, PromptStream, Result, SessionHandle, SessionOptions,
};
pub use event::SessionEvent;
pub use policy::{
    PermissionChoice, PermissionDecision, PermissionRequest, PolicyError, ScopedPolicy,
    SessionPolicy,
};

/// `_meta` for `session/new` that passes `options` to `claude-agent-acp`'s
/// Claude Code SDK (`_meta.claudeCode.options`), e.g.
/// `{"disallowedTools": ["Bash", "WebFetch", "WebSearch"], "settingSources": []}`.
pub fn claude_code_meta(options: serde_json::Value) -> acp::Meta {
    let mut meta = acp::Meta::new();
    meta.insert(
        "claudeCode".into(),
        serde_json::json!({ "options": options }),
    );
    meta
}
