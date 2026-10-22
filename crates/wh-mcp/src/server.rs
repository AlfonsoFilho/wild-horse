//! The MCP side: tool definitions (names, descriptions, input schemas), the
//! name → [`Backend`] dispatch shared with the bridge host, and the `rmcp`
//! stdio server.

use std::sync::Arc;

use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation, JsonObject, ListToolsResult,
    PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool, ToolAnnotations,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData as McpError, RoleServer, ServerHandler, ServiceExt};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::view::ProseEdit;
use crate::{Backend, Op, ToolError, ToolResult};

pub const OUTLINE: &str = "wh_outline";
pub const GET_NODE: &str = "wh_get_node";
pub const APPLY_OPS: &str = "wh_apply_ops";
pub const EDIT_PROSE: &str = "wh_edit_prose";
pub const CHECK: &str = "wh_check";
pub const RUN_SPECS: &str = "wh_run_specs";
pub const SPEC: &str = "wh_spec";

pub const TOOL_NAMES: &[&str] = &[OUTLINE, GET_NODE, APPLY_OPS, EDIT_PROSE, CHECK, RUN_SPECS, SPEC];

/// Something that answers tool calls by name: a local [`Backend`] or the bridge.
pub trait ToolCaller: Send + Sync {
    fn call(&self, tool: &str, args: Value) -> ToolResult<Value>;
}

/// A [`Backend`] answering calls in this process.
pub struct Local(pub Arc<dyn Backend>);

impl ToolCaller for Local {
    fn call(&self, tool: &str, args: Value) -> ToolResult<Value> {
        dispatch(&*self.0, tool, args)
    }
}

#[derive(Deserialize)]
struct IdArgs {
    id: String,
}

#[derive(Deserialize)]
struct OptIdArgs {
    #[serde(default)]
    id: Option<String>,
}

#[derive(Deserialize)]
struct CheckArgs {
    #[serde(default)]
    cargo: bool,
}

fn args<T: serde::de::DeserializeOwned>(tool: &str, v: Value) -> ToolResult<T> {
    let v = if v.is_null() { json!({}) } else { v };
    serde_json::from_value(v).map_err(|e| ToolError::new(format!("bad arguments for {tool}: {e}")))
}

/// `ops` as an array of op objects (also accepted: one op, or the array as a JSON string).
fn parse_ops(v: Value) -> ToolResult<Vec<Op>> {
    let ops = match v {
        Value::Object(mut m) => m.remove("ops").ok_or_else(|| ToolError::new("missing `ops`"))?,
        other => other,
    };
    let ops = match ops {
        Value::String(s) => serde_json::from_str(&s).map_err(|e| ToolError::new(format!("`ops` is not JSON: {e}")))?,
        o => o,
    };
    let ops = match ops {
        Value::Array(a) => a,
        o @ Value::Object(_) => vec![o],
        _ => return Err(ToolError::new("`ops` must be an array of op objects")),
    };
    ops.into_iter()
        .enumerate()
        .map(|(i, o)| serde_json::from_value::<Op>(o).map_err(|e| ToolError::new(format!("op {i}: {e} (see the wh_apply_ops schema)"))))
        .collect()
}

fn to_value<T: serde::Serialize>(v: T) -> ToolResult<Value> {
    serde_json::to_value(v).map_err(|e| ToolError::new(e.to_string()))
}

/// Run one tool against a backend. Results are JSON (`wh_spec`: a string).
pub fn dispatch(b: &dyn Backend, tool: &str, a: Value) -> ToolResult<Value> {
    match tool {
        OUTLINE => to_value(b.outline()?),
        GET_NODE => to_value(b.get_node(&args::<IdArgs>(tool, a)?.id)?),
        APPLY_OPS => to_value(b.apply_ops(&parse_ops(a)?)?),
        EDIT_PROSE => to_value(b.edit_prose(&args::<ProseEdit>(tool, a)?)?),
        CHECK => to_value(b.check(args::<CheckArgs>(tool, a)?.cargo)?),
        RUN_SPECS => to_value(b.run_specs(args::<OptIdArgs>(tool, a)?.id.as_deref())?),
        SPEC => Ok(Value::String(b.spec()?)),
        other => Err(ToolError::new(format!("unknown tool `{other}`"))),
    }
}

/// Field types used in the op schema.
#[derive(Clone, Copy)]
enum F {
    Str,
    OptStr,
    Int,
    OptInt,
    Strs,
    Pairs,
    Card,
}

/// One `wh_edit::Op` variant: tag, description, fields.
type OpSchema = (&'static str, &'static str, &'static [(&'static str, F)]);

