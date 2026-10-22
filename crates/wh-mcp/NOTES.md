# wh-mcp

The `wh` MCP server (design doc, Core idea 2, Role A). The author agent reads
and changes a Wild Horse program through semantic tools. Every change is a
structured `wh_edit::Op` or a prose edit that never touches code. The agent
does not rewrite `.wh.md` files with its own Write/Edit tools.

Built on `rmcp` 3.7 (the official Rust MCP SDK). Features used: `server` and
`transport-io` only, no macros. The server implements `ServerHandler` by hand
(`list_tools`/`call_tool`), so the input schemas are written out explicitly.
Initialize negotiates any MCP version from 2024-11-05 to 2026-07-28; Claude
Code uses 2025-06-18.

## Layout

| file | what |
|---|---|
| `src/lib.rs` | `Backend` trait, `ToolError`, `SPEC`, `Target`, `run_stdio`, and `cli_main` (shared by the binary and `wh mcp`) |
| `src/view.rs` | Model-level pieces every backend reuses: `outline`, `node_view`, `diagnostics`, `check_result`, `plan_ops`, `plan_prose`, `apply_to_store`, `apply_result`, `store_model`, plus all result types |
| `src/file.rs` | `FileBackend` (file mode) |
| `src/server.rs` | Tool names, descriptions and JSON schemas (including one schema per `Op` variant), `dispatch(&dyn Backend, tool, args)`, and the `rmcp` handler `WhServer` |
| `src/bridge.rs` | Unix-socket protocol, host-side `serve`, and the shim's `BridgeClient` |
| `src/author.rs` | Attaching the server to an author session: `McpServerConfig`, `SessionOptions`, `DISALLOWED_TOOLS`, `PROMPT_RULES` |
| `src/main.rs` | The `wh-mcp` binary: `wh-mcp [--project <dir>]` or `wh-mcp --bridge <socket>` |

## Tools

All results are JSON in a text content block, except `wh_spec`, which is
Markdown. A failure is a normal tool result with `isError: true` and a
readable message such as `op 1 (rename to x): unknown node \`m.y\``, never a
JSON-RPC error. Node ids are `module.name`. Paths are relative to the
project root.

- **`wh_outline()`** returns `{files: [{path, module, nodes, sections: [{index, level, title, parent, nodes}], diagnostics}], diagnostics}`.
  - Each node is `{id, kind, signature (header line), line, hole?, errors?, warnings?}`.
  - A section's `index` is what `add_def`/`add_struct`/`add_item` take as `section`.
  - `hole` is set for defs without `extern`. In file mode its value is
    `missing` (no file, or still the `todo!()` stub), `synthesized`
    (`// wh:key=` header), `pinned` or `manual`. File mode does not compute
    staleness; that needs `wh_synth` keys.
- **`wh_get_node(id)`** returns:
  - `{id, kind, module, path, line, end_line, section, prose, source, signature}`;
  - `resolved`: params with resolved types and modes, `returns`, `raises`,
    `uses` capabilities, `is_async`;
  - `details`: fields, variants, methods, flow steps, provides or machine
    states, depending on the kind;
  - `clauses: [{index, kind, text}]`, where `index` is what
    `replace_clause`/`remove_clause` take;
  - `patterns`, `derives`, `hole`, and the node's `diagnostics`;
  - `uses` and `used_by`: identifier resolution over the node sources;
  - `mentioned_in`: `[[ref]]`s in prose, as `path:line`.

  An unknown id suggests matches by short name.
- **`wh_apply_ops(ops)`** takes `ops`, an array of `wh_edit::Op` JSON objects
  tagged by `op` (`add_def`, `add_clause`, `rename`, …). The schema has a
  `oneOf` entry for every variant. A single op object, or the array passed as
  a JSON string, is also accepted.
  - Each op is planned with `wh_edit::plan` against the model left by the
    previous ops, so op 2 can edit what op 1 created.
  - `plan` already refuses an edit that adds parse errors.
  - The combined result is diffed into minimal edits and applied as one
    `Origin::Agent` transaction, so it is one undo step. If any op fails,
    nothing is applied.
  - Returns `{applied: [labels], edited: [{path, start_line, end_line, text}], counts, introduced, diagnostics}`.
    `introduced` lists the errors and warnings that were not there before.
    Type errors do not block the edit; they are reported.
