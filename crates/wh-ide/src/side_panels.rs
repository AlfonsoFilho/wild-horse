//! The Agent panel (author chat + synthesis queue) and the Log panel (build
//! / agent log and the node-mapped run trace). UI only: everything goes
//! through the `Backend`.

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{ActiveTheme, Sizable, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;

use crate::backend::{ChatRole, LogLevel};
use crate::state::{AppEvent, AppState, ProblemLevel, SelectionSource};

pub struct AgentPanel {
    state: Entity<AppState>,
    focus: FocusHandle,
    input: Entity<InputState>,
    /// "Retry with note…" input, and the failed hole it is open for.
    note: Entity<InputState>,
    note_for: Option<String>,
    /// Context chips the user removed (until the selection or file changes).
    dropped_node: Option<String>,
    dropped_file: Option<String>,
    /// Activity groups (index of their first chat row) shown expanded.
    expanded: std::collections::BTreeSet<usize>,
    _subs: Vec<Subscription>,
}

crate::dock_panel!(AgentPanel, "wh.agent", "Agent");

/// Canned author requests for the selected node (UX review A3).
const NODE_ACTIONS: [(&str, &str); 4] = [
    ("Suggest properties", "Suggest property clauses (invariants that must hold for every input) for this definition, and add the ones that are clearly right."),
    ("Write prose", "Write or improve the prose that explains this definition: what it is for and why, for a reader of the document."),
    ("Review contracts", "Review this definition's contracts and examples: are they correct, complete and implementable? Fix what is wrong."),
    ("Split this def", "This definition does too much. Split it into smaller definitions (a flow over defs if that reads better), keeping its behaviour."),
];

impl AgentPanel {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Ask the author agent…"));
        let note = cx.new(|cx| InputState::new(window, cx).placeholder("a hint for the compiler agent"));
        let subs = vec![
            cx.observe(&state, |_, _, cx| cx.notify()),
            cx.subscribe_in(&input, window, |this, _, ev: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { .. } = ev {
                    this.send(window, cx);
                }
            }),
            cx.subscribe_in(&note, window, |this, _, ev: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { .. } = ev {
                    this.retry_with_note(window, cx);
                }
            }),
        ];
        AgentPanel {
            state,
            focus: cx.focus_handle(),
            input,
            note,
            note_for: None,
            dropped_node: None,
            dropped_file: None,
            expanded: Default::default(),
            _subs: subs,
        }
    }

    /// The context chips in effect: (selected node, active file).
    fn context(&self, cx: &App) -> (Option<String>, Option<String>) {
        let st = self.state.read(cx);
        let node = st.project.selected_node().map(|n| n.id.clone()).filter(|n| self.dropped_node.as_ref() != Some(n));
        let file = st.active_path().filter(|f| self.dropped_file.as_ref() != Some(f));
        (node, file)
    }

    fn ask(&mut self, text: String, cx: &mut Context<Self>) {
        let (node, file) = self.context(cx);
        let text = match file {
            Some(f) if node.is_none() => format!("(The user is looking at {f}.)\n{text}"),
            _ => text,
        };
        self.state.update(cx, |s, cx| s.ask_author_scoped(text, node, cx));
    }

    fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.input.read(cx).value().trim().to_string();
        if text.is_empty() {
            return;
        }
        self.ask(text, cx);
        self.input.update(cx, |s, cx| s.set_value("", window, cx));
    }

    fn retry_with_note(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.note.read(cx).value().trim().to_string();
        let Some(node) = self.note_for.take() else { return };
        if !text.is_empty() {
            self.state.update(cx, |s, cx| s.retry_with_note(node, &text, cx));
        }
        self.note.update(cx, |s, cx| s.set_value("", window, cx));
        cx.notify();
    }

    fn section(title: impl Into<SharedString>, cx: &App) -> Div {
        div().px_2().pt_2().text_xs().font_weight(FontWeight::SEMIBOLD).text_color(cx.theme().muted_foreground).child(title.into())
    }

    /// N7: one card per failed hole, with the gate, the evidence and the ways out.
    fn failure_cards(&self, cx: &mut Context<Self>) -> Option<Div> {
        let theme = cx.theme().clone();
        let mono = theme.mono_font_family.clone();
        let failures: Vec<crate::backend::SynthFailure> = self.state.read(cx).failures.values().cloned().collect();
        if failures.is_empty() {
            return None;
        }
        let mut col = v_flex().gap_2().px_2();
        for (i, f) in failures.into_iter().enumerate() {
            let label = crate::capsule::selection_label(&self.state.read(cx).project.display, &f.node).unwrap_or_else(|| f.node.clone());
            let detail: Vec<String> = f.detail.lines().take(8).map(String::from).collect();
            let node = f.node.clone();
            let mut card = v_flex()
                .gap_1()
                .p_1p5()
                .rounded_md()
                .border_1()
                .border_color(theme.danger.opacity(0.6))
                .child(
                    h_flex()
                        .id(("fail-head", i))
                        .gap_2()
                        .cursor_pointer()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let n = node.clone();
                            this.state.update(cx, |s, cx| s.select(Some(n), true, SelectionSource::Log, cx))
                        }))
                        .child(div().text_color(theme.danger).font_weight(FontWeight::BOLD).child("✗"))
                        .child(div().flex_1().text_sm().font_weight(FontWeight::SEMIBOLD).child(label))
                        .child(div().px_1().rounded_sm().text_xs().bg(theme.danger.opacity(0.15)).text_color(theme.danger).child(format!("{} gate", f.gate)))
                        .child(div().text_xs().text_color(theme.muted_foreground).child(format!("{} round(s)", f.rounds))),
                )
                .child(v_flex().font_family(mono.clone()).text_xs().p_1().rounded_sm().bg(theme.muted.opacity(0.35)).children(detail.into_iter().map(|l| div().child(l))));
            for n in &f.notes {
                card = card.child(div().text_xs().text_color(theme.muted_foreground).child(format!("note: {n}")));
            }
            let mut actions = h_flex().gap_1().flex_wrap();
            let n = f.node.clone();
            actions = actions.child(Button::new(("retry", i)).xsmall().primary().label("Retry").on_click(cx.listener(move |this, _, _, cx| {
                let n = n.clone();
                this.state.update(cx, |s, cx| s.synthesize(n, cx))
            })));
            let n = f.node.clone();
            actions = actions.child(Button::new(("retry-note", i)).xsmall().outline().label("Retry with note…").on_click(cx.listener(move |this, _, window, cx| {
                this.note_for = Some(n.clone());
                this.note.update(cx, |s, cx| s.focus(window, cx));
                cx.notify();
            })));
            if let Some(path) = f.hole_file.clone() {
                actions = actions.child(Button::new(("open-hole", i)).xsmall().ghost().label("Open hole file").on_click(move |_, _, cx| cx.open_with_system(&path)));
                let ff = f.clone();
                actions = actions.child(Button::new(("pin", i)).xsmall().ghost().label("Pin manual impl").on_click(cx.listener(move |this, _, _, cx| {
                    let ff = ff.clone();
                    this.state.update(cx, |s, cx| s.pin_manual(&ff, cx))
                })));
            }
            let ff = f.clone();
            actions = actions.child(Button::new(("revise", i)).xsmall().ghost().label("Ask author to revise spec").on_click(cx.listener(move |this, _, _, cx| {
                let ff = ff.clone();
                this.state.update(cx, |s, cx| s.ask_author_to_revise(&ff, cx))
            })));
            card = card.child(actions);
            if self.note_for.as_ref() == Some(&f.node) {
                card = card.child(
                    h_flex()
                        .gap_1()
                        .child(div().flex_1().child(Input::new(&self.note).xsmall()))
                        .child(Button::new(("note-go", i)).xsmall().primary().label("Add note & retry").on_click(cx.listener(|this, _, w, cx| this.retry_with_note(w, cx)))),
                );
            }
            col = col.child(card);
        }
        Some(col)
    }

    /// N6: proposed edits, hunk by hunk.
    fn review_cards(&self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let mono = theme.mono_font_family.clone();
        let mut review = v_flex().gap_2().px_2();
        let pending = self.state.read(cx).pending.clone();
        for p in &pending {
            let id = p.id;
            let (kept, all) = p.counts();
            let mut body = v_flex().gap_1();
            for (fi, f) in p.files.iter().enumerate() {
                if p.files.len() > 1 {
                    body = body.child(div().text_xs().font_weight(FontWeight::SEMIBOLD).child(f.path.clone()));
                }
                for (hi, h) in f.hunks.iter().enumerate() {
                    let mut diff = v_flex().font_family(mono.clone()).text_xs().p_1().flex_1().min_w_0();
                    for (tag, line) in h.lines.iter().take(40) {
                        let (fg, bg) = match tag {
                            '+' => (hsla(0.33, 0.6, 0.6, 1.0), hsla(0.33, 0.6, 0.4, 0.15)),
                            '-' => (theme.danger, theme.danger.opacity(0.12)),
                            _ => (theme.muted_foreground, gpui_kit::transparent_black()),
                        };
                        diff = diff.child(div().whitespace_nowrap().text_color(fg).bg(bg).child(format!("{tag} {line}")));
                    }
                    let key = id as usize * 10_000 + fi * 1000 + hi;
                    body = body.child(
                        h_flex()
                            .items_start()
                            .gap_1()
                            .rounded_sm()
                            .bg(theme.muted.opacity(0.35))
                            .when(!h.keep, |d| d.opacity(0.45))
                            .child(diff.overflow_x_scrollbar())
                            .child(
                                Button::new(("hunk", key))
                                    .xsmall()
                                    .ghost()
                                    .toggled(h.keep)
                                    .label(if h.keep { "✓ keep" } else { "✗ skip" })
                                    .on_click(cx.listener(move |this, _, _, cx| this.state.update(cx, |s, cx| s.toggle_hunk(id, fi, hi, cx)))),
                            ),
                    );
                }
            }
            let accept = if kept == all { "Accept all".to_string() } else { format!("Accept {kept} of {all}") };
            review = review.child(
                v_flex()
                    .gap_1()
                    .p_1p5()
                    .rounded_md()
                    .border_1()
                    .border_color(theme.warning.opacity(0.6))
                    .child(
                        h_flex()
                            .gap_1()
                            .child(div().flex_1().text_sm().font_weight(FontWeight::SEMIBOLD).child(format!("{} — {}", p.paths(), p.label)))
                            .child(div().text_xs().text_color(theme.muted_foreground).child(format!("{all} change(s)")))
                            .child(Button::new(("accept", id as usize)).xsmall().primary().label(accept).on_click(cx.listener(move |this, _, _, cx| {
                                this.state.update(cx, |s, cx| s.accept_pending(id, cx))
                            })))
                            .child(Button::new(("reject", id as usize)).xsmall().outline().label("Reject all").on_click(cx.listener(move |this, _, _, cx| {
                                this.state.update(cx, |s, cx| s.reject_pending(id, cx))
                            }))),
                    )
                    .when_some(p.error.clone(), |d, e| d.child(div().text_xs().text_color(theme.danger).child(e)))
                    .child(body),
            );
        }
        review
    }

    /// N12: the chat, with consecutive tool-call rows folded into one activity row.
    fn chat(&self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let chat_rows = self.state.read(cx).chat.clone();
        let mut chat = v_flex().gap_2().p_2();
        if chat_rows.is_empty() {
            chat = chat.child(div().text_xs().text_color(theme.muted_foreground).child(
                "Author-role chat. The agent edits through the wh tools: its changes appear live in every view, highlighted, and undo with ⌘Z (or wait for your review).",
            ));
        }
        let is_activity = |r: &(ChatRole, String)| r.0 == ChatRole::System && r.1.starts_with("→ ");
        let mut i = 0;
        while i < chat_rows.len() {
            if is_activity(&chat_rows[i]) {
                let start = i;
                while i < chat_rows.len() && is_activity(&chat_rows[i]) {
                    i += 1;
                }
                let group = &chat_rows[start..i];
                let open = self.expanded.contains(&start);
                let mut rows = v_flex()
                    .id(("activity", start))
                    .px_2()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !this.expanded.remove(&start) {
                            this.expanded.insert(start);
                        }
                        cx.notify();
                    }))
                    .child(format!("{} {} tool call(s) · {}", if open { "▾" } else { "▸" }, group.len(), group.last().map(|r| r.1.trim_start_matches("→ ")).unwrap_or("")));
                if open {
                    for r in group {
                        rows = rows.child(div().pl_3().child(r.1.trim_start_matches("→ ").to_string()));
                    }
                }
                chat = chat.child(rows);
                continue;
            }
            let (role, text) = &chat_rows[i];
            let (who, bg) = match role {
                ChatRole::User => ("you", theme.primary.opacity(0.12)),
                ChatRole::Agent => ("agent", theme.muted.opacity(0.6)),
                ChatRole::System => ("system", theme.warning.opacity(0.12)),
            };
            chat = chat.child(v_flex().p_2().rounded_md().bg(bg).child(div().text_xs().text_color(theme.muted_foreground).child(who)).child(div().text_sm().child(text.clone())));
            i += 1;
        }
        chat
    }
}

