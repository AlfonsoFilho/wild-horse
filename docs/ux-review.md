# Wild Horse IDE — UX review

*Reviewed 2026-10-22 against the `wh-ide` build of that evening, on copies of `examples/signup`, `examples/todo` and `examples/pong` with
`WH_IDE_MOCK=1`. Screenshots are in `ux-review/shots/` (`target/` is git-ignored, so they are local evidence for this
review, not committed assets).*

## Summary

Wild Horse already does the hard part. One `.wh.md` buffer drives four projections, and all four are live and editable. A graph
gesture rewrites the text in place, flashes it, and `⌘Z` undoes it. A selection in any view moves every other view. No
other tool we looked at offers both edit directions over one literate source; Enso is the closest (see [R-EN]).

The UX does not yet show that power, mostly because of four problems:

1. **The graphs can't be read at their default view.** *Fit* stops at 25% zoom, aligns to the top and ignores section boxes.
   At the zoom where all of signup fits, node text renders at about 3–5 px, and section boxes and labels overlap (§3).
2. **AI and verification status is almost invisible.** A failed synthesis shows as a small white `×` that looks like a close
   button. The node badge disagreed with the queue in our run. The document has no gutter badges, and contracts and examples
   show no pass/fail anywhere (§6). For an AI-first tool this is the main trust gap.
3. **The graph can quietly misstate the program.** A step that calls an unknown function disappears from the flow graph and from
   the Rust preview instead of showing up as an error (§3, §8). Errors are hard to navigate: there is no Problems list, the
   error count can't be clicked, and diagnostic rows in the log don't jump to the source (§8).
4. **The three views don't yet feel like one object.** A def and each of its call sites are separate nodes. `[[refs]]` and
   Markdown are shown as raw text. The inspector's header names the flow, not the selected step. Selecting a relationship in
   the data model doesn't update the inspector (§2, §4).

The proposals below are grouped by area. Each finding has a severity (P0 blocks or misleads, P1 major friction, P2 moderate,
P3 polish), the reference pattern it borrows, and a rough effort (S ≤ 1 day, M 2–5 days, L 1–2 weeks, XL > 2 weeks). §16 maps
them to crates and files, ordered as quick wins, next and later. Work already under way in the `ide-menus` session (menus,
file watching, layout persistence, `NodeResult` test results, author review mode, `MoveFlowStep`) is marked **in flight**. The
proposals here refine its UX rather than repeat it.

---

