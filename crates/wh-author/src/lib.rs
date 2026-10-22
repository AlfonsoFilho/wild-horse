//! Role A — the author agent (design doc, Core idea 2): an architecture-level
//! collaborator that edits the literate `.wh.md` sources.
//!
//! `claude-agent-acp` edits files with its own Write/Edit tools instead of
//! ACP `fs/write_text_file` (see `crates/wh-agent/NOTES.md`), so every edit
//! arrives first as a permission request carrying the tool input. The
//! [`AuthorPolicy`] turns those into [`AuthorEdit`]s against the live buffer
//! (which the IDE applies with `DocStore::merge_write` as an `Agent`
//! transaction), allows edits of `.wh.md` files under the project, and
//! rejects everything else. Agents that do use `fs/write_text_file` are
//! handled the same way.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Result, anyhow};
use wh_agent::{
    AgentConfig, AgentHandle, PermissionDecision, PermissionRequest, PolicyError, SessionEvent, SessionOptions,
    SessionPolicy,
};

/// An edit of one `.wh.md` file by the author agent.
#[derive(Debug, Clone, PartialEq)]
pub struct AuthorEdit {
    /// Path relative to the project root.
    pub path: String,
    /// The text the agent's edit was computed against.
    pub base: String,
    pub new_text: String,
    pub label: String,
}

/// What the author session reports while a turn runs.
#[derive(Debug, Clone)]
pub enum AuthorEvent {
    Text(String),
    Tool(String),
    Edit(AuthorEdit),
    Finished,
    Error(String),
}

pub const SPEC: &str = include_str!("../../../docs/language-spec.md");

/// Tools the author never needs.
pub const DISALLOWED_TOOLS: &[&str] = &["Bash", "BashOutput", "KillShell", "WebFetch", "WebSearch", "NotebookEdit"];

/// Routes the agent's requests: reads under the project, writes only to
/// `.wh.md` files (mirrored into the document store), nothing else.
pub struct AuthorPolicy {
    root: PathBuf,
    /// Live buffer texts by relative path, updated as edits are emitted.
    buffers: Mutex<BTreeMap<String, String>>,
    sink: Box<dyn Fn(AuthorEdit) + Send + Sync>,
}

impl AuthorPolicy {
    pub fn new(root: impl Into<PathBuf>, buffers: BTreeMap<String, String>, sink: impl Fn(AuthorEdit) + Send + Sync + 'static) -> Self {
        let root = root.into();
        let root = root.canonicalize().unwrap_or(root);
        Self { root, buffers: Mutex::new(buffers), sink: Box::new(sink) }
    }

    fn rel(&self, path: &Path) -> Option<String> {
        let abs = if path.is_absolute() { path.to_path_buf() } else { self.root.join(path) };
        let abs = abs.canonicalize().unwrap_or(abs);
        let rel = abs.strip_prefix(&self.root).ok()?.to_string_lossy().replace('\\', "/");
        rel.ends_with(".wh.md").then_some(rel)
    }

    fn current(&self, rel: &str) -> Option<String> {
        if let Some(t) = self.buffers.lock().unwrap().get(rel) {
            return Some(t.clone());
        }
        std::fs::read_to_string(self.root.join(rel)).ok()
    }

    fn emit(&self, rel: String, base: String, new_text: String, label: String) {
        self.buffers.lock().unwrap().insert(rel.clone(), new_text.clone());
        (self.sink)(AuthorEdit { path: rel, base, new_text, label });
    }

    /// Compute the edit a Write/Edit/MultiEdit tool call would make.
    fn edit_from_tool(&self, req: &PermissionRequest) -> Option<(String, String, String)> {
        let input = req.raw_input.as_ref()?.as_object()?;
        let path = input.get("file_path").or_else(|| input.get("path"))?.as_str()?;
        let rel = self.rel(Path::new(path))?;
        let base = self.current(&rel).unwrap_or_default();
        if let Some(content) = input.get("content").and_then(|v| v.as_str()) {
            return Some((rel, base, content.to_string()));
        }
        let apply = |text: &str, e: &serde_json::Map<String, serde_json::Value>| -> Option<String> {
            let old = e.get("old_string")?.as_str()?;
            let new = e.get("new_string")?.as_str()?;
            let all = e.get("replace_all").and_then(|v| v.as_bool()).unwrap_or(false);
            if !text.contains(old) {
                return None;
            }
            Some(if all { text.replace(old, new) } else { text.replacen(old, new, 1) })
        };
        if let Some(edits) = input.get("edits").and_then(|v| v.as_array()) {
            let mut text = base.clone();
            for e in edits {
                text = apply(&text, e.as_object()?)?;
            }
            return Some((rel, base, text));
        }
        let text = apply(&base, input)?;
        Some((rel, base, text))
    }
}