/// Every `wh_edit::Op` variant. Kept in sync with `wh_edit::Op` by the
/// `op_schema_covers_every_op` test.
const OPS: &[OpSchema] = &[
    ("add_def", "New `def` in its own block (with a TODO prose line) at the end of `section` (index from wh_outline) or of the file. `signature` is e.g. `normalize(e: Email) -> Email`; `clauses` are lines like `requires …`, `ensures …`, `example f(1) == 2`.",
        &[("file", F::Str), ("section", F::OptInt), ("signature", F::Str), ("clauses", F::Strs)]),
    ("add_struct", "New `struct` in its own block. `fields` are [name, type] pairs.",
        &[("file", F::Str), ("section", F::OptInt), ("name", F::Str), ("fields", F::Pairs)]),
    ("add_item", "Any new definition(s) given as `wh` source text, in a new block preceded by `prose` (Markdown).",
        &[("file", F::Str), ("section", F::OptInt), ("source", F::Str), ("prose", F::OptStr)]),
    ("add_field", "Add a field to struct `owner` (node id).", &[("owner", F::Str), ("name", F::Str), ("ty", F::Str)]),
    ("retype_field", "Change a struct field's type.", &[("owner", F::Str), ("field", F::Str), ("ty", F::Str)]),
    ("rename_field", "Rename a struct field (this definition only).", &[("owner", F::Str), ("field", F::Str), ("new_name", F::Str)]),
    ("remove_field", "Remove a struct field.", &[("owner", F::Str), ("field", F::Str)]),
    ("set_cardinality", "Make a field `one` (T), `optional` (T | None), `many` (list[T]) or `set` (set[T]).",
        &[("owner", F::Str), ("field", F::Str), ("cardinality", F::Card)]),
    ("add_variant", "Add a variant line (e.g. `Refunded` or `Failed(reason: str)`) to enum `owner`.", &[("owner", F::Str), ("variant", F::Str)]),
    ("add_clause", "Append a clause line (`requires`, `ensures`, `raises E when …`, `example …`, `property …`, `pattern …`, `note …`) to def `node`.",
        &[("node", F::Str), ("clause", F::Str)]),
    ("replace_clause", "Replace clause `index` (see wh_get_node `clauses`) of def `node`.", &[("node", F::Str), ("index", F::Int), ("clause", F::Str)]),
    ("remove_clause", "Remove clause `index` of def `node`.", &[("node", F::Str), ("index", F::Int)]),
    ("edit_signature", "Replace the signature of a def or flow (everything after `def`/`flow`, e.g. `f(x: int) -> str raises E`).",
        &[("node", F::Str), ("signature", F::Str)]),
    ("set_pattern", "Set (or with null, remove) the `pattern` clause of def `node`.", &[("node", F::Str), ("pattern", F::OptStr)]),
    ("insert_flow_step", "Insert `|> callee` as the `position`-th step of pipeline statement `stmt` in flow `flow`.",
        &[("flow", F::Str), ("stmt", F::Int), ("position", F::Int), ("callee", F::Str)]),
    ("remove_flow_step", "Remove step `position` of pipeline statement `stmt` in flow `flow`.", &[("flow", F::Str), ("stmt", F::Int), ("position", F::Int)]),
    ("move_flow_step", "Move the `from`-th `|>` step of pipeline statement `stmt` in flow `flow` to position `to`.",
        &[("flow", F::Str), ("stmt", F::Int), ("from", F::Int), ("to", F::Int)]),
    ("insert_flow_binding", "Insert the binding `name = value` as statement `index` of flow `flow` (DAG form: a new step fed by earlier bindings or parameters).",
        &[("flow", F::Str), ("index", F::Int), ("name", F::Str), ("value", F::Str)]),
    ("rename", "Rename a definition and every reference to it in code and `[[refs]]`.", &[("node", F::Str), ("new_name", F::Str)]),
    ("remove_node", "Remove a definition (its whole block if it is alone in it).", &[("node", F::Str)]),
];

fn field_schema(f: F) -> Value {
    match f {
        F::Str => json!({ "type": "string" }),
        F::OptStr => json!({ "type": ["string", "null"] }),
        F::Int => json!({ "type": "integer", "minimum": 0 }),
        F::OptInt => json!({ "type": ["integer", "null"], "minimum": 0 }),
        F::Strs => json!({ "type": "array", "items": { "type": "string" } }),
        F::Pairs => json!({ "type": "array", "items": { "type": "array", "items": { "type": "string" }, "minItems": 2, "maxItems": 2 } }),
        F::Card => json!({ "type": "string", "enum": ["One", "Optional", "Many", "Set"] }),
    }
}

