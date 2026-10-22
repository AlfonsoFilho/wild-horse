//! The Flow graph and Data model panels: `wh-graph` projections drawn on the
//! shared canvas, edited through `wh_edit::Op`s (`Origin::Graph`).

use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonGroup, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme, Disableable, Selectable, Sizable, h_flex, v_flex};
use gpui_kit::*;
use wh_doc::Origin;
use wh_edit::{Cardinality as EditCard, Op};
use wh_graph::{DataOptions, FlowOptions, GNodeKind, Graph, PortDir, PortRef};
use wh_model::{Model, NodeKind, Ty};

use crate::canvas::{CanvasEvent, GraphCanvas};
use crate::state::{AppEvent, AppState, SelectionSource};

actions!(graph, [DeleteSelected, RenameSelected]);

pub fn bind_keys(cx: &mut App) {
    crate::palette::bind_keys(cx);
    cx.bind_keys([
        KeyBinding::new("shift-1", crate::canvas::FitView, Some("GraphCanvas")),
        KeyBinding::new("f", crate::canvas::ZoomToSelection, Some("GraphCanvas")),
        KeyBinding::new("shift-2", crate::canvas::ZoomToSelection, Some("GraphCanvas")),
        KeyBinding::new("=", crate::canvas::ZoomIn, Some("GraphCanvas")),
        KeyBinding::new("-", crate::canvas::ZoomOut, Some("GraphCanvas")),
        KeyBinding::new("backspace", DeleteSelected, Some("GraphCanvas")),
        KeyBinding::new("delete", DeleteSelected, Some("GraphCanvas")),
        KeyBinding::new("f2", RenameSelected, Some("GraphCanvas")),
        KeyBinding::new("enter", RenameSelected, Some("GraphCanvas")),
    ]);
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum GraphKind {
    Flow,
    Data,
}

impl GraphKind {
    fn key(self) -> &'static str {
        match self {
            GraphKind::Flow => "flow",
            GraphKind::Data => "data",
        }
    }
}

pub struct GraphPanel {
    state: Entity<AppState>,
    kind: GraphKind,
    focus: FocusHandle,
    canvas: Entity<GraphCanvas>,
    whole_project: bool,
    overlay: bool,
    /// Draw `calls` edges from steps to their definitions (off by default, G3).
    show_calls: bool,
    rename: Entity<InputState>,
    a: Entity<InputState>,
    b: Entity<InputState>,
    _subs: Vec<Subscription>,
}