impl SessionPolicy for AuthorPolicy {
    fn read_text_file(&self, path: &Path) -> Result<String, PolicyError> {
        let abs = if path.is_absolute() { path.to_path_buf() } else { self.root.join(path) };
        let abs = abs.canonicalize().map_err(|_| PolicyError::NotFound(path.to_path_buf()))?;
        if !abs.starts_with(&self.root) {
            return Err(PolicyError::Denied(format!("{} is outside the project", path.display())));
        }
        if let Some(rel) = self.rel(&abs) {
            if let Some(t) = self.current(&rel) {
                return Ok(t);
            }
        }
        std::fs::read_to_string(&abs).map_err(PolicyError::Io)
    }

    fn write_text_file(&self, path: &Path, content: &str) -> Result<(), PolicyError> {
        let rel = self.rel(path).ok_or_else(|| PolicyError::Denied("the author agent may only edit `.wh.md` sources".into()))?;
        let base = self.current(&rel).unwrap_or_default();
        self.emit(rel, base, content.to_string(), "author agent edit".into());
        Ok(())
    }

    fn decide_permission(&self, request: &PermissionRequest) -> PermissionDecision {
        // the `wh` MCP tools are the intended way to edit
        if wh_mcp::author::is_wh_tool_request(request) {
            return PermissionDecision::AllowOnce;
        }
        if request.kind == Some(wh_agent::acp::ToolKind::Read) {
            return PermissionDecision::AllowOnce;
        }
        match self.edit_from_tool(request) {
            Some((rel, base, new_text)) => {
                let label = request.title.clone().unwrap_or_else(|| "author agent edit".into());
                self.emit(rel, base, new_text, label);
                PermissionDecision::AllowOnce
            }
            None => PermissionDecision::RejectOnce,
        }
    }
}

/// The author agent of one project.
pub struct Author {
    root: PathBuf,
    config: AgentConfig,
    agent: Mutex<Option<AgentHandle>>,
}

impl Author {
    pub fn new(root: impl Into<PathBuf>, config: AgentConfig) -> Self {
        Self { root: root.into(), config, agent: Mutex::new(None) }
    }

    fn agent(&self) -> Result<AgentHandle> {
        let mut slot = self.agent.lock().unwrap();
        if let Some(a) = slot.as_ref() {
            return Ok(a.clone());
        }
        let a = AgentHandle::spawn(self.config.clone()).map_err(|e| anyhow!("starting the author agent: {e}"))?;
        *slot = Some(a.clone());
        Ok(a)
    }

    /// Ask the author agent to change the program. `buffers` are the live
    /// texts of the project's `.wh.md` files; `selection` is the source of the
    /// selected definition, if any. Blocks until the turn ends.
    pub fn ask(
        &self,
        instruction: &str,
        buffers: BTreeMap<String, String>,
        selection: Option<(&str, &str)>,
        on_event: impl Fn(AuthorEvent) + Send + Sync + Clone + 'static,
    ) -> Result<()> {
        self.ask_with(instruction, buffers, selection, None, on_event)
    }

    /// Like [`ask`](Self::ask); with `server` (the `wh` MCP server, see
    /// `wh_mcp::author`), the agent edits only through its semantic tools
    /// (`wh_apply_ops`, `wh_edit_prose`) and its own file-edit tools are disabled.
    pub fn ask_with(
        &self,
        instruction: &str,
        buffers: BTreeMap<String, String>,
        selection: Option<(&str, &str)>,
        server: Option<wh_agent::McpServerConfig>,
        on_event: impl Fn(AuthorEvent) + Send + Sync + Clone + 'static,
    ) -> Result<()> {
        let semantic = server.is_some();
        let spec_path = self.root.join(".wh/language-spec.md");
        if let Some(d) = spec_path.parent() {
            std::fs::create_dir_all(d)?;
        }
        std::fs::write(&spec_path, SPEC)?;
        let files: Vec<String> = buffers.keys().cloned().collect();
        let sink = on_event.clone();
        let policy = Arc::new(AuthorPolicy::new(&self.root, buffers, move |e| sink(AuthorEvent::Edit(e))));
        let agent = self.agent()?;
        let options = match server {
            Some(server) => wh_mcp::author::session_options(server),
            None => SessionOptions {
                meta: Some(wh_agent::claude_code_meta(serde_json::json!({
                    "disallowedTools": DISALLOWED_TOOLS,
                    "settingSources": [],
                }))),
                ..Default::default()
            },
        };
        let session = futures_lite_block_on(agent.new_session_with(&self.root, options, policy)).map_err(|e| anyhow!("{e}"))?;
        let mut prompt = author_prompt(instruction, &files, &spec_path, selection);
        if semantic {
            prompt = prompt.replace(
                "- Edit only the `.wh.md` files listed below; use your file edit tools (prefer small, targeted edits).\n",
                &format!("- {}\n", wh_mcp::author::PROMPT_RULES),
            );
        }
        let mut turn = session.prompt(prompt).map_err(|e| anyhow!("{e}"))?;
        while let Some(ev) = turn.recv_blocking() {
            match ev {
                SessionEvent::AgentText(t) => on_event(AuthorEvent::Text(t)),
                SessionEvent::ToolCallStarted { title, .. } => on_event(AuthorEvent::Tool(title)),
                SessionEvent::Error(e) if e.contains("Authentication required") => {
                    on_event(AuthorEvent::Error(format!(
                        "the author agent `{}` is not signed in: run `claude` and `/login`, or set ANTHROPIC_API_KEY in `[agent.author] env`",
                        self.config.display()
                    )));
                    return Ok(());
                }
                SessionEvent::Error(e) => {
                    on_event(AuthorEvent::Error(e));
                    return Ok(());
                }
                SessionEvent::Finished { .. } => {
                    on_event(AuthorEvent::Finished);
                    return Ok(());
                }
                _ => {}
            }
        }
        on_event(AuthorEvent::Finished);
        Ok(())
    }
}