impl Render for AgentPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        // a removed chip comes back when the selection or the file changes
        {
            let st = self.state.read(cx);
            let sel = st.project.selected_node().map(|n| n.id.clone());
            if self.dropped_node.is_some() && self.dropped_node != sel {
                self.dropped_node = None;
            }
            if self.dropped_file.is_some() && self.dropped_file != st.active_path() {
                self.dropped_file = None;
            }
        }
        let st = self.state.read(cx);
        let mut queue = v_flex().gap_0p5().p_2();
        if st.synthesis.is_empty() {
            queue = queue.child(div().text_xs().text_color(theme.muted_foreground).child("No synthesis jobs."));
        }
        let positions = crate::capsule::queue_positions(&st.synthesis);
        let tokens = crate::tokens::tokens(cx);
        for (i, (node, status)) in st.synthesis.iter().enumerate() {
            let n = node.clone();
            let c = crate::capsule::synth_chip(status, positions.get(node).copied());
            let tip = c.detail.clone();
            let label = crate::capsule::selection_label(&st.project.display, node).unwrap_or_else(|| node.clone());
            queue = queue.child(
                h_flex()
                    .id(("synth", i))
                    .gap_2()
                    .text_xs()
                    .cursor_pointer()
                    .hover(|d| d.bg(theme.list_hover))
                    .tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(window, cx))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let n = n.clone();
                        this.state.update(cx, |s, cx| s.select(Some(n), true, SelectionSource::Log, cx))
                    }))
                    .child(div().flex_1().child(label))
                    .child(crate::capsule::chip(&c, tokens, px(11.), false)),
            );
        }
        // budget meter: what the agents spent this session against `[synthesis] budget_usd`
        let (cost, budget) = (st.agent_cost, st.budget_usd);
        let meter = h_flex()
            .px_2()
            .gap_2()
            .text_xs()
            .text_color(theme.muted_foreground)
            .child(match budget {
                Some(b) => format!("agent cost ${cost:.2} of ${b:.2}"),
                None => format!("agent cost ${cost:.2} (no budget set)"),
            })
            .when_some(budget.filter(|b| *b > 0.0), |d, b| {
                let frac = (cost / b).clamp(0.0, 1.0) as f32;
                let color = if frac > 0.9 { theme.danger } else if frac > 0.6 { theme.warning } else { theme.primary };
                d.child(div().w(px(90.)).h(px(5.)).rounded_full().bg(theme.muted).child(div().h_full().rounded_full().w(px(90. * frac)).bg(color)))
            });
        let reviewing = st.review;
        let pending = st.pending.len();
        let has_failure = |n: &str| st.failures.contains_key(n);
        let selected = st.project.selected_node().map(|n| (n.id.clone(), n.name.clone()));
        let explain = selected.as_ref().is_some_and(|(id, _)| has_failure(id));
        let author_header = h_flex()
            .px_2()
            .pt_2()
            .gap_1()
            .child(div().flex_1().text_xs().font_weight(FontWeight::SEMIBOLD).text_color(theme.muted_foreground).child("AUTHOR"))
            .child(div().text_xs().text_color(theme.muted_foreground).child("edits apply"))
            .child(Button::new("apply-live").xsmall().toggled(!reviewing).label("Live").on_click(cx.listener(|this, _, _, cx| {
                this.state.update(cx, |s, cx| s.set_review(false, cx))
            })))
            .child(Button::new("apply-review").xsmall().toggled(reviewing).label("After review").on_click(cx.listener(|this, _, _, cx| {
                this.state.update(cx, |s, cx| s.set_review(true, cx))
            })));
        // context chips: what the next message is about
        let (node, file) = self.context(cx);
        let chip = |id: &'static str, text: String, cx: &mut Context<Self>, on_x: fn(&mut AgentPanel, &App)| {
            h_flex()
                .gap_1()
                .px_1p5()
                .rounded_full()
                .text_xs()
                .bg(theme.primary.opacity(0.12))
                .child(text)
                .child(div().id(id).cursor_pointer().text_color(theme.muted_foreground).child("×").on_click(cx.listener(move |this, _, _, cx| {
                    on_x(this, cx);
                    cx.notify();
                })))
        };
        let mut chips = h_flex().gap_1().flex_wrap().child(div().text_xs().text_color(theme.muted_foreground).child("context:"));
        let st = self.state.read(cx);
        if let Some(n) = &node {
            let label = crate::capsule::selection_label(&st.project.display, n).unwrap_or_else(|| n.clone());
            chips = chips.child(chip("chip-node", label, cx, |this, cx| {
                this.dropped_node = this.state.read(cx).project.selected_node().map(|n| n.id.clone())
            }));
        }
        let st = self.state.read(cx);
        if let Some(f) = &file {
            let name = f.rsplit('/').next().unwrap_or(f).to_string();
            let _ = st;
            chips = chips.child(chip("chip-file", name, cx, |this, cx| this.dropped_file = this.state.read(cx).active_path()));
        }
        if node.is_none() && file.is_none() {
            chips = chips.child(div().text_xs().child("whole project"));
        }
        let mut actions = h_flex().gap_1().flex_wrap();
        if let Some((_, name)) = &selected {
            for (k, (label, prompt)) in NODE_ACTIONS.iter().enumerate() {
                let text = format!("{prompt} ({name})");
                actions = actions.child(Button::new(("node-action", k)).xsmall().ghost().label(*label).on_click(cx.listener(move |this, _, _, cx| this.ask(text.clone(), cx))));
            }
            if explain {
                actions = actions.child(Button::new("explain").xsmall().ghost().label("Explain failure").on_click(cx.listener(|this, _, _, cx| {
                    let f = this.state.read(cx).project.selected_node().and_then(|n| this.state.read(cx).failures.get(&n.id).cloned());
                    if let Some(f) = f {
                        let text = format!(
                            "Explain why the compiler agent failed to implement `{}` ({} gate, {} rounds) and what to change in the spec:\n{}",
                            f.node, f.gate, f.rounds, f.detail
                        );
                        this.ask(text, cx);
                    }
                })));
            }
        }
        let failures = self.failure_cards(cx);
        let review = self.review_cards(cx);
        let chat = self.chat(cx);
        v_flex()
            .size_full()
            .track_focus(&self.focus)
            .child(Self::section("SYNTHESIS QUEUE", cx))
            .child(queue)
            .when_some(failures, |d, f| d.child(Self::section("FAILED", cx)).child(div().max_h(px(360.)).child(f.overflow_y_scrollbar())))
            .child(meter)
            .child(author_header)
            .when(pending > 0, |d| {
                d.child(div().px_2().pt_1().text_xs().text_color(theme.warning).child(format!("{pending} proposed edit(s) waiting for review")))
                    .child(div().max_h(px(420.)).child(review.overflow_y_scrollbar()))
            })
            .child(div().flex_1().min_h_0().child(chat.overflow_y_scrollbar()))
            .child(
                v_flex()
                    .gap_1()
                    .p_2()
                    .border_t_1()
                    .border_color(theme.border)
                    .child(chips)
                    .when(selected.is_some(), |d| d.child(actions))
                    .child(h_flex().gap_1().child(div().flex_1().child(Input::new(&self.input).small())).child(
                        Button::new("send").small().primary().label("Send").on_click(cx.listener(|this, _, w, cx| this.send(w, cx))),
                    )),
            )
    }
}