impl GraphPanel {
    pub fn new(state: Entity<AppState>, kind: GraphKind, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let canvas = cx.new(GraphCanvas::new);
        let rename = cx.new(|cx| InputState::new(window, cx).placeholder("name"));
        let (pa, pb) = match kind {
            GraphKind::Flow => ("new def: name(x: T) -> R", ""),
            GraphKind::Data => ("name", "type"),
        };
        let a = cx.new(|cx| InputState::new(window, cx).placeholder(pa));
        let b = cx.new(|cx| InputState::new(window, cx).placeholder(pb));
        let mut subs = vec![
            cx.subscribe_in(&state, window, |this, _, ev: &AppEvent, window, cx| this.on_app_event(ev, window, cx)),
            cx.subscribe_in(&canvas, window, |this, _, ev: &CanvasEvent, window, cx| this.on_canvas(ev, window, cx)),
            // synthesis, trace and spec results arrive as plain notifies on the state
            cx.observe(&state, |this, _, cx| this.refresh_status(cx)),
        ];
        subs.push(cx.subscribe_in(&rename, window, |this, _, ev: &InputEvent, window, cx| {
            if let InputEvent::PressEnter { .. } = ev {
                this.do_rename(window, cx);
            }
        }));
        for inp in [a.clone(), b.clone()] {
            subs.push(cx.subscribe_in(&inp, window, |this, _, ev: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { .. } = ev {
                    this.do_add(window, cx);
                }
            }));
        }
        let mut this =
            GraphPanel {
            state,
            kind,
            focus: cx.focus_handle(),
            canvas,
            whole_project: kind == GraphKind::Data,
            overlay: false,
            show_calls: false,
            rename,
            a,
            b,
            _subs: subs,
        };
        this.rebuild(cx);
        this
    }

    #[cfg(feature = "snapshots")]
    pub fn canvas(&self) -> Entity<GraphCanvas> {
        self.canvas.clone()
    }

    fn model<'a>(&self, cx: &'a App) -> &'a Model {
        &self.state.read(cx).project.display
    }

    fn rebuild(&mut self, cx: &mut Context<Self>) {
        let st = self.state.read(cx);
        let p = &st.project;
        let graph = match self.kind {
            GraphKind::Flow => {
                let module = if self.whole_project { None } else { p.display.files.get(st.active_file).map(|f| f.module.clone()) };
                wh_graph::flow_graph(&p.display, &FlowOptions { module, hide_calls: !self.show_calls })
            }
            GraphKind::Data => wh_graph::data_model_graph(&p.display, &DataOptions { module: None, overlay: self.overlay }),
        };
        let pos = wh_graph::place(&graph, &p.layout.positions(self.kind.key()));
        let ghosts = p.ghosts.clone();
        let executed = st.executed.clone();
        let chips = node_chips(&graph, st);
        let values = edge_values(st);
        let selection = p.selection.clone();
        let accepts = self.accepts(cx);
        let can_drop = self.can_drop(cx);
        self.canvas.update(cx, |c, cx| {
            c.ghosts = ghosts;
            c.executed = executed;
            c.chips = chips;
            c.edge_values = values;
            c.accepts = accepts;
            c.can_drop = can_drop;
            c.set_graph(graph, pos, cx);
            if c.selected_edge.is_none() {
                c.select_id(selection.as_deref(), false, cx);
            }
        });
        cx.notify();
    }

    /// Port compatibility from the model's types.
    fn accepts(&self, cx: &App) -> crate::canvas::Accepts {
        let model = self.model(cx).clone();
        match self.kind {
            GraphKind::Flow => Rc::new(move |g: &Graph, from: PortRef, to: PortRef| {
                let src = &g.nodes[from.node];
                if !matches!(src.kind, GNodeKind::Step | GNodeKind::Param) {
                    return false;
                }
                let Some(ty) = from.row.and_then(|r| src.rows[r].ty.clone()) else { return false };
                let dst = &g.nodes[to.node];
                let first_param = |id: &str| -> Option<Ty> { model.nodes.get(id)?.sig()?.params.first().map(|p| p.ty.clone()) };
                let port_ty = match (dst.kind, to.row) {
                    (GNodeKind::Def | GNodeKind::Flow, _) => first_param(&dst.id),
                    (GNodeKind::Step, Some(r)) if dst.rows[r].port == Some(PortDir::In) => dst.rows[r].ty.clone(),
                    (GNodeKind::Step, _) => dst.callee.as_deref().and_then(first_param),
                    _ => return false,
                };
                port_ty.is_some_and(|p| wh_graph::port_accepts(&model, &p, &ty))
            }),
            GraphKind::Data => Rc::new(|g: &Graph, from: PortRef, to: PortRef| {
                g.nodes[from.node].kind == GNodeKind::Struct
                    && matches!(g.nodes[to.node].kind, GNodeKind::Struct | GNodeKind::Enum | GNodeKind::ErrorEnum | GNodeKind::Refined | GNodeKind::Alias)
            }),
        }
    }

    /// Flow graph: a `|>` step may be dropped onto another step of the same pipeline.
    fn can_drop(&self, cx: &App) -> crate::canvas::CanDrop {
        let model = self.state.read(cx).project.model.clone();
        if self.kind != GraphKind::Flow {
            return Rc::new(|_, _, _| false);
        }
        Rc::new(move |g: &Graph, a: usize, b: usize| {
            let (na, nb) = (&g.nodes[a], &g.nodes[b]);
            na.kind == GNodeKind::Step
                && nb.kind == GNodeKind::Step
                && na.model_node == nb.model_node
                && match (pipe_pos(&model, na), pipe_pos(&model, nb)) {
                    (Some((s1, Some(_))), Some((s2, Some(_)))) => s1 == s2,
                    _ => false,
                }
        })
    }

    /// Synthesis, spec and trace state changed: refresh chips and the executed path.
    fn refresh_status(&mut self, cx: &mut Context<Self>) {
        let st = self.state.read(cx);
        let executed = st.executed.clone();
        let chips = node_chips(&self.canvas.read(cx).graph, st);
        let values = edge_values(st);
        self.canvas.update(cx, |c, cx| {
            if c.chips != chips || c.executed != executed || c.edge_values != values {
                c.chips = chips;
                c.executed = executed;
                c.edge_values = values;
                cx.notify();
            }
        });
    }

    fn on_app_event(&mut self, ev: &AppEvent, window: &mut Window, cx: &mut Context<Self>) {
        match ev {
            AppEvent::Changed(_) | AppEvent::Results | AppEvent::Files => self.rebuild(cx),
            AppEvent::ActiveFile if self.kind == GraphKind::Flow && !self.whole_project => self.rebuild(cx),
            AppEvent::Selected { id, from, .. } => {
                let center = *from != SelectionSource::Graph;
                let id = id.clone();
                self.canvas.update(cx, |c, cx| {
                    // the data model shows the type, the flow graph the definition or step
                    let found = id.as_deref().and_then(|i| c.graph.index_of(i).map(|_| i.to_string()).or_else(|| i.split('#').next().filter(|b| c.graph.index_of(b).is_some()).map(String::from)));
                    c.select_id(found.as_deref(), center, cx);
                });
                self.sync_rename(window, cx);
            }
            _ => {}
        }
        // trace/synthesis state changes arrive as notifies on the state
        cx.notify();
    }

    fn sync_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = self.selected(cx).map(|n| n.title.clone()).unwrap_or_default();
        self.rename.update(cx, |s, cx| s.set_value(name, window, cx));
    }

    fn selected<'a>(&self, cx: &'a App) -> Option<&'a wh_graph::GNode> {
        let c = self.canvas.read(cx);
        c.selected_node.and_then(|i| c.graph.nodes.get(i))
    }

    fn on_canvas(&mut self, ev: &CanvasEvent, window: &mut Window, cx: &mut Context<Self>) {
        let graph = self.canvas.read(cx).graph.clone();
        match ev {
            CanvasEvent::SelectNode(i) => {
                let id = graph.nodes[*i].id.clone();
                self.state.update(cx, |s, cx| s.select(Some(id), true, SelectionSource::Graph, cx));
                self.sync_rename(window, cx);
            }
            CanvasEvent::SelectEdge(_) | CanvasEvent::ClearSelection => cx.notify(),
            CanvasEvent::Moved(i, p) => {
                let id = graph.nodes[*i].id.clone();
                let key = self.kind.key();
                self.state.update(cx, |s, _| {
                    s.set_position(key, &id, *p);
                    let _ = s.project.layout.save(&s.project.dir);
                });
            }
            CanvasEvent::DoubleClick(_) => {
                self.rename.update(cx, |s, cx| s.focus(window, cx));
            }
            CanvasEvent::DropOnto { node, target } => {
                let model = self.state.read(cx).project.model.clone();
                let (a, b) = (&graph.nodes[*node], &graph.nodes[*target]);
                let flow = a.model_node.clone().unwrap_or_default();
                if let (Some((stmt, Some(from))), Some((_, Some(to)))) = (pipe_pos(&model, a), pipe_pos(&model, b)) {
                    self.move_step(flow, stmt, from, to, cx);
                } else {
                    self.rebuild(cx);
                }
            }
            CanvasEvent::Connect { from, to } => {
                if let Some(op) = self.connect_op(&graph, *from, *to, cx) {
                    self.apply(op, cx);
                }
            }
        }
    }

    fn apply(&mut self, op: Op, cx: &mut Context<Self>) -> bool {
        self.state.update(cx, |s, cx| s.apply_op(Origin::Graph, op, cx))
    }

    /// Reorder a pipeline; the flow's steps go back to automatic positions.
    fn move_step(&mut self, flow: String, stmt: usize, from: usize, to: usize, cx: &mut Context<Self>) {
        let prefix = format!("{flow}#");
        self.state.update(cx, |s, _| {
            if let Some(g) = s.project.layout.graphs.get_mut("flow") {
                g.positions.retain(|k, _| !k.starts_with(&prefix) || k.contains("#in:"));
            }
        });
        if !self.apply(Op::MoveFlowStep { flow, stmt, from, to }, cx) {
            self.rebuild(cx);
        }
    }

    /// Delete / Backspace in a graph.
    fn delete_selected(&mut self, cx: &mut Context<Self>) {
        if let Some((owner, field)) = self.selected_edge(cx).and_then(|e| e.field).filter(|_| self.selected(cx).is_none()) {
            self.apply(Op::RemoveField { owner, field }, cx);
            return;
        }
        let Some(n) = self.selected(cx).cloned() else { return };
        let model = self.state.read(cx).project.model.clone();
        match n.kind {
            GNodeKind::Step => match pipe_pos(&model, &n) {
                Some((stmt, Some(position))) => {
                    self.apply(Op::RemoveFlowStep { flow: n.model_node.clone().unwrap_or_default(), stmt, position }, cx);
                }
                _ => self.state.update(cx, |s, cx| {
                    s.push_log(crate::backend::LogLevel::Warn, "edit", format!("`{}` is not a `|>` step; edit its statement in the document", n.title), None);
                    cx.notify();
                }),
            },
            GNodeKind::Param => {}
            _ if n.badges.iter().any(|b| b == "external") => {}
            _ => {
                self.apply(Op::RemoveNode { node: n.id.clone() }, cx);
            }
        }
    }

    /// The op that feeds the output of `src` (a step or a flow parameter) into
    /// `callee`: a `|>` insert in pipeline form, a new binding in DAG form
    /// (when the step's statement binds a name and the step ends it).
    fn feed_op(&self, model: &Model, src: &wh_graph::GNode, callee_id: &str, callee: &str) -> Option<Op> {
        let flow = src.model_node.clone()?;
        let steps = steps_of(model, &flow)?;
        match src.kind {
            GNodeKind::Step => {
                let idx = steps.iter().position(|s| s.id == src.id)?;
                let (stmt, pos, binds, last) = wh_edit::step_site(model, &flow, idx)?;
                if let (Some(arg), true) = (binds.first(), last) {
                    let name = fresh_binding(model, &flow, callee_id);
                    return Some(Op::InsertFlowBinding { flow, index: stmt + 1, name, value: format!("{callee}({arg})") });
                }
                Some(Op::InsertFlowStep { flow, stmt, position: pos.map_or(0, |p| p + 1), callee: callee.to_string() })
            }
            GNodeKind::Param => {
                let dag = !steps.is_empty() && wh_edit::step_site(model, &flow, 0).is_some_and(|(_, _, b, _)| !b.is_empty());
                if dag {
                    let name = fresh_binding(model, &flow, callee_id);
                    return Some(Op::InsertFlowBinding { flow, index: 0, name, value: format!("{callee}({})", src.title) });
                }
                Some(Op::InsertFlowStep { flow, stmt: 0, position: 0, callee: callee.to_string() })
            }
            _ => None,
        }
    }

    /// Name to write for definition `id` from inside `module`.
    fn name_in(&self, model: &Model, module: &str, id: &str) -> Option<String> {
        let n = model.nodes.get(id)?;
        if model.resolve_name(module, &n.name).map(String::as_str) == Some(id) {
            return Some(n.name.clone());
        }
        let alias = model.scopes.get(module)?.module_aliases.iter().find(|(_, m)| **m == n.module).map(|(a, _)| a.clone())?;
        Some(format!("{alias}.{}", n.name))
    }

    fn connect_op(&self, g: &Graph, from: PortRef, to: PortRef, cx: &App) -> Option<Op> {
        let model = &self.state.read(cx).project.model;
        let src = &g.nodes[from.node];
        let dst = &g.nodes[to.node];
        match self.kind {
            GraphKind::Flow => {
                let flow = src.model_node.clone()?;
                let module = model.nodes.get(&flow)?.module.clone();
                let target = match dst.kind {
                    GNodeKind::Step => dst.callee.clone()?,
                    _ => dst.id.clone(),
                };
                let callee = self.name_in(model, &module, &target)?;
                self.feed_op(model, src, &target, &callee)
            }
            GraphKind::Data => {
                let owner = src.id.clone();
                let module = model.nodes.get(&owner)?.module.clone();
                let ty = self.name_in(model, &module, &dst.id)?;
                match from.row.and_then(|r| src.rows[r].field.clone()) {
                    Some(field) => Some(Op::RetypeField { owner, field, ty }),
                    None => {
                        let mut name = snake(&dst.title);
                        if let NodeKind::Struct(s) = &model.nodes.get(&owner)?.kind {
                            let base = name.clone();
                            let mut k = 2;
                            while s.fields.iter().any(|f| f.name == name) {
                                name = format!("{base}{k}");
                                k += 1;
                            }
                        }
                        Some(Op::AddField { owner, name, ty })
                    }
                }
            }
        }
    }

    fn do_rename(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let new_name = self.rename.read(cx).value().trim().to_string();
        let Some(n) = self.selected(cx) else { return };
        if new_name.is_empty() || new_name == n.title || matches!(n.kind, GNodeKind::Step | GNodeKind::Param) {
            return;
        }
        let node = n.id.clone();
        self.apply(Op::Rename { node, new_name }, cx);
    }

    fn do_add(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let a = self.a.read(cx).value().trim().to_string();
        let b = self.b.read(cx).value().trim().to_string();
        if a.is_empty() {
            return;
        }
        let st = self.state.read(cx);
        let file = st.project.files[st.active_file].clone();
        let sel = self.selected(cx).cloned();
        let section = sel.as_ref().and_then(|n| st.project.model.nodes.get(n.model_node.as_deref()?)).filter(|n| st.project.model.files[n.file].path == file).and_then(|n| n.section);
        let op = match (self.kind, sel.as_ref().map(|n| n.kind)) {
            (GraphKind::Flow, _) => Op::AddDef { file, section, signature: a, clauses: vec![] },
            (GraphKind::Data, Some(GNodeKind::Struct)) if !b.is_empty() => Op::AddField { owner: sel.unwrap().id, name: a, ty: b },
            (GraphKind::Data, Some(GNodeKind::Enum | GNodeKind::ErrorEnum)) => Op::AddVariant { owner: sel.unwrap().id, variant: a },
            (GraphKind::Data, _) => {
                let (fname, fty) = b.split_once(':').map(|(n, t)| (n.trim().to_string(), t.trim().to_string())).unwrap_or(("id".into(), "int".into()));
                Op::AddStruct { file, section, name: a, fields: vec![(fname, fty)] }
            }
        };
        self.apply(op, cx);
        self.a.update(cx, |s, cx| s.set_value("", window, cx));
        self.b.update(cx, |s, cx| s.set_value("", window, cx));
    }

    fn selected_edge(&self, cx: &App) -> Option<wh_graph::GEdge> {
        let c = self.canvas.read(cx);
        c.selected_edge.and_then(|i| c.graph.edges.get(i)).cloned()
    }

    fn toolbar(&mut self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let mut bar = h_flex().gap_1().px_2().py_1().flex_wrap().border_b_1().border_color(theme.border).text_xs();
        bar = bar
            .child(Button::new("fit").ghost().xsmall().label("Fit").on_click(cx.listener(|this, _, _, cx| {
                this.canvas.update(cx, |c, cx| {
                    c.fit();
                    cx.notify()
                })
            })))
            .child(Button::new("zin").ghost().xsmall().label("+").on_click(cx.listener(|this, _, _, cx| {
                this.canvas.update(cx, |c, cx| {
                    c.zoom_by(1.2, None);
                    cx.notify()
                })
            })))
            .child(Button::new("zout").ghost().xsmall().label("−").on_click(cx.listener(|this, _, _, cx| {
                this.canvas.update(cx, |c, cx| {
                    c.zoom_by(1.0 / 1.2, None);
                    cx.notify()
                })
            })));
        match self.kind {
            GraphKind::Flow => {
                bar = bar.child(Button::new("calls").xsmall().outline().toggled(self.show_calls).label("Calls").tooltip("Show which definition each step calls").on_click(
                    cx.listener(|this, _, _, cx| {
                        this.show_calls = !this.show_calls;
                        this.rebuild(cx);
                    }),
                ));
                bar = bar.child(
                    Button::new("scope").xsmall().outline().label(if self.whole_project { "Whole project" } else { "Active module" }).on_click(cx.listener(
                        |this, _, _, cx| {
                            this.whole_project = !this.whole_project;
                            this.rebuild(cx);
                            this.canvas.update(cx, |c, cx| {
                                c.fit();
                                cx.notify()
                            });
                        },
                    )),
                )
            }
            GraphKind::Data => {
                bar = bar.child(Button::new("overlay").xsmall().outline().toggled(self.overlay).label("Producers/consumers").on_click(cx.listener(
                    |this, _, _, cx| {
                        this.overlay = !this.overlay;
                        this.rebuild(cx);
                    },
                )))
            }
        }
        bar = bar.child(div().w(px(12.)));
        let sel = self.selected(cx).cloned();
        let edge = self.selected_edge(cx);
        let model = self.state.read(cx).project.model.clone();
        if let Some(e) = edge.filter(|_| sel.is_none()) {
            if let Some((owner, field)) = e.field.clone() {
                bar = bar.child(div().text_color(theme.muted_foreground).child(format!("{}.{field}:", owner.rsplit('.').next().unwrap_or(&owner))));
                // a segmented control showing the current value; each option previews the text it writes (M1)
                let current = current_card(&model, &owner, &field, e.cardinality.as_ref());
                let target = self.canvas.read(cx).graph.nodes.get(e.to.node).map(|n| n.title.clone()).unwrap_or_else(|| "T".into());
                let options = [("1", EditCard::One), ("0..1", EditCard::Optional), ("0..*", EditCard::Many), ("set", EditCard::Set)];
                let mut group = ButtonGroup::new("cardinality").xsmall();
                for (label, card) in options {
                    let preview = match card {
                        EditCard::One => target.clone(),
                        EditCard::Optional => format!("{target} | None"),
                        EditCard::Many => format!("list[{target}]"),
                        EditCard::Set => format!("set[{target}]"),
                    };
                    group = group.child(
                        Button::new(SharedString::from(format!("card-{label}")))
                            .label(label)
                            .selected(current == Some(card))
                            .tooltip(format!("{field}: {preview}")),
                    );
                }
                let (o, f) = (owner.clone(), field.clone());
                bar = bar.child(group.on_click(cx.listener(move |this, ix: &Vec<usize>, _, cx| {
                    if let Some(&k) = ix.first() {
                        let card = options[k].1;
                        if current != Some(card) {
                            this.apply(Op::SetCardinality { owner: o.clone(), field: f.clone(), cardinality: card }, cx);
                        }
                    }
                })));
                let (o, f) = (owner.clone(), field.clone());
                bar = bar.child(Button::new("rmfield").xsmall().danger().label("Remove field").on_click(cx.listener(move |this, _, _, cx| {
                    this.apply(Op::RemoveField { owner: o.clone(), field: f.clone() }, cx);
                })));
            } else {
                bar = bar.child(div().text_color(theme.muted_foreground).child(format!("{} edge", e.kind.label())));
            }
            return bar;
        }
        match sel {
            Some(n) if n.error.is_some() => {
                let t = crate::tokens::tokens(cx);
                bar = bar.child(div().text_color(t.danger).child(format!("✗ {}", n.error.clone().unwrap_or_default())));
                let module = n.model_node.as_ref().and_then(|f| model.nodes.get(f)).map(|f| f.module.clone()).unwrap_or_default();
                if let Some(fix) = wh_graph::flow::did_you_mean(&model, &module, &n.title) {
                    let path = model.files[n.file].path.clone();
                    let span = n.span;
                    bar = bar.child(Button::new("fix-callee").xsmall().primary().label(format!("Use {fix}")).on_click(cx.listener(move |this, _, _, cx| {
                        let (path, fix) = (path.clone(), fix.clone());
                        this.state.update(cx, |s, cx| s.edit_text(&path, span.range(), &fix, cx));
                    })));
                }
                let name = n.title.clone();
                bar = bar.child(Button::new("ask-create").xsmall().ghost().label("Ask agent to create it").on_click(cx.listener(move |this, _, _, cx| {
                    let text = format!("The flow calls `{name}`, which does not exist. Add a `def {name}` with a signature that fits this step, in the right section.");
                    this.state.update(cx, |s, cx| s.ask_author(text, cx));
                })));
            }
            Some(n) if n.kind == GNodeKind::Step => {
                let flow = n.model_node.clone().unwrap_or_default();
                let pos = pipe_pos(&model, &n);
                bar = bar.child(div().font_weight(FontWeight::SEMIBOLD).child(format!("step {}", n.title)));
                if let Some((stmt, Some(position))) = pos {
                    let len = pipeline_len(&model, &flow, stmt);
                    let f = flow.clone();
                    bar = bar.child(Button::new("mvl").xsmall().ghost().label("◀").disabled(position == 0).on_click(
                        cx.listener(move |this, _, _, cx| this.move_step(f.clone(), stmt, position, position - 1, cx)),
                    ));
                    let f = flow.clone();
                    bar = bar.child(Button::new("mvr").xsmall().ghost().label("▶").disabled(position + 1 >= len).on_click(
                        cx.listener(move |this, _, _, cx| this.move_step(f.clone(), stmt, position, position + 1, cx)),
                    ));
                    let f = flow.clone();
                    bar = bar.child(Button::new("rmstep").xsmall().danger().label("Remove step").on_click(cx.listener(move |this, _, _, cx| {
                        this.apply(Op::RemoveFlowStep { flow: f.clone(), stmt, position }, cx);
                    })));
                }
                // palette: defs whose input accepts this step's output
                let out_ty = n.rows.last().and_then(|r| r.ty.clone());
                if let Some(ty) = out_ty {
                    let module = model.nodes.get(&flow).map(|f| f.module.clone()).unwrap_or_default();
                    bar = bar.child(div().text_color(theme.muted_foreground).child("insert after:"));
                    let mut k = 0;
                    for d in model.nodes.values().filter(|d| matches!(d.kind, NodeKind::Def(_) | NodeKind::Flow(_)) && d.id != flow) {
                        let ok = d.sig().and_then(|s| s.params.first()).is_some_and(|prm| wh_graph::port_accepts(&model, &prm.ty, &ty));
                        let Some(name) = self.name_in(&model, &module, &d.id) else { continue };
                        if !ok || k >= 8 {
                            continue;
                        }
                        let Some(op) = self.feed_op(&model, &n, &d.id, &name) else { continue };
                        k += 1;
                        bar = bar.child(Button::new(SharedString::from(format!("ins-{}", d.id))).xsmall().outline().label(name.clone()).on_click(
                            cx.listener(move |this, _, _, cx| {
                                this.apply(op.clone(), cx);
                            }),
                        ));
                    }
                }
            }
            Some(n) if n.kind != GNodeKind::Param && !n.badges.iter().any(|b| b == "external") => {
                bar = bar
                    .child(div().text_color(theme.muted_foreground).child(n.kind.label()))
                    .child(div().w(px(140.)).child(Input::new(&self.rename).xsmall()))
                    .child(Button::new("rename").xsmall().label("Rename").on_click(cx.listener(|this, _, w, cx| this.do_rename(w, cx))));
                match (self.kind, n.kind) {
                    (GraphKind::Data, GNodeKind::Struct) => {
                        bar = bar
                            .child(div().w(px(90.)).child(Input::new(&self.a).xsmall()))
                            .child(div().w(px(110.)).child(Input::new(&self.b).xsmall()))
                            .child(Button::new("addf").xsmall().label("Add field").on_click(cx.listener(|this, _, w, cx| this.do_add(w, cx))));
                    }
                    (GraphKind::Data, GNodeKind::Enum | GNodeKind::ErrorEnum) => {
                        bar = bar
                            .child(div().w(px(120.)).child(Input::new(&self.a).xsmall()))
                            .child(Button::new("addv").xsmall().label("Add variant").on_click(cx.listener(|this, _, w, cx| this.do_add(w, cx))));
                    }
                    (GraphKind::Flow, GNodeKind::Def) => {
                        let id = n.id.clone();
                        bar = bar.child(Button::new("synth").xsmall().outline().label("Synthesize").on_click(
                            cx.listener(move |this, _, _, cx| this.state.update(cx, |s, cx| s.synthesize(id.clone(), cx))),
                        ));
                    }
                    _ => {}
                }
                let id = n.id.clone();
                bar = bar.child(Button::new("del").xsmall().danger().label("Delete").on_click(cx.listener(move |this, _, _, cx| {
                    this.apply(Op::RemoveNode { node: id.clone() }, cx);
                })));
            }
            _ => {
                let label = match self.kind {
                    GraphKind::Flow => "Add def",
                    GraphKind::Data => "Add struct",
                };
                bar = bar.child(div().w(px(220.)).child(Input::new(&self.a).xsmall()));
                if self.kind == GraphKind::Data {
                    bar = bar.child(div().w(px(110.)).child(Input::new(&self.b).xsmall()));
                }
                bar = bar.child(Button::new("add").xsmall().primary().label(label).on_click(cx.listener(|this, _, w, cx| this.do_add(w, cx))));
            }
        }
        bar
    }
}

