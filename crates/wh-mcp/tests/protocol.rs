//! MCP protocol level: spawn `wh-mcp --project <copy>` and talk JSON-RPC to it.

mod common;

use serde_json::{Value, json};

#[test]
fn initialize_list_outline_apply() {
    let root = common::fresh_copy("protocol");
    let mut mcp = common::McpClient::spawn(&["--project", root.to_str().unwrap()]);

    let init = mcp.initialize();
    assert_eq!(init["protocolVersion"], "2025-06-18");
    assert_eq!(init["serverInfo"]["name"], "wh");
    assert!(init["capabilities"]["tools"].is_object());
    assert!(init["instructions"].as_str().unwrap().contains("wh_apply_ops"));

    let list = mcp.request("tools/list", json!({}));
    let tools = list["tools"].as_array().unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["wh_outline", "wh_get_node", "wh_apply_ops", "wh_edit_prose", "wh_check", "wh_run_specs", "wh_spec"]);
    let apply = &tools[2];
    let variants = apply["inputSchema"]["properties"]["ops"]["items"]["oneOf"].as_array().unwrap();
    assert!(variants.iter().any(|v| v["properties"]["op"]["const"] == "add_def"));
    assert_eq!(apply["annotations"]["readOnlyHint"], false);
    assert_eq!(tools[0]["annotations"]["readOnlyHint"], true);

    let (is_error, text) = mcp.call("wh_outline", json!({}));
    assert!(!is_error, "{text}");
    let outline: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(outline["files"][1]["path"], "src/signup.wh.md");
    assert_eq!(outline["diagnostics"]["errors"], 0);

    let (is_error, text) = mcp.call(
        "wh_apply_ops",
        json!({ "ops": [{ "op": "add_clause", "node": "signup.normalize_email", "clause": "example normalize_email(\"X@Y.io\") == \"x@y.io\"" }] }),
    );
    assert!(!is_error, "{text}");
    let r: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(r["counts"]["errors"], 0, "{text}");
    assert_eq!(r["edited"][0]["path"], "src/signup.wh.md");
    let on_disk = std::fs::read_to_string(root.join("src/signup.wh.md")).unwrap();
    assert!(on_disk.contains("    pattern Transform\n    example normalize_email(\"X@Y.io\") == \"x@y.io\"\n"), "{on_disk}");

    // tool failures are tool results with isError, not protocol errors
    let (is_error, text) = mcp.call("wh_apply_ops", json!({ "ops": [{ "op": "remove_node", "node": "signup.ghost" }] }));
    assert!(is_error);
    assert!(text.contains("unknown node `signup.ghost`"), "{text}");
    let (is_error, text) = mcp.call("wh_spec", json!({}));
    assert!(!is_error && text.contains("Wild Horse"));
}

#[test]
fn bad_arguments_exit_with_usage() {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_wh-mcp")).arg("--bogus").output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("usage: wh-mcp"));
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_wh-mcp")).args(["--project", "/nonexistent/wh"]).output().unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("no project at"));
}
