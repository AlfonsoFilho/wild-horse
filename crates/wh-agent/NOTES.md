# wh-agent

ACP client layer shared by the author role (`wh-author`) and the compiler role
(`wh-synth`). It spawns agents, routes their file and permission requests to a
per-session policy, and streams prompt turns as events. It contains no
synthesis or authoring logic.

Built on `agent-client-protocol` 3.5 (schema 1.12, protocol v1, no crate
features enabled). The protocol types are re-exported as `wh_agent::acp`.

## API

```rust
use wh_agent::{AgentConfig, AgentHandle, ScopedPolicy, SessionEvent, SessionOptions};

let agent = AgentHandle::spawn(AgentConfig::claude_agent_acp())?;   // or `start(..).await`
agent.auth_methods();                         // from `initialize`
// agent.authenticate_blocking("claude-ai-login")?;  // only if the agent needs it

let policy = Arc::new(ScopedPolicy::new(&scratch).allow_write(&hole_file));
let session = agent.new_session_blocking(&scratch, vec![], policy)?;  // or `new_session(..).await`

let mut turn = session.prompt("…")?;          // a PromptStream
while let Some(event) = turn.recv_blocking() { // or `.recv().await`, or use it as a `futures::Stream`
    match event {
        SessionEvent::AgentText(t) => …,
        SessionEvent::Finished { stop_reason } => …,   // always last (or `Error`)
        _ => …,
    }
}
session.cancel()?;                            // sends `session/cancel`; the turn ends `Finished { Cancelled }`
```

- **`AgentConfig { command, args, env, cwd }`** can be deserialized from
  `[agent.compiler]` / `[agent.author]` tables. `env` is added to the inherited
  environment. `cwd` is the process's working directory; each session has its
  own `cwd`. `AgentConfig::default()` is `npx -y @agentclientprotocol/claude-agent-acp`.
- **`AgentHandle`** is `Clone`.
  - `spawn`/`start` launch the process and run `initialize`. The client
    advertises `fs.readTextFile` and `fs.writeTextFile`, and `terminal: false`.
  - `connect`/`connect_blocking` take any ACP transport instead, such as
    `agent_client_protocol::Channel` for an in-process agent.
  - Accessors: `agent_info()`, `auth_methods()`, `initialize_response()`,
    `authenticate(id)`, and `stderr_tail()` (the last 200 stderr lines).
  - Sessions open with `new_session(cwd, mcp_servers, policy)`, or with
    `new_session_with(cwd, SessionOptions { mcp_servers, additional_directories, meta }, policy)`.
    `meta` is the `_meta` sent with `session/new`.
  - `shutdown()` stops the connection immediately. The agent is also stopped
    when the last `AgentHandle` or `SessionHandle` is dropped. Either way the
    process group is SIGKILLed, so the `npx → node` grandchild dies too.
- **`McpServerConfig`** is an alias for ACP's `McpServer` (stdio, http or sse).
- **`SessionHandle`**:
  - `prompt(text)` / `prompt_blocks(blocks)` return a `PromptStream`. Only one
    turn runs at a time; a second call returns `Error::PromptInProgress`.
  - `cancel()` sends `session/cancel`.
  - `id()` and `agent()` are accessors.
  - Dropping the handle unroutes the session: the agent's later requests for it
    get an error.
- **`SessionEvent`** variants:
  - Content: `AgentText`, `AgentThought`.
  - Tool calls: `ToolCallStarted { id, title, kind, status }` and
    `ToolCallUpdated { id, title?, status? }`.
  - `Plan(entries)`.
  - File requests, each reported with the policy's result: `FileRead { path, result }`
    and `FileWrite { path, result }`.
  - `PermissionDecided { request, decision, selected }`.
  - `Other(SessionUpdate)` for usage, modes, commands and similar updates.
  - Terminal events: `Finished { stop_reason }` or `Error(msg)`. A stream always
    ends with one of them. If the agent dies mid-turn, the stream synthesizes
    `Error`.