## Contents
1. [Method and caveats](#1-method-and-caveats)
2. [Current state, with screenshots](#2-current-state-with-screenshots)
3. [North star: one object, three faces](#3-north-star-one-object-three-faces)
4. Findings: [4 Layout & navigation](#4-overall-layout--navigation) ·
   [5 Document / block editor](#5-document--block-editor) · [6 Flow graph](#6-flow-graph) ·
   [7 Data model](#7-data-model) · [8 Inspector](#8-inspector) · [9 Agent & synthesis, review and trust](#9-agent--synthesis-ux-reviewing-ai-edits-and-trust) ·
   [10 Live evaluation, trace & debugging](#10-live-evaluation-trace--debugging) · [11 Feedback & status](#11-feedback--status) ·
   [12 Keyboard & command palette](#12-keyboard--command-palette) · [13 Visual design system](#13-visual-design-system) ·
   [14 Accessibility](#14-accessibility) · [14b Performance perception](#14b-performance-perception) ·
   [14c Onboarding & empty states](#14c-onboarding--empty-states)
5. [15 UI style: critique, direction and style guide](#15-ui-style-critique-direction-and-style-guide)
6. [16 Prioritized roadmap](#16-prioritized-roadmap)
7. [17 Sources](#17-sources)

---

## 1. Method and caveats

- **Setup.** `cargo build -p wh-ide`. The binary was wrapped in a throw-away `.app` (`ux-review/WHReview.app`) so
  computer use could drive it in the background. It was run on `ux-review/projects/{signup,todo,pong}` with
  `WH_IDE_MOCK=1`, so no agents ran. `examples/` was not touched.
- **Tasks performed:** read a def and its contract; select across views; pan and zoom; drag a step's output onto a def to
  insert a step; undo; change `User.email` from 1 to 0..* and undo it; type an error (`validate` → `validatexx`) and try to
  navigate to it; Check; Run app (mock trace); author chat (mock); Synthesize (mock, ends *failed*); dock the Data model into
  the right tab group; switch the flow scope; open all three projects.
- **Limits.** In the background the GPUI window repaints on the *next* input event, so we nudged the status bar after each
  action. That artifact doesn't affect users and isn't counted as a finding. Shifted characters can't be typed in the
  background (as `NOTES.md` already says), and the mock backend has no real agent, tests or values. The agent and trace
  findings are therefore partly based on the code (`backend.rs`, `side_panels.rs`, `state.rs`) and the design doc.
- **Code facts checked while reviewing:**
  - `canvas.rs` `fit()` clamps zoom to `0.25..1.2` and top-aligns the content.
  - Cardinality labels are hidden below zoom 0.45, edge labels below 0.6, and node badges below 0.5.
  - Every port dot uses the same `accent` colour.
  - The synthesis status in a node header is the first word of the status label, in plain text.
  - `main.rs` binds only `⌘Z`, `⇧⌘Z`, `⌘S` and `⌘Q`, and forces `ThemeMode::Dark`.
  - Prose is rendered by a hand-written line parser (`document.rs` `styled_inline`) that only tints `[[refs]]` and `` `code` ``.

## 2. Current state, with screenshots

| | |
|---|---|
| ![overview](../ux-review/shots/01-overview-signup.jpg) **01** Default layout on signup: Document, Flow graph over Data model, Inspector/Agent, Log, status bar. Both graphs are fitted, and their text is too small to read. | ![flow closeup](../ux-review/shots/02-flow-default-zoomed.jpg) **02** The flow graph at default fit, magnified. The labels "Validation and normalization" and "Registering" overlap. Step nodes and def nodes repeat each other. The `Db` capability is cut off on the right. |
| ![inspector](../ux-review/shots/04-def-selected-inspector.jpg) **04** `normalize_email` selected. The document scrolls and highlights it, and the inspector shows its signature, clauses (each with a `✓`/`×`), pattern, prose and Rust preview. Clause text is cut off at the right edge. | ![prose](../ux-review/shots/05-prose-refs-unrendered.jpg) **05** Prose rendering: `` `example.org` `` keeps its backticks, and `[[ValidationError]]` is unstyled text with brackets and can't be clicked. |
| ![nodes](../ux-review/shots/06-flow-nodes-closeup.jpg) **06** Node detail. The `hole` badge overlaps the output port, the signature in the header is cut off, a moved node (`normalize_email`) has left its section box, and every port dot is the same colour. | ![insert](../ux-review/shots/07-step-inserted-selected.jpg) **07** After dragging a step's output onto `normalize_form`: the step is inserted (`|> normalize_form` twice), the tab is marked dirty, a contextual toolbar appears, and the inspector shows *FLOW STEP*. The whole graph was laid out again. |
| ![undo](../ux-review/shots/08-undo-flash.jpg) **08** `⌘Z` restores the flow and flashes the block. The status bar still shows the deleted step's id. | ![cardinality](../ux-review/shots/09-datamodel-field-selected.jpg) **09** Data model, `User.email` selected. The `1 / 0..1 / 0..* / set` buttons don't show which one is current, and cardinality labels are hidden at this zoom. |
| ![error](../ux-review/shots/10-error-unknown-step-vanishes.jpg) **10** After typing `validatexx`, the error shows under the block and the status bar says `1 error(s)`. The step *disappears* from the graph, and the Rust preview silently drops it. | ![log](../ux-review/shots/11-log-after-check.jpg) **11** After *Check*: the results are appended to the earlier ones, the same lint is WARN at start-up and INFO here, and clicking the error row does nothing. |
| ![trace](../ux-review/shots/12-run-trace.jpg) **12** Run app (mock): executed steps get green outlines, the trace tab lists node-mapped events with durations, and the inspector shows recent events. No values are shown. | ![agent](../ux-review/shots/13-agent-chat-mock.jpg) **13** Agent tab: synthesis queue and author chat. A plain chat, with no edit history or diffs (review mode is in flight). |
| ![synth](../ux-review/shots/14-synth-failed-badge.jpg) **14** Synthesize, then *failed*. The only sign on the node is a white `×` in the header, which reads as "close". Green outlines (trace) would clash with a green "accepted" colour. | ![dock](../ux-review/shots/15-panel-docked-right.jpg) **15** Docking works: Data model dragged into the right tab group. The flow graph doesn't fit itself to the larger area. |
| ![todo](../ux-review/shots/16-todo-overview.jpg) **16** todo: `*is*` and `*which*` are shown with their asterisks, the front matter appears as raw `---` lines, and section boxes overlap. | ![match](../ux-review/shots/19-todo-match-unlabeled.jpg) **19** todo `execute`: the `match` fans out to five arms with **no case labels**. The input reads `in0 ?`, and `commit` appears four times. |
| ![pong](../ux-review/shots/20-pong-overview.jpg) **20** pong: both graphs are unreadable at default fit, and the data model's section labels are covered by nodes. | ![cluster labels](../ux-review/shots/17-todo-cluster-label-overlap.jpg) **17** Section labels covered by nodes ("Storage", "Operations", "Removing", "Showing"). |

**What already works well (keep):**
- Bidirectional editing, with flash highlighting and global undo across every origin.
- Selection sync, including moving the cursor in code to select the step under it.
- Contextual toolbars that change with the selection.
- Type-filtered "insert after" suggestions.
- Greyed ghosts for blocks that fail to parse.
- The Rust preview in the inspector.
- Executed-path outlines in the graph.
- Ops recorded in the log ("insert step normalize_form", "undo …").
- Dockable panels.
- Hint lines that teach the gestures.

## 3. North star: one object, three faces

The product promise is that **prose, a code block and a graph node are the same thing**. Today each view renders a node its own
way. Give every model node one shared *identity capsule* and render it the same way everywhere:

```
 kind glyph + colour   name            status chip           facts
 ┌──────────────────────────────────────────────────────────────────────────┐
 │ ƒ  normalize_email    ✓ accepted · 2/2 examples · 1 property · 3 ms      │   ← same capsule in:
 └──────────────────────────────────────────────────────────────────────────┘     doc block header, graph node
                                                                                  header, inspector title, palette
                                                                                  results, problems rows, trace rows
```

- **Document**: a block header replaces the bare `wh` label. It shows the capsules of the defs in the block, plus
  *Show in graph*, *Synthesize* and *Run specs*. Gutter badges sit on each def line.
- **Graph**: the node header *is* the capsule. Hovering shows the attached prose, the "intent", as a tooltip.
- **Inspector**: the capsule is the title. Prose is an editable field next to the contract, so the intent and the spec are
  edited together.
- **Hover sync** (new): hovering a node in any view outlines it in the others. In the same way, hovering an Unreal pin
  emphasizes its wires [R-UE-CN]. Selection sync already exists, and hover sync makes the three faces feel like one thing
  at little cost.
- **References are typed links with backlinks**: a Notion @-mention creates a backlink on its target [R-NO-MEN]. A
  `[[ref]]` in prose should do the same, so a def's capsule can say "mentioned in 3 paragraphs · called by 2 flows".

Every proposal below either reuses the capsule or adds a channel to it: status, tests, trace values or agent provenance.

---

## 4. Overall layout & navigation

**L1 — Five panels at once leave every projection cramped (P1, M).**
*Evidence:* screenshot 01. At 1600×994 the flow graph gets about 575×250 px, and the document column wraps code at about
45 characters.
*Reference:*
- Zed hides every toolbar element that isn't needed, and its command palette is "the main way to access actions" [R-ZD-CFG].
- VS Code has Zen Mode [R-VS-UI].
- Blender ships workspace tabs (Layout, Shading, Geometry Nodes…), each a preset arrangement of the same editors [R-BL-WS].
- Notion shows one dataset in several named views, each with its own layout and filters [R-NO-DB].
*Proposal:* **workspace presets** in the title bar. Each preset is a stored dock layout (this builds on the in-flight layout
persistence):

```
┌ Wild Horse — signup ──────────[ Write | Design | Debug ]──────── ● 1 error  ⋯ 2  ✓ 14 ┐
│ Write  : Document (wide) │ Inspector                       (graphs as tabs)            │
│ Design : Flow graph (wide) over Data model │ Document (narrow) │ Inspector              │
│ Debug  : Flow graph │ Run trace + values │ Inspector  ;  Problems at the bottom         │
└────────────────────────────────────────────────────────────────────────────────────────┘
```
Also add **maximize panel** (`⇧⎋` or double-click the tab, like VS Code's "Maximize Editor Group") and re-run Fit
whenever a canvas is resized (screenshot 15).

**L2 — There is no project navigator or outline (P1, M).**
*Evidence:* the only ways in are document tabs and finding things on the canvas. pong has 3 files and 30 nodes, and finding
`step` means scanning tiny text.
*Reference:* VS Code's Outline view and breadcrumbs [R-VS-BC]. Blender's Outliner.
*Proposal:* an **Outline** panel (left dock, tabbed with Document) with the tree *module › section › node*. Each row has a
kind glyph and a status chip, and the tree can be filtered by text or status ("show failing"). It doubles as the
accessible, keyboard-navigable alternative to the canvases (§14). Add **breadcrumbs** above the document
(`signup › Validation and normalization › normalize_email`), synced with the scroll position and the selection.

**L3 — Navigation has no history (P2, S).**
*Proposal:* add *Go Back* / *Go Forward* (`⌃-` / `⌃⇧-`, as in VS Code [R-VS-KB]) over the selection history kept in
`AppState`. Selecting a node 3 hops away and coming back is the most common move in a graph IDE.

**L4 — The Inspector and Agent share one tab group (P2, S).**
*Evidence:* you can't see the synthesis queue while inspecting a node (screenshot 13).
*Proposal:* move the per-node agent transcript and synthesis status *into* the inspector, under "Implementation", as the
design doc intends. Keep the Agent panel for the project-wide chat and queue, and put it in its own dock slot in the Debug
and Design presets.

**L5 — Selection is not shown in human terms (P3, S).**
*Evidence:* the status bar shows `signup.register#normalize_form[1]` and keeps showing it after the step is undone
(screenshot 08).
*Proposal:* show the capsule ("step 2 of register · normalize_form") and clear or re-resolve it when the id disappears.

## 5. Document / block editor

**D1 — Markdown is only partly rendered (P1, S–M).**
*Evidence:* screenshots 05 and 16. Inline code keeps its backticks, `*emphasis*` is literal, a heading shows
`` The `todo` command `` with backticks, lists are joined into one paragraph, and a line wrap can split `[` from `[ref]]`.
The front matter appears as raw `---` lines.
*Reference:* Jupyter Markdown cells are fully rendered next to code cells [R-JU-NB]. Notion blocks render inline and
Markdown shortcuts create blocks as you type [R-NO-BL].
*Proposal:* render with `pulldown-cmark` (already a common dependency) into styled runs: emphasis, strong, code (mono, no
backticks), links, lists, block quotes and tables. Render the front matter as a row of **page-property pills**
(`module: signup`), like Notion page properties. Keep the existing "click to edit source" behaviour.

**D2 — `[[refs]]` are neither chips nor links (P1, S).**
*Evidence:* screenshot 05.
*Reference:* Notion @-mentions and backlinks [R-NO-MEN]. VS Code's peek definition (`⌥F12`) [R-VS-NAV].
*Proposal:* render each ref as a capsule chip (kind glyph + name + status dot). Click selects the node everywhere, and hover
shows a **peek card** with the signature, the first prose line and the status. An unresolved ref is a red dashed chip with
"Create def…". Typing `[[` opens a node autocomplete. Each def also gets a **"Referenced by"** footer listing prose
mentions and flow steps that call it. These are backlinks, and they make the document navigable as a hypertext.

**D3 — Lint bars dominate the page (P1, S).**
*Evidence:* screenshots 01 and 04. Almost every block ends in one or more yellow "has no prose or docstring" bars. On
signup, 15 of 15 start-up log entries are this lint.
*Reference:* VS Code shows problems as squiggles plus counts in the status bar, collected in the Problems panel
[R-VS-ERR]. CodeLens and inlay hints carry low-priority facts without interrupting the text [R-VS-ERR].
*Proposal:* show "missing prose" as a dim **gutter dot** with the action *"Draft prose with the author agent"* (a one-click
author request scoped to that node), and count these lints separately from real warnings. Keep the inline bars for real
errors only, and make each bar clickable to place the caret.

**D4 — Code blocks have no chrome or actions (P1, M).**
*Evidence:* a block's header is the word `wh`.
*Reference:*
- Jupyter cells with an execution count and outputs [R-JU-FMT].
- Notion's `⋮⋮` drag handle and its *Turn into / Duplicate / Ask AI* menu [R-NO-BL].
- VS Code CodeLens above symbols ("N references", clickable) and testing gutter icons [R-VS-ERR][R-VS-TEST].
*Proposal:* add a **block header** with the def capsules (§3), *Synthesize*, *Run specs* and *Show in graph*, plus a
hover-only `⋮⋮` handle that drags blocks between sections (a structured move op) and opens a block menu: duplicate, move
to section, convert to a section, ask the agent. Put gutter status badges on each def line, as `design.md` specifies
(✓ ✗ ◌ ⋯), and they are not implemented yet.

**D5 — Revealing a selection can land it at the bottom edge (P2, S).**
*Evidence:* selecting `normalize_form` in the graph scrolled the document so its signature sat on the last visible line.
*Proposal:* scroll so the def's first line sits at about ⅓ of the viewport height, and pulse the left gutter instead of
tinting the whole block.

**D6 — Edits outside the viewport give no sign (P1, S).**
*Evidence:* inserting a step or changing a cardinality from the graph flashed text that was off-screen. The only feedback was
a log line and the tab's `•`.
*Proposal:* when a transaction from another view or the agent lands outside the document viewport, show a pill **"↓ 1
change in `register` — jump"**. Also show a short toast ("Inserted step normalize_form · Undo"), as Figma and Notion do
for structural edits.

**D7 — Slash commands and templates for new definitions (P2, M).**
*Reference:* Notion's slash menu [R-NO-SL]. Blender's Shift+A add menu.
*Proposal:* typing `/` at the start of an empty prose line opens: `/def`, `/struct`, `/enum`, `/flow`, `/capability`,
`/example`, `/ask …`. Each inserts a skeleton block with a placeholder prose paragraph (as the design doc describes for graph
adds). This replaces the graph toolbar's free-form "new def: name(x: T) -> R" field as the main way to create things.

**D8 — Prose source mode may not exit on blur (P3, verify).**
Clicking away from an edited heading left the Markdown source box open (screenshot 10). Check what counts as blur (a click
on a non-focusable area).

## 6. Flow graph

**G1 — Default view is unreadable; Fit doesn't fit (P0, S).**
*Evidence:* screenshots 01, 02, 18 and 20.
- `fit()` clamps zoom to 0.25 or more, so wide graphs overflow to the right (signup's `Db`, todo, pong).
- It aligns content to the top, so zooming in with `+` (which zooms around the centre) pushes the content off the top-left.
- It ignores section boxes.
- At the resulting zoom, header text is about 3–5 px.

*Reference:* Figma's *Zoom to fit* (`⇧1`) and *Zoom to selection* (`⇧2`), with the zoom % always shown [R-FG-NAV].
Unreal's `Home` / `F` / `A` frame-selection and fit-graph keys [R-UE-CS].
*Proposal:* make Fit include section boxes, center on both axes, and drop the 0.25 floor. Add *zoom to selection* (`F` or
`⇧2`), plus `⇧1` for Fit. Add semantic zoom (G2) so a fitted big graph stays legible. Re-fit when the canvas is resized
and when the scope changes.

**G2 — Semantic zoom, level of detail (P0, M).**
*Reference:* Unreal comment-box titles scale with zoom so a zoomed-out graph still reads as labeled regions [R-UE-CM].
Blender can collapse nodes (`H`) and hide unused sockets (`Ctrl-H`) [R-BL-PARTS]. Map apps use the same idea.
*Proposal:* three levels.

```
 zoom ≥ 0.7   full nodes: header capsule + typed port rows + test chips
 0.35–0.7     compact: header capsule only (name drawn at a fixed ≥ 11 px), ports collapse to one in/out, edges kept
 < 0.35       overview: section boxes as titled tiles (title ≥ 14 px) + node dots coloured by status; edges bundled
```
Section titles must never be covered by nodes. Put titles in a header band that layout reserves (see G6).

**G3 — Step and def nodes repeat each other (P1, M).**
*Evidence:* screenshots 02, 06 and 14. Each pipeline step is a node, *and* its callee def is another node joined by a grey
"calls" edge that often loops backwards. On signup, half the nodes in the flow cluster are duplicates.
*Reference:* in Unreal, collapsed graphs and functions appear as one node; hovering previews it and double-clicking opens
the definition's own graph [R-UE-COL]. Blender node groups are "similar to functions in programming", with a Group
Input/Output interface and a breadcrumb for the nesting level [R-BL-NG][R-BL-PARTS].
*Proposal:* make `hide_calls` the default. A step node then carries its callee's capsule (status, tests, pattern), and
*Enter* or double-click opens the def: in the document, or as a **peek** sub-graph for flows. Defs not used by any flow in
scope go into a collapsible **"Library" rail** on the left of the canvas. You can drag them from there into a flow, which
is also the palette the design doc asks for.

**G4 — Branches have no labels; `match` projects as an opaque node (P1, M).**
*Evidence:* screenshot 19. `match` shows `in0 ?` and `out str`, and five arms fan out unlabeled. You can't tell which
command leads to `remove`.
*Reference:* Unreal's *Switch on Enum* flow-control node has one labeled exec output per enum value (a standard Blueprint
node, not re-checked against Epic's JS-rendered docs in this review [R-UE-NODES]). Blender's *Menu Switch* is the
equivalent.
*Proposal:* project `match` as a **switch node** with one labeled output per `case` (`Add(title)`, `Done(id)`, …). Arms are
laid out as horizontal lanes, and the bindings (`title`, `id`) are shown as pins. Shared tail steps (`commit` ×4) are
drawn once per lane, with a faint "same def" link on hover.

**G5 — Ports carry no type identity, and errors don't flow (P1, M).**
*Evidence:* every port dot is the same accent colour. Error types are written as `ValidForm ! ValidationError`, a notation
that isn't in the language (`raises`). How errors propagate, through `from` conversions into the flow's `raises`, is
invisible.
*Reference:*
- Unreal colour-codes pins by type and keeps white exec pins apart from data pins [R-UE-CN][R-UE-FND].
- Blender gives each socket "a shape and color that indicate the type of data it handles": circle (dynamic), rectangle
  (single), diamond (field), grid, list [R-BL-PARTS].
- Blender draws field links dashed, and an invalid link solid red [R-BL-FIELDS].
*Proposal:*
- **Colour by type family**: refined and newtype, struct, enum, collection, capability.
- **Shape by mode**: ● value, ◆ `own`, ◇ `mut`, ▤ collection or stream.
- **A separate red "raises" port** on the node's lower edge. Error edges run along a bottom "error rail" to the flow's
  `raises` output, labeled with the `from` conversion. This is railway-style error flow, and it makes the language's
  `raises` semantics visible.

**G6 — Layout: overlapping sections and a full relayout on every edit (P1, L).**
*Evidence:* screenshots 02, 17 and 20.
- Section boxes overlap and nest wrongly.
- Section labels are covered by nodes.
- A node dragged out of its section leaves the box behind (06).
- Inserting one step re-laid out the whole graph, so every node moved (07).

*Reference:*
- Moving a Blender frame moves its children, a frame can *Shrink* to its contents, and `F` while dragging attaches or
  detaches a node [R-BL-FR].
- Unreal comment boxes have a "Group Movement" mode [R-UE-CM].
- Preserving the user's mental map is the standard goal for incremental layout (Misue, Eades, Lai & Sugiyama, "Layout
  Adjustment and the Mental Map", *J. Visual Languages & Computing*, 1995).
*Proposal:*
- Make `layout.rs` cluster-aware: lay out per section and then pack the sections, each with a reserved title band.
- Make placement incremental: existing nodes keep their positions, and a new step is placed between its neighbours with a
  local shift.
- Animate position changes for about 150 ms.
- Section boxes are computed from their member nodes, so they always contain them.
- Dragging a node into another section is an explicit structured op ("move def to section …"). Otherwise it snaps back.

**G7 — Interaction gaps (P2, M).**
*Evidence:* code facts and NOTES.
- No Delete, F2 or arrow-key handling on the canvas.
- Double-click only focuses the rename field.
- Rename and Delete live in a toolbar far from the node.
- Dropping a link onto empty canvas does nothing.

*Reference:*
- Blender **link-drag-search**: drop a link in empty space and a search lists only the compatible nodes and sockets.
  Dropping a node *on a link* inserts it [R-BL-NE][R-BL-EDIT].
- Unreal does the same when you release a wire in empty space [R-UE-CN]. While dragging, compatible pins show a green
  check and incompatible ones show *the reason* [R-UE-CN].
- Unreal reroute nodes and comment boxes [R-UE-FND][R-UE-CM].
- Node-RED's click-port-then-click-target wiring [R-NR-WIRE] is also what WCAG 2.5.7 asks for: an alternative to dragging
  [R-WCAG-257].

*Proposal:*
- Dropping a link on empty canvas opens a **type-filtered searcher** at the cursor (it reuses `assignable`) and inserts
  the chosen def as the next step.
- While dragging, incompatible targets show *why* ("expects `Email`, got `ValidForm`") instead of only greying out.
- Dropping a def onto an existing edge inserts it there.
- **Click-to-connect** (click an output, then a target) as the non-drag alternative.
- Inline rename on double-click, a floating mini-toolbar next to the selection, and keyboard nav (§12).
- `MoveFlowStep` (in flight) as drag-to-reorder along the lane.
- Later: reroute dots and free comment boxes stored in `.wh/layout.json`, so commentary that isn't part of the program
  doesn't pollute the document.

**G8 — An unresolved step disappears (P0, S–M).**
*Evidence:* screenshot 10. After `|> validatexx` the graph silently dropped the step and re-laid out, and the Rust preview
lost the `validate` line.
*Reference:* Blender shows a warning icon in the node title, with the message on hover [R-BL-INSP]. Node-RED marks a
misconfigured node with a red triangle [R-NR-ST]. Blender draws an invalid link solid red [R-BL-FIELDS].
*Proposal:* keep the step as a red **error node** that shows the diagnostic ("unknown function `validatexx`"). Offer
*quick fixes*: the closest names by edit distance ("did you mean `validate`?"), *Create def…*, and *Ask agent*. The
inspector and the Rust preview show the step struck through with the error.

**G9 — Edge readability (P2, M).**
Edges are told apart by colour alone. The grey "calls" edges cross nodes and loop backwards, and edges to `Email` all pile
up on one port.
*Proposal:* dash patterns per edge kind (data solid, uses dashed, calls dotted, implements hollow arrow). Route edges
orthogonally or with port-aware splines, keeping them out of node boxes. Bundle edges into a shared target. Add a legend
popover from the toolbar.

**G10 — Minimap (P3, S–M).**
*Reference:* VS Code's minimap is a draggable overview with region markers [R-VS-UI]. Zed's is off by default and shows
its thumb on hover [R-ZD-CFG]. At pong's size a canvas minimap pays off.
*Proposal:* a minimap in the bottom-right of the canvas, toggled with a button, showing the viewport rectangle and status
dots.

## 7. Data model

**M1 — Cardinality editing doesn't show the current value (P1, S).**
*Evidence:* screenshot 09. `1 / 0..1 / 0..* / set` render as four identical buttons (exposed as AX checkboxes with no
checked state).
*Proposal:* a **segmented control** with the current value selected, plus a short preview of the text it will write
("`email: list[Email]`") on hover.

**M2 — Cardinality is invisible at normal zoom (P1, S).**
Labels are hidden below zoom 0.45, which is the default for every example.
*Reference:* crow's-foot ER notation draws cardinality as end glyphs, so it stays legible when small.
*Proposal:* draw **crow's-foot glyphs** at the edge ends (│ one, ○│ zero-or-one, ○< zero-or-many, │< one-or-many, `N` for
arrays, a key symbol for `dict[K, V]`), sized in screen pixels. Clicking the glyph cycles the value, with the segmented
control as the precise editor.

**M3 — Selecting a relationship doesn't sync (P2, S).**
Selecting `User.email` changed only the data-model toolbar. The inspector and document stayed on the previous node.
*Proposal:* treat a field or relationship as a selectable sub-node (`User.email`). The document highlights the field line,
and the inspector shows the field form: name, type, cardinality, `key` and `sensitive` toggles, default value, and the
invariants that mention it.

**M4 — No focus, search or filters (P2, M).**
*Evidence:* in pong the whole model is fitted into about 200 px (screenshot 20).
*Reference:* design.md §IDE 3 already lists focus mode (N hops), filters and search. Unreal's *Find in Blueprints*
searches nodes, pins and values with a typed filter syntax, in one asset or the whole project [R-UE-FIND].
*Proposal:* a filter field in the toolbar, kind toggles (structs, enums, errors, refined types), and **focus mode**:
select a type and press `F` to dim everything more than N hops away. "Producers/consumers" stays an overlay toggle that
links into the flow graph.

**M5 — Section labels covered, edges under nodes (P2, M).**
These are the same layout problems as G6, and they need the same fix in `layout.rs`.

## 8. Inspector

**I1 — `✓` means "apply" here, and "passed" everywhere else (P1, S).**
*Evidence:* screenshot 04. Each clause row ends in `✓ ×`. In a verification tool, `✓` beside `example …` reads as "this
example passes".
*Proposal:* commit on Enter or blur and show a "modified" dot. Use `×` → a trash icon with a tooltip. Use the right-hand
slot for **test status** instead: ✓ passed, ✗ failed (with counterexample), ◌ not run. This is where `NodeResult` (in
flight) belongs.

```
 CONTRACT & SPECS                                         last run 12 s ago  ▶ Run specs
 ensures  result == e.lower()                                         ✓ held in 214 cases
 example  normalize_email("A@B.com") == "a@b.com"                     ✓
 property idempotent(x: Email):                                       ✗ counterexample ▸
          normalize_email(normalize_email(x)) == normalize_email(x)        x = "Ä@b.c"  → shrunk
 + add clause…   (ensures | requires | example | property | raises when | note | budget)
```

**I2 — Clauses are cut-off single-line inputs (P1, M).**
*Evidence:* `example normalize_email("A@B.com") == "a@` is truncated. The multi-line `property` is squashed into one line
with odd spacing.
*Proposal:* each clause is an auto-growing **code editor** with the `wh` highlighter and completion, and the kind is a
dropdown chip. "Add clause" offers a kind picker with a skeleton, instead of a free-text field.

**I3 — The signature is a raw string (P2, M).**
*Proposal:* structured **parameter rows** (name, mode `own`/`mut`/borrow, type with completion), a return type, `raises`
chips and `uses` capability chips. Each edit emits `EditSignature`. Keep a "text" toggle for power users.

**I4 — The header shows the flow, not the selected step (P2, S).**
*Evidence:* screenshot 07. The title reads "flow register" while a step is selected.
*Proposal:* when a step is selected, the title is the **step capsule** ("step 2 of `register` · normalize_form"), with
callee facts and *Open def*. The flow is a breadcrumb above it.

**I5 — Missing sections from the design doc (P2, M).**
Add the impl source with its key and staleness, *Pin* / *Unpin*, *Re-synthesize*, and the per-node agent transcript. These
are already listed in `design.md` §IDE 4. Make "Generated Rust" a collapsible section with *Open in editor*.

## 9. Agent & synthesis UX: reviewing AI edits and trust

The author agent edits the spec and the compiler agent writes code that nobody reads by default. People will only trust it if
three things are always visible and easy to read: **what the AI changed**, **whether the result is verified**, and **what it
is doing now**.

**A1 — Synthesis status vocabulary and placement (P0, S–M).**
*Evidence:* screenshot 14.
- *Failed* shows as a white `×` in the header, which looks like a close button.
- During our run the node said `queued` while the queue said `✕ no compiler agent`.
- Only the first word of the label is shown, with no colour.
- The document has no badges, and steps don't show their callee's status.

*Reference:*
- Node-RED's node status (an icon plus text under the node), with a blue dot for undeployed changes [R-NR-ST].
- Blender's warning icon in the node title, and its per-node timings overlay [R-BL-INSP].
- VS Code's testing gutter icons and inline failure messages [R-VS-TEST].
- Jupyter's per-cell execution count [R-JU-FMT].
*Proposal:* one status model, rendered the same way in the capsule, the document gutter, the outline, the queue and the
status bar.

| state | glyph | colour token | text |
|---|---|---|---|
| spec only / stale | ◌ | `muted` | "needs synthesis" / "stale: spec changed" |
| queued | ⋯ | `muted` | "queued (3rd)" |
| synthesizing | spinner | `info` | "writing · 12 s" |
| verifying | ◔ k/N | `info` | "verifying round 2/3 · examples" |
| accepted | ✓ | `success` | "verified · 5/5 specs" |
| failed / unsynthesized | ✗ | `danger` | short reason, e.g. "property idempotent fails" |
| pinned | 📌 | `muted` | "human-written, verified" |

Never use colour alone (§14). Clicking the chip opens the inspector's Implementation section. Use a **different hue for
"executed in last run"**, for example a blue outline, so that trace and verification don't share green. Every node change
must re-render from `AppState.synthesis` (the stale `queued` badge suggests the canvas `status` map is refreshed on another
path than the queue).

**A2 — Reviewing author edits (P1, L; builds on in-flight review mode).**
*Reference:*
- Zed's agent *Review Changes* multibuffer, where "you can accept or reject each individual change hunk, or the whole set"
  [R-ZD-AG].
- VS Code's *pending* agent edits: a marker on tabs, Up/Down to step between edits, Keep/Undo per change, and auto-advance
  to the next file [R-VS-AIREV].
- VS Code's Refactor Preview, where individual changes can be unticked before Accept [R-VS-REF].
- Checkpoints that restore the pre-turn state (VS Code, Claude Code `/rewind`, Cursor), each honest about what they don't
  track [R-VS-CKPT][R-CC-CKPT][R-CU].

*Proposal:* when an author turn finishes, the Agent panel shows a **change set card**:

```
 ┌ Author · "add an age check step to register" · 3 changes ─────────── Accept all  Reject all ┐
 │ signup.wh.md › Registering › register     + |> check_age          [view] ✓ accept  ✗ reject │
 │ signup.wh.md › Validation › check_age     + def check_age(…)      [view] ✓        ✗        │
 │ signup.wh.md › Validation (prose)         ~ 2 lines               [view] ✓        ✗        │
 │ Effects: 1 new hole (check_age ◌) · 0 errors · flow register changed                         │
 └──────────────────────────────────────────────────────────────────────────────────────────────┘
```
- *View* scrolls the document to an inline diff of the block: removed lines struck through in red, added lines in green.
- The graph shows a **diff overlay**: added nodes and edges green-outlined, removed ones as red ghosts, changed nodes amber.
- *Accept all* commits one undo step. In live mode the same card appears *after* the edit was applied, with *Revert*.
- Lines written by the agent keep a thin **provenance bar** in the gutter until the user touches them or presses *Mark
  reviewed*. The status bar counts "3 unreviewed AI edits", and `⌥↓` / `⌥↑` step through them.
- **Checkpoint per author turn**: "Restore spec to before this turn" on each message. It is cheap here, because every edit
  is already a `wh-doc` transaction.

**A3 — The author chat shows no activity, scope or cost (P2, M).**
*Proposal:*
- Add **context chips** above the input (selection `register` ×, file `signup.wh.md` ×, "+ add node"), so the scope is
  explicit and can be removed.
- Show collapsible **activity rows** for tool calls ("read signup.wh.md", "applied 2 ops", "ran wh_check: 0 errors"),
  as Zed's agent panel does [R-ZD-AG].
- Show a token/budget meter (the design doc has a per-session budget).
- **Follow the agent** (Zed [R-ZD-AG], Figma's follow mode [R-FG-FOL]): an opt-out toggle. While it is on, the canvas and
  document pan to the node the author is editing or the compiler is synthesizing, under a coloured border and a "Following
  author · Stop" banner.
- Offer canned **node actions** from the capsule's context menu: *Suggest properties*, *Write prose*, *Review contracts*,
  *Split this def*, *Explain failure*.

**A4 — Inline assist in a code block (P2, M).**
*Reference:* Zed's inline assistant (`ctrl-enter`), which with several cursors sends one prompt to all of them [R-ZD-IA].
For Wild Horse that means one prompt applied to several selected nodes.
*Proposal:* `⌘K` inside a `wh` block or on a selected node opens a one-line prompt scoped to that node. The result shows as
an inline diff to accept (`⌘↩`) or reject (`⎋`).

**A5 — Failures need a recovery path (P1, M).**
*Proposal:* a failed hole's card shows:
- which gate failed (check, example, contract, property, clippy), with the counterexample or compiler message;
- the number of rounds used;
- the actions *Retry*, *Retry with note…* (adds a `note` clause), *Open hole file*, *Pin manual impl*, and
  *Ask author to revise spec*.

This is how the human-AI loop closes. Without it, ✗ is a dead end.

**A6 — Permission prompts and agent errors belong in the UI (P2, S).**
The design doc puts permission prompts in the bottom panel. Render them as blocking cards with the exact path or command
and *Allow once* / *Deny*. Never put them only in the log.

## 10. Live evaluation, trace & debugging

**T1 — The trace shows *that* something ran, never *what* it computed (P1, L).**
*Evidence:* screenshot 12 has messages and durations only.
*Reference:*
- Unreal *Watch this value* on a pin, latest values on hover while paused, and an execution trace in the Blueprint
  Debugger [R-UE-DBG].
- Blender shows the last evaluated value when you hover a socket, with an explicit "no data" state. Its *Viewer node*
  (`Shift-Ctrl-click`) plus the Spreadsheet editor give a pinned table [R-BL-INSP][R-BL-VIEW][R-BL-SS].
- Quokka shows "runtime values … next to your code as you type" [R-QK].
- Enso's dual text/visual model shows node visualizations [R-EN] (partly verified).
- Bret Victor's "see the state" and "follow the flow" [R-BV].

*Proposal:*
- **Data tips on wires**: codegen records each step's input and output as `Debug` text, redacted for `sensitive`, in the
  trace span fields. The IDE draws a small value chip at the middle of each edge (`ValidForm{email: "a@b.io", age: 30}`).
  Hovering expands it.
- **Pin a viewer** on a node to keep its last value visible.
- In the document, the last value goes in the margin of each `|>` line (Quokka style).
- In the inspector, every `example` shows its actual value on failure.

**T2 — Trace navigation (P2, M).**
*Proposal:*
- Group the trace by **run** and then by entry flow.
- Use human labels (capsule) instead of raw ids, and keep the id in a tooltip.
- A **step-through scrubber** (← →) moves a "current event" marker along the graph, highlighting the active node and its
  in/out values.
- Show a per-node duration heat tint, summed per section box as Blender's timings overlay sums per frame
  [R-BL-INSP], and show the error event's node in red.
- Borrow the Node-RED debug sidebar's hygiene: filter by selected node or current flow, a bounded buffer, and *pause* with a
  count of dropped events [R-NR-DBG].
- This becomes the M2 timeline (Bret Victor's "follow the flow" slider [R-BV]).

**T3 — Inline evaluation of examples (P3, XL; "later milestone" in the design).**
Hot-reloading a dylib to evaluate `example` lines on every keystroke is the Quokka/Light Table end-state. Until then, show the
last `cargo test` result per example (I1).

**T4 — Breakpoints and watches (P3, L).**
Blueprint-style breakpoints on steps (pause in the generated wrapper via an IDE channel) and watches. Do this later, once
values exist (T1).

## 11. Feedback & status

**S1 — No Problems view; errors can't be navigated (P1, S–M).**
*Evidence:* screenshots 10 and 11. The error count can't be clicked, diagnostic rows in the log have no node and don't
navigate, *Check* appends a second copy of every result, and a lint is WARN at start-up but INFO after *Check*.
*Reference:* VS Code's Problems panel (`⇧⌘M`), *Go to Next Problem* with an inline zone that shows the message and code
actions, and the error and warning counts in the status bar [R-VS-ERR][R-VS-TIPS]. The tips page binds next problem to
`F8`, the editing page to `⌥F8`. Zed renders project diagnostics as an editable multibuffer [R-ZD-MB].
*Proposal:*
- A **Problems** tab next to Log that *replaces* its content on each check. Group it by file › node, with severity filters
  (errors, warnings, lints, specs failing).
- Clicking a row selects the node and places the caret.
- `F8` cycles through problems.
- The status bar error and warning counts open it.
- The log becomes an event history (ops, runs, agent), with wall-clock times instead of "seconds since start".
- Diagnostics need a node id. Map spans to nodes in `wh-model` (also requested in NOTES).

**S2 — The status bar is passive (P2, S).**
*Proposal:* turn it into clickable segments:

```
 signup ● unsaved │ ⨯ 1  ⚠ 3  ⓘ 15 │ ◌ 4  ⋯ 1  ✓ 14  ✗ 1 │ checking… │ author: connected │ step 2 of register · normalize_form
```
Each segment opens its panel: Problems, the queue filtered by state, the agent. *checking…* shows the debounced live pipeline
at work, so the user knows results are pending rather than final.

**S3 — Toasts for structural edits with Undo (P2, S).**
See D6. This also covers *Delete* and *Remove field*, which are one-click red buttons without confirmation. That's fine
given global undo, but only if the undo is advertised at that moment.

**S4 — Log hygiene (P3, S).**
- Fixed-width source column (it is misaligned in screenshot 11).
- Level filter.
- Copy row.
- Collapse repeated rows ("× 3").

## 12. Keyboard & command palette

**K1 — Command palette and quick open (P1, M).**
*Evidence:* only four bindings exist, and there are no menus yet (menus are in flight).
*Reference:* VS Code's Command Palette (`⇧⌘P`) and Quick Open (`⌘P`), which switches mode by prefix: `>` for commands,
`@` for symbols in the file, `#` for workspace symbols, `?` for help [R-VS-UI][R-VS-TIPS]. Zed's command palette [R-ZD-CP].
*Proposal:* one action registry feeds the palette, the menus (in flight) and the keymap.
- `⇧⌘P`: every command with its shortcut, fuzzy-searched. Context commands (Synthesize selected, Run specs, Remove step)
  come first when a node is selected.
- `⌘P`: **go to node** across the project, with capsules in the results; `@` restricts to the current module and `#` to
  sections.
- `⌘.`: quick actions on the selection (the same list as the node context menu).

**K2 — Canvas keyboard model (P1, M).**
*Reference:*
- React Flow's documented graph keyboard model: Tab through nodes and edges, Enter/Space select, Esc clear, arrows move,
  auto-pan to focus, and announced moves [R-RF].
- Blender editing keys (`Shift-X` delete with reconnect, `J` join, `F` frame) [R-BL-EDIT].
- Figma Space-drag pan [R-FG-NAV].

| key | action |
|---|---|
| ← → ↑ ↓ | move selection to the neighbour along edges / within the lane |
| ⏎ / ⌥⏎ | open in document / peek def |
| Tab | insert step after (opens the type-filtered searcher, G7) |
| ⌫ / ⇧⌫ | remove step / remove *and reconnect* neighbours (Blender `Shift-X`), with Undo toast |
| F2 | inline rename |
| F | zoom to selection; `⇧1` fit |
| ⌘D | duplicate; `⌘G` group into section |
| Space-drag | pan (Figma [R-FG-NAV]) |
| ⎋ | clear selection / cancel drag |

**K3 — Global keys (P2, S).**
- `⌘1…⌘5`: focus Document, Flow, Data, Inspector, Agent.
- `⌘\`: toggle the side docks.
- `F8`: next problem.
- `⌃-` / `⌃⇧-`: back and forward.
- `⌘K`: inline assist.
- `?`: a **shortcut cheat sheet** overlay.

Add a **keybinding editor** later. VS Code's searchable table is the model [R-VS-KB].

## 13. Visual design system

*§15 turns V1–V6 into a concrete style guide, with a loadable gpui-kit theme file and mockups.*

**V1 — Formalize tokens (P2, M).** The code uses gpui-component's default dark theme plus ad-hoc colours per node kind
(`canvas.rs` `kind_color`, `edge_color`). Define a `wh` token set and use it in all views:
- **kind**: def, flow, step, struct, enum, error, refined, alias, capability, adapter, app;
- **status**: success, danger, warning, info, muted;
- **provenance**: agent, graph, inspector;
- **edge**: data, uses, calls, implements;
- **type families**: G5.

Put the tokens in one module (`crates/wh-ide/src/tokens.rs`) so the capsule, canvas, document and inspector can't drift
apart.

**V2 — Typography scale (P2, S).**
- UI 13 px, small 12 px, mono 12.5 px, prose 15 px at a 1.55 line height. The prose already reads well.
- Canvas text never below 11 screen px; switch to the LOD (G2) instead of shrinking further.
- Headings in the document use a clear scale (24, 19, 16).
- Section titles on the canvas at 13–14 px, semibold.

**V3 — Emphasis and density in toolbars (P3, S).**
- "Run app", "Add def" and "Add struct" are filled white (primary) while the panels' main content is dim, so the eye
  lands on toolbar buttons first.
- Inline form fields in toolbars ("new def: name(x: T) -> R", "name", "type") read as forms.

Proposal: give primary emphasis only to the single most important action per context. Move creation into the palette,
`/` commands or the Library rail. Make toolbar inputs appear only after a "+ Add" click (a popover).

**V4 — Iconography (P3, S).** Replace text glyphs (`⋯`, `✓`, `×`, `•`) with Lucide icons, which gpui-kit already bundles.
Use the same glyph per node kind in every view. The icon table and the `icon_assets!` set-up are in §15.6.

**V5 — Light theme and system appearance (P2, S–M).** `main.rs` forces `ThemeMode::Dark`. Follow the system appearance by
default and offer a toggle. Graph colours need checking in both themes (G5 and V1 tokens). Provide a **high-contrast**
variant (VS Code ships high-contrast themes; not re-checked in this review).

**V6 — Flash and motion (P3, S).** The 1.6 s flash is good. Make it a gutter pulse plus a light tint (the current full-block
fill on undo is heavy, screenshot 08) and honour *Reduce motion*.

## 14. Accessibility

**X1 — The canvases are invisible to assistive technology (P1, L).**
*Evidence:* the accessibility tree of the window exposes the canvases as a bare `AXGroup`, with no nodes, ports or edges.
*Reference:* React Flow's accessibility model (focusable nodes and edges with labels, an `aria-live` region announcing
moves, default instruction text) [R-RF]. GPUI has no DOM, so Wild Horse needs its own focus model and AX tree.
*Proposal:*
- The **Outline** panel (L2) is the accessible equivalent of both graphs: a tree with node, kind and status, and
  edges as child rows ("output ValidForm → step check_age").
- The keyboard model (K2) operates the canvas.
- Expose canvas nodes as AX elements where GPUI allows it.
- Announce status changes ("normalize_form synthesis failed") through a live region or VoiceOver announcement.

**X2 — Colour is the only signal (P1, S–M).** Edge kinds, trace outlines and port types are told apart only by hue.
Fix with dash patterns (G9), glyph and shape status (A1) and port shapes (G5). See WCAG 2.2 SC 1.4.1 Use of Color
[R-WCAG-141]. Edges, port markers, cardinality glyphs, selection outlines and focus rings also need 3:1 against adjacent
colours (SC 1.4.11 [R-WCAG-1411]). The dim grey "calls" edges should be checked against this.

**X3 — Text size, not contrast, is the failure (P1, S).** The theme's `muted.foreground` (neutral-400, #a3a3a3) on
`background` (neutral-950, #0a0a0a) is about 7.8:1, which passes WCAG AA (4.5:1) [R-WCAG-143]. But canvas text scales
with zoom down to about 3 px, and badge text is `9 px × zoom`. Apply the minimum screen size (V2) and LOD (G2). Add
app-wide UI zoom (`⌘+` / `⌘−`).

**X4 — Every drag needs a click alternative; targets too small (P1, S–M).** Inserting a step, adding a field and retyping
a field currently work only by dragging (insert-after has a toolbar alternative, the others don't). SC 2.5.7 requires a
single-pointer alternative, and its own example is connecting two items by two clicks [R-WCAG-257]. Use click-to-connect
(G7) plus menu actions. `+`, `−` and the cardinality buttons are about 14×17 px. SC 2.5.8 asks for 24×24 px targets or
enough spacing [R-WCAG-258]. Use icon buttons of at least 24 px.

**X5 — Focus visibility (P2, S).** Make sure every toolbar button, chip and canvas node shows a focus ring when reached by
keyboard. Today the canvas cannot take focus at all.

## 14b. Performance perception

- **P-1 — The relayout jump (P1)** is the biggest perceived "glitch" (G6). Incremental placement plus a 150 ms tween makes
  edits feel continuous.
- **P-2 — Show pipeline progress (P2, S)**: show *checking…* and *synthesizing n* in the status bar (S2), and give the
  queue elapsed timers. A debounced pipeline without visible state reads as "nothing happened".
- **P-3 — Optimistic graph edits (P3)**: draw the inserted step in place on drop, before the reparse finishes, then
  reconcile. This only matters once projects get big.
- **P-4 — Keep GPUI's frame budget**: at 120 Hz that's 8.33 ms per frame [R-ZD-120].
  - Keep parsing, layout and trace ingestion off the main thread.
  - Virtualize long documents (pong's `view.wh.md`).
  - Cache edge geometry between frames when nothing moved.
  - When animating (G6), keep ProMotion's frame-rate drop in mind, which Zed works around [R-ZD-METAL].

## 14c. Onboarding & empty states

**O1 — Empty inspector (P2, S).** "Select a node in any view." is wasted space. With nothing selected, show the **project
overview**:
- the entry flow (click to open);
- status totals (◌ 4 need synthesis, ✗ 1 failing);
- problems;
- recent agent changes;
- a "next steps" checklist ("Write prose for 6 nodes", "Synthesize 4 holes", "Run app").

**O2 — A flow graph with no flows (P2, S).** In todo's `tasks.wh.md` the "Flow graph" shows only loose defs. Show "This
module has no flows. Its defs are used by `main.execute` → *Open*", next to the Library rail.

**O3 — Hint lines (P3, S).** The hint text under each canvas ("drag from a step's output…") is helpful the first time and
noise afterwards. Show it as an empty-state or coach-mark until the user has performed the gesture once (a flag in the
user config), then keep it in the `?` cheat sheet.

**O4 — First run (P2, M; welcome/recent projects in flight).** Offer to open a bundled example and run a 4-step coach-mark
tour:
1. "This is your program as a document."
2. "Same program as a flow, so drag to add a step."
3. "Click ✓/✗ to see why a node is (not) verified."
4. "Ask the author agent."

Plus a *New project* template that already contains one flow, one struct and one failing example, so every view is
non-empty.

---

## 15. UI style: critique, direction and style guide

§13 listed visual-system issues as UX findings. This part looks at the *look* itself. It proposes a direction and turns it into
a style guide that an implementation session can apply directly. Deliverables under `ux-review/style/`:

| file | what |
|---|---|
| `wild-horse-theme.json` | a **gpui-kit theme set** ("Wild Horse Dark" and "Wild Horse Light"): 84 colour keys, fonts, radii and syntax highlight styles. It was checked by parsing it with gpui-kit 0.8's own `ThemeSet` deserializer in a scratch crate; both themes load, including the `highlight.syntax` styles. |
| `tokens.rs` | a sketch of `crates/wh-ide/src/tokens.rs`: the Wild Horse roles that `ThemeColor` has no slot for (node kinds, type families, edges, status, provenance), plus drop-in replacements for `canvas::kind_color` and `edge_color`. It was not compiled. |
| `node-before-after.svg/.png` | the current node next to the proposed nodes (verified and failing states) |
| `graph-style-legend.svg/.png` | kinds, ports, edges, crow's-foot ends, status chips, frames |
| `document-before-after.svg/.png` | a prose and code block today vs proposed (ref chip, peek card, block header, gutter status, inline failure) |
| `flow-match-after.svg/.png` | todo's `execute` as a labeled switch with lanes inside a section frame (compare screenshot 19) |

### 15.1 Critique of the current look (from screenshots 01–21)

- **Colour.**
  - The chrome is gpui-kit's stock *Default Dark*: pure neutral greys (Tailwind `neutral-950…50`) with no hue.
  - Colour shows up only on the canvas, as ad-hoc HSL values in `canvas.rs` (`kind_color`, `edge_color`) and `document.rs`
    (syntax). Nothing ties them together.
  - Kind hues sit at 38–45% lightness on near-black. They read as muddy rather than vivid.
  - Green means two things: the `flow` kind (#378D35) and the "executed" trace outline. Red means both `@error enum` and
    danger.
- **Contrast** (measured against `#0A0A0A`):
  - Body text is fine. `muted.foreground` reaches 7.9:1.
  - The canvas fails in places. The small kind labels (`def`, `step`) are #3475B2, 4.1:1. The `@error` label is #B23434,
    3.2:1. Both are tiny text, so they need 4.5:1.
  - "calls" edges are 1.7:1 and producer/consumer edges 2.0:1, both under the 3:1 that SC 1.4.11 asks of meaningful
    graphics.
  - Borders (#262626) are 1.3:1. That's fine for dividers, but it means focus and selection can't rely on border colour.
- **Typography.**
  - Prose is the best part (system sans, about 15 px, generous line height).
  - Code is Menlo 13, which is fine, but it wraps at about 45 characters in the default layout.
  - UI labels mix 13 px buttons with uppercase 11 px section heads (`SIGNATURE`) in the inspector.
  - On the canvas everything scales with zoom down to about 3 px, and there is no hierarchy: header name, kind label and
    signature differ only by weight and grey level.
- **Density and spacing.**
  - Toolbars are dense rows of equal-weight buttons and inline inputs.
  - Panels have no padding rhythm. The document uses about 20 px, the inspector about 10 px, and the log has none.
  - Section boxes use no padding and overlap.
- **Borders and shadows.**
  - Every surface is #0A0A0A with a #262626 border, so panels, code blocks, nodes and canvas all look the same.
  - `shadow_sm` on nodes is invisible on black.
  - The selected node is a slightly lighter border, which is easy to miss.
- **Iconography.**
  - Almost none. State is text glyphs (`×`, `✓`, `•`, `⋯`) in the default font colour. The panel menus use gpui-kit's `⋯`.
  - The kind label is a word ("def", "step"), not a glyph.
- **Nodes and wires.**
  - Header tint at 18% plus a coloured header border.
  - Port dots are identical, and ports sit *inside* the node.
  - Bezier wires all have the same weight, and only colour separates their kinds.
  - The edge labels and badges fight the port dots (`hol●`).
- **Focus, selection and hover.**
  - No hover state on nodes or rows.
  - No keyboard focus ring on the canvas.
  - The executed outline (2 px green) is *stronger* than the selection outline, so the eye goes to the wrong thing.
- **Motion.** The 1.6 s flash is the only motion, and it's good. Relayout jumps instantly, with no tween (G6).
- **Dark/light parity.** None. `ThemeMode::Dark` is forced, and the hard-coded canvas HSL values were tuned only for dark.

### 15.2 Direction: "a quiet instrument with loud signals"

The product is a precision tool where three views of the same program sit next to each other, and an AI changes things you
then have to verify. The look should step back so that **meaning** (kind, type, status, provenance) carries all the colour.

1. **Restrained chrome** (Zed). Panels, tabs and toolbars stay neutral and low-contrast with no ornament. Toolbar items
   appear only when they apply [R-ZD-CFG]. There is one accent colour, used only for focus, selection, links and "running".
2. **Colour is semantic, with four separate jobs.** Each job has its own place and shape, so the hues never compete:
   - *kind*: a 3 px stripe, a glyph and a 10% header tint;
   - *type family*: port fills and wire colour;
   - *status*: chips and gutter glyphs;
   - *provenance*: a violet gutter bar.
3. **Node and socket conventions people already know** (Blender and Unreal).
   - Colour by type and shape by structure [R-BL-PARTS][R-UE-CN].
   - Ports on the node border.
   - Frames with title bands [R-BL-FR][R-UE-CM].
   - Dashed lines for indirect relations [R-BL-FIELDS].
   - Red for an invalid link.
4. **Quiet when good, loud when bad.** A verified node shows a small outline ✓. A failing node gets a red chip, red border
   and the reason. A project where everything passes looks calm, like VS Code, where squiggles appear only for problems.
5. **A reading column for prose** (Notion-like whitespace). The prose measure is at most 72 characters, with more space
   between blocks than inside them, headings as the main hierarchy, and code blocks as quiet inset panels.
6. **The same values in both themes.** Every role has a dark and a light value, checked for contrast. VS Code-style
   theming means everything goes through tokens, and nothing is hard-coded in views.

### 15.3 Colour tokens (dark / light), with WCAG contrast

All values were computed with the WCAG 2.x relative-luminance formula [R-WCAG-143][R-WCAG-1411]. Every *role* colour
(status, kind, type, provenance, accent) also passes **4.5:1 as text** on its surface, so the same token works for a glyph,
a label or a stroke. Only the two edge greys are graphics-only (≥ 3:1).

| token | role | dark | light | contrast dark / light |
|---|---|---|---|---|
| `surface.canvas` | graph canvas background | `#0F1012` | `#F6F7F9` | — |
| `surface.panel` | panels, document, inspector | `#16171A` | `#FFFFFF` | — |
| `surface.sunken` | code blocks, inputs, log, tab bar, title and status bars | `#121316` | `#F3F4F6` | — |
| `surface.raised` | graph nodes, cards, chips | `#1D1F23` | `#FFFFFF` | — |
| `surface.overlay` | popovers, palette, peek cards | `#24272C` | `#FFFFFF` (+ shadow) | — |
| `surface.hover` | row / button hover | `#23262B` | `#EEF0F3` | — |
| `border.subtle` | panel dividers, node border (decorative) | `#2A2D33` | `#E3E5E9` | 1.3 / 1.3 vs panel (decorative only) |
| `border.strong` | hovered node, section frame | `#3A3E46` | `#C9CDD4` | 1.7 / 1.6 vs panel (decorative only) |
| `border.input` | input and control boundary | `#666C76` | `#868D97` | 3.4 / 3.3 vs panel (need 3:1) |
| `text.primary` | body, names | `#E6E8EB` | `#1C1F24` | 14.6 / 16.5 vs panel (need 4.5:1) |
| `text.secondary` | labels, types, prose meta | `#A9AEB7` | `#4E5560` | 8.0 / 7.5 vs panel |
| `text.tertiary` | hints, timestamps, placeholders | `#8A909A` | `#656C77` | 5.6 / 5.3 vs panel |
| `accent` | selection ring, focus, links, running | `#6EA0FF` | `#2F6BE4` | 6.9 / 4.8 vs panel |
| `accent.button` | primary button fill (white text) | `#3D6FE0` | `#2F6BE4` | white on it 4.6 / 4.8 |
| `selection.text` | text selection background | `#2B3F66` | `#D6E4FF` | text.primary on it 8.5 / 12.9 |
| `status.success` | verified / accepted | `#4CC38A` | `#1E7F4E` | 7.5 / 5.0 vs raised |
| `status.danger` | failing, errors | `#F2676B` | `#C8323A` | 5.4 / 5.3 vs raised |
| `status.warning` | stale, warnings | `#E8B44C` | `#8A5A00` | 8.7 / 5.9 vs raised |
| `status.info` | running, queued (= accent) | `#6EA0FF` | `#2F6BE4` | 6.4 / 4.8 vs raised |
| `status.muted` | spec only / not run | `#8A909A` | `#656C77` | 5.1 / 5.3 vs raised |
| `provenance.agent` | AI-written, not yet reviewed | `#C08CFF` | `#7C3AED` | 7.2 / 5.7 vs panel |
| `kind.def` | def, step (computation) | `#56C2E6` | `#0E7490` | 8.1 / 5.4 vs raised |
| `kind.flow` | flow, app (orchestration) | `#9AA7FF` | `#4F46E5` | 7.3 / 6.3 vs raised |
| `kind.effect` | capability, adapter | `#F0A35E` | `#B4530F` | 7.9 / 5.0 vs raised |
| `kind.data` | struct, enum, refined, alias | `#C3A6FF` | `#7C3AED` | 8.0 / 5.7 vs raised |
| `kind.error` | `@error` enum | `#F47FA0` | `#BE185D` | 6.6 / 6.0 vs raised |
| `kind.param` | flow input / param | `#A9AEB7` | `#4E5560` | 7.4 / 7.5 vs raised |
| `type.scalar` | bool, int, float, sized ints, refined numbers (`Age`) | `#7FD8B4` | `#0F7A55` | 9.7 / 5.3 vs raised |
| `type.text` | str, char, bytes, refined str (`Email`) | `#F59EC4` | `#B4236E` | 8.3 / 6.2 vs raised |
| `type.record` | struct, tuple | `#7DB3FF` | `#2563EB` | 7.7 / 5.2 vs raised |
| `type.enum` | enum | `#C3A6FF` | `#7C3AED` | 8.0 / 5.7 vs raised |
| `type.capability` | `uses Cap` | `#F0A35E` | `#B4530F` | 7.9 / 5.0 vs raised |
| `type.error` | `raises E` | `#F2676B` | `#C8323A` | 5.4 / 5.3 vs raised |
| `type.unit` | unit / None (hollow port) | `#8A909A` | `#656C77` | 5.1 / 5.3 vs raised |
| `edge.data` | neutral data wire (when not type-coloured) | `#8E96A3` | `#6B7380` | 6.4 / 4.5 vs canvas (need 3:1) |
| `edge.calls` | calls and other secondary wires | `#6B717C` | `#858C96` | 3.9 / 3.2 vs canvas (need 3:1) |

**Rules for using the colours:**
- **Tints** are the role colour at fixed alpha: header tint 10%, chip fill 14–16%, inline failure zone 8%, drop target 15%.
  Never use a role colour as a solid fill behind text, except `accent.button` and the danger and success *button* fills,
  which take white text.
- **Hue collisions are designed out:**
  - Flow is indigo and success is green, so they no longer share green.
  - "Executed" uses the accent blue (animated dots), not green.
  - `kind.error` (rose) is a *data* kind and always has the `!` glyph, while `status.danger` (red) always has ✗ or a chip.
- **Literals match their port colours.** In code, numbers and booleans use `type.scalar`, strings use `type.text`, type
  names use `type.record` or `kind.data`, and function names use `kind.def`. A string literal is the same colour as a
  `str` port, so reading code primes graph reading. These choices are encoded in the theme's `highlight.syntax` section.

### 15.4 Type scale

| role | font | size / line | weight | notes |
|---|---|---|---|---|
| Prose body | system UI (SF Pro on macOS) | 15 / 24 | 400 | measure ≤ 72ch, paragraph gap 10 px |
| Prose H1 / H2 / H3 | system UI | 26/32 · 20/28 · 16/24 | 600 | top margin 28 / 24 / 16; inline code in headings rendered as code |
| Prose inline code | mono | 13.5 | 400 | `surface.hover` background, 4 px radius, **no backticks** |
| Code blocks | **JetBrains Mono** (OFL; embed with `add_fonts`), fallback Menlo | 13 / 20 | 400; keywords 600 | ligatures off; line numbers `text.tertiary` |
| UI labels, buttons, tabs | system UI | 13 / 18 | 400; active tab 500 | `font.size` 14 in the theme keeps gpui-kit's rem sizes close to this |
| Small UI (meta, chips, log) | system UI | 12 / 16 | 500 for chips | log durations use tabular numerals |
| Section headings in panels | system UI | 11 / 14 | 600, +0.04em, uppercase | `text.tertiary`; used sparingly (inspector sections) |
| Canvas node title | system UI | 12.5 | 600 | **never below 11 px on screen** (LOD, G2) |
| Canvas port label / type | UI / mono | 12 / 11.5 | 400 | the type in mono `text.secondary` |
| Canvas frame title | system UI | 12.5–14 (by heading level) | 600 | in a reserved title band |

### 15.5 Spacing, radius, elevation

- **Spacing**: a 4 px base. The scale is `2 · 4 · 8 · 12 · 16 · 24 · 32`.
  - Panel padding 16 (document gutter 24).
  - Toolbar height 32 with 8 px gaps.
  - List row 28.
  - Node header 32, port row 22, node padding 12.
  - Frame padding 16, frame title band 28.
  - Grid snap 8, canvas dot grid 24 (hidden below zoom 0.5).
- **Radius**: chips 4 (status pills fully rounded); buttons and inputs 6 (`radius`); nodes and code blocks 8; frames and
  dialogs 10 (`radius.lg`).
- **Elevation**: in dark, shadows don't show, so elevation is mostly the *surface step*, and shadows are only for overlays.

| level | used for | dark | light |
|---|---|---|---|
| 0 | panels, canvas | `surface.panel` / `surface.canvas`, no border | same |
| 1 | nodes, code blocks, cards | `surface.raised` + `border.subtle`, shadow `0 1 2 #00000073` | white + `border.subtle`, shadow `0 1 2 #1C1F2414` |
| 2 | popovers, peek cards, menus | `surface.overlay` + `border.strong`, shadow `0 8 24 #00000080` | white + `border.subtle`, shadow `0 8 24 #1C1F241F` |
| 3 | command palette, dialogs | `surface.overlay`, shadow `0 16 48 #0000008C` + `overlay` scrim | same with a light scrim |

The theme sets `"shadow": false`, so gpui-kit's buttons and inputs stay flat (Zed-like). The elevation shadows above are
applied by Wild Horse's own nodes and popovers.

### 15.6 Icons: Lucide, through gpui-kit

[Lucide](https://lucide.dev) [R-LUCIDE] is the right set, and it's already what gpui-kit ships. `gpui-kit-assets` bundles
the full Lucide catalog (1,830 SVGs), but the default `Assets` embeds only the 101 component icons. Extra icons are added
per app with the `icon_assets!` macro, at about 2 KB each, instead of `AllAssets` (+1 MB) [R-GK-ASSETS]. `wh-ide` already
registers `Assets` (`main.rs`). Add one `icon_assets!(WhIcons, [...])` source that falls back to `Assets`.

Rules:
- 16 px in chrome, 14 px in chips and canvas headers, 12 px inline in text.
- Stroke 1.5 (Lucide default 2 looks heavy at 14 px on dark).
- Colour is always a role token, and an icon is never the only carrier of meaning: add a tooltip or text label.

| role | Lucide icon | in default bundle? |
|---|---|---|
| kind: def / step | `square-function` | extra |
| kind: flow / app | `workflow` | extra |
| kind: capability / adapter | `plug` (adapter), `zap` (capability) | extra |
| kind: struct / enum / refined / alias | `braces` · `list-tree` · `shield-check` · `equal` | extra |
| kind: @error enum | `octagon-alert` | extra |
| status: verified | `circle-check` | default |
| status: failing | `circle-x` | default |
| status: running | `loader-circle` (rotating) | default |
| status: needs synthesis / stale | `circle-dashed` / `circle-dot-dashed` | extra |
| status: pinned | `pin` | extra |
| provenance: AI | `sparkles` | extra |
| actions: run specs / run app | `flask-conical` / `play` | extra / default |
| actions: synthesize, ask agent | `wand-sparkles`, `bot` | extra, default |
| actions: show in graph / locate | `locate-fixed` | extra |
| match / switch node | `split` | extra |
| key field / references | `key-round` | extra |
| problems / warnings | `circle-alert` / `triangle-alert` | default |
| undo / retry | `undo-2` / `rotate-ccw` | default / extra |
| maximize panel, frame | `maximize-2`, `frame` | extra, default |

### 15.7 Node and graph styling rules

See `graph-style-legend.png` and `node-before-after.png`.

![node before/after](../ux-review/style/node-before-after.png)
![graph legend](../ux-review/style/graph-style-legend.png)

**Node anatomy (full LOD):**
- `surface.raised`, 1 px `border.subtle`, radius 8, elevation 1.
- Header 32 px: a 3 px **kind stripe** at the left, inset 8 px top and bottom; a kind glyph in the kind colour; the name
  at 12.5/600 `text.primary`; then secondary meta ("step 2", the module for external stubs) in `text.tertiary`; and the
  status chip right-aligned.
- Header background: the kind colour at 10% tint.
- The signature is **not** in the header. It lives in the port rows and the hover card.
- Footer row (optional): a spec summary ("2 ex · 1 prop · 0.2 ms") in `text.tertiary`.

**Ports:**
- Ports are centred on the node border: inputs on the left, outputs on the right, `raises` at the bottom centre.
- Radius 5, with a 2 px ring in the node colour so they read against wires.
- Fill = **type family** colour.
- Shape = **structure**:
  - ● single `T`;
  - half-filled ◐ for `T | None`;
  - rounded bar ▬ for list, set, dict and array;
  - ◆ for `Stream[T]`;
  - hollow ○ for unit;
  - a red ▼ for `raises`.
- Mode is a small chip after the label (`own`, `mut`). Borrow has no chip.
- The hit target is 16 px, even though the port is drawn at 10 px.
- While dragging, compatible ports get an accent halo, and incompatible ones fade to 30% with a reason tooltip.

**Headers by kind:** def and step `kind.def` with `square-function`; flow and app `kind.flow` with `workflow`; capability
and adapter `kind.effect` with `zap`/`plug`; data kinds `kind.data` with a per-kind glyph; `@error` `kind.error` with
`octagon-alert`; params `kind.param` with a dashed border and an `→` glyph.

**Edges:**

| kind | stroke | colour | notes |
|---|---|---|---|
| data | 1.75 px solid | source port's type colour at 85% (or `edge.data` when "colour wires by type" is off) | bezier, port-aware; a value chip at the midpoint after a run |
| result | 2.25 px solid + arrowhead | `kind.flow` | into the flow output |
| uses / provides | 1.25 px dash 6·4 | `kind.effect` | |
| implements | 1.25 px dot 1.5·3.5 + hollow triangle | `kind.effect` 70% | UML-style |
| calls | 1 px dot 1·3 | `edge.calls` | hidden by default (G3) |
| error rail | 1.5 px dash 5·4 | `status.danger` | orthogonal, along the bottom of the lane |
| has (data model) | 1.5 px solid + crow's-foot ends | `text.secondary` | end glyphs drawn at screen size |
| references | 1.5 px dash 6·4 + `key-round` at the owner end | `type.record` | |
| variant-of / converts-from / refines | solid `kind.data` / dash `status.danger` / dot `kind.data` 70% | | |
| selected or hovered | +1 px, `accent` | | connected edges of a hovered node also brighten |
| executed (last run) | 2 px dot 2·6, animated 600 ms | `accent` | not green |
| invalid (while dragging) | 2 px solid | `status.danger` | as in Blender |

**Status chips:** the canonical set, with glyph, token and text as in §9 A1.
- A *failing* node gets the danger chip **and** a `status.danger` border at 80%.
- *Verified* is just the outline glyph, with no chip and no border change.
- *Stale* uses a warning chip; *running* and *verifying* use info chips with a rotating `loader-circle`.
- *Unreviewed AI edit* puts a `sparkles` chip in `provenance.agent` on the node, matching the gutter bar in the document.

**Section frames and comments:**
- A section frame is a box around its members' bounds plus 16 px, filled with 2.5% white (dark) or 2.5% black (light),
  with a 1 px `border.strong` and radius 10.
- Its 28 px title band holds the heading text at 12.5–14/600 `text.secondary`, with section totals right-aligned
  ("5 nodes · ✗ 1").
- Nested sections are inset by 12 px, with the title one step smaller.
- **Comments** (layout-only notes) use a dashed `border.subtle` and no fill.
- Frame titles are drawn at a fixed screen size at every zoom (Unreal comment boxes [R-UE-CM]).

**State styling (all views):**

| state | rule |
|---|---|
| hover | node: `border.strong`, connected edges brighten; row: `surface.hover` |
| selected | 2 px `accent` ring outside the border (1 px gap); the label is not recoloured |
| keyboard focus | same ring + a 2 px offset dashed inner line (distinguishable from selection) |
| multi-select | rings on each + count chip in the toolbar |
| ghost (block doesn't parse) | 40% opacity, dashed `status.warning` border, `triangle-alert` in the header |
| dimmed (focus mode / incompatible) | 30% opacity |
| flash (edited by another view or the agent) | 2 px `accent` ring (or `provenance.agent` for AI edits) fading over 1.6 s; replaces the full fill |

**Motion:** 120–150 ms ease-out for hover and selection, 150 ms for relayout tweens, 1.6 s for the flash fade, and 600 ms
loops for the "running" spinner and executed-wire dots. Everything is disabled when *Reduce motion* is on.

### 15.8 Document and chrome styling rules

![document before/after](../ux-review/style/document-before-after.png)
![todo execute as a switch](../ux-review/style/flow-match-after.png)

- **Document**:
  - A centred reading column (≤ 72ch, 24 px gutter on the left for status and provenance).
  - Prose sits directly on `surface.panel`.
  - Code blocks are inset `surface.sunken` cards, radius 8, with a 32 px block header in `surface.raised`. The header
    holds the capsule, chips and actions.
  - Ref chips are kind-tinted pills (10% tint, 35% border), with a status dot.
  - Prose lint is a grey 6 px dot at the right margin, not a bar.
- **Panels and tabs** (Zed-like):
  - The tab bar, title bar and status bar use `surface.sunken`.
  - The active tab uses `surface.panel` with `text.primary`, and inactive tabs use `text.tertiary` with no borders between
    tabs.
  - Panel headers carry at most one primary action.
- **Toolbars**:
  - Ghost buttons by default. The single primary action per panel uses `accent.button`.
  - Destructive actions are ghost buttons with `status.danger` text and become filled only in confirmation.
- **Inputs**: `surface.sunken` with a `border.input` (3:1) border and an `accent` 2 px focus ring.
- **Status bar**: 24 px, 12 px text, clickable segments (S2) with Lucide glyphs in role colours.
- **Log and trace**: mono 12, tabular numerals, fixed columns, and severity as a coloured glyph rather than a coloured
  word.

### 15.9 Applying it with gpui-kit's theme system

gpui-kit themes are `ThemeSet` JSON files (`name`, `themes[] { name, mode, font.size, font.family, mono_font.family,
mono_font.size, radius, radius.lg, shadow, colors{…}, highlight{…} }`), loaded through `ThemeRegistry`. The keys used here
were checked against `gpui-component-0.8.0/src/theme/schema.rs`. Steps for the implementation session:

1. **Ship the theme.**
   - Copy `ux-review/style/wild-horse-theme.json` to `crates/wh-ide/themes/wild-horse.json`.
   - At start-up (in `main.rs`, where `Theme::change(ThemeMode::Dark, None, cx)` is today):

   ```rust
   use gpui_kit::component::{Theme, ThemeRegistry};
   ThemeRegistry::global_mut(cx).load_themes_from_str(include_str!("../themes/wild-horse.json"))?;
   let themes = ThemeRegistry::global(cx).themes().clone();
   if let (Some(dark), Some(light)) = (themes.get("Wild Horse Dark"), themes.get("Wild Horse Light")) {
       Theme::global_mut(cx).dark_theme = dark.clone();
       Theme::global_mut(cx).light_theme = light.clone();
   }
   Theme::sync_system_appearance(None, cx); // instead of forcing Dark; re-run on window appearance change
   ```

   Also add a "Theme: System / Light / Dark" command for the palette (`Theme::change(mode, None, cx)`).
2. **Map the tokens** onto gpui-kit keys:

   | Wild Horse token | gpui-kit theme key(s) |
   |---|---|
   | `surface.panel` | `background`, `list.background`, `sidebar.background`, `tab.active.background` |
   | `surface.sunken` | `muted.background`, `tab_bar.background`, `tab.background`, `title_bar.background`, `status_bar.background`, `group_box.background`, `highlight."editor.background"` |
   | `surface.overlay` | `popover.background` |
   | `surface.hover` | `accent.background`, `list.hover.background`, `secondary.background` |
   | `border.subtle` / `border.input` | `border`, `title_bar.border`, `status_bar.border`, `window.border` / `input.border` |
   | `text.primary` / `text.secondary` / `text.tertiary` | `foreground` / `muted.foreground` / `tab.foreground` (other uses via `tokens.rs`) |
   | `accent` | `ring`, `caret`, `link`, `drag.border`, `list.active.border`, `base.blue` |
   | `accent.button` | `primary.background` (+ hover / active), `info.background` |
   | `selection.text` | `selection.background`, `list.active.background` |
   | status colours | `success.*` / `danger.*` / `warning.*` are **button fills** (white text, AA). The text and glyph versions live in `highlight` (`error`, `warning`, `info`, `success`, `hint` + `.background` / `.border`, used for editor diagnostics) and in `tokens.rs` |
   | diff review (A2) | `highlight.created` / `created.background`, `deleted` / `deleted.background`, `modified` / `modified.background` |
   | kind and type palettes | `base.cyan / magenta / green / red / yellow` and `chart.1–5` hold a subset; the full set is in `tokens.rs`, because `ThemeColor` has no slots for them |
   | syntax | `highlight.syntax.{keyword, type, function, string, number, boolean, constant, comment, operator, attribute, …}` |
   | fonts and shape | `font.size` 14, `mono_font.family` "JetBrains Mono", `mono_font.size` 13, `radius` 6, `radius.lg` 10, `shadow` false |

3. **Replace the hard-coded colours.**
   - `canvas.rs` `kind_color` / `edge_color` → `tokens::kind` / `tokens::edge` (the sketch has the exact replacements).
   - `document.rs` syntax colours (the `Class::* => hsla(…)` match, about line 88) →
     `cx.theme().highlight_theme.style("keyword")` and so on, so the code colours follow the theme and the light mode.
   - The executed outline → `accent`, and the flash → an `accent` or `provenance.agent` ring.
4. **Fonts**: embed JetBrains Mono (SIL OFL) with `cx.text_system().add_fonts(...)` at start-up. If it's absent, gpui-kit
   falls back to Menlo.
5. **Icons**: `gpui_kit::assets::icon_assets!(WhIcons, [SquareFunction, Workflow, Plug, Zap, Braces, …])`, composed with
   `Assets` in an `AssetSource` passed to `with_assets`. Then use `Icon::new(IconName::SquareFunction)` and so on.

`ThemeRegistry::watch_dir` also lets users drop their own theme JSON into a folder. Wild Horse roles that the JSON can't
express stay in `tokens.rs`. A later step could read an extra `"wh"` object from the same file: gpui-kit's `ThemeConfig`
doesn't reject unknown keys, so it can be parsed separately with serde.

---

## 16. Prioritized roadmap

The items are written to be picked up directly. "Files" are the main touch points. Items marked ⟂ depend on in-flight work
in the `ide-menus` session, so pick them up after it lands, or coordinate with it.

### Quick wins (each ≤ 2 days; mostly local to one file)

| # | Item (findings) | Files | Done when |
|---|---|---|---|
| Q1 | **Fit that fits**: include section boxes, center on both axes, drop the 0.25 floor; `⇧1` fit, `F` zoom to selection; re-fit on resize and scope change (G1, L1) | `wh-ide/src/canvas.rs` (`fit`, `zoom_by`), `graph_panel.rs` | pong and todo fit entirely in the default panel; `+` keeps content centred |
| Q2 | **Minimum on-screen text + compact LOD** (header-only below 0.7, name at ≥ 11 px) (G2 part 1, X3) | `canvas.rs` `render_node` | at default fit on signup every node name is readable |
| Q3 | **Status chip v1**: colour + glyph + tooltip from one `SynthStatus → (glyph, token, text)` map; failed = red ✗ with reason; executed-path outline switched to blue; fix the badge/queue mismatch (A1) | `canvas.rs`, `graph_panel.rs` (status refresh), `backend.rs` (`SynthStatus::label`) | queue and node always agree; failed is unmistakable |
| Q4 | **Unresolved step as error node** with "did you mean" (G8) | `wh-graph/src/flow.rs` (emit node for unresolved callee), `canvas.rs` | `|> validatexx` shows a red node in place |
| Q5 | **Cardinality segmented control with current value**; crow's-foot end glyphs at screen size (M1, M2) | `graph_panel.rs` toolbar, `canvas.rs` edge paint, `wh-graph/src/data.rs` (cardinality already on edges) | the current value is visible at default zoom |
| Q6 | **Markdown rendering** via `pulldown-cmark`; front matter as pills; `[[ref]]` chips that select the node on click (D1, D2 part 1) | `wh-ide/src/document.rs` (`render_prose`, `styled_inline`) | screenshots 05 and 16 render cleanly; refs are clickable |
| Q7 | **Problems tab** (replaces content per check), clickable status-bar counts, `F8`; log rows with `file:line` navigate (S1) | `side_panels.rs`, `state.rs`, `main.rs`; span→node in `wh-model` | typing an error → `F8` lands the caret on it |
| Q8 | **Lint noise**: "missing prose" moves to a gutter dot plus a *Draft prose* action; consistent severities (D3, S1) | `document.rs`, `live_backend.rs` / check source | signup shows no yellow bars when it has 0 errors |
| Q9 | **Inspector clause rows**: commit on Enter or blur, trash icon, right slot reserved for test status; multi-line clauses wrap (I1, I2 part 1) | `inspector.rs` | no `✓` buttons; long examples are readable |
| Q10 | **Selection reveal at ⅓ height; off-screen change pill; Undo toast** (D5, D6, S3) | `document.rs`, `state.rs` | a graph edit always gives visible feedback in the document |
| Q11 | **Human-readable selection in the status bar**, cleared on undo (L5) | `main.rs` status bar, `state.rs` | no raw `#…[k]` ids in the chrome |
| Q12 | **`hide_calls` on by default**; step nodes carry the callee's status (G3 part 1) | `graph_panel.rs` (`FlowOptions`), `wh-graph/src/flow.rs` | signup's flow shows one node per step |

### Next (1–2 weeks each; the core UX of the product)

| # | Item (findings) | Files | Notes |
|---|---|---|---|
| N1 | **Command palette + Go to node + `⌘.` quick actions**, from one action registry shared with menus (K1, K3) ⟂ menus | new `wh-ide/src/palette.rs`, `main.rs` | gpui-component has `List`/`Popover` building blocks |
| N2 | **Canvas keyboard model + inline rename + link-drop searcher** (K2, G7) | `canvas.rs` (focus handle, key context `GraphCanvas`), `graph_panel.rs` | the searcher reuses `accepts()` / `wh_model::assignable` |
| N3 | **Outline panel + breadcrumbs + back/forward** (L2, L3, X1 part 1) | new `wh-ide/src/outline.rs`, `state.rs` (history), `document.rs` (breadcrumb) | doubles as the accessible graph alternative |
| N4 | **Identity capsule** component used by doc block headers, graph headers, inspector title, outline, palette and problems; doc gutter badges; hover sync (§3, D4, A1) | new `wh-ide/src/capsule.rs`, `document.rs`, `canvas.rs`, `inspector.rs`, `state.rs` (`hovered`) | the single most "one object" change |
| N5 | **Specs on nodes and clauses**: per-clause ✓/✗/counterexample in the inspector, test chips in capsules (I1, A1) ⟂ `NodeResult` | `inspector.rs`, `canvas.rs`, `backend.rs` | needs `NodeResult` per clause from `wh-build` |
| N6 | **Author change-set review**: card with per-hunk accept/reject, inline block diffs, graph diff overlay, provenance bars, "unreviewed" count (A2) ⟂ review mode | `side_panels.rs`, `document.rs`, `canvas.rs`, `state.rs` (pending edits → hunks via `diffy`) | the same UI serves live mode as "Revert" |
| N7 | **Synthesis failure card + recovery actions** (A5, I5) | `side_panels.rs`, `inspector.rs`, `backend.rs` (`SynthStatus::Failed` → structured gate + detail) | needs the gate name and detail from `wh-synth` |
| N8 | **`match` as a labeled switch node; error ("raises") ports; typed port colours and shapes** (G4, G5) | `wh-graph/src/flow.rs` (case labels on `GEdge.label`, error rows), `lib.rs` (`Row` port kind), `canvas.rs` | todo `execute` reads as five labeled lanes |
| N9 | **Cluster-aware incremental layout + animated moves; section boxes from members** (G6, M5, P-1) | `wh-graph/src/layout.rs`, `store.rs`, `canvas.rs` | inserting a step moves ≤ 3 nodes |
| N10 | **Workspace presets + maximize panel** (L1, L4) ⟂ layout persistence | `main.rs`, `panels.rs` | three presets, stored with the layout sidecar |
| N11 | **Data-model field selection sync + focus mode + filter** (M3, M4) | `graph_panel.rs`, `wh-graph/src/data.rs`, `inspector.rs` (field form) | |
| N12 | **Agent panel: context chips, activity rows, budget meter, node actions** (A3, A6) | `side_panels.rs`, `live_backend.rs` | |

### Style track (from §15; it can run alongside the UX items)

| # | Item | Files | Done when |
|---|---|---|---|
| St1 | **Load the Wild Horse theme set and follow the system appearance**; add a Theme command (§15.9 steps 1–2) | new `crates/wh-ide/themes/wild-horse.json` (from `ux-review/style/`), `main.rs` | the app starts in light or dark with the OS, and every gpui-kit widget uses the new chrome |
| St2 | **`tokens.rs`**, and replace `kind_color` / `edge_color` and the syntax `hsla` constants (§15.9 step 3) | new `tokens.rs` (from the sketch), `canvas.rs`, `document.rs` | `rg 'hsla\(' crates/wh-ide/src` finds no colour literals outside `tokens.rs` |
| St3 | **Node restyle**: kind stripe + glyph + 10% tint, signature out of the header, ports on the border with type colours and shapes, `raises` port, selection ring vs focus ring, quiet ✓ and loud ✗ (§15.7) | `canvas.rs` (`render_node`, port painting), `wh-graph/src/lib.rs` (`Row` port kind and type family) | `node-before-after.png` "after" reproduced in the app |
| St4 | **Edge styles**: dash patterns per kind, executed = animated accent dots, crow's-foot ends (§15.7) | `canvas.rs` edge painter (dashes need segmenting the bezier) | screenshot 09/14 equivalents match the legend |
| St5 | **Lucide icons** via `icon_assets!` for kinds, status and actions; replace the text glyphs (§15.6) | `main.rs` (asset source), all views | no `✓ × ⋯ •` text glyphs left in the chrome |
| St6 | **Typography**: embed JetBrains Mono, prose measure ≤ 72ch, type scale (§15.4) | `main.rs` (`add_fonts`), `document.rs` | |
| St7 | **Frames**: reserved title band, totals, comment boxes (§15.7) | `canvas.rs`, `wh-graph/src/layout.rs` (with N9) | no frame title is ever covered |

### Later (bigger bets; mostly milestone-2+ in `design.md`)

| # | Item | Files / crates | Notes |
|---|---|---|---|
| L-a | **Values on wires, viewers, margin values in the doc** (T1) | `wh-codegen` (record step I/O as redacted `Debug` in spans), `backend.rs` (`TraceEvent.fields`), `canvas.rs`, `document.rs` | respect `sensitive` |
| L-b | **Trace runs, step-through scrubber, duration heat; M2 timeline** (T2) | `side_panels.rs`, `canvas.rs` | |
| L-c | **Inline assist (`⌘K`) in blocks and on nodes** (A4) | `document.rs`, `graph_panel.rs`, `wh-author` | the inline diff reuses N6 |
| L-d | **High-contrast theme variant** (V5; light and dark are in St1) | `themes/wild-horse.json` | |
| L-e | **Slash commands, block drag handles, Library rail palette** (D7, D4, G3 part 2) | `document.rs`, `graph_panel.rs`, `wh-edit` (move block / move def to section ops) | |
| L-f | **Minimap, reroute dots, comment boxes** (G10, G7) | `canvas.rs`, `wh-graph/src/store.rs` | |
| L-g | **AX exposure of canvas nodes; live-region announcements** (X1) | `canvas.rs` (as GPUI allows) | |
| L-h | **Breakpoints/watches; Quokka-style example evaluation** (T3, T4) | `wh-build`, `wh-project` | design "later milestones" |
| L-i | **Onboarding tour, project overview, new-project template** (O1, O4) ⟂ welcome screen | `inspector.rs`, `config.rs` | |

**Suggested order:**
1. Q1–Q4 first. They fix legibility and the two ways the graph currently misleads.
2. Then Q6, Q7 and Q12, so the document and the errors stop being noise.
3. Then N4 (capsule) and N5 (specs on nodes). Together they deliver the "one object, verified" story.
4. Then N1, N2 and N3 for flow and navigation.
5. Then N6 and N7 for AI review and recovery.

The style track is separate: St1 and St2 are about a day each and touch only `main.rs`, a new `tokens.rs` and the colour
literals, so they can go first and give every later item the right tokens. Do St3 and St4 together with Q2 and Q3,
since they rewrite the same `render_node` code.

## 17. Sources

Facts about Wild Horse come from the running app (screenshots above) and from `crates/wh-ide/src/*.rs`,
`crates/wh-ide/NOTES.md` and `docs/design.md`. External patterns were checked on the cited pages on 2026-10-22. Entries
marked *(not re-checked)* are well-known behaviour whose page could not be fetched or wasn't consulted. Treat them as
leads, not quotes.

**Unreal Engine Blueprints**
- [R-UE-FND] Blueprint Foundations: exec vs data pins, reroute nodes, navigation. <https://dev.epicgames.com/documentation/unreal-engine/blueprint-foundations?lang=en-US>
- [R-UE-CN] Connecting Nodes: pin colours, drop-feedback with reasons, drag-off-pin context menu, wire hover emphasis. <https://dev.epicgames.com/documentation/en-us/unreal-engine/connecting-nodes-in-unreal-engine>
- [R-UE-CS] Blueprint Editor Cheat Sheet: `Home`, `F`, selection and wire keys. <https://dev.epicgames.com/documentation/en-us/unreal-engine/blueprint-editor-cheat-sheet-in-unreal-engine>
- [R-UE-CM] Comments: comment boxes, group movement, zoom-scaled titles. <https://dev.epicgames.com/documentation/en-us/unreal-engine/comments-in-unreal-engine>
- [R-UE-COL] Collapsing Graphs: collapse to graph, function or macro, and expand. <https://dev.epicgames.com/documentation/en-us/unreal-engine/collapsing-graphs-in-unreal-engine>
- [R-UE-FIND] Searching in Blueprints (Find in Blueprints). <https://dev.epicgames.com/documentation/en-us/unreal-engine/searching-in-blueprints-in-unreal-engine>
- [R-UE-DBG] Blueprint Debugger: breakpoints, watches, execution trace. <https://dev.epicgames.com/documentation/unreal-engine/blueprint-debugger-in-unreal-engine>
- [R-UE-NODES] Nodes in Unreal Engine (the *Switch on Enum* behaviour itself is *not re-checked*). <https://dev.epicgames.com/documentation/en-us/unreal-engine/nodes-in-unreal-engine>

**Blender node editor / Geometry Nodes**
- [R-BL-PARTS] Node Parts: socket shapes and colours, collapse, hide sockets, context path. <https://docs.blender.org/manual/en/latest/interface/controls/nodes/parts.html>
- [R-BL-FIELDS] Fields: dashed field links, red invalid links. <https://docs.blender.org/manual/en/latest/modeling/geometry_nodes/fields.html>
- [R-BL-INSP] Inspection: socket values on hover, node warnings, timings overlay. <https://docs.blender.org/manual/en/latest/modeling/geometry_nodes/inspection.html>
- [R-BL-VIEW] Viewer Node. <https://docs.blender.org/manual/en/latest/modeling/geometry_nodes/output/viewer.html>
- [R-BL-SS] Spreadsheet editor. <https://docs.blender.org/manual/en/latest/editors/spreadsheet.html>
- [R-BL-NG] Node Groups. <https://docs.blender.org/manual/en/latest/interface/controls/nodes/groups.html>
- [R-BL-FR] Frame node. <https://docs.blender.org/manual/en/latest/interface/controls/nodes/types/layout/frame.html>
- [R-BL-NE] Node Editors (link-drag-search, overlays). <https://docs.blender.org/manual/en/latest/interface/controls/nodes/node_editors.html>
- [R-BL-EDIT] Editing nodes (insert on link, `Shift-X`, `J`, cut links). <https://docs.blender.org/manual/en/latest/interface/controls/nodes/editing.html>
- [R-BL-WS] Workspaces *(not re-checked)*. <https://docs.blender.org/manual/en/latest/interface/window_system/workspaces.html>

**VS Code**
- [R-VS-UI] User Interface: command palette, Quick Open, breadcrumbs, minimap, Zen Mode. <https://code.visualstudio.com/docs/getstarted/userinterface>
- [R-VS-TIPS] Tips and Tricks: Quick Open prefixes, `F8` next problem. <https://code.visualstudio.com/docs/getstarted/tips-and-tricks>
- [R-VS-ERR] Editing evolved: Problems panel, next-problem zone, peek, CodeLens, inlay hints. <https://code.visualstudio.com/docs/editing/editingevolved>
- [R-VS-NAV], [R-VS-BC]: breadcrumbs and peek definition, on the same two pages as [R-VS-ERR] and [R-VS-UI].
- [R-VS-KB] Keybindings editor (Go Back and Go Forward are listed in the default keybindings). <https://code.visualstudio.com/docs/configure/keybindings>
- [R-VS-TEST] Testing: gutter run and status icons, inline failures. <https://code.visualstudio.com/docs/debugtest/testing>
- [R-VS-REF] Refactoring: Refactor Preview with per-change deselect. <https://code.visualstudio.com/docs/editing/refactoring>
- [R-VS-AIREV] Review AI-generated code edits: pending edits, Keep/Undo, step through. <https://code.visualstudio.com/docs/copilot/chat/review-code-edits>
- [R-VS-CKPT] Chat checkpoints. <https://code.visualstudio.com/docs/copilot/chat/chat-checkpoints>

**Zed**
- [R-ZD-AG] Agent Panel: Review Changes with per-hunk accept/reject, follow the agent, checkpoints, @-context. <https://zed.dev/docs/ai/agent-panel>
- [R-ZD-IA] Inline Assistant. <https://zed.dev/docs/ai/inline-assistant>
- [R-ZD-MB] Multibuffers (diagnostics and search as editable excerpts). <https://zed.dev/docs/multibuffers>
- [R-ZD-CFG] Configuring Zed: toggleable toolbar items, minimap modes. <https://zed.dev/docs/configuring-zed>
- [R-ZD-CP] Command Palette. <https://zed.dev/docs/command-palette>
- [R-ZD-120] "Leveraging Rust and the GPU to render user interfaces at 120 FPS". <https://zed.dev/blog/videogame>
- [R-ZD-METAL] "Optimizing the Metal pipeline to maintain 120 FPS". <https://zed.dev/blog/120fps>

**Notion**
- [R-NO-BL] Writing and editing basics: blocks, `⋮⋮` handle, Turn into, Markdown shortcuts. <https://www.notion.com/help/writing-and-editing-basics>
- [R-NO-SL] Using slash commands. <https://www.notion.com/help/guides/using-slash-commands>
- [R-NO-MEN] Create links and backlinks. <https://www.notion.com/help/create-links-and-backlinks>
- [R-NO-DB] Intro to databases (multiple named views of one dataset, linked views). <https://www.notion.com/help/intro-to-databases>

**Others**
- [R-JU-NB] Jupyter Notebook docs. <https://jupyter-notebook.readthedocs.io/en/stable/notebook.html>
- [R-JU-FMT] nbformat: `execution_count`, outputs. <https://nbformat.readthedocs.io/en/latest/format_description.html>
- [R-FG-NAV] Figma: Adjust your view of the canvas (`⇧1`, `⇧2`, Space-drag). <https://help.figma.com/hc/en-us/articles/360041065034-Adjust-your-view-of-the-canvas>
- [R-FG-FOL] Figma: Follow collaborators. <https://help.figma.com/hc/en-us/articles/360040322673-Follow-collaborators-in-a-file>
- [R-QK] Quokka.js docs. <https://quokkajs.com/docs/>. Light Table's inline results are *not re-checked*.
- [R-EN] Enso: dual visual/textual representation, searcher *(partly verified, from older secondary sources)*. <https://github.com/enso-org/enso/blob/develop/README.md>, <https://dev.to/enso_org/enso-dev-blog-19th-june-2020-52da>
- [R-NR-ST] Node-RED Nodes: status, error triangle, changed dot. <https://nodered.org/docs/user-guide/editor/workspace/nodes>
- [R-NR-WIRE] Node-RED Wires: click-to-connect, splice onto a wire. <https://nodered.org/docs/user-guide/editor/workspace/wires>
- [R-NR-DBG] Node-RED Debug sidebar. <https://nodered.org/docs/user-guide/editor/sidebar/debug>
- [R-CC-CKPT] Claude Code checkpointing (`/rewind`). <https://code.claude.com/docs/en/checkpointing>
- [R-CU] Cursor Agent overview: snapshots and restore. <https://cursor.com/docs/agent/overview>
- [R-BV] Bret Victor, *Learnable Programming*. <https://worrydream.com/LearnableProgramming/>
- [R-RF] React Flow accessibility: a reference keyboard and ARIA model for node graphs. <https://reactflow.dev/learn/advanced-use/accessibility>
- [R-LUCIDE] Lucide icons (ISC license; the set gpui-kit bundles). <https://lucide.dev>
- [R-GK-ASSETS] gpui-kit-assets 0.8 README: the default 101 icons, `icon_assets!` for chosen extras, `AllAssets`. Read from
  `~/.cargo/registry/src/*/gpui-kit-assets-0.8.0/README.md`. gpui-kit theme schema: `gpui-component-0.8.0/src/theme/{schema,registry,mod}.rs`.

**Accessibility (WCAG 2.2 Understanding docs)**
- [R-WCAG-143] Contrast (Minimum). <https://www.w3.org/WAI/WCAG22/Understanding/contrast-minimum.html>
- [R-WCAG-1411] Non-text Contrast. <https://www.w3.org/WAI/WCAG22/Understanding/non-text-contrast.html>
- [R-WCAG-141] Use of Color. <https://www.w3.org/WAI/WCAG22/Understanding/use-of-color.html>
- [R-WCAG-257] Dragging Movements. <https://www.w3.org/WAI/WCAG22/Understanding/dragging-movements.html>
- [R-WCAG-258] Target Size (Minimum) *(not re-checked)*. <https://www.w3.org/WAI/WCAG22/Understanding/target-size-minimum.html>

**Graph layout**
- K. Misue, P. Eades, W. Lai, K. Sugiyama, "Layout Adjustment and the Mental Map", *Journal of Visual Languages and
  Computing* 6(2), 1995.
