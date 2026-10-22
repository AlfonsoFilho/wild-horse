//! The client against an in-process mock agent built with the SDK's agent
//! side: fs reads and writes, permission requests and streamed updates must
//! go through the session policy and come back as events, in order.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agent_client_protocol::{self as sdk, Channel, ConnectionTo};
use wh_agent::{
    AgentHandle, Error, PermissionDecision, PermissionRequest, PolicyError, ScopedPolicy,
    SessionEvent, SessionPolicy, acp,
};

/// What the mock agent saw, reported back to the test.
type Log = Arc<Mutex<Vec<String>>>;

fn text(s: &str) -> acp::ContentChunk {
    acp::ContentChunk::new(acp::ContentBlock::Text(acp::TextContent::new(s)))
}

fn update(cx: &ConnectionTo<sdk::Client>, session: &acp::SessionId, update: acp::SessionUpdate) {
    cx.send_notification(acp::SessionNotification::new(session.clone(), update))
        .unwrap();
}

/// The scripted turn for the prompt "work <root>": read lib.rs, write the
/// hole (allowed) and lib.rs (disallowed), ask permission to run a shell
/// command, then stream a plan, a thought and some text.
async fn work_turn(
    cx: ConnectionTo<sdk::Client>,
    session: acp::SessionId,
    root: PathBuf,
    log: Log,
) {
    let record = |line: String| log.lock().unwrap().push(line);

    let read = cx
        .send_request(
            acp::ReadTextFileRequest::new(session.clone(), root.join("src/lib.rs"))
                .line(2)
                .limit(1),
        )
        .block_task()
        .await;
    record(format!("read lib.rs: {:?}", read.map(|r| r.content)));

    let read = cx
        .send_request(acp::ReadTextFileRequest::new(
            session.clone(),
            root.join("../outside.txt"),
        ))
        .block_task()
        .await;
    record(format!(
        "read outside: {}",
        if read.is_ok() { "ok" } else { "err" }
    ));

    for (name, path) in [
        ("hole", root.join("src/hole.rs")),
        ("lib", root.join("src/lib.rs")),
    ] {
        let write = cx
            .send_request(acp::WriteTextFileRequest::new(
                session.clone(),
                path,
                "fn hole() {}\n",
            ))
            .block_task()
            .await;
        record(format!(
            "write {name}: {}",
            if write.is_ok() { "ok" } else { "err" }
        ));
    }

    update(
        &cx,
        &session,
        acp::SessionUpdate::ToolCall(
            acp::ToolCall::new("call-1", "cargo test").kind(acp::ToolKind::Execute),
        ),
    );
    let permission = cx
        .send_request(acp::RequestPermissionRequest::new(
            session.clone(),
            acp::ToolCallUpdate::new(
                "call-1",
                acp::ToolCallUpdateFields::new()
                    .kind(acp::ToolKind::Execute)
                    .title("cargo test".to_string())
                    .raw_input(serde_json::json!({ "command": "cargo test" })),
            ),
            vec![
                acp::PermissionOption::new(
                    "allow-once",
                    "Allow",
                    acp::PermissionOptionKind::AllowOnce,
                ),
                acp::PermissionOption::new(
                    "reject",
                    "Reject",
                    acp::PermissionOptionKind::RejectOnce,
                ),
            ],
        ))
        .block_task()
        .await
        .unwrap();
    record(format!("permission: {:?}", permission.outcome));
    update(
        &cx,
        &session,
        acp::SessionUpdate::ToolCallUpdate(acp::ToolCallUpdate::new(
            "call-1",
            acp::ToolCallUpdateFields::new().status(acp::ToolCallStatus::Failed),
        )),
    );

    update(
        &cx,
        &session,
        acp::SessionUpdate::Plan(acp::Plan::new(vec![acp::PlanEntry::new(
            "write the hole",
            acp::PlanEntryPriority::High,
            acp::PlanEntryStatus::InProgress,
        )])),
    );
    update(
        &cx,
        &session,
        acp::SessionUpdate::AgentThoughtChunk(text("thinking")),
    );
    update(
        &cx,
        &session,
        acp::SessionUpdate::AgentMessageChunk(text("Hello, ")),
    );
    update(
        &cx,
        &session,
        acp::SessionUpdate::AgentMessageChunk(text("world")),
    );
}