fn steps_of<'m>(model: &'m Model, flow: &str) -> Option<&'m Vec<wh_model::FlowStep>> {
    match &model.nodes.get(flow)?.kind {
        NodeKind::Flow(f) => Some(&f.steps),
        NodeKind::App(a) => a.entry.as_ref().map(|e| &e.steps),
        _ => None,
    }
}

/// `(stmt, |> position)` of a step graph node (see `wh_edit::pipeline_position`).
fn pipe_pos(model: &Model, n: &wh_graph::GNode) -> Option<(usize, Option<usize>)> {
    let flow = n.model_node.as_deref()?;
    let idx = steps_of(model, flow)?.iter().position(|s| s.id == n.id)?;
    wh_edit::pipeline_position(model, flow, idx)
}

/// Number of `|>` steps in statement `stmt` of `flow`.
fn pipeline_len(model: &Model, flow: &str, stmt: usize) -> usize {
    let n = steps_of(model, flow).map_or(0, |s| s.len());
    (0..n).filter(|&i| matches!(wh_edit::pipeline_position(model, flow, i), Some((s, Some(_))) if s == stmt)).count()
}

/// A binding name for the result of `callee` that `flow` does not use yet.
fn fresh_binding(model: &Model, flow: &str, callee: &str) -> String {
    let ret = model.nodes.get(callee).and_then(|n| n.sig()).map(|s| s.ret.to_string()).unwrap_or_default();
    let short = callee.rsplit('.').next().unwrap_or(callee);
    let base = if !ret.is_empty() && ret.chars().all(|c| c.is_ascii_alphanumeric()) && ret != "unit" {
        crate::config::snake_ident(&ret)
    } else {
        format!("{short}_out")
    };
    let mut used: Vec<String> = steps_of(model, flow).map(|ss| ss.iter().flat_map(|s| s.bindings.clone()).collect()).unwrap_or_default();
    if let Some(sig) = model.nodes.get(flow).and_then(|n| n.sig()) {
        used.extend(sig.params.iter().map(|p| p.name.clone()));
    }
    let mut name = base.clone();
    let mut k = 2;
    while used.contains(&name) || wh_syntax::is_hard_keyword(&name) {
        name = format!("{base}{k}");
        k += 1;
    }
    name
}

