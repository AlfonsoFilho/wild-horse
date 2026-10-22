//! Bridge mode: a mock host behind `bridge::serve`, reached directly through
//! `BridgeClient` and end to end through `wh-mcp --bridge`.

mod common;

use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use wh_mcp::bridge::{self, BridgeClient};
use wh_mcp::server::ToolCaller;
use wh_mcp::view::{ApplyResult, CheckResult, DiagCounts, NodeView, Outline, SpecRun};
use wh_mcp::{Backend, Op, ProseEdit, ToolError, ToolResult};

/// Records every call and answers with canned results.
#[derive(Default)]
struct MockHost {
    calls: Mutex<Vec<String>>,
}

impl MockHost {
    fn log(&self, s: String) {
        self.calls.lock().unwrap().push(s);
    }
}

fn applied(labels: Vec<String>) -> ApplyResult {
    ApplyResult { applied: labels, edited: vec![], counts: DiagCounts::default(), introduced: vec![], diagnostics: vec![] }
}

impl Backend for MockHost {
    fn outline(&self) -> ToolResult<Outline> {
        self.log("outline".into());
        Ok(Outline { files: vec![], diagnostics: DiagCounts { errors: 1, warnings: 2, lints: 3 } })
    }

    fn get_node(&self, id: &str) -> ToolResult<NodeView> {
        self.log(format!("get_node {id}"));
        Err(ToolError::new(format!("unknown node `{id}`")))
    }

    fn apply_ops(&self, ops: &[Op]) -> ToolResult<ApplyResult> {
        self.log(format!("apply_ops {}", ops.len()));
        Ok(applied(ops.iter().map(Op::label).collect()))
    }

    fn edit_prose(&self, edit: &ProseEdit) -> ToolResult<ApplyResult> {
        self.log(format!("edit_prose {} {:?}", edit.target, edit.mode));
        Ok(applied(vec![edit.text.clone()]))
    }

    fn check(&self, cargo: bool) -> ToolResult<CheckResult> {
        self.log(format!("check {cargo}"));
        Ok(CheckResult { counts: DiagCounts::default(), diagnostics: vec![], cargo: None })
    }

    fn run_specs(&self, id: Option<&str>) -> ToolResult<SpecRun> {
        self.log(format!("run_specs {id:?}"));
        Ok(SpecRun { success: true, passed: 1, failed: 0, tests: vec![], build_errors: vec![] })
    }

    fn spec(&self) -> ToolResult<String> {
        Ok("# spec".into())
    }
}

#[test]
fn client_round_trip_through_a_mock_host() {
    let sock = common::socket_path("client");
    let host = Arc::new(MockHost::default());
    let server = bridge::serve(&sock, host.clone()).unwrap();
    let client = BridgeClient::new(&sock);

    let o = client.call("wh_outline", json!({})).unwrap();
    assert_eq!(o["diagnostics"], json!({ "errors": 1, "warnings": 2, "lints": 3 }));
    let err = client.call("wh_get_node", json!({ "id": "m.x" })).unwrap_err();
    assert_eq!(err.0, "unknown node `m.x`");
    let r = client.call("wh_apply_ops", json!({ "ops": [{ "op": "remove_node", "node": "m.f" }, { "op": "rename", "node": "m.g", "new_name": "h" }] })).unwrap();
    assert_eq!(r["applied"], json!(["remove m.f", "rename to h"]));
    client.call("wh_edit_prose", json!({ "target": "m#0", "text": "hi", "mode": "append" })).unwrap();
    client.call("wh_check", json!({ "cargo": true })).unwrap();
    client.call("wh_run_specs", json!({})).unwrap();
    assert_eq!(client.call("wh_spec", Value::Null).unwrap(), json!("# spec"));
    // bad arguments are reported by the host, without calling the backend
    assert!(client.call("wh_get_node", json!({})).unwrap_err().0.contains("bad arguments"));
    assert!(client.call("wh_nope", json!({})).unwrap_err().0.contains("unknown tool"));
    assert_eq!(
        *host.calls.lock().unwrap(),
        ["outline", "get_node m.x", "apply_ops 2", "edit_prose m#0 Append", "check true", "run_specs None"]
    );

    // the host goes away and comes back: the client reconnects
    drop(server);
    assert!(!sock.exists(), "the socket file is removed");
    assert!(client.call("wh_outline", json!({})).is_err());
    let _server = bridge::serve(&sock, host.clone()).unwrap();
    assert!(client.call("wh_outline", json!({})).is_ok());
}

#[test]
fn unreachable_host_is_a_tool_error() {
    let client = BridgeClient::new(common::socket_path("nobody"));
    let err = client.call("wh_outline", json!({})).unwrap_err();
    assert!(err.0.contains("not reachable"), "{err}");
}

/// The agent's view: MCP over stdio to the shim, the shim over the socket to the host.
#[test]
fn shim_forwards_mcp_calls_to_the_host() {
    let sock = common::socket_path("shim");
    let host = Arc::new(MockHost::default());
    let _server = bridge::serve(&sock, host.clone()).unwrap();
    let mut mcp = common::McpClient::spawn(&["--bridge", sock.to_str().unwrap()]);
    mcp.initialize();
    let tools = mcp.request("tools/list", json!({}));
    assert_eq!(tools["tools"].as_array().unwrap().len(), 7);

    let (is_error, text) = mcp.call("wh_outline", json!({}));
    assert!(!is_error);
    let v: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["diagnostics"]["warnings"], 2);
    let (is_error, text) = mcp.call("wh_apply_ops", json!({ "ops": [{ "op": "add_variant", "owner": "m.E", "variant": "X" }] }));
    assert!(!is_error, "{text}");
    assert!(text.contains("add variant X"));
    let (is_error, text) = mcp.call("wh_get_node", json!({ "id": "m.q" }));
    assert!(is_error);
    assert_eq!(text, "unknown node `m.q`");
    assert_eq!(*host.calls.lock().unwrap(), ["outline", "apply_ops 1", "get_node m.q"]);
}

/// A real backend behind the bridge (what the IDE does with its own store).
#[test]
fn file_backend_behind_the_bridge() {
    let root = common::fresh_copy("bridge_file");
    let sock = common::socket_path("file");
    let _server = bridge::serve(&sock, Arc::new(wh_mcp::FileBackend::open(&root).unwrap())).unwrap();
    let mut mcp = common::McpClient::spawn(&["--bridge", sock.to_str().unwrap()]);
    mcp.initialize();
    let (is_error, text) = mcp.call("wh_apply_ops", json!({ "ops": [{ "op": "add_variant", "owner": "signup.DbError", "variant": "Timeout" }] }));
    assert!(!is_error, "{text}");
    assert!(std::fs::read_to_string(root.join("src/signup.wh.md")).unwrap().contains("    Unavailable\n    Timeout\n"));
}
