#![allow(dead_code)]

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde_json::{Value, json};

/// A fresh copy of `examples/signup` under `target/wh-mcp-tests/<name>`.
pub fn fresh_copy(name: &str) -> PathBuf {
    let ws = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let dir = ws.join("target/wh-mcp-tests").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let example = ws.join("examples/signup");
    std::fs::copy(example.join("wildhorse.toml"), dir.join("wildhorse.toml")).unwrap();
    copy_tree(&example.join("src"), &dir.join("src"));
    copy_tree(&example.join("impl"), &dir.join("impl"));
    dir.canonicalize().unwrap()
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            copy_tree(&p, &to.join(e.file_name()));
        } else {
            std::fs::copy(&p, to.join(e.file_name())).unwrap();
        }
    }
}

/// A socket path short enough for `sun_path`.
pub fn socket_path(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("wh-mcp-test-{name}-{}.sock", std::process::id()))
}

/// A minimal MCP client talking JSON-RPC lines to a spawned `wh-mcp`.
pub struct McpClient {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl McpClient {
    pub fn spawn(args: &[&str]) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_wh-mcp"))
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawning wh-mcp");
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Self { child, stdin, stdout, next_id: 1 }
    }

    pub fn notify(&mut self, method: &str, params: Value) {
        let msg = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        writeln!(self.stdin, "{msg}").unwrap();
        self.stdin.flush().unwrap();
    }

    /// Send a request and return its `result` (panics on a JSON-RPC error).
    pub fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        let msg = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        writeln!(self.stdin, "{msg}").unwrap();
        self.stdin.flush().unwrap();
        loop {
            let mut line = String::new();
            let n = self.stdout.read_line(&mut line).unwrap();
            assert!(n > 0, "wh-mcp closed stdout while waiting for {method}");
            let v: Value = serde_json::from_str(&line).unwrap_or_else(|e| panic!("non-JSON on stdout ({e}): {line}"));
            if v["id"] == json!(id) {
                if let Some(err) = v.get("error") {
                    panic!("{method} failed: {err}");
                }
                return v["result"].clone();
            }
        }
    }

    pub fn initialize(&mut self) -> Value {
        let r = self.request(
            "initialize",
            json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "wh-mcp-test", "version": "0.0.0" }
            }),
        );
        self.notify("notifications/initialized", json!({}));
        r
    }

    /// `tools/call`; returns `(is_error, text of the first content block)`.
    pub fn call(&mut self, tool: &str, arguments: Value) -> (bool, String) {
        let r = self.request("tools/call", json!({ "name": tool, "arguments": arguments }));
        let text = r["content"][0]["text"].as_str().unwrap_or_default().to_string();
        (r["isError"].as_bool().unwrap_or(false), text)
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
