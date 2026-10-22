//! One agent connection, driven on its own tokio thread.
//!
//! The ACP connection (and the subprocess) live on a dedicated thread with a
//! current-thread tokio runtime. Everything this module exposes is
//! runtime-agnostic: async methods only await `futures`/`async-channel`
//! primitives, so they can be polled by any executor (GPUI's included), and
//! each has a `_blocking` twin for plain threads.

use std::collections::HashMap;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use agent_client_protocol::{self as sdk, ConnectTo, ConnectionTo};
use futures::Stream;
use futures::channel::oneshot;

use crate::acp;
use crate::config::AgentConfig;
use crate::event::SessionEvent;
use crate::policy::{PermissionRequest, PolicyError, SessionPolicy, slice_lines};
use crate::process::{self, ChildGuard, StderrTail};

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// An MCP server attached to a session (ACP's `McpServer`: stdio, http or sse).
pub type McpServerConfig = acp::McpServer;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("failed to spawn agent `{command}`: {source}")]
    Spawn {
        command: String,
        source: std::io::Error,
    },
    #[error("agent returned an error: {0}")]
    Protocol(#[from] acp::Error),
    #[error("agent connection closed{}", stderr_suffix(.stderr))]
    Closed { stderr: Vec<String> },
    #[error("a prompt is already running in this session")]
    PromptInProgress,
}

fn stderr_suffix(stderr: &[String]) -> String {
    if stderr.is_empty() {
        String::new()
    } else {
        format!("; agent stderr:\n{}", stderr.join("\n"))
    }
}

/// Options for [`AgentHandle::new_session_with`].
#[derive(Clone, Default)]
pub struct SessionOptions {
    pub mcp_servers: Vec<McpServerConfig>,
    pub additional_directories: Vec<PathBuf>,
    /// `_meta` of `session/new`: agent-specific options, e.g.
    /// [`crate::claude_code_meta`] for `claude-agent-acp`.
    pub meta: Option<acp::Meta>,
}

type Sessions = Arc<Mutex<HashMap<acp::SessionId, Arc<SessionState>>>>;

struct SessionState {
    policy: Arc<dyn SessionPolicy>,
    /// Where the running prompt turn's events go; `None` between turns.
    turn: Mutex<Option<async_channel::Sender<SessionEvent>>>,
}

impl SessionState {
    fn emit(&self, event: SessionEvent) {
        match self.turn.lock().unwrap().as_ref() {
            Some(turn) => {
                let _ = turn.try_send(event);
            }
            None => tracing::trace!(?event, "session event outside a prompt turn"),
        }
    }
}

struct Connection {
    cx: ConnectionTo<sdk::Agent>,
    sessions: Sessions,
    init: acp::InitializeResponse,
    stderr: StderrTail,
    stop: Mutex<Option<oneshot::Sender<()>>>,
}

impl Connection {
    fn map_err(&self, err: acp::Error) -> Error {
        map_err(&self.stderr, err)
    }
}

fn map_err(stderr: &StderrTail, err: acp::Error) -> Error {
    if sdk::is_incoming_transport_closed(&err) || sdk::is_connection_terminated(&err) {
        Error::Closed {
            stderr: stderr.lines(),
        }
    } else {
        Error::Protocol(err)
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.lock().unwrap().take() {
            let _ = stop.send(());
        }
    }
}

/// A running, initialized agent. Cheap to clone; the agent is shut down
/// when the last handle (including session handles) is dropped, or by
/// [`AgentHandle::shutdown`].
#[derive(Clone)]
pub struct AgentHandle {
    conn: Arc<Connection>,
}

impl AgentHandle {
    /// Spawn the agent subprocess and run `initialize`.
    pub async fn start(config: AgentConfig) -> Result<Self> {
        let stderr = StderrTail::default();
        let command = config.display();
        let tail = stderr.clone();
        Self::start_with(stderr, move || {
            process::spawn(&config, tail)
                .map(|(transport, child)| (transport, Some(child)))
                .map_err(|source| Error::Spawn { command, source })
        })
        .await
    }