/// JSON schema of one `wh_edit::Op`.
pub fn op_schema() -> Value {
    let variants: Vec<Value> = OPS
        .iter()
        .map(|(tag, desc, fields)| {
            let mut props = serde_json::Map::new();
            props.insert("op".into(), json!({ "const": tag }));
            let mut required = vec![json!("op")];
            for (name, f) in fields.iter() {
                props.insert((*name).into(), field_schema(*f));
                if !matches!(f, F::OptStr | F::OptInt) {
                    required.push(json!(name));
                }
            }
            json!({ "type": "object", "description": desc, "properties": props, "required": required, "additionalProperties": false })
        })
        .collect();
    json!({ "oneOf": variants })
}

fn schema(v: Value) -> Arc<JsonObject> {
    match v {
        Value::Object(m) => Arc::new(m),
        _ => unreachable!(),
    }
}

fn tool(name: &'static str, desc: String, input: Value, read_only: bool) -> Tool {
    let mut t = Tool::new(name, desc, schema(input));
    t.annotations = Some(ToolAnnotations::new().read_only(read_only).destructive(false).open_world(false));
    t
}

/// The tool list (the same in file and bridge mode).
pub fn tools() -> Vec<Tool> {
    let none = json!({ "type": "object", "properties": {} });
    let op_tags: Vec<&str> = OPS.iter().map(|o| o.0).collect();
    vec![
        tool(OUTLINE, "Outline of the Wild Horse program: files (path, module), their sections (`index` is what ops take as `section`), \
            and the definitions in each (id `module.name`, kind, header line, hole state, diagnostics counts). Start here.".into(), none.clone(), true),
        tool(GET_NODE, "One definition by id (`module.name`): source, prose, resolved signature and types, clauses (with the indices clause ops take), \
            fields/variants/steps, diagnostics, what it uses and what uses it.".into(),
            json!({ "type": "object", "properties": { "id": { "type": "string", "description": "node id, e.g. `signup.normalize_email`" } }, "required": ["id"] }), true),
        tool(APPLY_OPS, format!(
            "Change the program with structured edit operations, applied atomically as one undoable step: every op is planned against the \
             result of the previous ones, and if any op fails (unknown node, invalid change, or an edit that would break parsing) nothing \
             is applied. Prose, comments and formatting outside the edited ranges are kept. Returns the edited line ranges, the \
             diagnostics after the edit, and which of them are new. Ops (field `op`): {}. Use `add_item` for anything without a \
             dedicated op (new enums, capabilities, flows, …); use wh_edit_prose for prose.", op_tags.join(", ")),
            json!({ "type": "object", "properties": { "ops": { "type": "array", "items": op_schema(), "minItems": 1 } }, "required": ["ops"] }), false),
        tool(EDIT_PROSE, "Replace or add literate prose (Markdown paragraphs) without touching code blocks. `target`: a node id (the prose right \
            before its block), `<file or module>#<section index or title>` (the section's introduction), or `<file or module>` (the \
            file's introduction). `mode`: `replace` (default), `append` (after the existing prose; for a section or file: at its end), \
            `prepend`. With `append` on a section or file, `heading` starts a new subsection. The text may use `[[module.name]]` \
            references but no headings or fenced code.".into(),
            json!({ "type": "object", "properties": {
                "target": { "type": "string" },
                "text": { "type": "string" },
                "mode": { "type": "string", "enum": ["replace", "append", "prepend"] },
                "heading": { "type": "string" },
            }, "required": ["target", "text"] }), false),
        tool(CHECK, "Diagnostics of the whole program from the checker (instant). With `cargo: true`, also generate the Rust crate and \
            `cargo check` it (slow), with errors mapped to nodes.".into(),
            json!({ "type": "object", "properties": { "cargo": { "type": "boolean", "default": false } } }), true),
        tool(RUN_SPECS, "Generate the crate and run the specs (examples, properties, contracts) of one definition, or of the whole program \
            without `id`. Slow (runs cargo test); needs implemented holes.".into(),
            json!({ "type": "object", "properties": { "id": { "type": "string" } } }), true),
        tool(SPEC, "The Wild Horse language specification (Markdown). Read it before writing new definitions.".into(), none, true),
    ]
}

pub const INSTRUCTIONS: &str = "Semantic access to a Wild Horse literate program (`*.wh.md`). Read with wh_outline and wh_get_node; \
change definitions only with wh_apply_ops and prose only with wh_edit_prose (never by writing files); verify with wh_check. \
wh_spec returns the language specification.";

/// The MCP server: forwards tool calls to a [`ToolCaller`] on a blocking thread.
#[derive(Clone)]
pub struct WhServer {
    tools: Arc<dyn ToolCaller>,
}

impl WhServer {
    pub fn new(tools: Arc<dyn ToolCaller>) -> Self {
        Self { tools }
    }
}