- **`wh_edit_prose({target, text, mode?, heading?})`** edits prose. The
  target can be:
  - a node id: the prose paragraph right before its block;
  - `<file or module>#<section index or title>`: the section's introduction,
    the prose right after the heading;
  - `<file or module>`: the file's introduction, after the front matter.

  `mode` is `replace` (the default; inserts if there is no prose yet),
  `append` (after the prose; for a section or file, at its end) or `prepend`.
  `heading` together with `append` starts a subsection one level deeper.

  Refused:
  - text containing fences or headings;
  - any change to a `wh` block's code (the plan is re-parsed and every
    block's content compared);
  - new parse errors.
- **`wh_check({cargo?})`** returns the model diagnostics
  `{counts, diagnostics: [{path, line, column, severity, message, node?}]}`.
  With `cargo: true` it also runs `wh_project::live::check`. The result then
  includes `cargo: {success, diagnostics: [{node?, level, message, rendered}], error?}`.
- **`wh_run_specs({id?})`** runs `wh_project::live::test(project, model, id)`
  and returns `{success, passed, failed, tests: [{name, node, passed, output}], build_errors}`.
  An unknown id is refused before cargo runs.
- **`wh_spec()`** returns `docs/language-spec.md`, embedded with `include_str!`.

## Backends

```rust
pub trait Backend: Send + Sync {
    fn outline(&self) -> ToolResult<Outline>;
    fn get_node(&self, id: &str) -> ToolResult<NodeView>;
    fn apply_ops(&self, ops: &[Op]) -> ToolResult<ApplyResult>;   // atomic
    fn edit_prose(&self, edit: &ProseEdit) -> ToolResult<ApplyResult>;
    fn check(&self, cargo: bool) -> ToolResult<CheckResult>;
    fn run_specs(&self, id: Option<&str>) -> ToolResult<SpecRun>;
    fn spec(&self) -> ToolResult<String> { Ok(SPEC.into()) }
}
```

**File mode** (`FileBackend::open(root)`):
- loads `wh_project::Project` and keeps the sources in a `DocStore`;
- before every call, merges changes on disk with `reload_from_disk` and
  picks up added or removed files;
- after a successful apply, writes the changed files and calls `mark_saved`.

**Bridge mode** (`wh-mcp --bridge <socket>`):
- The stdio server forwards every `tools/call` unchanged to the host. The
  tool list is local.
- The protocol is newline-delimited JSON over a Unix stream socket, one
  request at a time per connection:

  ```text
  → {"id":1,"tool":"wh_apply_ops","arguments":{"ops":[…]}}
  ← {"id":1,"result":{…}}      or      {"id":1,"error":"message"}
  ```
- Host side: `wh_mcp::bridge::serve(path, Arc<dyn Backend>) -> BridgeHost`.
  - It binds the socket (mode 0600) and serves each connection on its own
    thread, calling `dispatch`.
  - Dropping the `BridgeHost` closes the open connections and removes the
    socket file.
  - No tokio is needed on the host side.
- `bridge::default_socket_path(project_id)` returns
  `$TMPDIR/wh-<id>-<pid>.sock`. This stays under macOS's 104-byte `sun_path`
  limit, which a path inside the project may exceed.
- The shim connects lazily and reconnects if the host restarts. If the host
  is unreachable, the agent gets a readable tool error.

## Wiring (to be done by the session that owns each crate)

### wh-author

Add the dependency `wh-mcp = { path = "../wh-mcp" }`. This is safe because
`wh-mcp` depends on `wh-agent`, not on `wh-author`. Then, in `Author::ask`
(or in a variant that takes the server config):