/// Start the mock agent on its own thread and connect a client to it.
fn connect_mock(log: Log) -> AgentHandle {
    let (client_end, agent_end) = Channel::duplex();
    let cancelled = Arc::new(tokio::sync::Notify::new());
    let on_cancel = cancelled.clone();
    let (hang_up_tx, hang_up_rx) = futures::channel::oneshot::channel::<()>();
    let hang_up = Arc::new(Mutex::new(Some(hang_up_tx)));
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let agent = sdk::Agent
            .builder()
            .name("mock-agent")
            .on_receive_request(
                async move |request: acp::InitializeRequest, responder, _cx| {
                    assert!(request.client_capabilities.fs.read_text_file);
                    assert!(request.client_capabilities.fs.write_text_file);
                    assert!(!request.client_capabilities.terminal);
                    responder.respond(
                        acp::InitializeResponse::new(request.protocol_version)
                            .agent_info(acp::Implementation::new("mock-agent", "0.0.0")),
                    )
                },
                sdk::on_receive_request!(),
            )
            .on_receive_request(
                async move |_request: acp::NewSessionRequest, responder, _cx| {
                    responder.respond(acp::NewSessionResponse::new("session-1"))
                },
                sdk::on_receive_request!(),
            )
            .on_receive_notification(
                async move |_cancel: acp::CancelNotification, _cx| {
                    on_cancel.notify_one();
                    Ok(())
                },
                sdk::on_receive_notification!(),
            )
            .on_receive_request(
                async move |request: acp::PromptRequest,
                            responder,
                            cx: ConnectionTo<sdk::Client>| {
                    let acp::ContentBlock::Text(prompt) = &request.prompt[0] else {
                        panic!("text prompt expected");
                    };
                    let prompt = prompt.text.clone();
                    let log = log.clone();
                    let cancelled = cancelled.clone();
                    let session = request.session_id.clone();
                    let turn_cx = cx.clone();
                    let hang_up = hang_up.clone();
                    // The turn talks back to the client, so it must not hold
                    // the dispatch loop.
                    cx.spawn(async move {
                        let stop_reason = if let Some(root) = prompt.strip_prefix("work ") {
                            work_turn(turn_cx, session, PathBuf::from(root), log).await;
                            acp::StopReason::EndTurn
                        } else if prompt == "wait" {
                            update(
                                &turn_cx,
                                &session,
                                acp::SessionUpdate::AgentMessageChunk(text("waiting")),
                            );
                            cancelled.notified().await;
                            acp::StopReason::Cancelled
                        } else if prompt == "hang up" {
                            update(
                                &turn_cx,
                                &session,
                                acp::SessionUpdate::AgentMessageChunk(text("bye")),
                            );
                            if let Some(hang_up) = hang_up.lock().unwrap().take() {
                                let _ = hang_up.send(());
                            }
                            futures::future::pending::<()>().await;
                            unreachable!()
                        } else {
                            acp::StopReason::Refusal
                        };
                        responder.respond(acp::PromptResponse::new(stop_reason))
                    })
                },
                sdk::on_receive_request!(),
            );
        // "hang up" drops the agent's end of the transport mid-turn.
        runtime.block_on(futures::future::select(
            Box::pin(agent.connect_to(agent_end)),
            hang_up_rx,
        ));
    });
    AgentHandle::connect_blocking(client_end).expect("connect to mock agent")
}

struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().canonicalize().unwrap();
    let root = base.join("crate");
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("src/lib.rs"),
        "// line 1\npub mod hole;\n// line 3\n",
    )
    .unwrap();
    std::fs::write(root.join("src/hole.rs"), "fn hole() { todo!() }\n").unwrap();
    std::fs::write(base.join("outside.txt"), "secret").unwrap();
    Fixture { _dir: dir, root }
}