/// The first live value seen leaving each step (L-a: values on wires).
fn edge_values(st: &AppState) -> std::collections::BTreeMap<String, (String, bool)> {
    st.live.steps.iter().filter_map(|(k, v)| v.first().map(|s| (k.clone(), (s.value.clone(), s.error)))).collect()
}

/// The status chip per graph node: definitions show their own state, steps
/// their callee's (G3), from the one vocabulary in `capsule` (A1).
fn node_chips(graph: &Graph, st: &AppState) -> std::collections::BTreeMap<String, crate::capsule::Chip> {
    let queue = crate::capsule::queue_positions(&st.synthesis);
    graph
        .nodes
        .iter()
        .filter_map(|n| {
            let key = match n.kind {
                GNodeKind::Step => n.callee.as_deref()?,
                GNodeKind::Param => return None,
                _ => n.id.as_str(),
            };
            let synth = st.synthesis.iter().find(|(id, _)| id == key).map(|(_, s)| s);
            let chip = crate::capsule::node_chip(synth, queue.get(key).copied(), st.tests.get(key))?;
            Some((n.id.clone(), chip))
        })
        .collect()
}

/// The cardinality a field has now, as the toolbar's options name it.
fn current_card(model: &Model, owner: &str, field: &str, card: Option<&wh_graph::Cardinality>) -> Option<EditCard> {
    let is_set = match &model.nodes.get(owner)?.kind {
        NodeKind::Struct(s) => s.fields.iter().find(|f| f.name == field).is_some_and(|f| matches!(f.ty, Ty::Set(_) | Ty::SortedSet(_))),
        _ => false,
    };
    match card? {
        wh_graph::Cardinality::One => Some(EditCard::One),
        wh_graph::Cardinality::ZeroOrOne => Some(EditCard::Optional),
        wh_graph::Cardinality::Many if is_set => Some(EditCard::Set),
        wh_graph::Cardinality::Many => Some(EditCard::Many),
        _ => None,
    }
}

