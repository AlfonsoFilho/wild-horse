//! A [`Driver`] backed by an ACP agent (default: `claude-agent-acp`).

use std::sync::{Arc, Mutex};

use anyhow::{Result, anyhow};
use wh_agent::{AgentConfig, AgentHandle, ScopedPolicy, SessionEvent, SessionOptions};

use crate::{AttemptOutcome, Driver, SynthesisRequest};

/// Tools the compiler agent must never use: it writes one file and the
/// compiler (not the agent) runs everything.
pub const DISALLOWED_TOOLS: &[&str] = &["Bash", "BashOutput", "KillShell", "WebFetch", "WebSearch", "Task", "NotebookEdit"];

pub struct AcpDriver {
    config: AgentConfig,
    agent: Mutex<Option<AgentHandle>>,
    /// Sum of the final reported cost of every session, in USD.
    spent: Mutex<Option<f64>>,
}

impl AcpDriver {
    pub fn new(config: AgentConfig) -> Self {
        Self { config, agent: Mutex::new(None), spent: Mutex::new(None) }
    }

    fn agent(&self) -> Result<AgentHandle> {
        let mut slot = self.agent.lock().unwrap();
        if let Some(a) = slot.as_ref() {
            return Ok(a.clone());
        }
        let a = AgentHandle::spawn(self.config.clone()).map_err(|e| anyhow!("starting agent `{}`: {e}", self.config.display()))?;
        tracing::info!(agent = %self.config.display(), info = ?a.agent_info(), "compiler agent started");
        *slot = Some(a.clone());
        Ok(a)
    }
}

impl Driver for AcpDriver {
    fn id(&self) -> String {
        self.config.display()
    }

    fn spent_usd(&self) -> Option<f64> {
        *self.spent.lock().unwrap()
    }

    fn attempt(&self, req: &SynthesisRequest) -> Result<AttemptOutcome> {
        let agent = self.agent()?;
        // only the hole file is writable; every other permission request is rejected
        let policy = Arc::new(ScopedPolicy::new(&req.workdir).allow_write(&req.hole_file));
        let options = SessionOptions {
            meta: Some(wh_agent::claude_code_meta(serde_json::json!({
                "disallowedTools": DISALLOWED_TOOLS,
                // user/project settings must not auto-allow tools behind the policy's back
                "settingSources": [],
            }))),
            ..Default::default()
        };
        let session = futures_block_on(agent.new_session_with(&req.workdir, options, policy))
            .map_err(|e| anyhow!("opening a session: {e}"))?;
        let mut turn = session.prompt(req.prompt.clone()).map_err(|e| anyhow!("prompting: {e}"))?;
        let mut transcript = String::new();
        // cost is cumulative per session; one session per attempt
        let mut session_cost: Option<f64> = None;
        while let Some(ev) = turn.recv_blocking() {
            if let SessionEvent::Other(update) = &ev {
                if let wh_agent::acp::SessionUpdate::UsageUpdate(u) = update.as_ref() {
                    if let Some(c) = u.cost.as_ref().filter(|c| c.currency.eq_ignore_ascii_case("USD")) {
                        session_cost = Some(c.amount);
                    }
                }
            }
            match &ev {
                SessionEvent::AgentText(t) => transcript.push_str(t),
                SessionEvent::ToolCallStarted { title, .. } => transcript.push_str(&format!("\n[tool] {title}\n")),
                SessionEvent::PermissionDecided { request, selected, .. } => {
                    transcript.push_str(&format!("\n[permission] {:?} → {}\n", request.title, selected.as_deref().unwrap_or("cancelled")))
                }
                SessionEvent::FileWrite { path, result } => {
                    transcript.push_str(&format!("\n[write] {} {:?}\n", path.display(), result))
                }
                SessionEvent::Error(e) if e.contains("Authentication required") => {
                    return Err(anyhow!(
                        "the agent `{}` is not signed in. Sign in once with `claude` (then `/login`), \
                         or put ANTHROPIC_API_KEY in the agent's environment (`[agent.compiler] env` in wildhorse.toml)",
                        self.config.display()
                    ));
                }
                SessionEvent::Error(e) => {
                    return Err(anyhow!("agent turn failed: {e}\nagent stderr:\n{}", agent.stderr_tail().join("\n")));
                }
                _ => {}
            }
            if ev.is_terminal() {
                break;
            }
        }
        if let Some(c) = session_cost {
            let mut spent = self.spent.lock().unwrap();
            *spent = Some(spent.unwrap_or(0.0) + c);
            transcript.push_str(&format!("\n[cost] ${c:.4}\n"));
        }
        Ok(AttemptOutcome { transcript })
    }
}

/// wh-agent's async API runs on any executor; a minimal block_on suffices here.
fn futures_block_on<F: std::future::Future>(f: F) -> F::Output {
    use std::task::{Context, Poll, Wake, Waker};
    struct W(std::thread::Thread);
    impl Wake for W {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }
    let waker = Waker::from(Arc::new(W(std::thread::current())));
    let mut cx = Context::from_waker(&waker);
    let mut f = std::pin::pin!(f);
    loop {
        if let Poll::Ready(v) = f.as_mut().poll(&mut cx) {
            return v;
        }
        std::thread::park();
    }
}