- **`SessionPolicy`** is the trait every request is routed through:

  ```rust
  trait SessionPolicy: Send + Sync {
      fn read_text_file(&self, path: &Path) -> Result<String, PolicyError>;
      fn write_text_file(&self, path: &Path, content: &str) -> Result<(), PolicyError>;
      fn decide_permission(&self, request: &PermissionRequest) -> PermissionDecision;
  }
  ```

  - Every `fs/read_text_file`, `fs/write_text_file` and
    `session/request_permission` goes through it.
  - `read_text_file` returns the whole file; the client applies ACP's
    `line`/`limit` itself.
  - `PolicyError::{Denied, NotFound, Io}` become JSON-RPC errors the agent sees:
    `-32000` with the message, or `resource_not_found`.
  - `PermissionDecision` is `AllowOnce | AllowAlways | RejectOnce | RejectAlways | Select(id) | Cancel`.
    It resolves to an option the agent offered. When the exact kind is missing
    it falls back within the same polarity, and it never turns a reject into an
    allow. A reject with no reject option offered answers `cancelled`.
  - **Policies run on the connection thread inside the dispatch loop. Keep
    them fast and never block on the session's own events.**
- **`ScopedPolicy::new(root).allow_write(path)…`** is the compiler-role default:
  - Reads are allowed under `root`.
  - Writes are allowed only to the listed files.
  - Paths are canonicalized, so `..` and symlinks cannot escape.
  - Permission requests are rejected, except an `edit` (or `read`) tool call
    whose named files (`locations`, plus `raw_input.file_path`/`path`/`notebook_path`)
    are all writable (or all under root). The next section explains why this
    exception is needed.
- **`claude_code_meta(json)`** builds `{"claudeCode":{"options": json}}` for
  `SessionOptions.meta`.