    /// Blocking [`AgentHandle::start`].
    pub fn spawn(config: AgentConfig) -> Result<Self> {
        futures::executor::block_on(Self::start(config))
    }

    /// Connect over an arbitrary ACP transport (an in-process agent, a
    /// socket, …) and run `initialize`.
    pub async fn connect(transport: impl ConnectTo<sdk::Client>) -> Result<Self> {
        Self::start_with(StderrTail::default(), move || Ok((transport, None))).await
    }

    /// Blocking [`AgentHandle::connect`].
    pub fn connect_blocking(transport: impl ConnectTo<sdk::Client>) -> Result<Self> {
        futures::executor::block_on(Self::connect(transport))
    }

    async fn start_with<T, F>(stderr: StderrTail, make_transport: F) -> Result<Self>
    where
        T: ConnectTo<sdk::Client>,
        F: FnOnce() -> Result<(T, Option<ChildGuard>)> + Send + 'static,
    {
        let sessions = Sessions::default();
        let (cx_tx, cx_rx) = oneshot::channel::<Result<ConnectionTo<sdk::Agent>>>();
        let (stop_tx, stop_rx) = oneshot::channel::<()>();
        let handlers = sessions.clone();

        std::thread::Builder::new()
            .name("wh-agent-acp".into())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(source) => {
                        let command = "tokio runtime".into();
                        let _ = cx_tx.send(Err(Error::Spawn { command, source }));
                        return;
                    }
                };
                runtime.block_on(run_connection(handlers, make_transport, cx_tx, stop_rx));
            })
            .map_err(|source| Error::Spawn {
                command: "wh-agent-acp thread".into(),
                source,
            })?;

        let closed = || Error::Closed {
            stderr: stderr.lines(),
        };
        let cx = cx_rx.await.map_err(|_| closed())??;

        let capabilities = acp::ClientCapabilities::new()
            .fs(acp::FileSystemCapabilities::new()
                .read_text_file(true)
                .write_text_file(true))
            .terminal(false);
        let request = acp::InitializeRequest::new(sdk::schema::ProtocolVersion::V1)
            .client_capabilities(capabilities)
            .client_info(acp::Implementation::new(
                "wild-horse",
                env!("CARGO_PKG_VERSION"),
            ));
        let init = cx
            .send_request(request)
            .block_task()
            .await
            .map_err(|err| map_err(&stderr, err))?;
        tracing::info!(
            agent = ?init.agent_info,
            protocol = ?init.protocol_version,
            auth_methods = init.auth_methods.len(),
            "agent initialized"
        );
        Ok(AgentHandle {
            conn: Arc::new(Connection {
                cx,
                sessions,
                init,
                stderr,
                stop: Mutex::new(Some(stop_tx)),
            }),
        })
    }

    /// The agent's full `initialize` response.
    pub fn initialize_response(&self) -> &acp::InitializeResponse {
        &self.conn.init
    }

    pub fn agent_info(&self) -> Option<&acp::Implementation> {
        self.conn.init.agent_info.as_ref()
    }

    pub fn auth_methods(&self) -> &[acp::AuthMethod] {
        &self.conn.init.auth_methods
    }

    /// The last lines the agent printed to stderr (empty for non-process
    /// transports).
    pub fn stderr_tail(&self) -> Vec<String> {
        self.conn.stderr.lines()
    }

    pub async fn authenticate(&self, method_id: impl Into<acp::AuthMethodId>) -> Result<()> {
        self.conn
            .cx
            .send_request(acp::AuthenticateRequest::new(method_id))
            .block_task()
            .await
            .map_err(|err| self.conn.map_err(err))?;
        Ok(())
    }

    pub fn authenticate_blocking(&self, method_id: impl Into<acp::AuthMethodId>) -> Result<()> {
        futures::executor::block_on(self.authenticate(method_id))
    }

    /// Open a session whose fs and permission requests go to `policy`.
    pub async fn new_session(
        &self,
        cwd: impl Into<PathBuf>,
        mcp_servers: Vec<McpServerConfig>,
        policy: Arc<dyn SessionPolicy>,
    ) -> Result<SessionHandle> {
        let options = SessionOptions {
            mcp_servers,
            ..SessionOptions::default()
        };
        self.new_session_with(cwd, options, policy).await
    }

    pub fn new_session_blocking(
        &self,
        cwd: impl Into<PathBuf>,
        mcp_servers: Vec<McpServerConfig>,
        policy: Arc<dyn SessionPolicy>,
    ) -> Result<SessionHandle> {
        futures::executor::block_on(self.new_session(cwd, mcp_servers, policy))
    }

    pub async fn new_session_with(
        &self,
        cwd: impl Into<PathBuf>,
        options: SessionOptions,
        policy: Arc<dyn SessionPolicy>,
    ) -> Result<SessionHandle> {
        let cwd = cwd.into();
        let request = acp::NewSessionRequest::new(cwd.clone())
            .mcp_servers(options.mcp_servers)
            .additional_directories(options.additional_directories)
            .meta(options.meta);
        let response = self
            .conn
            .cx
            .send_request(request)
            .block_task()
            .await
            .map_err(|err| self.conn.map_err(err))?;
        let state = Arc::new(SessionState {
            policy,
            turn: Mutex::new(None),
        });
        self.conn
            .sessions
            .lock()
            .unwrap()
            .insert(response.session_id.clone(), state.clone());
        tracing::info!(session = %response.session_id, cwd = %cwd.display(), "session opened");
        Ok(SessionHandle {
            id: response.session_id,
            conn: self.conn.clone(),
            state,
        })
    }

    /// Stop the connection and kill the agent now, even if handles remain.
    pub fn shutdown(&self) {
        if let Some(stop) = self.conn.stop.lock().unwrap().take() {
            let _ = stop.send(());
        }
    }
}

