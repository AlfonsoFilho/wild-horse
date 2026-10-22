//! The Inspector: the selected node's signature, clauses, pattern, prose and
//! Rust mapping, with editable fields that emit `Op`s (`Origin::Inspector`).

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Icon, IconName};
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::{ActiveTheme, Disableable, Sizable, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use wh_doc::Origin;
use wh_edit::Op;
use wh_model::{Node, NodeKind};
use wh_syntax::ast::{ClauseKind, ItemKind};

use crate::capsule;
use crate::state::{AppEvent, AppState, SelectionSource};
use crate::tokens::tokens;

pub struct InspectorPanel {
    state: Entity<AppState>,
    focus: FocusHandle,
    /// Node the inputs were built for, with the doc version they reflect.
    built_for: Option<(String, u64)>,
    signature: Option<Entity<InputState>>,
    /// One auto-growing editor per clause, with the source it was built from.
    clauses: Vec<(Entity<TextareaState>, String)>,
    new_clause: Option<Entity<InputState>>,
    pattern: Option<Entity<InputState>>,
    _subs: Vec<Subscription>,
    _input_subs: Vec<Subscription>,
}

crate::dock_panel!(InspectorPanel, "wh.inspector", "Inspector");

fn src(n: &Node, text: &str, span: wh_syntax::Span) -> String {
    let _ = n;
    text.get(span.range()).unwrap_or("").to_string()
}

impl InspectorPanel {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let sub = cx.subscribe_in(&state, window, |this, _, ev: &AppEvent, window, cx| match ev {
            AppEvent::Changed(_) | AppEvent::Selected { .. } => this.rebuild(window, cx),
            _ => {}
        });
        let state_for_observe = state.clone();
        let mut this = InspectorPanel {
            state,
            focus: cx.focus_handle(),
            built_for: None,
            signature: None,
            clauses: vec![],
            new_clause: None,
            pattern: None,
            _subs: vec![sub, cx.observe(&state_for_observe, |_, _, cx| cx.notify())],
            _input_subs: vec![],
        };
        this.rebuild(window, cx);
        this
    }

    fn node<'a>(&self, cx: &'a App) -> Option<&'a Node> {
        self.state.read(cx).project.selected_node()
    }

    fn rebuild(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let st = self.state.read(cx);
        let version = st.project.docs.version();
        let Some(n) = st.project.selected_node().cloned() else {
            self.built_for = None;
            self.signature = None;
            self.clauses.clear();
            self.new_clause = None;
            self.pattern = None;
            cx.notify();
            return;
        };
        if self.built_for.as_ref() == Some(&(n.id.clone(), version)) {
            cx.notify();
            return;
        }
        let text = st.project.display.files[n.file].text.clone();
        self.built_for = Some((n.id.clone(), version));
        self._input_subs.clear();
        let input = |v: String, placeholder: &'static str, window: &mut Window, cx: &mut Context<Self>| {
            cx.new(|cx| InputState::new(window, cx).placeholder(placeholder).default_value(v))
        };
        let sig_span = match &n.item.kind {
            ItemKind::Def(d) => Some(d.sig.span),
            ItemKind::Flow(f) => Some(f.sig.span),
            _ => None,
        };
        self.signature = sig_span.map(|s| input(src(&n, &text, s), "signature", window, cx));
        if let Some(sig) = &self.signature {
            let id = n.id.clone();
            let orig = src(&n, &text, sig_span.unwrap());
            self._input_subs.push(cx.subscribe_in(sig, window, move |this, inp, ev: &InputEvent, _, cx| {
                if matches!(ev, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                    let signature = inp.read(cx).value().trim().to_string();
                    if signature != orig.trim() {
                        this.apply(Op::EditSignature { node: id.clone(), signature }, cx);
                    }
                }
            }));
        }
        self.clauses.clear();
        self.new_clause = None;
        self.pattern = None;
        if let ItemKind::Def(d) = &n.item.kind {
            for (i, c) in d.clauses.iter().enumerate() {
                let source = src(&n, &text, c.span);
                let lines = source.lines().count().max(1);
                let inp = cx.new(|cx| TextareaState::new(window, cx).auto_grow(lines, lines.max(12)).soft_wrap(true).default_value(source.clone()));
                let id = n.id.clone();
                let orig = source.clone();
                // commit on blur, so a click elsewhere never loses an edit (I1)
                self._input_subs.push(cx.subscribe_in(&inp, window, move |this, inp, ev: &InputEvent, _, cx| {
                    if let InputEvent::Blur = ev {
                        let clause = inp.read(cx).value().trim_end().to_string();
                        if clause != orig.trim_end() && !clause.trim().is_empty() {
                            this.apply(Op::ReplaceClause { node: id.clone(), index: i, clause }, cx);
                        }
                    }
                }));
                self.clauses.push((inp, source));
            }
            let nc = input(String::new(), "add clause: ensures …, example …, note \"…\"", window, cx);
            let id = n.id.clone();
            self._input_subs.push(cx.subscribe_in(&nc, window, move |this, inp, ev: &InputEvent, _, cx| {
                if let InputEvent::PressEnter { .. } = ev {
                    let clause = inp.read(cx).value().trim().to_string();
                    if !clause.is_empty() {
                        this.apply(Op::AddClause { node: id.clone(), clause }, cx);
                    }
                }
            }));
            self.new_clause = Some(nc);
            let pat = input(n.patterns.first().cloned().unwrap_or_default(), "pattern (e.g. Transform)", window, cx);
            let id = n.id.clone();
            let orig = n.patterns.first().cloned().unwrap_or_default();
            self._input_subs.push(cx.subscribe_in(&pat, window, move |this, inp, ev: &InputEvent, _, cx| {
                if matches!(ev, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                    let p = inp.read(cx).value().trim().to_string();
                    if p != orig {
                        this.apply(Op::SetPattern { node: id.clone(), pattern: (!p.is_empty()).then_some(p) }, cx);
                    }
                }
            }));
            self.pattern = Some(pat);
        }
        cx.notify();
    }

    fn node_id(&self) -> Option<String> {
        self.built_for.as_ref().map(|(id, _)| id.clone())
    }



    fn commit_new_clause(&mut self, cx: &mut Context<Self>) {
        let (Some(node), Some(c)) = (self.node_id(), &self.new_clause) else { return };
        let clause = c.read(cx).value().trim().to_string();
        if !clause.is_empty() {
            self.apply(Op::AddClause { node, clause }, cx);
        }
    }


    fn apply(&mut self, op: Op, cx: &mut Context<Self>) {
        self.state.update(cx, |s, cx| s.apply_op(Origin::Inspector, op, cx));
    }

    /// Section heading: 11 px, 600, uppercase, `text.tertiary` (§15.4).
    fn section(title: &str, cx: &App) -> Div {
        div().mt_4().mb_1().text_size(px(11.)).font_weight(FontWeight::SEMIBOLD).text_color(tokens(cx).text_tertiary).child(title.to_uppercase())
    }
}