```rust
let server: wh_agent::McpServerConfig = /* from the caller: file mode or bridge mode, see below */;
let options = wh_mcp::author::session_options(server);
// = SessionOptions { mcp_servers: vec![server],
//     meta: claude_code_meta({ disallowedTools: wh_mcp::author::DISALLOWED_TOOLS,
//                              allowedTools: ["mcp__wh__wh_outline", …],   // auto-approve the wh tools
//                              settingSources: [] }) }
let session = agent.new_session_with(&self.root, options, policy).await?;
```

- `DISALLOWED_TOOLS` is: Write, Edit, MultiEdit, NotebookEdit, Bash,
  BashOutput, KillShell, Task, Agent, WebFetch, WebSearch. It replaces
  `wh_author::DISALLOWED_TOOLS`. `Task` and `Agent` are included because a
  sub-agent would get its own Write/Edit.
- `AuthorPolicy::decide_permission` should return `AllowOnce` when
  `wh_mcp::author::is_wh_tool_request(req)`. Normally the agent never asks,
  because of `allowedTools`. Keep rejecting everything else. The
  Write/Edit-mirroring path can stay as a fallback for agents that ignore
  `disallowedTools`.
- In `author_prompt`, replace "use your file edit tools" with
  `wh_mcp::author::PROMPT_RULES`. The spec is available through `wh_spec`,
  so `.wh/language-spec.md` no longer has to be written. Mentioning it does
  no harm.
- Use `wh_mcp::SPEC` instead of `wh_author::SPEC`, or keep both; they are the
  same file.

### wh-cli (`wh mcp`, and `wh ask` in file mode)

Add the dependency `wh-mcp = { path = "../wh-mcp" }`.

```rust
/// Serve the `wh` MCP server on stdio (spawned by agents).
Mcp {
    /// Project directory (file mode).
    #[arg(long, default_value = ".")] project: PathBuf,
    /// Forward to a running IDE over this socket instead (bridge mode).
    #[arg(long)] bridge: Option<PathBuf>,
},
…
Command::Mcp { project, bridge } => {
    let target = match bridge { Some(s) => wh_mcp::Target::Bridge(s), None => wh_mcp::Target::Project(project) };
    wh_mcp::run_stdio(target)?;   // builds its own tokio runtime; logs only to stderr
    Ok(true)
}
```

Alternatively, pass the raw arguments through:
`std::process::exit(wh_mcp::cli_main(args_after_mcp))`. It accepts
`--project <dir>`, `--bridge <socket>`, or a bare directory.

`wh ask` in file mode:
`wh_mcp::author::file_mode_server(std::env::current_exe()?, &["mcp"], &p.root)`
gives the server config `wh mcp --project <root>`. Pass it to the author.
Edits then land on disk through `FileBackend`, so they no longer arrive as
`AuthorEvent::Edit`. Re-read the model after the turn, as `wh ask` already
does.

### wh-ide

1. Implement `wh_mcp::Backend` for a handle to the IDE's `Project` state
   (`crates/wh-ide/src/project.rs`: `docs: DocStore`, `model: Model`).
   - Bridge calls arrive on bridge threads, so the implementation must hop to
     the UI thread. For example, send a closure over a channel that the GPUI
     loop drains, then wait on a oneshot for the reply.
   - Every method is a few lines on top of `wh_mcp::view`:
     - `outline`: `view::outline(&project.model, &hole_state)`. `hole_state`
       can use `wh_synth::hole_state_with_prose` to report
       stale/soft-stale, which file mode cannot.
     - `get_node`: `view::node_view(&project.model, id, &hole_state)`.
     - `apply_ops`:
       `let edits = view::plan_ops(&project.model, ops)?; let before = project.model.clone(); let (edited, _) = view::apply_to_store(&mut project.docs, &view::ops_label(ops), edits)?;`
       then rebuild the model the way the IDE already does after a
       transaction, and return
       `view::apply_result(&before, &project.model, ops.iter().map(Op::label).collect(), edited)`.
       The transaction is `Origin::Agent`, so it is live in all views, flashed
       through `Transaction::changed_ranges`, and undoable as one step. Do not
       write to disk; the IDE's save logic owns that. For "review before
       apply", hold the planned `edits` and return only after the user decides.
     - `edit_prose`: the same, with `view::plan_prose`.
     - `check`: `view::check_result(&project.model)`, plus
       `wh_project::live::check` when `cargo` is set. Run the cargo part off
       the UI thread on a model clone.
     - `run_specs`: `wh_project::live::test` on a model clone, then
       `view::spec_run(&result)`.
