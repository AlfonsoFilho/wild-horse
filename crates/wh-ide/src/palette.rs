//! The command palette (UX review K1, N1): `⇧⌘P` lists every menu command
//! with its shortcut (the menus are the action registry), `⌘P` goes to any
//! node of the project, and `⌘.` lists quick actions on the selection.

use std::rc::Rc;

use gpui_kit::component::command::{Command, CommandItem, CommandState};
use gpui_kit::component::{IndexPath, WindowExt, h_flex};
use gpui_kit::*;
use wh_doc::Origin;
use wh_edit::Op;
use wh_model::NodeKind;

use crate::capsule;
use crate::state::{AppState, SelectionSource};
use crate::tokens::tokens;

actions!(palette, [CommandPalette, GoToNode, QuickActions]);

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-shift-p", CommandPalette, None),
        KeyBinding::new("cmd-p", GoToNode, None),
        KeyBinding::new("cmd-.", QuickActions, None),
    ]);
}

/// Handle the palette actions on `el` (a panel or the workspace root).
pub fn on_palette<E: InteractiveElement>(el: E, state: &Entity<AppState>) -> E {
    let (a, b, c) = (state.clone(), state.clone(), state.clone());
    el.on_action(move |_: &CommandPalette, window, cx| open(a.clone(), Mode::Commands, window, cx))
        .on_action(move |_: &GoToNode, window, cx| open(b.clone(), Mode::Nodes, window, cx))
        .on_action(move |_: &QuickActions, window, cx| open(c.clone(), Mode::Quick, window, cx))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Commands,
    Nodes,
    Quick,
}

type Run = Rc<dyn Fn(&mut Window, &mut App)>;

struct Entry {
    glyph: SharedString,
    color: Hsla,
    label: SharedString,
    detail: SharedString,
    keywords: Vec<SharedString>,
    run: Run,
}

/// Open the palette in `mode` over the window.
pub fn open(state: Entity<AppState>, mode: Mode, window: &mut Window, cx: &mut App) {
    let entries: Rc<Vec<Entry>> = Rc::new(match mode {
        Mode::Commands => commands(window, cx),
        Mode::Nodes => nodes(&state, cx),
        Mode::Quick => quick(&state, cx),
    });
    let placeholder = match mode {
        Mode::Commands => "Run a command…",
        Mode::Nodes => "Go to a definition, step or type…",
        Mode::Quick => "Act on the selection…",
    };
    let cmd = cx.new(|cx| CommandState::new(window, cx));
    let first = std::cell::Cell::new(true);
    window.open_dialog(cx, move |dialog, window, cx| {
        if first.replace(false) {
            let focus = cmd.read(cx).focus_handle(cx);
            focus.focus(window, cx);
        }
        let t = tokens(cx);
        let items = entries.iter().map(|e| {
            let (glyph, color, label, detail) = (e.glyph.clone(), e.color, e.label.clone(), e.detail.clone());
            CommandItem::new().label(e.label.clone()).keywords(e.keywords.clone()).child(move |_, _| {
                h_flex()
                    .w_full()
                    .gap_2()
                    .child(div().w(px(18.)).flex_none().text_color(color).font_weight(FontWeight::BOLD).child(glyph.clone()))
                    .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().text_color(t.text_primary).child(label.clone()))
                    .child(div().flex_none().text_size(px(12.)).text_color(t.text_tertiary).child(detail.clone()))
            })
        });
        let run = entries.clone();
        dialog.w(px(620.)).p_0().close_button(false).child(
            Command::new(&cmd)
                .placeholder(placeholder)
                .bordered(false)
                .max_h(px(420.))
                .items(items)
                .empty(|_, _, _| div().p_3().child("Nothing matches."))
                .on_confirm(move |ix: IndexPath, window, cx| {
                    window.close_dialog(cx);
                    if let Some(e) = run.get(ix.row) {
                        let f = e.run.clone();
                        window.defer(cx, move |window, cx| f(window, cx));
                    }
                })
                .on_cancel(|window, cx| window.close_dialog(cx)),
        )
    });
}

