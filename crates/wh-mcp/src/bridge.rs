//! Bridge mode: the stdio server spawned by the agent is a thin shim that
//! forwards each tool call to a host (the IDE) over a local Unix socket. The
//! host implements [`Backend`] on its live document store and serves it with
//! [`serve`].
//!
//! Protocol: newline-delimited JSON over one stream connection, one request
//! at a time per connection (the host serves connections concurrently).
//!
//! ```text
//! → {"id": 1, "tool": "wh_apply_ops", "arguments": {"ops": [...]}}
//! ← {"id": 1, "result": {...}}            (wh_spec: a string)
//! ← {"id": 1, "error": "op 0 (rename to x): unknown node `m.y`"}
//! ```
//!
//! `tool` and `arguments` are exactly the MCP tool name and arguments, so the
//! shim needs no knowledge of the tools and the host reuses [`dispatch`].

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::server::{ToolCaller, dispatch};
use crate::{Backend, ToolError, ToolResult};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub id: u64,
    pub tool: String,
    #[serde(default)]
    pub arguments: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    pub id: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// A socket path for a project's bridge, short enough for `sun_path`
/// (104 bytes on macOS): `$TMPDIR/wh-<project id>-<pid>.sock`.
pub fn default_socket_path(project_id: &str) -> PathBuf {
    std::env::temp_dir().join(format!("wh-{project_id}-{}.sock", std::process::id()))
}

/// A running bridge host. Dropping it stops accepting connections, closes
/// the open ones (a request in flight still finishes, but its reply is
/// lost) and removes the socket file.
pub struct BridgeHost {
    path: PathBuf,
    stop: Arc<AtomicBool>,
    conns: Arc<Mutex<BTreeMap<u64, UnixStream>>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl BridgeHost {
    pub fn socket_path(&self) -> &Path {
        &self.path
    }
}

impl Drop for BridgeHost {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // wake the accept loop
        let _ = UnixStream::connect(&self.path);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        for c in std::mem::take(&mut *self.conns.lock().unwrap_or_else(|e| e.into_inner())).into_values() {
            let _ = c.shutdown(std::net::Shutdown::Both);
        }
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Serve `backend` on a Unix socket at `socket_path` (replacing a stale
/// socket file). Each connection gets its own thread; `backend` is called
/// from those threads.
pub fn serve(socket_path: impl AsRef<Path>, backend: Arc<dyn Backend>) -> std::io::Result<BridgeHost> {
    let path = socket_path.as_ref().to_path_buf();
    if path.exists() {
        std::fs::remove_file(&path)?;
    }
    let listener = UnixListener::bind(&path)?;
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }
    let stop = Arc::new(AtomicBool::new(false));
    let conns: Arc<Mutex<BTreeMap<u64, UnixStream>>> = Arc::default();
    let (s, open) = (stop.clone(), conns.clone());
    let thread = std::thread::Builder::new().name("wh-mcp-bridge".into()).spawn(move || {
        let mut next = 0u64;
        for conn in listener.incoming() {
            if s.load(Ordering::SeqCst) {
                break;
            }
            let Ok(conn) = conn else { continue };
            next += 1;
            let id = next;
            if let Ok(c) = conn.try_clone() {
                open.lock().unwrap_or_else(|e| e.into_inner()).insert(id, c);
            }
            let (backend, open) = (backend.clone(), open.clone());
            let _ = std::thread::Builder::new().name("wh-mcp-bridge-conn".into()).spawn(move || {
                handle(conn, &*backend);
                open.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
            });
        }
    })?;
    Ok(BridgeHost { path, stop, conns, thread: Some(thread) })
}

fn handle(conn: UnixStream, backend: &dyn Backend) {
    let Ok(mut out) = conn.try_clone() else { return };
    for line in BufReader::new(conn).lines() {
        let Ok(line) = line else { return };
        if line.trim().is_empty() {
            continue;
        }
        let resp = match serde_json::from_str::<Request>(&line) {
            Ok(req) => {
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| dispatch(backend, &req.tool, req.arguments)))
                    .unwrap_or_else(|_| Err(ToolError::new("the host panicked while handling the tool call")));
                match r {
                    Ok(v) => Response { id: req.id, result: Some(v), error: None },
                    Err(e) => Response { id: req.id, result: None, error: Some(e.0) },
                }
            }
            Err(e) => Response { id: 0, result: None, error: Some(format!("bad request: {e}")) },
        };
        let mut s = serde_json::to_string(&resp).unwrap_or_default();
        s.push('\n');
        if out.write_all(s.as_bytes()).and_then(|_| out.flush()).is_err() {
            return;
        }
    }
}

struct Conn {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
}

/// The shim's side: forwards tool calls to the host, connecting lazily and
/// reconnecting after the host restarts.
pub struct BridgeClient {
    path: PathBuf,
    conn: Mutex<Option<Conn>>,
    next_id: AtomicU64,
}

impl BridgeClient {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into(), conn: Mutex::new(None), next_id: AtomicU64::new(1) }
    }

    fn connect(&self) -> ToolResult<Conn> {
        let s = UnixStream::connect(&self.path).map_err(|e| {
            ToolError::new(format!("the Wild Horse IDE is not reachable at {}: {e} (is the project open?)", self.path.display()))
        })?;
        Ok(Conn { reader: BufReader::new(s.try_clone()?), writer: s })
    }

    fn send(conn: &mut Conn, line: &str) -> std::io::Result<()> {
        conn.writer.write_all(line.as_bytes())?;
        conn.writer.flush()
    }
}

impl ToolCaller for BridgeClient {
    fn call(&self, tool: &str, arguments: Value) -> ToolResult<Value> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let mut line = serde_json::to_string(&Request { id, tool: tool.into(), arguments }).map_err(|e| ToolError::new(e.to_string()))?;
        line.push('\n');
        let mut slot = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        // a write to a dead connection is safe to retry: the host never saw it
        let sent = matches!(slot.as_mut().map(|c| Self::send(c, &line)), Some(Ok(())));
        if !sent {
            let mut c = self.connect()?;
            Self::send(&mut c, &line).map_err(|e| ToolError::new(format!("sending to the IDE: {e}")))?;
            *slot = Some(c);
        }
        let conn = slot.as_mut().unwrap();
        let mut reply = String::new();
        let n = conn.reader.read_line(&mut reply);
        if !matches!(n, Ok(n) if n > 0) {
            *slot = None;
            return Err(ToolError::new("the IDE closed the bridge before answering"));
        }
        let resp: Response = serde_json::from_str(&reply).map_err(|e| ToolError::new(format!("bad reply from the IDE: {e}")))?;
        if resp.id != id {
            *slot = None;
            return Err(ToolError::new(format!("reply for request {} while waiting for {id}", resp.id)));
        }
        match (resp.result, resp.error) {
            (_, Some(e)) => Err(ToolError(e)),
            (Some(v), None) => Ok(v),
            (None, None) => Ok(Value::Null),
        }
    }
}
