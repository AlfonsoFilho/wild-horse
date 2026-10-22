//! The agent subprocess: stdio transport, stderr capture, teardown.

use std::collections::VecDeque;
use std::process::Stdio;
use std::sync::{Arc, Mutex};

use agent_client_protocol::ByteStreams;
use tokio::io::AsyncBufReadExt;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use crate::config::AgentConfig;

const STDERR_TAIL_LINES: usize = 200;

/// The last lines the agent wrote to stderr, for error reports.
#[derive(Debug, Clone, Default)]
pub(crate) struct StderrTail(Arc<Mutex<VecDeque<String>>>);

impl StderrTail {
    fn push(&self, line: String) {
        let mut lines = self.0.lock().unwrap();
        if lines.len() == STDERR_TAIL_LINES {
            lines.pop_front();
        }
        lines.push_back(line);
    }

    pub(crate) fn lines(&self) -> Vec<String> {
        self.0.lock().unwrap().iter().cloned().collect()
    }
}

/// Kills the agent and, on unix, its whole process group (launchers like
/// `npx` leave the real agent as a grandchild) when dropped.
pub(crate) struct ChildGuard(tokio::process::Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(pid) = self
            .0
            .id()
            .and_then(|id| rustix::process::Pid::from_raw(id as i32))
        {
            let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
        }
        let _ = self.0.start_kill();
    }
}

pub(crate) type ProcessTransport = ByteStreams<
    tokio_util::compat::Compat<tokio::process::ChildStdin>,
    tokio_util::compat::Compat<tokio::process::ChildStdout>,
>;

/// Spawn `config` with piped stdio. Must run inside a tokio runtime.
pub(crate) fn spawn(
    config: &AgentConfig,
    stderr_tail: StderrTail,
) -> std::io::Result<(ProcessTransport, ChildGuard)> {
    let mut command = tokio::process::Command::new(&config.command);
    command
        .args(&config.args)
        .envs(&config.env)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(cwd) = &config.cwd {
        command.current_dir(cwd);
    }
    #[cfg(unix)]
    command.process_group(0);

    let mut child = command.spawn()?;
    let pid = child.id();
    tracing::info!(command = %config.display(), ?pid, "spawned agent");

    let stdin = child.stdin.take().expect("piped stdin");
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    tokio::spawn(async move {
        let mut lines = tokio::io::BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            tracing::debug!(target: "wh_agent::stderr", ?pid, "{line}");
            stderr_tail.push(line);
        }
    });

    let transport = ByteStreams::new(stdin.compat_write(), stdout.compat());
    Ok((transport, ChildGuard(child)))
}