/// One ACP session. Dropping it stops routing the agent's requests for this
/// session (they are answered with an error).
pub struct SessionHandle {
    id: acp::SessionId,
    conn: Arc<Connection>,
    state: Arc<SessionState>,
}

impl SessionHandle {
    pub fn id(&self) -> &str {
        &self.id.0
    }

    /// Send a text prompt. Events stream in until `Finished` or `Error`.
    pub fn prompt(&self, text: impl Into<String>) -> Result<PromptStream> {
        let block = acp::ContentBlock::Text(acp::TextContent::new(text.into()));
        self.prompt_blocks(vec![block])
    }

    pub fn prompt_blocks(&self, blocks: Vec<acp::ContentBlock>) -> Result<PromptStream> {
        let (tx, rx) = async_channel::unbounded();
        {
            let mut turn = self.state.turn.lock().unwrap();
            if turn.is_some() {
                return Err(Error::PromptInProgress);
            }
            *turn = Some(tx.clone());
        }
        let sent = self
            .conn
            .cx
            .send_request(acp::PromptRequest::new(self.id.clone(), blocks));
        let state = self.state.clone();
        let conn = Arc::downgrade(&self.conn);
        let session = self.id.clone();
        let spawned = self.conn.cx.spawn(async move {
            let event = match sent.block_task().await {
                Ok(response) => SessionEvent::Finished {
                    stop_reason: response.stop_reason,
                },
                Err(err) => {
                    let message = match conn.upgrade() {
                        Some(conn) => conn.map_err(err).to_string(),
                        None => err.to_string(),
                    };
                    SessionEvent::Error(message)
                }
            };
            tracing::debug!(%session, ?event, "prompt turn ended");
            state.turn.lock().unwrap().take();
            let _ = tx.try_send(event);
            Ok(())
        });
        if let Err(err) = spawned {
            self.state.turn.lock().unwrap().take();
            return Err(self.conn.map_err(err));
        }
        Ok(PromptStream {
            events: Box::pin(rx),
            done: false,
        })
    }