#[test]
fn scoped_policy_turn_routes_fs_and_permissions() {
    let log = Log::default();
    let agent = connect_mock(log.clone());
    assert_eq!(
        agent.agent_info().map(|i| i.name.as_str()),
        Some("mock-agent")
    );

    let f = fixture();
    let hole = f.root.join("src/hole.rs");
    let policy = Arc::new(ScopedPolicy::new(&f.root).allow_write(&hole));
    let session = agent
        .new_session_blocking(&f.root, Vec::new(), policy)
        .unwrap();
    assert_eq!(session.id(), "session-1");

    let events = session
        .prompt(format!("work {}", f.root.display()))
        .unwrap()
        .collect_blocking();

    // What the agent was told.
    assert_eq!(
        *log.lock().unwrap(),
        [
            "read lib.rs: Ok(\"pub mod hole;\\n\")",
            "read outside: err",
            "write hole: ok",
            "write lib: err",
            "permission: Selected(SelectedPermissionOutcome { option_id: PermissionOptionId(\"reject\"), meta: None })",
        ]
    );
    // What reached the disk.
    assert_eq!(std::fs::read_to_string(&hole).unwrap(), "fn hole() {}\n");
    assert_eq!(
        std::fs::read_to_string(f.root.join("src/lib.rs")).unwrap(),
        "// line 1\npub mod hole;\n// line 3\n"
    );

    // What the client reported, in order.
    let mut events = events.into_iter();
    let mut next = || events.next().expect("more events");
    assert!(
        matches!(next(), SessionEvent::FileRead { path, result: Ok(()) } if path.ends_with("src/lib.rs"))
    );
    assert!(
        matches!(next(), SessionEvent::FileRead { result: Err(e), .. } if e.contains("outside"))
    );
    assert_eq!(
        next(),
        SessionEvent::FileWrite {
            path: hole.clone(),
            result: Ok(())
        }
    );
    assert!(
        matches!(next(), SessionEvent::FileWrite { result: Err(e), .. } if e.contains("not allowed"))
    );
    assert_eq!(
        next(),
        SessionEvent::ToolCallStarted {
            id: "call-1".into(),
            title: "cargo test".into(),
            kind: acp::ToolKind::Execute,
            status: acp::ToolCallStatus::Pending,
        }
    );
    match next() {
        SessionEvent::PermissionDecided {
            request,
            decision,
            selected,
        } => {
            assert_eq!(request.title.as_deref(), Some("cargo test"));
            assert_eq!(request.options.len(), 2);
            assert_eq!(decision, PermissionDecision::RejectOnce);
            assert_eq!(selected.as_deref(), Some("reject"));
        }
        other => panic!("expected a permission decision, got {other:?}"),
    }
    assert_eq!(
        next(),
        SessionEvent::ToolCallUpdated {
            id: "call-1".into(),
            title: None,
            status: Some(acp::ToolCallStatus::Failed),
        }
    );
    assert!(
        matches!(next(), SessionEvent::Plan(entries) if entries[0].content == "write the hole")
    );
    assert_eq!(next(), SessionEvent::AgentThought("thinking".into()));
    assert_eq!(next(), SessionEvent::AgentText("Hello, ".into()));
    assert_eq!(next(), SessionEvent::AgentText("world".into()));
    assert_eq!(
        next(),
        SessionEvent::Finished {
            stop_reason: acp::StopReason::EndTurn
        }
    );
    assert!(events.next().is_none());
}

/// Allows everything, writes nothing, and remembers what it was asked.
#[derive(Default)]
struct RecordingPolicy {
    asked: Mutex<Vec<String>>,
}

impl SessionPolicy for RecordingPolicy {
    fn read_text_file(&self, path: &Path) -> Result<String, PolicyError> {
        self.asked
            .lock()
            .unwrap()
            .push(format!("read {}", path.display()));
        Ok("a\nb\nc\n".into())
    }

    fn write_text_file(&self, path: &Path, content: &str) -> Result<(), PolicyError> {
        self.asked
            .lock()
            .unwrap()
            .push(format!("write {} {content:?}", path.display()));
        Ok(())
    }

    fn decide_permission(&self, request: &PermissionRequest) -> PermissionDecision {
        self.asked
            .lock()
            .unwrap()
            .push(format!("permission {}", request.tool_call_id));
        PermissionDecision::AllowAlways
    }
}

#[test]
fn custom_policy_receives_every_request() {
    let log = Log::default();
    let agent = connect_mock(log.clone());
    let f = fixture();
    let policy = Arc::new(RecordingPolicy::default());
    let session =
        futures::executor::block_on(agent.new_session(&f.root, Vec::new(), policy.clone()))
            .unwrap();

    // Drive the turn as an async stream, the way the IDE does.
    let events: Vec<SessionEvent> = futures::executor::block_on(futures::StreamExt::collect(
        session
            .prompt(format!("work {}", f.root.display()))
            .unwrap(),
    ));
    assert!(matches!(events.last(), Some(SessionEvent::Finished { .. })));

    let lib = f.root.join("src/lib.rs");
    assert_eq!(
        *policy.asked.lock().unwrap(),
        [
            format!("read {}", lib.display()),
            format!("read {}", f.root.join("../outside.txt").display()),
            format!(
                "write {} \"fn hole() {{}}\\n\"",
                f.root.join("src/hole.rs").display()
            ),
            format!("write {} \"fn hole() {{}}\\n\"", lib.display()),
            "permission call-1".to_string(),
        ]
    );
    let log = log.lock().unwrap();
    assert_eq!(log[0], "read lib.rs: Ok(\"b\\n\")");
    // AllowAlways falls back to the offered allow-once option.
    assert!(log[4].contains("allow-once"), "{}", log[4]);
}

