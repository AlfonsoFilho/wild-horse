# wh-ide / wh-graph — status notes

> **Update (main session):** the IDE now starts with the real backend (`src/live_backend.rs`) whenever the folder has a
> `wildhorse.toml`; `WH_IDE_MOCK=1` selects the mock. It provides:
> - a debounced live pipeline: generate from the unsaved buffers, then `cargo check`, then hole states as badges, then (with `synthesis.mode = "auto"`) synthesis by the compiler agent;
> - Synthesize, Run app and author chat. The run uses `WH_TRACE_FILE`, so trace events carry `wh.node`. The author's edits arrive as `AuthorEdit` through `wh-author`;
> - real generated Rust in the inspector.
>
> Typing now coalesces into one undo step per burst (`DocStore::apply_coalescing`). The alias-name and per-block parse-flag requests below are still open.


`cargo test -p wh-edit` passes 11 tests, `-p wh-graph` 14 and `-p wh-ide` 11 (the slow one, `check_and_run_with_trace`, runs
real cargo on a copy of signup). `cargo build -p wh-ide` builds without warnings.
`cargo run -p wh-ide -- examples/signup` opens the IDE (also `examples/todo` and `examples/pong`). With no argument it opens
the current folder if it has a `wildhorse.toml`, or else a welcome window (New Project… / Open Project… / recent projects).

## Ownership (since the split)
- **ide-menus** owns the plumbing: `main.rs`, `state.rs`, `backend.rs`, `live_backend.rs`, `jobs.rs`, `mcp.rs`,
  `side_panels.rs`, `project.rs`, `panels.rs`, `config.rs`, `watch.rs`.
- **ide-ux** owns the views: `canvas.rs`, `graph_panel.rs`, `document.rs`, `inspector.rs`, `highlight.rs`, `rustgen.rs`,
  `tokens.rs`, `theme.rs`, `capsule.rs`, `markdown.rs` and other view files, plus `crates/wh-graph` and `crates/wh-edit`.
- A change one side needs in the other's files is sent as a message rather than edited directly.

## Build from the IDE (ide-menus)
- **Project menu and toolbar row** (top of the window):
  - Build (⌘B), Build Release (⇧⌘R), Synthesize All, Check (⇧⌘B), Test (⌘U), Run (⌘R), Optimize Selected Hole…, and
    Cancel (⌘.).
  - The toolbar disables the job buttons while a job runs and shows "Build: compiling 3/5 — detail (12 s)".
- **`Backend::run_job(model, JobKind)` / `cancel_job()`.** Events: `JobStarted`, `JobProgress { phase, done, total, detail }`
  and `JobFinished { job, ok, summary, artifacts, cost_usd }`. `JobPhase` is one of generating, synthesizing, compiling,
  installing, testing, benchmarking, optimizing.
- **`jobs.rs` (`Job::run`)** does what the CLI does, on the live model (unsaved buffers included):
  - `wh synth`: a `Synthesizer` with the cache and a `Budgeted` driver, with per-hole `Synthesis` events and k/n progress.
  - `wh build`: refuses `todo!()` holes; runs `wh_build::Cargo::build`, reporting node-mapped errors as log entries,
    then `Project::install_bins` into `.wh/bin`.
  - `--release` adds the perf gates (`wh_perf::bench_project` with `BaselinePolicy::IfMissing`) and the mutation gates
    (`mutation_score_with`), as `perf_gates` in wh-cli does.
  - `wh test`: `live::test`, then `NodeResults`.
  - `wh optimize`: `wh_perf::optimize` for the selected hole, then caches the winner as the CLI does.
- **Concurrency and cancelling:** a job takes the live pipeline's `busy` flag, so it never shares the generated crate with
  a check. Cancel wraps the agent driver (`Cancellable`) so no further attempt starts, and skips the remaining phases.
  cargo is never killed half-way.
- **Results:** a toast when the job ends (success auto-hides; failure stays), a log line with the time and agent cost, and
  node-mapped errors and warnings in `AppState::job_problems`, which the Problems tab shows.
- **Verified** in the running app on a copy of `examples/todo`:
  - Build: synthesis "all holes are up to date", then compiling, then `.wh/bin/todo` installed.
  - Test: 29 tests, 10 nodes ✓.
  - Build Release + Cancel: the release compile finished, then the job stopped with a "Build Release failed — cancelled"
    toast.
  - A later full Build Release installed the release binary. Its gate output could not be read because the screen
    locked.
  - `jobs::tests::build_test_and_synthesize_todo` covers synthesize, build, test and cancel headlessly.
