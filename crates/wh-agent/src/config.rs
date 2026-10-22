//! How to launch an agent subprocess.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Launch configuration for one ACP agent, as written in `wildhorse.toml`:
///
/// ```toml
/// [agent.compiler]
/// command = "npx"
/// args = ["-y", "@agentclientprotocol/claude-agent-acp"]
/// env = { ANTHROPIC_MODEL = "claude-opus-5-5" }
/// cwd = "/path/to/scratch"
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentConfig {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// Extra environment variables; the parent's environment is inherited.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Working directory of the agent process (not of its sessions, which
    /// each get their own `cwd`). `None` inherits the parent's.
    #[serde(default)]
    pub cwd: Option<PathBuf>,
}

impl AgentConfig {
    pub fn new(command: impl Into<String>) -> Self {
        Self {
            command: command.into(),
            args: Vec::new(),
            env: BTreeMap::new(),
            cwd: None,
        }
    }

    pub fn arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self
    }

    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.insert(key.into(), value.into());
        self
    }

    pub fn cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    /// `npx -y @agentclientprotocol/claude-agent-acp`, the default for both roles.
    pub fn claude_agent_acp() -> Self {
        Self::new("npx")
            .arg("-y")
            .arg("@agentclientprotocol/claude-agent-acp")
    }

    /// The command line, for logs and error messages.
    pub fn display(&self) -> String {
        std::iter::once(self.command.as_str())
            .chain(self.args.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self::claude_agent_acp()
    }
}