**Threading model.** Each agent gets a `wh-agent-acp` thread running a
current-thread tokio runtime that drives the connection and the child process.
The public async methods await only `futures` or `async-channel` primitives, so
any executor (GPUI's included) can poll them without tokio. The `_blocking`
twins use `futures::executor::block_on`.

Logging uses `tracing` under the `wh_agent` targets. Agent stderr goes to
`wh_agent::stderr` at debug level.

**Tests.** `cargo test -p wh-agent` runs an in-process mock agent built with the
SDK's `Agent` side over `Channel::duplex`. The tests cover:
- reads, including a `line`/`limit` slice and a read outside the root;
- an allowed and a disallowed write;
- a permission request;
- streamed tool, plan, thought and text events, with their order;
- cancel, and the one-prompt-at-a-time rule;
- the agent hanging up mid-turn;
- spawn failure, and early exit with stderr captured;
- TOML config parsing.

`real_claude_agent_acp` is `#[ignore]`d. It needs npx, network access and a
Claude login:
`cargo test -p wh-agent -- --ignored real_claude --nocapture`.
It has not been run yet.

## Findings: `@agentclientprotocol/claude-agent-acp` 0.94.0

Read from the npm tarball without running it. The package uses
claude-agent-sdk 0.3.303 and the JS ACP SDK 2.0.0. Line numbers refer to
`dist/acp-agent.js` unless another file is named.

1. **File edits are NOT routed through `fs/write_text_file`, and reads are not
   routed through `fs/read_text_file`.**
   - The adapter has `readTextFile`/`writeTextFile` pass-throughs (970-974,
     7151) but nothing calls them.
   - There is no `mcp__acp__Read/Write/Edit` server, and nothing inspects
     `clientCapabilities.fs` or `clientCapabilities.terminal`.
   - Claude Code's built-in Read/Write/Edit/NotebookEdit run in the CLI process
     and touch the disk directly. Advertising fs capabilities changes nothing.
2. **Bash runs locally even with `terminal: false`.** Built-in tools default to
   the full `claude_code` preset (8389). The terminal capability only affects
   display `_meta`.
3. **Permissions.**
   - `session/request_permission` is sent only from the SDK's `canUseTool`, that
     is, only when Claude Code's rules and mode resolve a tool use to "ask".
   - In `default` mode, Write, Edit and Bash ask. In `acceptEdits` and
     `bypassPermissions`, edits never ask.
   - Option ids are `allow-once` (allow_once), `allow-with-updates`
     (allow_always) and `reject` (reject_once).
   - A selected `reject` is respected: the tool is denied with "User refused
     permission to run tool" (`dist/permissions/effects.js`).
   - A `cancelled` outcome is respected too: it throws "Tool use aborted".
   - Selecting an option id that was not offered throws.
4. **The initial permission mode comes from settings, not from the client.**
   - The mode is taken from `permissions.defaultMode` in the user's
     `~/.claude/settings.json` (`dist/session-mode.js:22-35`).
   - Escalating modes are filtered out of repo-committed project settings
     (`dist/settings.js:33`), but not out of user-level settings.
   - `_meta.claudeCode.options.permissionMode` is overridden by the adapter
     (8499). The client can only switch modes later, with `session/set_mode`.
5. **Settings can auto-allow tools without asking the client.**
   - The adapter sets `settingSources: ["user","project","local"]` (8463), so
     `permissions.allow` rules in those settings files skip `canUseTool`.
   - A client can override this with `_meta.claudeCode.options.settingSources`
     (for example `[]`), because the spread comes after 8463. Managed policy
     still applies.
6. **Restricting tools through `_meta.claudeCode.options`** (spread at 8483):
   - `disallowedTools` (for example `["Bash","WebFetch","WebSearch"]`) is merged
     with the adapter's own list.
   - `tools: [...]` replaces the preset as an allowlist.
   - `_meta.disableBuiltInTools: true` removes all built-in tools.
   - Note that `allowedTools` means auto-approve, not restrict.
   - The adapter forces `cwd`, `permissionMode`, `canUseTool` and
     `includePartialMessages`.
7. **Auth.**
   - `authMethods` is empty unless the client advertises terminal auth. With it
     the methods are `claude-ai-login` and `console-login`, or `claude-login`
     over SSH or with `NO_BROWSER`.
   - `session/new` has no auth gate. Missing credentials show up later as an
     `authRequired` error during a turn.
8. **Not verified:**
   - how the CLI treats the thrown "Tool use aborted" (presumably as a denial);
   - whether `disallowedTools: ["Bash"]` also covers every subprocess path;
   - when the old `mcp__acp__*` fs routing was removed (no changelog in the
     tarball).

### Consequences for the roles

- **Compiler sandbox.** With this agent, the `fs/write_text_file` interception
  that the design relies on never fires. The guarantees come from:
  - a. the permission gate: in `default` mode every Write/Edit asks, and
    `ScopedPolicy` allows only edits of the hole file;
  - b. `_meta.claudeCode.options = { disallowedTools: ["Bash", "WebFetch", "WebSearch", …], settingSources: [] }`,
    which keeps settings `allow` rules from bypassing the gate;
  - c. the design's post-job hash of the scratch tree, which stays mandatory.

  Point (c) matters because a user-level `defaultMode: "acceptEdits"` or
  `"bypassPermissions"` would skip the gate entirely. The adapter reads that
  setting regardless of `settingSources`; this was not verified.

  OS-level sandboxing (`sandbox-exec`, landlock) is the real fix.
- **Author role.** Edits to `.wh.md` happen on disk, not through the
  doc-store merge. The author policy will see them only as permission requests
  carrying `raw_input`: Write/Edit `file_path` with `content`, or
  `old_string`/`new_string`.

  Options for the author role:
  - Reject those requests and reload from disk.
  - Apply the edit itself from `raw_input` into the doc store, then reject or
    allow the tool call.
  - Better: ship the `wh` MCP server (M2) and disallow Write/Edit/MultiEdit,
    so that every edit arrives as a semantic op.
- An agent that does use `fs/*`, such as Zed's or a future claude-agent-acp,
  already works with `SessionPolicy`. No code changes are needed.