- **Not verified:** Optimize (it needs the compiler agent). The perf-gate log output in the UI.

## MCP bridge (ide-menus)
- **Server:** each project window serves `wh_mcp::bridge::serve(default_socket_path(project.id()), IdeBridge)`, kept in
  `Workspace::_bridge`. The socket goes away when the window closes. The log says which `wh mcp --bridge <socket>` to use,
  so any agent can attach to the live buffers.
- **`mcp::IdeBridge` implements `wh_mcp::Backend`.**
  - Each call becomes a `UiCall` closure, which `AppState` runs on its 100 ms poll, while the bridge thread waits.
  - `apply_ops` / `edit_prose` plan with `wh_mcp::view` and apply as one `Origin::Agent` transaction (`agent_edited`
    rebuilds and flashes). Nothing is written to disk.
  - In review mode the planned edits become a multi-file `PendingEdit` with a reply channel. The tool call returns when
    the user accepts (the exact planned edits as one undo step, or a 3-way merge if the buffers moved) or rejects (the
    agent gets a tool error).
  - `check(cargo)` and `run_specs` run cargo on the bridge thread, on a model snapshot.
- **Author:** `LiveBackend::ask_author` passes `wh_mcp::author::bridge_mode_server(program, prefix, socket)` to
  `Author::ask_with`. The program is `$WH_MCP_BIN`, or `wh-mcp` (or `wh mcp`) next to the IDE binary. Without one it falls
  back to the file-edit / `AuthorEdit` path.
- **Tested:** planning and apply plumbing (`mcp::tests`). **Not verified end to end with a real agent**: no agent is
  signed in here, and screenshots stopped when the screen locked.

## Problems tab, F8 (ide-menus, for UX Q7)
- The Log panel tabs are Log | Problems | Run trace.
- `AppState::problems()` lists the model diagnostics (by file and offset) plus the last job's node-mapped problems. They
  are grouped file › node, with error/warning/lint toggles.
- A row click calls `reveal_at(path, offset)`, which selects the node and emits `AppEvent::RevealAt` (the document places
  the caret; ide-ux).
- F8 / ⇧F8 step through errors and warnings. Clicking the status bar's error count opens the Problems tab.
- `LogLevel::Lint` keeps lints muted and consistent.

## Live values (wh-live; ide-menus backend, ide-ux renders)
- `Backend::set_live(on)` and `live_focus(active file, selected node)`. `AppState` pushes the focus on every file switch and
  selection change.
- **Events:** `BackendEvent::LiveValues { scope: wh_live::LiveScope, report: Arc<LiveReport> }` and
  `LivePending { running, reason }`. `LivePending` is quiet: the status bar shows "live …", never a toast.
- **`LiveBackend`:**
  - After a check with 0 cargo errors (before the spec tests), it evaluates `LiveScope::Node(selected)` then
    `LiveScope::File(active)`, with a 500 ms probe timeout and a 30 s run timeout.
  - A generation counter, bumped by `document_changed`, drops reports of models that have since changed.
  - A focus change alone runs live values only (debounced 300 ms), once the current model checked clean and nothing
    holds the generated crate. All probes are compiled in, so this costs about 0.1 s.
  - If the current model was never checked, the focus change triggers a check.
- **`AppState::live: LiveState { on, running, stale, nodes, steps, build_errors }`:**
  - A file report replaces that file's nodes (and their steps); a node report replaces that node.
  - Lookups: `get(node, kind, clause_index)`, `at(path, byte offset)`, `in_file(path)`, `counts()`.
  - `AppEvent::LiveValues` fires on new values and on the toggle.
- View → Live Values (⌥⌘L), on by default. The status bar shows "live n ✓ m ✗ k ⏱".
- **Tested:** `live_values_follow_a_clean_check_and_the_focus` (signup copy, real `wh_live`): the node report, then the file
  report, merged lookups, and a focus change giving a live-only run with no new `cargo check`.
- **Not done here:** drawing the values (ide-ux: inline in the document, on flow edges, dimmed when `LiveValue::source` no
  longer matches the text).