    /// Ask the agent to stop the running turn (`session/cancel`). The turn
    /// still ends with `Finished { stop_reason: Cancelled }`.
    pub fn cancel(&self) -> Result<()> {
        self.conn
            .cx
            .send_notification(acp::CancelNotification::new(self.id.clone()))
            .map_err(|err| self.conn.map_err(err))
    }

    /// The agent this session belongs to.
    pub fn agent(&self) -> AgentHandle {
        AgentHandle {
            conn: self.conn.clone(),
        }
    }
}

impl Drop for SessionHandle {
    fn drop(&mut self) {
        self.conn.sessions.lock().unwrap().remove(&self.id);
    }
}

/// The events of one prompt turn. Ends after `Finished` or `Error`; if the
/// connection dies mid-turn, a final `Error` is synthesized.
pub struct PromptStream {
    events: Pin<Box<async_channel::Receiver<SessionEvent>>>,
    done: bool,
}

impl PromptStream {
    fn deliver(&mut self, event: Option<SessionEvent>) -> Option<SessionEvent> {
        if self.done {
            return None;
        }
        let event = event.unwrap_or_else(|| SessionEvent::Error("agent connection closed".into()));
        self.done = event.is_terminal();
        Some(event)
    }

    pub async fn recv(&mut self) -> Option<SessionEvent> {
        if self.done {
            return None;
        }
        let event = self.events.recv().await.ok();
        self.deliver(event)
    }

    pub fn recv_blocking(&mut self) -> Option<SessionEvent> {
        if self.done {
            return None;
        }
        let event = self.events.recv_blocking().ok();
        self.deliver(event)
    }

    /// Wait for the whole turn and return every event, the terminal one last.
    pub fn collect_blocking(mut self) -> Vec<SessionEvent> {
        std::iter::from_fn(|| self.recv_blocking()).collect()
    }
}

impl Stream for PromptStream {
    type Item = SessionEvent;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<SessionEvent>> {
        if self.done {
            return Poll::Ready(None);
        }
        match self.events.as_mut().poll_next(cx) {
            Poll::Ready(event) => Poll::Ready(self.deliver(event)),
            Poll::Pending => Poll::Pending,
        }
    }
}

/// Runs on the connection thread until stopped or the agent hangs up.
async fn run_connection<T, F>(
    sessions: Sessions,
    make_transport: F,
    cx_tx: oneshot::Sender<Result<ConnectionTo<sdk::Agent>>>,
    stop_rx: oneshot::Receiver<()>,
) where
    T: ConnectTo<sdk::Client>,
    F: FnOnce() -> Result<(T, Option<ChildGuard>)>,
{
    let (transport, child) = match make_transport() {
        Ok(made) => made,
        Err(err) => {
            let _ = cx_tx.send(Err(err));
            return;
        }
    };
    let result = client_builder(sessions)
        .connect_with(transport, async move |cx: ConnectionTo<sdk::Agent>| {
            let _ = cx_tx.send(Ok(cx.clone()));
            let closed = std::pin::pin!(cx.incoming_closed());
            match futures::future::select(stop_rx, closed).await {
                futures::future::Either::Left(_) => tracing::debug!("agent connection stopped"),
                futures::future::Either::Right(_) => tracing::info!("agent closed the connection"),
            }
            Ok(())
        })
        .await;
    if let Err(err) = result {
        tracing::warn!(%err, "agent connection failed");
    }
    drop(child);
}

fn session(sessions: &Sessions, id: &acp::SessionId) -> Result<Arc<SessionState>, acp::Error> {
    sessions
        .lock()
        .unwrap()
        .get(id)
        .cloned()
        .ok_or_else(|| acp::Error::invalid_params().data(format!("unknown session {id}")))
}