impl Render for InspectorPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let mono = theme.mono_font_family.clone();
        let root = crate::palette::on_palette(v_flex(), &self.state).size_full().track_focus(&self.focus).text_sm();
        let Some(n) = self.node(cx).cloned() else {
            return root.child(div().p_4().text_color(theme.muted_foreground).child("Select a node in any view."));
        };
        let st = self.state.read(cx);
        let sel = st.project.selection.clone().unwrap_or_default();
        let ghost = st.project.ghosts.contains(&n.id);
        let file = &st.project.display.files[n.file];
        let (line, _) = wh_syntax::span::line_col(&file.text, n.span.start);
        let rust = st.backend.generated_rust(&st.project.model, &n.id).unwrap_or_else(|| crate::rustgen::preview(&st.project.display, &n));
        let synth = st.synthesis.iter().find(|(id, _)| *id == n.id).map(|(_, s)| s.label());
        let specs = st.tests.get(&n.id).cloned();
        let trace: Vec<String> = st
            .trace
            .iter()
            .rev()
            .filter(|(_, t)| t.node.as_deref().is_some_and(|x| x == n.id || x.starts_with(&format!("{}#", n.id))))
            .take(8)
            .map(|(_, t)| format!("{} {}{}", t.level.label(), t.message, t.duration.map(|d| format!(" ({} µs)", d.as_micros())).unwrap_or_default()))
            .collect();
        let step = if sel.contains('#') {
            match &n.kind {
                NodeKind::Flow(f) => f.steps.iter().find(|s| s.id == sel).cloned(),
                NodeKind::App(a) => a.entry.as_ref().and_then(|e| e.steps.iter().find(|s| s.id == sel).cloned()),
                _ => None,
            }
        } else {
            None
        };
        let path = file.path.clone();

        let t = tokens(cx);
        let queue = capsule::queue_positions(&st.synthesis);
        let synth_status = st.synthesis.iter().find(|(id, _)| *id == n.id).map(|(_, s)| s.clone());
        let chip = capsule::node_chip(synth_status.as_ref(), queue.get(&n.id).copied(), specs.as_ref());
        let kind = capsule::kind_of(&n);
        let kcolor = t.kind(kind);
        let mut body = v_flex().w_full().min_w_0().p_3().gap_1();
        // title: the capsule of the selected thing; a step shows its own capsule, with the flow as a breadcrumb (I4)
        let (title_kind, title, sub) = match &step {
            Some(s) => {
                let k = match &n.kind {
                    NodeKind::Flow(f) => f.steps.iter().position(|x| x.id == s.id).map_or(0, |k| k + 1),
                    NodeKind::App(a) => a.entry.as_ref().and_then(|e| e.steps.iter().position(|x| x.id == s.id)).map_or(0, |k| k + 1),
                    _ => 0,
                };
                (wh_graph::GNodeKind::Step, s.label.clone(), format!("step {k} of {}", n.name))
            }
            None => (kind, n.name.clone(), if n.is_error_type { "@error enum".to_string() } else { n.kind.label().to_string() }),
        };
        if step.is_some() {
            let flow = n.id.clone();
            body = body.child(
                div()
                    .id("crumb")
                    .text_size(px(12.))
                    .text_color(t.text_tertiary)
                    .cursor_pointer()
                    .hover(|d| d.text_color(t.accent))
                    .child(format!("{} › {}", n.module, n.name))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let flow = flow.clone();
                        this.state.update(cx, |st, cx| st.select(Some(flow), true, SelectionSource::Inspector, cx));
                    })),
            );
        }
        let step_chip = step.as_ref().and_then(|s| s.callee.as_ref()).and_then(|c| {
            let synth = st.synthesis.iter().find(|(id, _)| id == c).map(|(_, s)| s);
            capsule::node_chip(synth, queue.get(c).copied(), st.tests.get(c))
        });
        let head_chip = if step.is_some() { step_chip } else { chip.clone() };
        body = body.child(
            h_flex()
                .gap_2()
                .items_center()
                .child(div().text_size(px(16.)).font_weight(FontWeight::SEMIBOLD).text_color(t.kind(title_kind)).child(capsule::kind_glyph(title_kind)))
                .child(div().text_size(px(18.)).font_weight(FontWeight::SEMIBOLD).text_color(t.text_primary).child(title))
                .child(div().text_size(px(12.)).text_color(t.text_tertiary).child(sub))
                .child(div().flex_1())
                .when_some(head_chip.clone(), |d, c| {
                    let tip = c.detail.clone();
                    d.child(div().id("head-chip").tooltip(move |w, cx| Tooltip::new(tip.clone()).build(w, cx)).child(capsule::chip(&c, t, px(12.), false)))
                }),
        );
        body = body.child(div().text_size(px(12.)).text_color(t.text_tertiary).child(format!("{}:{}", path, line + 1)));
        if let Some(c) = head_chip.filter(|c| !c.is_quiet()) {
            body = body.child(div().mt_1().text_size(px(12.)).text_color(c.tone.color(t)).child(breakable(&c.detail)));
        }
        let _ = (synth, kcolor);
        if ghost {
            body = body.child(div().mt_1().p_2().rounded(px(6.)).bg(t.warning.opacity(0.1)).border_1().border_color(t.warning.opacity(0.4)).text_color(t.text_primary).text_size(px(12.)).child(
                "⚠ This block does not parse; showing the last valid version. Edits are disabled until it parses again.",
            ));
        }
        // a step whose callee does not resolve: the model has no step for it (G8)
        if let Some((name, at)) = sel.split_once("#?").and_then(|(_, r)| r.rsplit_once('@')) {
            let at: usize = at.parse().unwrap_or(0);
            let fix = wh_graph::flow::did_you_mean(&st.project.model, &n.module, name);
            let path = path.clone();
            body = body.child(Self::section("unresolved step", cx)).child(
                v_flex()
                    .gap_1()
                    .child(div().font_family(mono.clone()).text_size(px(12.5)).text_color(t.danger).line_through().child(format!("|> {name}")))
                    .child(div().text_size(px(12.)).text_color(t.text_primary).child(format!("✗ unknown function `{name}`")))
                    .when_some(fix, |d, fix| {
                        let (name, path) = (name.to_string(), path.clone());
                        d.child(h_flex().child(Button::new("use-fix").xsmall().primary().label(format!("Use {fix}")).on_click(cx.listener(move |this, _, _, cx| {
                            let (path, fix) = (path.clone(), fix.clone());
                            let range = at..at + name.len();
                            this.state.update(cx, |s, cx| s.edit_text(&path, range, &fix, cx));
                        }))))
                    }),
            );
        }
        if let Some(s) = &step {
            body = body.child(Self::section("flow step", cx)).child(
                v_flex()
                    .gap_1()
                    .child(div().font_family(mono.clone()).text_size(px(12.5)).child(format!("{} → {}{}", s.label, s.output, s.raises.as_ref().map(|r| format!(" raises {r}")).unwrap_or_default())))
                    .when_some(s.callee.clone(), |d, c| {
                        d.child(h_flex().child(Button::new("goto-callee").xsmall().outline().label(format!("Open def {}", c.rsplit('.').next().unwrap_or(&c))).on_click(cx.listener(move |this, _, _, cx| {
                            let c = c.clone();
                            this.state.update(cx, |st, cx| st.select(Some(c), true, SelectionSource::Inspector, cx));
                        }))))
                    }),
            );
        }
        if let Some(sig) = &self.signature {
            body = body.child(Self::section("signature", cx)).child(div().child(Input::new(sig).small().disabled(ghost)));
        }
        if !self.clauses.is_empty() || self.new_clause.is_some() {
            body = body.child(Self::section("contract & specs", cx));
            let kinds: Vec<ClauseKind> = match &n.item.kind {
                ItemKind::Def(d) => d.clauses.iter().map(|c| c.kind.clone()).collect(),
                _ => vec![],
            };
            let mut example = 0;
            for (i, (c, _)) in self.clauses.iter().enumerate() {
                let id = n.id.clone();
                // the right slot is test status, never an action (I1)
                let test = match kinds.get(i) {
                    Some(ClauseKind::Example { .. }) => {
                        example += 1;
                        Some(format!("example_{}:", example - 1))
                    }
                    Some(ClauseKind::Property { name, .. }) => Some(format!("property_{}:", name.name)),
                    _ => None,
                };
                let status = test.and_then(|key| {
                    let r = specs.as_ref()?;
                    let failure = r.failures.iter().find(|f| f.contains(&format!("__{key}")));
                    Some(match failure {
                        Some(f) => ("✗", t.danger, f.clone()),
                        None => ("✓", t.success, "passed in the last run".to_string()),
                    })
                });
                let status = status.unwrap_or(("◌", t.text_tertiary.opacity(if specs.is_some() { 1.0 } else { 0.0 }), "not run".to_string()));
                // live values (wh_live) are fresher than the last test run
                let live_kind = match kinds.get(i) {
                    Some(ClauseKind::Example { .. }) => Some(wh_live::ItemKind::Example),
                    Some(ClauseKind::Ensures(_)) => Some(wh_live::ItemKind::Ensures),
                    Some(ClauseKind::Property { .. }) => Some(wh_live::ItemKind::Property),
                    _ => None,
                };
                let live = live_kind.and_then(|k| st.live.get(&n.id, k, i)).map(capsule::live_chip);
                let status = match &live {
                    Some(c) => (c.glyph, c.tone.color(t), c.detail.clone()),
                    None => status,
                };
                body = body.child(
                    h_flex()
                        .gap_1()
                        .items_start()
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .child(Textarea::new(c).disabled(ghost))
                                .when_some(live.filter(|c| !c.text.is_empty()), |d, c| {
                                    d.child(div().px_1().pt_0p5().font_family(mono.clone()).text_size(px(11.5)).text_color(c.tone.color(t)).child(format!("{} {}", c.glyph, breakable(&c.text))))
                                }),
                        )
                        .child(
                            div()
                                .id(("clause-status", i))
                                .w(px(18.))
                                .pt_1()
                                .flex_none()
                                .text_center()
                                .font_weight(FontWeight::BOLD)
                                .text_color(status.1)
                                .tooltip({
                                    let tip = status.2.clone();
                                    move |w, cx| Tooltip::new(tip.clone()).build(w, cx)
                                })
                                .child(status.0),
                        )
                        .child(
                            Button::new(("rmclause", i))
                                .xsmall()
                                .ghost()
                                .icon(Icon::new(IconName::Delete))
                                .tooltip("Remove clause")
                                .disabled(ghost)
                                .on_click(cx.listener(move |this, _, _, cx| this.apply(Op::RemoveClause { node: id.clone(), index: i }, cx))),
                        ),
                );
            }
            if let Some(nc) = &self.new_clause {
                body = body.child(
                    h_flex()
                        .gap_1()
                        .child(div().flex_1().child(Input::new(nc).small().disabled(ghost)))
                        .child(Button::new("add-clause").xsmall().ghost().label("+ Add").on_click(cx.listener(|this, _, _, cx| this.commit_new_clause(cx)))),
                );
            }
        }
        if let Some(p) = &self.pattern {
            body = body.child(Self::section("pattern", cx)).child(div().child(Input::new(p).small().disabled(ghost)));
        }
        match &n.kind {
            NodeKind::Struct(s) => {
                body = body.child(Self::section("fields", cx));
                for f in &s.fields {
                    body = body.child(div().font_family(mono.clone()).text_xs().child(format!("{}{}: {}", if f.is_key { "key " } else { "" }, f.name, if f.written.is_empty() { f.ty.to_string() } else { f.written.clone() })));
                }
            }
            NodeKind::Enum(e) => {
                body = body.child(Self::section("variants", cx));
                for v in &e.variants {
                    let items: Vec<String> = v.items.iter().map(|i| format!("{}{}", if i.is_from { "from " } else { "" }, i.ty)).collect();
                    body = body.child(div().font_family(mono.clone()).text_xs().child(if items.is_empty() { v.name.clone() } else { format!("{}({})", v.name, items.join(", ")) }));
                }
            }
            _ => {}
        }
        if let Some(p) = &n.prose {
            body = body.child(Self::section("prose", cx)).child(div().text_color(theme.foreground).child(p.clone()));
        }
        body = body.child(Self::section("generated rust (preview)", cx)).child(
            div().p_2().rounded(px(8.)).bg(t.sunken).border_1().border_color(t.border_subtle).font_family(mono.clone()).text_size(px(12.)).whitespace_normal().child(rust),
        );
        let mut actions = h_flex().gap_1().mt_2();
        if n.is_hole() {
            let id = n.id.clone();
            actions = actions.child(Button::new("synth").small().primary().label("Synthesize").on_click(
                cx.listener(move |this, _, _, cx| this.state.update(cx, |s, cx| s.synthesize(id.clone(), cx))),
            ));
        }
        actions = actions.child(Button::new("check").small().outline().label("Run specs").on_click(cx.listener(|this, _, _, cx| this.state.update(cx, |s, cx| s.check(cx)))));
        body = body.child(Self::section("actions", cx)).child(actions);
        if !trace.is_empty() {
            body = body.child(Self::section("recent trace events", cx));
            for t in trace {
                body = body.child(div().font_family(mono.clone()).text_xs().child(t));
            }
        }
        root.child(div().flex_1().min_h_0().w_full().overflow_hidden().child(body.overflow_y_scrollbar()))
    }
}

/// Long identifiers such as test names (`signup__check_age__example_1`) have
/// no break opportunity; allow one after each `_` and `:`.
fn breakable(s: &str) -> String {
    s.replace('_', "_\u{200B}").replace(':', ":\u{200B}")
}