impl ServerHandler for WhServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("wh", env!("CARGO_PKG_VERSION")))
            .with_instructions(INSTRUCTIONS)
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(ListToolsResult::with_all_items(tools()))
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        tools().into_iter().find(|t| t.name == name)
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        let name = request.name.to_string();
        if !TOOL_NAMES.contains(&name.as_str()) {
            return Ok(CallToolResult::error(vec![ContentBlock::text(format!("unknown tool `{name}`"))]).into());
        }
        let args = request.arguments.map(Value::Object).unwrap_or(Value::Null);
        let tools = self.tools.clone();
        let result = tokio::task::spawn_blocking(move || tools.call(&name, args))
            .await
            .unwrap_or_else(|e| Err(ToolError::new(format!("the tool panicked: {e}"))));
        Ok(match result {
            Ok(Value::String(s)) => CallToolResult::success(vec![ContentBlock::text(s)]),
            Ok(v) => CallToolResult::success(vec![ContentBlock::text(serde_json::to_string_pretty(&v).unwrap_or_default())]),
            Err(e) => CallToolResult::error(vec![ContentBlock::text(e.0)]),
        }
        .into())
    }
}

/// Serve MCP over this process's stdin/stdout until the client disconnects.
pub async fn serve_stdio(tools: Arc<dyn ToolCaller>) -> anyhow::Result<()> {
    let service = WhServer::new(tools).serve(rmcp::transport::stdio()).await?;
    service.waiting().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Exhaustive on purpose: a new `Op` variant fails to compile here until
    /// it is added to [`OPS`].
    fn tag(op: &Op) -> &'static str {
        match op {
            Op::AddDef { .. } => "add_def",
            Op::AddStruct { .. } => "add_struct",
            Op::AddItem { .. } => "add_item",
            Op::AddField { .. } => "add_field",
            Op::RetypeField { .. } => "retype_field",
            Op::RenameField { .. } => "rename_field",
            Op::RemoveField { .. } => "remove_field",
            Op::SetCardinality { .. } => "set_cardinality",
            Op::AddVariant { .. } => "add_variant",
            Op::AddClause { .. } => "add_clause",
            Op::ReplaceClause { .. } => "replace_clause",
            Op::RemoveClause { .. } => "remove_clause",
            Op::EditSignature { .. } => "edit_signature",
            Op::SetPattern { .. } => "set_pattern",
            Op::InsertFlowStep { .. } => "insert_flow_step",
            Op::RemoveFlowStep { .. } => "remove_flow_step",
            Op::MoveFlowStep { .. } => "move_flow_step",
            Op::InsertFlowBinding { .. } => "insert_flow_binding",
            Op::Rename { .. } => "rename",
            Op::RemoveNode { .. } => "remove_node",
        }
    }

    fn sample(f: F) -> Value {
        match f {
            F::Str | F::OptStr => json!("x"),
            F::Int | F::OptInt => json!(0),
            F::Strs => json!(["requires true"]),
            F::Pairs => json!([["a", "int"]]),
            F::Card => json!("Many"),
        }
    }

    #[test]
    fn op_schema_covers_every_op() {
        for (name, _, fields) in OPS {
            let mut obj = serde_json::Map::new();
            obj.insert("op".into(), json!(name));
            for (f, ty) in fields.iter() {
                obj.insert((*f).into(), sample(*ty));
            }
            let op: Op = serde_json::from_value(Value::Object(obj.clone())).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(tag(&op), *name);
            // the schema names exactly the fields serde writes
            let back = serde_json::to_value(&op).unwrap();
            let keys: std::collections::BTreeSet<&String> = back.as_object().unwrap().keys().collect();
            assert_eq!(keys, obj.keys().collect(), "{name}");
        }
        assert_eq!(OPS.len(), 20, "every Op variant has a schema entry");
    }

    #[test]
    fn ops_are_accepted_in_lenient_shapes() {
        let one = json!({ "op": "remove_node", "node": "m.f" });
        assert_eq!(parse_ops(json!({ "ops": [one.clone()] })).unwrap().len(), 1);
        assert_eq!(parse_ops(json!({ "ops": one.clone() })).unwrap().len(), 1);
        assert_eq!(parse_ops(json!({ "ops": serde_json::to_string(&json!([one])).unwrap() })).unwrap().len(), 1);
        let err = parse_ops(json!({ "ops": [{ "op": "explode" }] })).unwrap_err();
        assert!(err.0.starts_with("op 0:"), "{err}");
    }

    #[test]
    fn every_tool_has_an_object_schema() {
        let ts = tools();
        assert_eq!(ts.iter().map(|t| t.name.as_ref()).collect::<Vec<_>>(), TOOL_NAMES);
        for t in ts {
            assert_eq!(t.input_schema.get("type"), Some(&json!("object")), "{}", t.name);
        }
    }
}