pub struct LogPanel {
    state: Entity<AppState>,
    focus: FocusHandle,
    /// 0 Log, 1 Problems, 2 Run trace.
    tab: usize,
    show: [bool; 3],
    _subs: Vec<Subscription>,
}

crate::dock_panel!(LogPanel, "wh.log", "Log");

impl LogPanel {
    pub fn new(state: Entity<AppState>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let subs = vec![
            cx.observe(&state, |_, _, cx| cx.notify()),
            cx.subscribe(&state, |this, _, ev: &AppEvent, cx| {
                if let AppEvent::ShowProblems = ev {
                    this.tab = 1;
                    cx.notify();
                }
            }),
        ];
        LogPanel { state, focus: cx.focus_handle(), tab: 0, show: [true, true, false], _subs: subs }
    }
}

fn level_color(l: LogLevel, theme: &gpui_kit::component::Theme) -> Hsla {
    match l {
        LogLevel::Error => theme.danger,
        LogLevel::Warn => theme.warning,
        LogLevel::Info => theme.foreground,
        _ => theme.muted_foreground,
    }
}

impl Render for LogPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let mono = theme.mono_font_family.clone();
        let st = self.state.read(cx);
        let started = st.started;
        let problems = st.problems();
        let count = |l: ProblemLevel| problems.iter().filter(|p| p.level == l).count();
        let (n_err, n_warn, n_lint) = (count(ProblemLevel::Error), count(ProblemLevel::Warning), count(ProblemLevel::Lint));
        let bar = h_flex()
            .gap_1()
            .pr_2()
            .border_b_1()
            .border_color(theme.border)
            .child(
                TabBar::new("log-tabs")
                    .selected_index(self.tab)
                    .children([
                        Tab::new().label(format!("Log ({})", st.log.len())),
                        Tab::new().label(format!("Problems ({})", n_err + n_warn)),
                        Tab::new().label(format!("Run trace ({})", st.trace.len())),
                    ])
                    .on_click(cx.listener(|this, ix: &usize, _, cx| {
                        this.tab = *ix;
                        cx.notify();
                    })),
            )
            .child(div().flex_1())
            .when(self.tab == 1, |d| {
                let mut d = d;
                for (k, (label, n)) in [("errors", n_err), ("warnings", n_warn), ("lints", n_lint)].into_iter().enumerate() {
                    d = d.child(Button::new(("pf", k)).xsmall().ghost().toggled(self.show[k]).label(format!("{n} {label}")).on_click(cx.listener(move |this, _, _, cx| {
                        this.show[k] = !this.show[k];
                        cx.notify();
                    })));
                }
                d
            })
            .child(Button::new("check").xsmall().outline().label("Check").on_click(cx.listener(|this, _, _, cx| this.state.update(cx, |s, cx| s.check(cx)))))
            .child(Button::new("run").xsmall().primary().label("Run app").on_click(cx.listener(|this, _, _, cx| {
                this.tab = 2;
                this.state.update(cx, |s, cx| s.run_app(cx))
            })))
            .child(Button::new("clear").xsmall().ghost().label("Clear").on_click(cx.listener(|this, _, _, cx| {
                let tab = this.tab;
                this.state.update(cx, |s, cx| {
                    if tab == 0 {
                        s.log.clear()
                    } else if tab == 1 {
                        s.job_problems.clear()
                    } else {
                        s.trace.clear();
                        s.executed.clear();
                    }
                    cx.notify()
                })
            })));
        let mut rows = v_flex().font_family(mono).text_xs().px_2().py_1();
        if self.tab == 0 {
            for (i, e) in st.log.iter().enumerate().rev().take(400) {
                let node = e.node.clone();
                rows = rows.child(
                    h_flex()
                        .id(("log", i))
                        .gap_2()
                        .when_node(node.is_some(), &theme)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(n) = node.clone() {
                                this.state.update(cx, |s, cx| s.select(Some(n), true, SelectionSource::Log, cx));
                            }
                        }))
                        .child(div().w(px(64.)).flex_none().whitespace_nowrap().text_color(theme.muted_foreground).child(format!("{:>7.2}s", (e.at - started).as_secs_f32())))
                        .child(div().w(px(44.)).text_color(level_color(e.level, &theme)).child(e.level.label()))
                        .child(div().w(px(56.)).text_color(theme.muted_foreground).child(e.source.clone()))
                        .child(div().flex_1().text_color(level_color(e.level, &theme)).child(e.message.clone())),
                );
            }
        } else if self.tab == 1 {
            // grouped file › node, in document order
            let visible: Vec<_> = problems
                .iter()
                .filter(|p| self.show[match p.level {
                    ProblemLevel::Error => 0,
                    ProblemLevel::Warning => 1,
                    ProblemLevel::Lint => 2,
                }])
                .cloned()
                .collect();
            if visible.is_empty() {
                rows = rows.child(div().text_color(theme.muted_foreground).child("No problems. F8 / ⇧F8 step through errors and warnings."));
            }
            let (mut file, mut node) = (None::<String>, None::<Option<String>>);
            for (i, p) in visible.into_iter().enumerate() {
                if file.as_ref() != Some(&p.path) {
                    file = Some(p.path.clone());
                    node = None;
                    rows = rows.child(div().pt_1().font_weight(FontWeight::SEMIBOLD).child(p.path.clone()));
                }
                if node.as_ref() != Some(&p.node) {
                    node = Some(p.node.clone());
                    let label = p.node.as_deref().and_then(|n| crate::capsule::selection_label(&st.project.display, n)).unwrap_or_else(|| "(outside definitions)".into());
                    rows = rows.child(div().pl_3().text_color(theme.muted_foreground).child(format!("› {label}")));
                }
                let (tag, color) = match p.level {
                    ProblemLevel::Error => ("error", theme.danger),
                    ProblemLevel::Warning => ("warn", theme.warning),
                    ProblemLevel::Lint => ("lint", theme.muted_foreground),
                };
                let (path, offset) = (p.path.clone(), p.offset);
                rows = rows.child(
                    h_flex()
                        .id(("problem", i))
                        .pl_6()
                        .gap_2()
                        .cursor_pointer()
                        .hover(|d| d.bg(theme.list_hover))
                        .on_click(cx.listener(move |this, _, _, cx| this.state.update(cx, |s, cx| s.reveal_at(&path, offset, cx))))
                        .child(div().w(px(36.)).flex_none().text_color(color).child(tag))
                        .child(div().w(px(56.)).flex_none().text_color(theme.muted_foreground).child(format!("line {}", p.line)))
                        .child(div().flex_1().text_color(color).child(p.message.clone()))
                        .child(div().text_color(theme.muted_foreground).child(p.source)),
                );
            }
        } else {
            if st.trace.is_empty() {
                rows = rows.child(div().text_color(theme.muted_foreground).child("Run the app to see its trace; click an event to select its node (wh.node)."));
            }
            for (i, (at, t)) in st.trace.iter().enumerate() {
                let node = t.node.clone();
                rows = rows.child(
                    h_flex()
                        .id(("trace", i))
                        .gap_2()
                        .when_node(node.is_some(), &theme)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(n) = node.clone() {
                                this.state.update(cx, |s, cx| s.select(Some(n), true, SelectionSource::Log, cx));
                            }
                        }))
                        .child(div().w(px(64.)).flex_none().whitespace_nowrap().text_color(theme.muted_foreground).child(format!("{:>7.2}s", (*at - started).as_secs_f32())))
                        .child(div().w(px(44.)).text_color(level_color(t.level, &theme)).child(t.level.label()))
                        .child(div().w(px(260.)).text_color(theme.link).child(t.node.clone().unwrap_or_default()))
                        .child(div().flex_1().child(t.message.clone()))
                        .child(div().text_color(theme.muted_foreground).child(t.duration.map(|d| format!("{} µs", d.as_micros())).unwrap_or_default())),
                );
            }
        }
        v_flex().size_full().track_focus(&self.focus).child(bar).child(div().flex_1().min_h_0().child(rows.overflow_y_scrollbar()))
    }
}

trait NodeRow {
    fn when_node(self, clickable: bool, theme: &gpui_kit::component::Theme) -> Self;
}

impl NodeRow for Stateful<Div> {
    fn when_node(self, clickable: bool, theme: &gpui_kit::component::Theme) -> Self {
        if clickable {
            let hover = theme.list_hover;
            self.cursor_pointer().hover(move |d| d.bg(hover))
        } else {
            self
        }
    }
}