pub fn author_prompt(instruction: &str, files: &[String], spec: &Path, selection: Option<(&str, &str)>) -> String {
    let mut p = String::from(
        "You are the author agent of a Wild Horse project: a literate program written as Markdown files (`*.wh.md`) \
         with fenced `wh` blocks in a Python-like language of types, function signatures, contracts, specs, \
         capabilities and flows. Implementations are NOT written here; the compiler synthesizes them.\n\n",
    );
    p.push_str(&format!("The language specification is at {} — read it before editing.\n\n", spec.display()));
    p.push_str("Rules:\n- Edit only the `.wh.md` files listed below; use your file edit tools (prefer small, targeted edits).\n");
    p.push_str("- Keep the literate style: explain new definitions in prose before their block, keep existing prose.\n");
    p.push_str("- State behaviour as contracts (`requires`/`ensures`/`raises … when`), `example`s and `property`s, never as code.\n");
    p.push_str("- Do not run commands. When done, summarize what you changed in two or three sentences.\n\n");
    p.push_str("Files:\n");
    for f in files {
        p.push_str(&format!("- {f}\n"));
    }
    if let Some((id, src)) = selection {
        p.push_str(&format!("\nThe user has selected `{id}`:\n```wh\n{src}\n```\n"));
    }
    p.push_str(&format!("\nRequest: {instruction}\n"));
    p
}

fn futures_lite_block_on<F: std::future::Future>(f: F) -> F::Output {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn request(raw: serde_json::Value) -> PermissionRequest {
        PermissionRequest {
            tool_call_id: "t".into(),
            title: Some("Edit".into()),
            kind: Some(wh_agent::acp::ToolKind::Edit),
            raw_input: Some(raw),
            locations: vec![],
            options: vec![],
        }
    }

    #[test]
    fn edits_of_sources_become_author_edits() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/a.wh.md"), "on disk").unwrap();
        std::fs::write(dir.path().join("notes.txt"), "x").unwrap();
        let got = Arc::new(Mutex::new(Vec::new()));
        let g = got.clone();
        let mut buffers = BTreeMap::new();
        buffers.insert("src/a.wh.md".to_string(), "def f(x: int) -> int\n".to_string());
        let policy = AuthorPolicy::new(dir.path(), buffers, move |e| g.lock().unwrap().push(e));
        let file = dir.path().join("src/a.wh.md");
        // Edit applies against the live buffer, not the disk
        let d = policy.decide_permission(&request(serde_json::json!({
            "file_path": file, "old_string": "-> int", "new_string": "-> str"
        })));
        assert_eq!(d, PermissionDecision::AllowOnce);
        // a second edit builds on the first
        policy.decide_permission(&request(serde_json::json!({
            "file_path": file, "edits": [{"old_string": "x: int", "new_string": "x: str"}]
        })));
        // anything else is rejected
        let d = policy.decide_permission(&request(serde_json::json!({ "file_path": dir.path().join("notes.txt"), "content": "y" })));
        assert_eq!(d, PermissionDecision::RejectOnce);
        let got = got.lock().unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].path, "src/a.wh.md");
        assert_eq!(got[0].new_text, "def f(x: int) -> str\n");
        assert_eq!(got[1].base, "def f(x: int) -> str\n");
        assert_eq!(got[1].new_text, "def f(x: str) -> str\n");
        // reads see the live buffer
        assert_eq!(policy.read_text_file(&file).unwrap(), "def f(x: str) -> str\n");
        assert!(policy.read_text_file(Path::new("/etc/hosts")).is_err());
    }
}
