//! A real `claude-agent-acp` author session with the `wh` server attached
//! (file mode), asked for one small change. Needs npx, network and a Claude
//! login: `cargo test -p wh-mcp --test author_live -- --ignored --nocapture`.

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use wh_agent::{AgentConfig, AgentHandle, PermissionDecision, PermissionRequest, PolicyError, SessionEvent, SessionPolicy};

/// Reads under the project; permission only for the `wh` tools; no writes.
struct WhOnly {
    root: PathBuf,
}

impl SessionPolicy for WhOnly {
    fn read_text_file(&self, path: &Path) -> Result<String, PolicyError> {
        let p = path.canonicalize().map_err(|_| PolicyError::NotFound(path.to_path_buf()))?;
        if !p.starts_with(&self.root) {
            return Err(PolicyError::Denied("outside the project".into()));
        }
        std::fs::read_to_string(p).map_err(PolicyError::Io)
    }

    fn write_text_file(&self, _path: &Path, _content: &str) -> Result<(), PolicyError> {
        Err(PolicyError::Denied("use wh_apply_ops or wh_edit_prose".into()))
    }

    fn decide_permission(&self, req: &PermissionRequest) -> PermissionDecision {
        eprintln!("permission: {:?}", req.title);
        if wh_mcp::author::is_wh_tool_request(req) || req.kind == Some(wh_agent::acp::ToolKind::Read) {
            PermissionDecision::AllowOnce
        } else {
            PermissionDecision::RejectOnce
        }
    }
}

#[test]
#[ignore = "runs a real claude-agent-acp session"]
fn real_author_session_edits_through_wh_tools() {
    let root = common::fresh_copy("author_live");
    let agent = AgentHandle::spawn(AgentConfig::claude_agent_acp()).expect("starting claude-agent-acp");
    let server = wh_mcp::author::file_mode_server(env!("CARGO_BIN_EXE_wh-mcp"), &[], &root);
    let options = wh_mcp::author::session_options(server);
    let session = futures::executor::block_on(agent.new_session_with(&root, options, Arc::new(WhOnly { root: root.clone() })))
        .expect("session/new");
    let prompt = format!(
        "{}\n\nRequest: add the clause `example normalize_email(\"Q@R.io\") == \"q@r.io\"` to `signup.normalize_email`. \
         Do not change anything else and do not run wh_check with cargo. Answer in one sentence when done.",
        wh_mcp::author::PROMPT_RULES
    );
    let mut turn = session.prompt(prompt).expect("prompt");
    let mut tools = Vec::new();
    while let Some(ev) = turn.recv_blocking() {
        match ev {
            SessionEvent::AgentText(t) => eprint!("{t}"),
            SessionEvent::ToolCallStarted { title, .. } => {
                eprintln!("\n→ {title}");
                tools.push(title);
            }
            SessionEvent::Error(e) => panic!("turn failed: {e}"),
            SessionEvent::Finished { stop_reason } => {
                eprintln!("\n[finished: {stop_reason:?}]");
                break;
            }
            _ => {}
        }
    }
    let text = std::fs::read_to_string(root.join("src/signup.wh.md")).unwrap();
    assert!(text.contains("example normalize_email(\"Q@R.io\") == \"q@r.io\""), "tools used: {tools:?}");
    assert!(tools.iter().any(|t| t.contains("wh_apply_ops")), "tools used: {tools:?}");
    agent.shutdown();
}