fn snake(s: &str) -> String {
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

impl Render for GraphPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let legend = match self.kind {
            GraphKind::Flow => "drag a step's output (right edge) onto a def to insert it · drag a step onto another to reorder · ⌫ delete · F2 rename · ⌘-scroll to zoom",
            GraphKind::Data => "drag from a struct's right edge onto a type to add a field · click an edge to change its cardinality · ⌫ delete · F2 rename",
        };
        let theme = cx.theme().clone();
        crate::palette::on_palette(v_flex(), &self.state)
            .size_full()
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &DeleteSelected, _, cx| this.delete_selected(cx)))
            .on_action(cx.listener(|this, _: &RenameSelected, window, cx| {
                if this.selected(cx).is_some_and(|n| !matches!(n.kind, GNodeKind::Step | GNodeKind::Param)) {
                    this.rename.update(cx, |s, cx| s.focus(window, cx));
                }
            }))
            .child(self.toolbar(cx))
            .child(div().flex_1().min_h_0().child(self.canvas.clone()))
            .child(div().px_2().py_0p5().text_xs().text_color(theme.muted_foreground).border_t_1().border_color(theme.border).child(legend))
    }
}

// two dock panels over the same widget
pub struct FlowGraphPanel {
    pub inner: Entity<GraphPanel>,
    focus: FocusHandle,
}
pub struct DataModelPanel {
    pub inner: Entity<GraphPanel>,
    focus: FocusHandle,
}

impl FlowGraphPanel {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        FlowGraphPanel { inner: cx.new(|cx| GraphPanel::new(state, GraphKind::Flow, window, cx)), focus: cx.focus_handle() }
    }
}
impl DataModelPanel {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        DataModelPanel { inner: cx.new(|cx| GraphPanel::new(state, GraphKind::Data, window, cx)), focus: cx.focus_handle() }
    }
}
crate::dock_panel!(FlowGraphPanel, "wh.flow", "Flow graph");
crate::dock_panel!(DataModelPanel, "wh.data", "Data model");
impl Render for FlowGraphPanel {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().track_focus(&self.focus).child(self.inner.clone())
    }
}
impl Render for DataModelPanel {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().track_focus(&self.focus).child(self.inner.clone())
    }
}