/// Every menu command, with its shortcut: the menus are the action registry.
fn commands(window: &mut Window, cx: &mut App) -> Vec<Entry> {
    let t = tokens(cx);
    let mut out = Vec::new();
    fn walk(menu: &OwnedMenu, path: &str, out: &mut Vec<(String, Box<dyn Action>)>) {
        for item in &menu.items {
            match item {
                OwnedMenuItem::Action { name, action, .. } => out.push((format!("{path}{name}"), action.boxed_clone())),
                OwnedMenuItem::Submenu(m) => walk(m, &format!("{path}{}: ", m.name), out),
                _ => {}
            }
        }
    }
    let mut found = Vec::new();
    for m in cx.get_menus().unwrap_or_default() {
        if m.name.as_ref() == "Wild Horse" || m.name.is_empty() {
            continue;
        }
        walk(&m, &format!("{}: ", m.name), &mut found);
    }
    for (name, action) in found {
        let keys = window.highest_precedence_binding_for_action(&*action).map(|b| b.keystrokes().iter().map(|k| k.to_string()).collect::<Vec<_>>().join(" ")).unwrap_or_default();
        let action: Rc<dyn Action> = action.into();
        out.push(Entry {
            glyph: "›".into(),
            color: t.text_tertiary,
            label: name.into(),
            detail: keys.into(),
            keywords: vec![],
            run: Rc::new(move |window, cx| window.dispatch_action(action.boxed_clone(), cx)),
        });
    }
    let canvas = [
        ("Graph: Fit to view", "⇧1", Box::new(crate::canvas::FitView) as Box<dyn Action>),
        ("Graph: Zoom to selection", "F", Box::new(crate::canvas::ZoomToSelection)),
    ];
    for (label, keys, action) in canvas {
        let action: Rc<dyn Action> = action.into();
        out.push(Entry {
            glyph: "›".into(),
            color: t.text_tertiary,
            label: label.into(),
            detail: format!("{keys} in a graph").into(),
            keywords: vec![],
            run: Rc::new(move |window, cx| window.dispatch_action(action.boxed_clone(), cx)),
        });
    }
    out
}

/// Every definition and flow step, with its capsule.
fn nodes(state: &Entity<AppState>, cx: &App) -> Vec<Entry> {
    let t = tokens(cx);
    let st = state.read(cx);
    let model = &st.project.model;
    let queue = capsule::queue_positions(&st.synthesis);
    let mut out = Vec::new();
    let mut ns: Vec<&wh_model::Node> = model.nodes.values().collect();
    ns.sort_by_key(|n| (n.file, n.span.start));
    for n in ns {
        let synth = st.synthesis.iter().find(|(id, _)| *id == n.id).map(|(_, s)| s);
        let chip = capsule::node_chip(synth, queue.get(&n.id).copied(), st.tests.get(&n.id));
        let kind = capsule::kind_of(n);
        let mut detail = format!("{} · {}", if n.is_error_type { "@error enum" } else { n.kind.label() }, n.module);
        if let Some(c) = chip {
            detail = format!("{} {}  {detail}", c.glyph, c.text);
        }
        out.push(select_entry(state, n.id.clone(), capsule::kind_glyph(kind), t.kind(kind), n.name.clone(), detail, vec![n.id.clone().into()]));
        let steps = match &n.kind {
            NodeKind::Flow(f) => &f.steps[..],
            NodeKind::App(a) => a.entry.as_ref().map_or(&[][..], |e| &e.steps[..]),
            _ => &[],
        };
        for (k, s) in steps.iter().enumerate() {
            let label = format!("{} · step {} of {}", s.label, k + 1, n.name);
            out.push(select_entry(state, s.id.clone(), capsule::kind_glyph(wh_graph::GNodeKind::Step), t.kind_def, label, format!("step · {}", n.module), vec![]));
        }
    }
    out
}