#[test]
fn cancel_ends_the_turn_and_one_prompt_at_a_time() {
    let agent = connect_mock(Log::default());
    let f = fixture();
    let session = agent
        .new_session_blocking(&f.root, Vec::new(), Arc::new(ScopedPolicy::new(&f.root)))
        .unwrap();

    let mut turn = session.prompt("wait").unwrap();
    assert_eq!(
        turn.recv_blocking(),
        Some(SessionEvent::AgentText("waiting".into()))
    );
    assert!(matches!(
        session.prompt("again"),
        Err(Error::PromptInProgress)
    ));

    session.cancel().unwrap();
    assert_eq!(
        turn.recv_blocking(),
        Some(SessionEvent::Finished {
            stop_reason: acp::StopReason::Cancelled
        })
    );
    assert_eq!(turn.recv_blocking(), None);

    // The session is free again.
    let events = session.prompt("anything").unwrap().collect_blocking();
    assert_eq!(
        events,
        [SessionEvent::Finished {
            stop_reason: acp::StopReason::Refusal
        }]
    );
}

#[test]
fn agent_hanging_up_mid_turn_ends_the_turn_with_an_error() {
    let agent = connect_mock(Log::default());
    let f = fixture();
    let session = agent
        .new_session_blocking(&f.root, Vec::new(), Arc::new(ScopedPolicy::new(&f.root)))
        .unwrap();
    let events = session.prompt("hang up").unwrap().collect_blocking();
    // The mock drops its transport at once, so "bye" may or may not make it.
    assert!(
        matches!(events.last(), Some(SessionEvent::Error(e)) if e.contains("closed")),
        "{events:?}"
    );
    assert!(matches!(
        agent.new_session_blocking(&f.root, Vec::new(), Arc::new(ScopedPolicy::new(&f.root))),
        Err(Error::Closed { .. })
    ));
}

#[test]
fn spawn_failure_and_early_exit_are_reported() {
    let missing = wh_agent::AgentConfig::new("/nonexistent/wh-agent-test-binary");
    assert!(matches!(
        AgentHandle::spawn(missing),
        Err(Error::Spawn { .. })
    ));

    let crashing = wh_agent::AgentConfig::new("sh")
        .arg("-c")
        .arg("echo \"boom: $WH_TEST_VAR in $(pwd)\" >&2; exit 3")
        .env("WH_TEST_VAR", "env-ok")
        .cwd(std::env::temp_dir());
    let started = std::time::Instant::now();
    match AgentHandle::spawn(crashing) {
        Err(Error::Closed { stderr }) => {
            let line = stderr.join("\n");
            assert!(line.contains("boom: env-ok in /"), "{line}");
        }
        Err(other) => panic!("expected Closed, got {other}"),
        Ok(_) => panic!("expected the agent to fail"),
    }
    assert!(started.elapsed() < Duration::from_secs(10));
}

#[test]
fn config_parses_from_toml() {
    #[derive(serde::Deserialize)]
    struct File {
        agent: std::collections::BTreeMap<String, wh_agent::AgentConfig>,
    }
    let file: File = toml::from_str(
        r#"
        [agent.compiler]
        command = "npx"
        args = ["-y", "@agentclientprotocol/claude-agent-acp"]
        env = { ANTHROPIC_MODEL = "claude-opus-5-5" }

        [agent.author]
        command = "my-agent"
        cwd = "/tmp"
        "#,
    )
    .unwrap();
    assert_eq!(
        file.agent["compiler"],
        wh_agent::AgentConfig::claude_agent_acp().env("ANTHROPIC_MODEL", "claude-opus-5-5")
    );
    assert_eq!(file.agent["author"].cwd.as_deref(), Some(Path::new("/tmp")));
}

/// Talks to the real `claude-agent-acp` (network, a Claude login, npx).
/// `cargo test -p wh-agent -- --ignored real_claude --nocapture`
#[test]
#[ignore]
fn real_claude_agent_acp() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new("wh_agent=debug"))
        .try_init();
    let f = fixture();
    let agent = AgentHandle::spawn(wh_agent::AgentConfig::claude_agent_acp()).unwrap();
    eprintln!(
        "agent: {:?}, auth: {:?}",
        agent.agent_info(),
        agent.auth_methods()
    );
    let hole = f.root.join("src/hole.rs");
    let options = wh_agent::SessionOptions {
        meta: Some(wh_agent::claude_code_meta(serde_json::json!({
            "disallowedTools": ["Bash", "WebFetch", "WebSearch"],
            "settingSources": [],
        }))),
        ..Default::default()
    };
    let policy = Arc::new(ScopedPolicy::new(&f.root).allow_write(&hole));
    let session =
        futures::executor::block_on(agent.new_session_with(&f.root, options, policy)).unwrap();
    let mut turn = session
        .prompt(format!(
            "Replace the body of `hole` in {} with `{{}}` (keep the signature), then run `ls` in a shell, then stop.",
            hole.display()
        ))
        .unwrap();
    while let Some(event) = turn.recv_blocking() {
        eprintln!("{event:?}");
    }
    eprintln!("hole.rs now: {}", std::fs::read_to_string(&hole).unwrap());
}