## Agent panel: failures, review, context, cost (UX N7, N6, N12; ide-menus)
- **N7 failure cards:** the event `BackendEvent::SynthFailed(SynthFailure { node, gate, detail, rounds, blocked_on, notes, hole_file })`
  is built by `SynthFailure::from_report` from the last `wh_synth::Verdict`.
  - `gate_of` classifies a rejection as compile / lint, example, contract, property, performance, signature, stub,
    sandbox or test build; `Blocked` gives "blocked". Tested in `backend::tests`.
  - The live pipeline and Synthesize All / Build emit it. `AppState::failures` keeps it until the hole is queued or
    accepted again.
  - The Agent panel shows a card per failure (gate chip, rounds, first lines of evidence, notes) with these actions:
    - Retry;
    - Retry with note… (adds a `note "…"` clause, then re-synthesizes);
    - Open hole file;
    - Pin manual impl (prepends `// wh:pinned` and opens the file);
    - Ask author to revise spec.
  - "Explain failure" is a node action.
- **N6 per-hunk review:** `PendingEdit.files[].hunks` comes from `similar` grouped ops with two lines of context; each hunk
  has a keep / skip toggle.
  - "Accept all" applies the agent's planned edits as one undo step.
  - "Accept k of n" merges the partial text (`PendingFile::result`).
  - A bridge call that is only partly accepted gets a tool error saying which parts were skipped.
  - Tested by `state::tests::hunks_can_be_kept_or_skipped`.
  - **Not done:** the live-mode "Revert" card, the graph diff overlay, provenance bars, per-turn checkpoints, inline
    block diffs in the document (that one is ide-ux's).
- **N12:**
  - Context chips above the input (selected node, active file), each with × (it comes back when the selection or file
    changes). `AppState::ask_author_scoped`.
  - Consecutive tool-call rows ("→ …") fold into one activity row that expands on click.
  - A budget meter: `AgentCost` events and `JobFinished.cost_usd` against `[synthesis] budget_usd`
    (`config::synthesis_budget`).
  - Node actions: Suggest properties, Write prose, Review contracts, Split this def, Explain failure.
  - **Not done:** follow-the-agent.
- **N10 presets and maximize:**
  - The toolbar has Write | Design | Debug; also View → Workspace and ⌃1 / ⌃2 / ⌃3.
  - Each preset's last arrangement is kept in `.wh/presets.json` (`dock.json` stays the current one). Reset Layout
    restores the current preset's built-in arrangement.
  - Maximize Panel is ⇧⎋ (gpui-kit `dock::ToggleZoom`, handled by the focused tab group), also in View.
- **Gotcha:** a test module under a file that glob-imports `gpui_kit::*` must use `#[::core::prelude::v1::test]`.
  Plain `#[test]` resolves to gpui's macro and recurses forever.

## Menus, files, layout, test badges, review mode (ide-menus session)

Everything below was checked in the running app on copies under `target/wh-ide-manual/` (signup with the live backend,
pong with `WH_IDE_MOCK=1`), driven by background computer use through a throw-away bundle `target/wh-ide-manual/WHIdeTest.app`.

- **Windows:** one window per project; `Windows` (a gpui global in `main.rs`) maps each window to its `Workspace`. Opening a
  project that is already open brings its window forward. Closing the last project window shows the welcome window.
  Each window has its own `AppState`, `DocStore`, model, backend and watcher.
- **Menus** (`set_menus` in `main.rs`; shortcuts come from the keymap):
  - App menu: About, Services, Hide (⌘H), Hide Others (⌥⌘H), Show All, Quit (⌘Q). Quit asks once for all projects with
    unsaved files: Save All / Don't Save / Cancel.
  - File: New Project… (⇧⌘N; a save panel names the new folder, then `wh_project::scaffold::new_project` writes
    `wildhorse.toml`, `src/main.wh.md` and `.gitignore`, the same as `wh new`), Open Project… (⇧⌘O; the folder must have `wildhorse.toml`),
    Open Recent (`recent.json` in the user config dir, or `$WH_IDE_CONFIG_DIR`; plus Clear Recent), Close Project (⇧⌘W),
    New File… (⌘N; save panel in the first source dir, writes front matter `module: <file stem>`), Open File… (⌘O; a
    `.wh.md` under the sources, which opens a tab), Save (⌘S, the active file), Save All (⌥⌘S), Save As… (⇧⌘S; inside
    the sources the copy becomes a new module named after the file, and the original goes back to its saved text, with
    undo available; outside them the buffer is exported), Revert File (asks; undoable), Close File (⌘W; asks Save / Don't
    Save / Cancel; Don't Save reverts; with no tab open ⌘W closes the project).
  - Edit: Undo / Redo (the global `DocStore`), Cut / Copy / Paste / Select All (the focused input's actions, as `os_action`s).
  - View: Toggle Document (⌘1), Toggle Inspector & Agent (⌘2), Toggle Log (⌘3), Reset Layout.
  - Project: Check (⇧⌘B), Run App (⌘R). Window: Minimize (⌘M), Zoom.
  - The window close button asks the same way as Close Project.
  - **gpui gotcha:** a window is leased while it dispatches an action, so a global action handler cannot update that same
    window. Every global handler therefore `cx.defer`s its work. Without the defer, menu items reached the handler but
    `AnyWindowHandle::update` failed silently.
  - Verified: menu contents, Check, Undo, Close File with prompt and Don't Save, Close Project with prompt, Quit prompt
    (cancelled), Open Recent (second window), welcome window (recent row reopens a project), New File… through the save
    panel, View → Toggle Log.
- **Tabs:** `AppState::open_tabs` (paths). The document panel shows those tabs, each with × (• when dirty, which goes
  through the ⌘W prompt). It shows a placeholder when no tab is open. Selecting a node in a closed file reopens its tab.
- **File watching** (`watch.rs`, `notify` 8): changed paths under the project, minus `.wh/`, `target/` and dotfiles, are
  drained on the state's 100 ms poll.
  - A changed source calls `Project::reload_file`, which is `DocStore::reload_from_disk`: it merges onto unsaved edits,
    flashes, and is undoable.
  - New sources join the project with a tab; deleted ones leave it.
  - `wildhorse.toml` re-reads `[author] apply`.
  - Verified: an external edit merged and re-checked; an external new file appeared; an external delete removed its tab.
- **Dock layout persistence:** `.wh/dock.json`, gpui-kit `DockArea::dump`, saved 500 ms after each `DockEvent::LayoutChanged`,
  on window close and on quit.
  - On open, the file is loaded with `DockArea::load` only when it names exactly our six panels. `register_panel`
    builders hand back the live panels through the `LoadingPanels` global.
  - Panels are not closable (`dock_panel!`), so a layout cannot lose one. View → Reset Layout restores the default.
  - Verified: the collapsed Log dock survived a restart, and every panel worked after the load.
- **Spec results as badges:** `BackendEvent::TestsStarted` and `BackendEvent::NodeResults(BTreeMap<NodeId, NodeResult>)`.
  - `NodeResult { passed, failed, failures }` has `badge()`, "✓ n" or "✗ f/n".
  - `LiveBackend` runs `wh_project::live::test` after a check with 0 errors, on the pipeline's background thread (so it is
    debounced like the check), and again after synthesis accepts something.
  - Tests of holes that are still `todo!()` stubs give no result; their synthesis status covers them.
  - Shown as:
    - a filled green/red pill in the graph node header (defs, flows, apps; a step shows its callee's), distinct from the
      bare synthesis glyph;
    - ✓ / ✗ in a gutter left of each code editor at the definition's first line, with a tooltip summary;
    - the inspector's header line, e.g. "✗ 1/2 1 of 2 spec test(s) fail: … panicked at …";
    - the status bar ("specs: 7 ✓ 1 ✗", or "specs running…");
    - Warn rows in the log, one per failing node, that select it.
  - The mock emits results for defs with examples; `WH_IDE_MOCK_FAIL=name,…` fails some.
  - Verified with the real backend on signup: 15 tests over 8 nodes all ✓. Editing an example on disk to be wrong gave
    "signup.check_age: 1 of 2 spec test(s) fail" and a red ✗ 1/2 in the graph and gutter.
- **Author review mode:** `[author] apply = "review"` in `wildhorse.toml` (`config::author_review`, read by wh-ide). While
  reviewing, `AuthorEdit`s become `AppState::pending`, shown in the Agent panel as a unified diff (`similar`) with
  Accept / Reject.
  - Accept goes through `merge_write(Origin::Agent)`. A conflict keeps the item and shows the error.
  - The Agent panel header has a Live / After review toggle (session only).
  - The status bar counts pending edits.
  - The mock proposes a prose edit when the message starts with "mock edit".
  - Verified on pong: the edit waited, then Accept applied it and dirtied the file.
- **Graph editing:**
  - `wh_edit::Op::MoveFlowStep { flow, stmt, from, to }` reorders `|>` steps. It swaps step texts and keeps the separators,
    so inline and multi-line pipelines both work.
  - `Op::InsertFlowBinding { flow, index, name, value }` inserts `name = value` as statement `index`. Helpers:
    `wh_edit::step_site` and `flow_len`. Tests are in `crates/wh-edit/tests/ops.rs`.
  - Flow graph:
    - Drag a `|>` step onto another step of the same pipeline to reorder it. The target is outlined while dragging
      (`CanvasEvent::DropOnto`, `GraphCanvas::can_drop`). That flow's stored step positions are dropped, so the chain is
      re-laid out.
    - The step toolbar has ◀ / ▶.
    - Feeding a step's output into a def (drag or the "insert after" palette) chooses by form: in DAG form (the step ends
      a `name = …` statement) it creates a binding `snake(ReturnType) = callee(name)`; otherwise it does a `|>` insert.
      A flow parameter in a DAG-form flow also gets a binding at the top.
  - ⌫ / Delete in a graph removes the selected `|>` step, definition or field relationship. F2 / Enter focuses the
    rename field. The canvas takes key focus on click (key context `GraphCanvas`).
  - **Clusters in layout:** `layout()` lays out each section's members on their own, then places the section boxes as
    blocks (`CLUSTER_PAD` / `CLUSTER_TITLE_H`, shared with the canvas). Section boxes cannot overlap each other or
    outside nodes; tested by `section_boxes_do_not_overlap`.
  - Verified: drag-reorder (normalize_form onto validate rewrote the pipeline and flashed; ⌘Z restored it), ⌫ on a step,
    and a DAG drag on pong's `tick` (`step` → `render` gave `frame = render(next)`).

Built on `gpui-kit` 0.8 (gpui-pre 0.3.9, gpui-component 0.8). A digest of the gpui-kit APIs this code relies on is in
the shared side-session notes folder (`gpui-kit-api.md`).

## Layout of the code

| file | what |
|---|---|
| `crates/wh-graph/src/lib.rs` | Shared graph shape: `Graph`/`GNode`/`Row`/`GEdge`/`PortRef`/`Cluster`, the canvas metrics (`HEADER_H`, `ROW_H`, `CHAR_W`, …) and `port_accepts`. |
| `crates/wh-graph/src/flow.rs` | `flow_graph(model, FlowOptions{module, hide_calls})`. |
| `crates/wh-graph/src/data.rs` | `data_model_graph(model, DataOptions{module, overlay})`. |
| `crates/wh-graph/src/layout.rs` | Sugiyama-style `layout(graph)` and `place(graph, stored)`. |
| `crates/wh-graph/src/store.rs` | `LayoutStore`, the `.wh/layout.json` sidecar. |
| `src/project.rs` | UI-free state: `DocStore`, `Model`, the display model with last-good "ghost" nodes, selection, layout, ops, undo/redo, save. Unit-tested. |
| `src/state.rs` | `AppState`, one per project window. It emits `AppEvent::{Changed, Selected, ActiveFile, Saved, Files, Results}`, holds the open tabs, log, trace, chat, synthesis queue, spec results and pending author edits, and polls the backend and the file watcher every 100 ms. |
| `src/config.rs` | `[author] apply`, recent projects, `.wh/dock.json` path, the new-file skeleton. |
| `src/jobs.rs` | Project jobs (Build, Build Release, Synthesize All, Test, Optimize) on a background thread. |
| `src/mcp.rs` | `IdeBridge`, the IDE's `wh_mcp::Backend`, for the MCP bridge. |
| `src/watch.rs` | `notify` watcher → project-relative changed paths. |
| `src/backend.rs` | The `Backend` trait and `MockBackend`. |
| `src/canvas.rs` | Reusable graph canvas widget. Emits `CanvasEvent`s and knows nothing about ops. |
| `src/graph_panel.rs` | Flow graph and Data model panels: canvas plus toolbar, mapping gestures to `wh_edit::Op` (`Origin::Graph`). |
| `src/document.rs` | Document panel: tabs, prose blocks and `wh` code editors with a lexer-token highlighter. |
| `src/inspector.rs` | Inspector panel: edits go out as `Op`s (`Origin::Inspector`). |
| `src/side_panels.rs` | Agent panel and Log panel (Log and Run trace tabs). |
| `src/rustgen.rs` | Read-only Rust preview shown when the backend provides no generated code. |
| `src/highlight.rs` | Token → class map built from `wh_syntax::lexer`. |
| `src/main.rs` | Windows registry, `Workspace` (one per project window), welcome window, menus, actions, prompts, dock layout persistence. |

**wh-graph details:**
- `flow.rs` node ids:
  - defs, flows and other definitions use the model `NodeId`
  - steps use `FlowStep.id`
  - flow params use `<flow>#in:<param>`
  - the app entry is treated as a flow of the app node
- `flow.rs` edges: data (`StepInput`), result, uses, implements, provides, calls (step → callee). With a module scope, definitions from other modules appear as `external` stub nodes.
- `data.rs` relationships are derived from the *written* field types, so aliases such as `UserId` are kept. Cardinality comes from the containers around the type:
  - `T` = 1, `T | None` = 0..1, `list`/`set`/`Stream` = 0..*, `NonEmpty`-style refined lists = 1..*, `dict[K, V]` = keyed by `K`, `array[T, N]` = N
- `data.rs` edge kinds:
  - **References** is used for a struct's `key` type, via `Model::key_owner`. When the key type is an alias, the owner must declare its key with that alias name.
  - The other kinds are has, variant-of, converts-from (`from`), refines, alias-of, and produces/consumes when the overlay is on.
- `layout.rs` steps: DFS cycle removal, longest-path layering (sources are pulled next to their first consumer), dummy nodes for long edges, 16 barycenter sweeps (keeping the best crossing count), then iterative column placement.
  - Connected components are stacked; lone nodes are packed in rows.
  - Section clusters are laid out on their own first, and the resulting boxes are then laid out as blocks.
  - `place` keeps stored positions, shifts the other nodes with their placed neighbours, and nudges them off overlaps.
- `store.rs`: positions are stored per graph (`flow`, `data`) and keyed by node id. `rekey` follows renames, including `id#…` step and param ids.

## What works (checked by hand in the running app, background computer-use screenshots)

### Layout and panels
- **Default layout** follows the design doc:
  - Document on the left.
  - Flow graph over Data model in the centre.
  - Inspector and Agent tabs on the right.
  - Log at the bottom.
  - A status bar shows the project name, dirty state, node and error counts, stale-node count and the current selection.
- **Document panel**:
  - One tab per `.wh.md`; a `•` marks a dirty tab.
  - Prose renders as Markdown: headings, paragraphs, `[[refs]]` and `` `code` `` highlighted, the front matter, and non-wh fences.
  - Clicking prose turns it into a Markdown source `Textarea`; edits sync into the document, and blur returns to the rendered view.
  - `wh` blocks are gpui-kit `Editor`s with a custom `InputHighlighter` driven by `wh_syntax::lexer` tokens.
  - Diagnostics appear inline as editor squiggles and as a list under each block.
  - Typing produces `Origin::Text` transactions; the new text is diffed against the block's last synced content into one minimal edit.
  - Regions written by Graph, Inspector, Agent or undo transactions flash for about 1.6 s: a fill decoration in code and a background tint on prose.
- **Flow graph**:
  - Clusters per section; port types are shown on rows; Data, Result, Uses, Calls and other edges are colour-coded.
  - Scroll to pan, ⌘-scroll or pinch to zoom, Fit / + / − buttons, drag a node to move it (the position is persisted to `.wh/layout.json`).
  - "Active module" / "Whole project" scope toggle.
- **Data model**: structs, enums, `@error` enums, refined types and aliases, with field and variant rows, cardinality labels and reference edges.

### Editing through the graphs and inspector
- **Flow graph edits:**
  - Drag from a step's or param's output port onto a def, flow or step to get `InsertFlowStep`. While dragging, incompatible targets are greyed using `wh_model::assignable`/`compatible`.
  - Selecting a step shows a toolbar: "Remove step" (`RemoveFlowStep`) and an "insert after" palette that lists only type-compatible defs.
  - Selecting a definition shows a toolbar: rename (`Rename`), Synthesize, Delete (`RemoveNode`).
  - With nothing selected, "Add def" (`AddDef` in the active file, in the selected node's section).
  - **Verified:** inserting `check_age` after `validate` rewrote the pipeline in place, flashed it, updated the graph and the inspector, and ⌘Z restored it. Drag-to-connect from a step's output was also verified.
- **Data model edits:**
  - Clicking a struct's field row (or an edge) selects that relationship and shows cardinality buttons 1 / 0..1 / 0..* / set (`SetCardinality`) plus "Remove field".
  - Drag from a struct's right edge onto a type to get `AddField`; drag from a field row to get `RetypeField`.
  - Toolbar actions: Add field, Add variant, Add struct, Rename, Delete.
  - "Producers/consumers" toggles the overlay.
  - **Verified:** `User.email` 1 → 0..* became `email: list[Email]` in the text (flashed), and ⌘Z restored it.
- **Inspector:**
  - Shows kind, id, file:line, signature (`EditSignature`), each clause (`ReplaceClause` ✓, `RemoveClause` ×), add clause (`AddClause`) and pattern (`SetPattern`).
  - Also shows prose, struct fields / enum variants, a Rust preview, Synthesize / Run specs buttons, and recent trace events for the node.
  - For a step, it shows the step's types, its id and "Go to callee".
  - Enter or the Apply / Add / Set buttons commit.
  - **Verified:** removing `pattern Transform` and adding `pure` both edited the text and flashed it.

### Selection, undo, save
- **Shared selection:**
  - Clicking in either graph selects everywhere: the document scrolls to the definition or step, switching tabs if needed, and highlights it, and the inspector updates.
  - Moving the cursor in a code block (mouse up or key up) selects the definition or step under it.
  - Log and trace rows that carry a node id select it.
  - The data model maps a step selection to its flow.
- **Global undo/redo:** ⌘Z / ⇧⌘Z go through the `DocStore`, whatever the origin.
  - The input widgets' own undo is overridden in the `Input` key context.
  - App-level fallback handlers cover the case where nothing has focus.
  - ⌘S writes dirty files and the layout (unit-tested in `project.rs`; not exercised on the example, so its sources stay untouched).

### Broken blocks, backend panels
- **Broken blocks:**
  - While a `wh` block fails to parse, its last valid nodes stay in the graphs, greyed (35% opacity) with "⚠ parse error".
  - The inspector shows a warning and disables its fields, and ops on those nodes are refused.
  - The status bar counts stale nodes.
  - **Verified** by typing `flowx register…`. Unit tests are in `project.rs`.
- **Agent / Log panels** (mock backend):
  - Author chat with the selection as context.
  - Synthesis queue (rows select the node).
  - Log with Check / Run app / Clear.
  - The Run trace tab lists events carrying `wh.node`; clicking one selects the step and the graph outlines executed nodes in green.

## What does not work yet / known gaps
- **Editor state across changes:**
  - Changing a block's structure while typing (for example typing a fence ```` ``` ````) rebuilds all editors of the file, which loses the cursor.
  - A non-Text change to a block resets that editor's cursor (`set_value`).
- **Layout:**
  - Stored (hand-placed) positions win over the cluster blocks, so `place` can still put a node into another section's box
    (seen with signup's checked-in `.wh/layout.json`). The auto layout itself is overlap-free.
  - Clusters are flat: a parent section's box holds only its direct members, not its subsections' boxes.
  - Section titles are hidden below 30 % zoom; at larger zoom they fit the title band.
  - Edge labels can overlap.
  - Edge hit-testing is pixel-based. That is why clicking a field row selects its relationship, which is easier than hitting the edge.
- **Graph editing:**
  - Rename is done in the toolbar input; F2, Enter and double-click focus it, but nothing selects its text.
  - A DAG binding cannot be removed or moved from the graph: ⌫ on it logs a hint. Only `|>` steps reorder.
  - Machines are not projected.
  - F2 was not checked visually; ⌫ was.
- **Menus and files:**
  - Open File… cannot start in the project folder (`PathPromptOptions` has no directory).
  - Save As outside the sources only exports.
  - New Project uses a save panel to name the folder; an existing empty folder also works.
  - A source deleted on disk closes its buffer (`DocStore::close`, which also drops the undo steps that touched it), so
    its unsaved edits are lost, with a warning in the log.
  - Open tabs are not persisted across restarts; every file opens as a tab.
  - The Live / After review toggle in the Agent panel is for the session only; it does not write `wildhorse.toml`.
- **Backend-dependent features:**
  - Spec results come only after a check with 0 cargo errors; editing clears nothing until the next run, so badges can
    be stale while you type.
  - The test run shares the pipeline thread, so a long `cargo test` delays the next check.
- **Background computer use:** the window repaints only on the next raw input (click the status bar to flush); gpui
  inputs ignore AX text writes, so type with per-key `app_key`. Menus work through `app_menu`.
- **Another session's files:** `src/theme.rs` and `src/tokens.rs` appeared in `wh-ide/src` during this work (from the
  UX-review style track, it seems). They are not declared in `main.rs` and so are not built; they were left untouched.
- **Inspector:** adapters' clauses are shown read-only, because `wh-edit` clause ops only target defs.
- **Verification limit:** typing shifted characters (`|`, `>`, `"`) could not be exercised through background computer use. Letters, Backspace and arrow keys could, and the typing path itself was verified (`save_user`→`save_usex` gave an error and a graph update; `flow`→`flowx` gave greyed ghosts).

## The `Backend` trait (`src/backend.rs`)

```rust
pub trait Backend: 'static {
    fn document_changed(&mut self, model: &Model);          // after every transaction (any origin)
    fn check(&mut self, model: &Model);                      // checker / cargo check → Log events
    fn synthesize(&mut self, model: &Model, node: &NodeId);  // (re)synthesize one hole
    fn run_app(&mut self, model: &Model);                    // build + run with WH_TRACE_FILE → Trace events
    fn ask_author(&mut self, model: &Model, text: &str, selection: Option<&NodeId>);
    fn generated_rust(&self, _model: &Model, _node: &NodeId) -> Option<String> { None }
    fn set_live(&mut self, on: bool) {}                       // live values (wh-live)
    fn live_focus(&mut self, file: Option<String>, node: Option<NodeId>) {}
    fn run_job(&mut self, model: &Model, job: JobKind);     // Build{release} | SynthesizeAll | Test | Optimize(node)
    fn cancel_job(&mut self);
    fn poll(&mut self) -> Vec<BackendEvent>;                 // drained by the UI every ~100 ms
}

pub enum BackendEvent {
    Log { level: LogLevel, source: String, message: String, node: Option<NodeId> },
    Synthesis { node: NodeId, status: SynthStatus },   // Stale | Queued | Synthesizing | Verifying{round,of} | Accepted | Failed(msg)
    Chat { role: ChatRole, text: String },             // User | Agent | System
    AuthorEdit { path: String, base: String, new_text: String, label: String },  // applied with DocStore::merge_write(Origin::Agent)
    Trace(TraceEvent { level, node: Option<NodeId> /* wh.node */, message, duration }),
    RunFinished { ok: bool },
    TestsStarted,                                       // a spec run started (status bar "specs running…")
    JobStarted { job }, JobProgress { phase, done, total, detail }, JobFinished { job, ok, summary, artifacts, cost_usd },
    SynthFailed(SynthFailure), AgentCost(f64),
    LiveValues { scope, report }, LivePending { running, reason },
    NodeResults(BTreeMap<NodeId, NodeResult>),         // NodeResult { passed, failed, failures }; replaces the previous set
}
```

- `model.files[i].text` holds the live, unsaved buffers.
- Paths are relative to the project directory (for example `src/signup.wh.md`). `Project::dir` is the canonical project root.
- `MockBackend` replays scripted events:
  - check: diagnostics
  - synthesize: queued → synthesizing → verifying → failed
  - run: a trace of the app's entry flow and the flows it calls
  - author: a canned reply
- Per the wh-agent notes, `claude-agent-acp` edits arrive as permission requests (`old_string`/`new_string`), not as `fs/write_text_file`. The backend should apply them to the base text it read and send an `AuthorEdit` carrying the full new text.

## API requests for other crates (not changed here)
- **wh-doc:** transaction coalescing for typing. For example:
  - `apply_coalescing(origin, label, edits, key)`, which merges into the previous undo entry when it has the same origin and key and is adjacent within a short time window, or
  - `amend_last(edits)`.
  
  Also an undo/redo result that says which post-change ranges to highlight. `wh-ide` currently derives it from `tx.edits` after an undo.
- **wh-model:**
  - Keep the written alias in types. Today `FieldInfo.ty` for `UserId` is `u64`, so the inspector shows `u64`, and `key_owner(u64)` would match any `u64` field. wh-graph works around this with the AST `TypeExpr`. A `Ty::Alias(id, Box<Ty>)` or a `FieldInfo.ty_written: String` would fix it.
  - A per-block "parses" flag or the block index on parse diagnostics. `wh-ide` re-parses each file to find broken blocks.
  - `NodeKind::Machine` details, for a state-diagram projection.
- **wh-edit** (open; `MoveFlowStep` and `InsertFlowBinding` are done):
  - Remove / move a DAG binding statement.
  - Clause ops on adapters.
  - `AddField` with a cardinality.
  - `RemoveClause` leaves the surrounding blank line as is, which is fine.