fn select_entry(state: &Entity<AppState>, id: String, glyph: &str, color: Hsla, label: String, detail: String, keywords: Vec<SharedString>) -> Entry {
    let state = state.clone();
    Entry {
        glyph: glyph.to_string().into(),
        color,
        label: label.into(),
        detail: detail.into(),
        keywords,
        run: Rc::new(move |_, cx| {
            let id = id.clone();
            state.update(cx, |s, cx| s.select(Some(id), true, SelectionSource::Log, cx));
        }),
    }
}

/// Actions on the selected node (the same list a node context menu would show).
fn quick(state: &Entity<AppState>, cx: &App) -> Vec<Entry> {
    let t = tokens(cx);
    let st = state.read(cx);
    let Some(sel) = st.project.selection.clone() else { return vec![] };
    let Some(n) = st.project.selected_node().cloned() else { return vec![] };
    let mut out = Vec::new();
    let action = |glyph: &str, color: Hsla, label: String, run: Run| Entry { glyph: glyph.to_string().into(), color, label: label.into(), detail: "".into(), keywords: vec![], run };
    let s = state.clone();
    let id = sel.clone();
    out.push(action("↧", t.text_secondary, "Show in document".into(), Rc::new(move |_, cx| {
        let id = id.clone();
        s.update(cx, |s, cx| s.select(Some(id), true, SelectionSource::Log, cx));
    })));
    let step = match &n.kind {
        NodeKind::Flow(f) => f.steps.iter().find(|x| x.id == sel).cloned(),
        NodeKind::App(a) => a.entry.as_ref().and_then(|e| e.steps.iter().find(|x| x.id == sel).cloned()),
        _ => None,
    };
    if let Some(step) = &step {
        if let Some(callee) = step.callee.clone() {
            let s = state.clone();
            out.push(action("ƒ", t.kind_def, format!("Open def {}", step.label), Rc::new(move |_, cx| {
                let c = callee.clone();
                s.update(cx, |s, cx| s.select(Some(c), true, SelectionSource::Log, cx));
            })));
        }
        let idx = match &n.kind {
            NodeKind::Flow(f) => f.steps.iter().position(|x| x.id == sel),
            NodeKind::App(a) => a.entry.as_ref().and_then(|e| e.steps.iter().position(|x| x.id == sel)),
            _ => None,
        };
        if let Some((stmt, Some(position))) = idx.and_then(|i| wh_edit::pipeline_position(&st.project.model, &n.id, i)) {
            let s = state.clone();
            let flow = n.id.clone();
            out.push(action("⌫", t.danger, "Remove step".into(), Rc::new(move |_, cx| {
                let op = Op::RemoveFlowStep { flow: flow.clone(), stmt, position };
                s.update(cx, |s, cx| s.apply_op(Origin::Graph, op, cx));
            })));
        }
        return out;
    }
    if n.is_hole() {
        let s = state.clone();
        let id = n.id.clone();
        out.push(action("✦", t.agent, "Synthesize".into(), Rc::new(move |_, cx| {
            let id = id.clone();
            s.update(cx, |s, cx| s.synthesize(id, cx));
        })));
    }
    let s = state.clone();
    out.push(action("▶", t.text_secondary, "Run specs (check)".into(), Rc::new(move |_, cx| s.update(cx, |s, cx| s.check(cx)))));
    if n.prose.is_none() {
        let s = state.clone();
        let name = n.name.clone();
        out.push(action("✎", t.agent, "Draft prose with the author agent".into(), Rc::new(move |_, cx| {
            let text = format!("Draft the prose paragraph that explains `{name}`: what it is for and why, in the document's voice.");
            s.update(cx, |s, cx| s.ask_author(text, cx));
        })));
    }
    let s = state.clone();
    let id = n.id.clone();
    out.push(action("⌫", t.danger, format!("Delete {}", n.name), Rc::new(move |_, cx| {
        let op = Op::RemoveNode { node: id.clone() };
        s.update(cx, |s, cx| s.apply_op(Origin::Graph, op, cx));
    })));
    out
}