fn policy_error(err: &PolicyError) -> acp::Error {
    match err {
        PolicyError::NotFound(path) => {
            acp::Error::resource_not_found(Some(path.display().to_string()))
        }
        PolicyError::Denied(_) | PolicyError::Io(_) => acp::Error::new(-32000, err.to_string()),
    }
}

fn client_builder(
    sessions: Sessions,
) -> sdk::Builder<sdk::Client, impl sdk::HandleDispatchFrom<sdk::Agent>> {
    let on_update = sessions.clone();
    let on_read = sessions.clone();
    let on_write = sessions.clone();
    let on_permission = sessions;
    sdk::Client
        .builder()
        .name("wild-horse")
        .on_receive_notification(
            async move |notification: acp::SessionNotification, _cx| {
                match on_update.lock().unwrap().get(&notification.session_id) {
                    Some(state) => state.emit(SessionEvent::from_update(notification.update)),
                    None => tracing::debug!(
                        session = %notification.session_id,
                        "update for unknown session"
                    ),
                }
                Ok(())
            },
            sdk::on_receive_notification!(),
        )
        .on_receive_request(
            async move |request: acp::ReadTextFileRequest, responder, _cx| {
                let state = match session(&on_read, &request.session_id) {
                    Ok(state) => state,
                    Err(err) => return responder.respond_with_error(err),
                };
                let result = state.policy.read_text_file(&request.path);
                tracing::debug!(path = %request.path.display(), ok = result.is_ok(), "fs/read_text_file");
                state.emit(SessionEvent::FileRead {
                    path: request.path.clone(),
                    result: result.as_ref().map(|_| ()).map_err(ToString::to_string),
                });
                match result {
                    Ok(text) => responder.respond(acp::ReadTextFileResponse::new(slice_lines(
                        &text,
                        request.line,
                        request.limit,
                    ))),
                    Err(err) => responder.respond_with_error(policy_error(&err)),
                }
            },
            sdk::on_receive_request!(),
        )
        .on_receive_request(
            async move |request: acp::WriteTextFileRequest, responder, _cx| {
                let state = match session(&on_write, &request.session_id) {
                    Ok(state) => state,
                    Err(err) => return responder.respond_with_error(err),
                };
                let result = state
                    .policy
                    .write_text_file(&request.path, &request.content);
                tracing::info!(path = %request.path.display(), ok = result.is_ok(), "fs/write_text_file");
                state.emit(SessionEvent::FileWrite {
                    path: request.path.clone(),
                    result: result.as_ref().map(|_| ()).map_err(ToString::to_string),
                });
                match result {
                    Ok(()) => responder.respond(acp::WriteTextFileResponse::new()),
                    Err(err) => responder.respond_with_error(policy_error(&err)),
                }
            },
            sdk::on_receive_request!(),
        )
        .on_receive_request(
            async move |request: acp::RequestPermissionRequest, responder, _cx| {
                let state = match session(&on_permission, &request.session_id) {
                    Ok(state) => state,
                    Err(err) => return responder.respond_with_error(err),
                };
                let flat = PermissionRequest::from_acp(&request);
                let decision = state.policy.decide_permission(&flat);
                let selected = decision.resolve(&flat.options).map(|o| o.id.clone());
                tracing::info!(
                    tool_call = %flat.tool_call_id,
                    title = ?flat.title,
                    ?decision,
                    ?selected,
                    "session/request_permission"
                );
                let outcome = match &selected {
                    Some(id) => acp::RequestPermissionOutcome::Selected(
                        acp::SelectedPermissionOutcome::new(id.clone()),
                    ),
                    None => acp::RequestPermissionOutcome::Cancelled,
                };
                state.emit(SessionEvent::PermissionDecided {
                    request: flat,
                    decision,
                    selected,
                });
                responder.respond(acp::RequestPermissionResponse::new(outcome))
            },
            sdk::on_receive_request!(),
        )
}