2. When the project opens:
   `let host = wh_mcp::bridge::serve(wh_mcp::bridge::default_socket_path(&wh_project.id()), Arc::new(ide_backend))?;`
   Keep `host` alive as long as the project is open; dropping it closes the
   socket.
3. For each author session, use
   `wh_mcp::author::bridge_mode_server(<wh-mcp or wh exe>, &[] /* or &["mcp"] */, host.socket_path())`
   with `wh_mcp::author::session_options(server)`.
   - The executable is `wh-mcp` next to the IDE binary, or `wh` with the
     prefix `["mcp"]`.
   - With bridge mode the author's edits arrive as transactions on the store
     directly; the `BackendEvent::AuthorEdit`/`merge_write` path is only the
     fallback.

## Tests

`cargo test -p wh-mcp` runs 17 tests; all pass (the live author test is `#[ignore]`d). Each test works on its own
copy of `examples/signup` under `target/wh-mcp-tests/<name>` and never
touches `examples/`.

- **Unit tests** (`src/server.rs`):
  - The op schema covers every `Op` variant. An exhaustive match fails to
    compile when `wh_edit` gains a variant; this happened once during
    development, when `move_flow_step` and `insert_flow_binding` appeared.
  - Every schema field round-trips through serde.
  - Lenient `ops` shapes are accepted.
  - The tool list is correct.
- **`tests/file_mode.rs`** covers every tool in file mode:
  - outline;
  - get_node, including suggestions for unknown ids;
  - atomic multi-op apply: a later op edits an earlier op's def; a failing
    second op leaves the file unchanged; a parse-breaking op is refused; a
    type error is applied and reported as `introduced`;
  - ops given as MCP JSON;
  - prose edits of all target kinds and modes, with every code block
    byte-identical afterwards, and the refusals;
  - merging changes made on disk;
  - spec;
  - `check(cargo)` and `run_specs` on a real cargo build: about 12 s warm,
    using the shared target dir `~/.cache/wild-horse/target`.
- **`tests/bridge.rs`**:
  - `BridgeClient` against a mock host: every tool, errors, bad arguments,
    and host restart with reconnect;
  - an unreachable host;
  - MCP stdio to the `wh-mcp --bridge` shim to the mock host;
  - `FileBackend` behind the bridge.
- **`tests/protocol.rs`** spawns `wh-mcp --project`, then runs `initialize`
  (2025-06-18), `tools/list` (with schemas and annotations), `wh_outline`,
  and `wh_apply_ops` (checking the file on disk), plus an `isError` result
  and the usage errors.
- **`tests/author_live.rs`** is `#[ignore]`d. It runs a real
  `claude-agent-acp` author session through `wh_agent` with the server
  attached in file mode, and asks for one example clause:
  `cargo test -p wh-mcp --test author_live -- --ignored --nocapture`.

  **Run on 2026-10-22: passed in 16.5 s.**
  - The agent called `ToolSearch` first: Claude Code defers loading MCP
    tools.
  - It then called `mcp__wh__wh_get_node`, `mcp__wh__wh_apply_ops` and
    `mcp__wh__wh_check`.
  - It summarized the edit in one sentence, and the clause was in the file
    on disk.
  - No permission request reached the policy, so `allowedTools`
    auto-approval works through `claude-agent-acp`.
  - No Write/Edit was attempted.
