//! Offscreen screenshots of the real panels on copies of the examples, for
//! checking the UX work without a visible screen (gpui's headless Metal
//! renderer, which must run on the main thread). Run with
//! `WH_IDE_SNAPSHOTS=1 cargo run -p wh-ide --features snapshots`;
//! PNGs land in `target/ide-ux/shots/after/`. The copies are made under
//! `target/ide-ux/snap/`, so `examples/` is never touched.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::{AnyWindowHandle, AppContext, Context, Entity, HeadlessAppContext, IntoElement, ParentElement, Render, Styled, Window, div, px, size};

use crate::backend::{MockBackend, NodeResult, SynthStatus};
use crate::document::DocumentPanel;
use crate::graph_panel::{DataModelPanel, FlowGraphPanel};
use crate::inspector::InspectorPanel;
use crate::project::Project;
use crate::state::{AppState, SelectionSource};

struct Shot {
    doc: Entity<DocumentPanel>,
    flow: Entity<FlowGraphPanel>,
    data: Entity<DataModelPanel>,
    insp: Entity<InspectorPanel>,
}

impl Render for Shot {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = crate::tokens::tokens(cx);
        h_flex()
            .size_full()
            .bg(t.panel)
            .text_color(t.text_primary)
            .child(div().w(px(470.)).h_full().border_r_1().border_color(t.border_subtle).child(self.doc.clone()))
            .child(
                v_flex()
                    .flex_1()
                    .h_full()
                    .child(div().h(px(450.)).border_b_1().border_color(t.border_subtle).child(self.flow.clone()))
                    .child(div().flex_1().child(self.data.clone())),
            )
            .child(div().w(px(400.)).h_full().border_l_1().border_color(t.border_subtle).child(self.insp.clone()))
    }
}

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

/// A fresh copy of `examples/<name>` without layout or build state.
fn copy(name: &str) -> PathBuf {
    let to = root().join("target/ide-ux/snap").join(name);
    let _ = std::fs::remove_dir_all(&to);
    copy_dir(&root().join("examples").join(name), &to);
    to
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let name = e.file_name();
        if name == "target" || name == ".wh" {
            continue;
        }
        if e.file_type().unwrap().is_dir() {
            copy_dir(&e.path(), &to.join(&name));
        } else {
            std::fs::copy(e.path(), to.join(&name)).unwrap();
        }
    }
}

struct Session {
    cx: HeadlessAppContext,
    handle: AnyWindowHandle,
    state: Entity<AppState>,
    shot: Entity<Shot>,
}

fn open(project: &str, light: bool) -> Session {
    let mut cx = HeadlessAppContext::with_platform(
        gpui_kit::platform::current_platform(true).text_system(),
        Arc::new(gpui_kit::assets::Assets),
        gpui_kit::platform::current_headless_renderer,
    );
    cx.update(|cx| {
        gpui_kit::init(cx);
        crate::theme::init(cx);
        crate::theme::set(if light { crate::theme::Appearance::Light } else { crate::theme::Appearance::Dark }, cx);
        crate::graph_panel::bind_keys(cx);
    });
    let dir = copy(project);
    let project = Project::open(&dir).unwrap();
    let mut state = None;
    let mut shot = None;
    let (handle, _) = cx
        .update(|cx| {
            gpui_kit::open_window(
                gpui_kit::WindowOptions {
                    window_bounds: Some(gpui_kit::WindowBounds::Windowed(gpui_kit::Bounds { origin: Default::default(), size: size(px(1440.), px(900.)) })),
                    focus: false,
                    show: false,
                    ..Default::default()
                },
                cx,
                |window, cx| {
                    let st = cx.new(|cx| AppState::new(project, Box::new(MockBackend::default()), cx));
                    let doc = cx.new(|cx| DocumentPanel::new(st.clone(), window, cx));
                    let flow = cx.new(|cx| FlowGraphPanel::new(st.clone(), window, cx));
                    let data = cx.new(|cx| DataModelPanel::new(st.clone(), window, cx));
                    let insp = cx.new(|cx| InspectorPanel::new(st.clone(), window, cx));
                    let s = cx.new(|_| Shot { doc, flow, data, insp });
                    state = Some(st);
                    shot = Some(s.clone());
                    s
                },
            )
        })
        .unwrap();
    let mut s = Session { cx, handle, state: state.unwrap(), shot: shot.unwrap() };
    s.frame();
    s
}

impl Session {
    fn frame(&mut self) {
        // two frames: the canvas learns its bounds in the first and fits in the second
        for _ in 0..3 {
            self.cx.update_window(self.handle, |_, window, cx| {
                window.refresh();
                window.draw(cx).clear(cx);
            })
            .unwrap();
            self.cx.run_until_parked();
        }
    }

    fn save(&mut self, name: &str) {
        self.frame();
        let img = self.cx.capture_screenshot(self.handle).expect("offscreen rendering");
        let dir = root().join("target/ide-ux/shots/after");
        std::fs::create_dir_all(&dir).unwrap();
        img.save(dir.join(format!("{name}.png"))).unwrap();
    }

    fn state<R>(&mut self, f: impl FnOnce(&mut AppState, &mut Context<AppState>) -> R) -> R {
        let st = self.state.clone();
        self.cx.update(|cx| st.update(cx, f))
    }

    fn flow_canvas(&mut self, f: impl FnOnce(&mut crate::canvas::GraphCanvas, &mut Context<crate::canvas::GraphCanvas>)) {
        let shot = self.shot.clone();
        self.cx.update(|cx| {
            let panel = shot.read(cx).flow.read(cx).inner.clone();
            let canvas = panel.read(cx).canvas();
            canvas.update(cx, f);
            panel.update(cx, |_, cx| cx.notify());
        });
    }

    fn data_canvas(&mut self, f: impl FnOnce(&mut crate::canvas::GraphCanvas, &mut Context<crate::canvas::GraphCanvas>)) {
        let shot = self.shot.clone();
        self.cx.update(|cx| {
            let panel = shot.read(cx).data.read(cx).inner.clone();
            let canvas = panel.read(cx).canvas();
            canvas.update(cx, f);
            panel.update(cx, |_, cx| cx.notify());
        });
    }

    fn palette(&mut self, mode: crate::palette::Mode) {
        let st = self.state.clone();
        self.cx.update_window(self.handle, |_, window, cx| crate::palette::open(st, mode, window, cx)).unwrap();
    }

    fn close_dialog(&mut self) {
        use gpui_kit::component::WindowExt;
        self.cx.update_window(self.handle, |_, window, cx| window.close_dialog(cx)).unwrap();
    }

    fn select(&mut self, id: &str) {
        let id = id.to_string();
        self.state(|s, cx| s.select(Some(id), true, SelectionSource::Log, cx));
    }
}

pub fn run() {
    // signup at its default fit, nothing selected
    let mut s = open("signup", false);
    s.save("01-signup-overview");
    // statuses: a failed synthesis, a queue, spec results
    s.state(|st, cx| {
        st.synthesis.push(("signup.validate".into(), SynthStatus::Failed("no compiler agent (mock backend)".into())));
        st.synthesis.push(("signup.normalize_email".into(), SynthStatus::Queued));
        st.synthesis.push(("signup.check_age".into(), SynthStatus::Accepted));
        st.tests.insert("signup.normalize_email".into(), NodeResult { passed: 2, failed: 0, failures: vec![] });
        st.tests.insert(
            "signup.check_age".into(),
            NodeResult { passed: 1, failed: 1, failures: vec!["signup__check_age__example_1: assertion failed".into()] },
        );
        cx.notify();
    });
    // live values (wh_live), as the live backend would report them
    s.state(|st, cx| {
        let n = st.project.model.nodes.get("signup.check_age").unwrap().clone();
        let text = st.project.model.files[n.file].text.clone();
        let path = st.project.model.files[n.file].path.clone();
        let wh_syntax::ast::ItemKind::Def(d) = &n.item.kind else { unreachable!() };
        let mut vals = Vec::new();
        for (i, c) in d.clauses.iter().enumerate() {
            let wh_syntax::ast::ClauseKind::Example { .. } = c.kind else { continue };
            let fail = i == 2;
            vals.push(wh_live::LiveValue {
                node: n.id.clone(),
                kind: wh_live::ItemKind::Example,
                clause_index: i,
                file: path.clone(),
                span: wh_live::SourceSpan::of(&text, c.span),
                source: text[c.span.range()].to_string(),
                status: if fail { wh_live::Status::Fail } else { wh_live::Status::Pass },
                message: None,
                values: if fail {
                    vec![
                        wh_live::NamedValue { label: "left".into(), value: "39".into(), span: None },
                        wh_live::NamedValue { label: "right".into(), value: "40".into(), span: None },
                    ]
                } else {
                    vec![wh_live::NamedValue { label: "result".into(), value: "Err(TooYoung)".into(), span: None }]
                },
                samples: vec![],
                steps: vec![],
                cases: None,
                duration_us: Some(120),
            });
        }
        st.live.nodes.insert(n.id.clone(), vals);
        let reg = st.project.model.nodes.get("signup.register").unwrap();
        if let wh_model::NodeKind::Flow(f) = &reg.kind {
            let sample = |v: &str| wh_live::StepSample { value: v.into(), error: false, node: "x".into(), kind: wh_live::ItemKind::Example, clause_index: 0 };
            st.live.steps.insert(f.steps[0].id.clone(), vec![sample("ValidForm { email: \"a@b.io\", age: 30 }")]);
            st.live.steps.insert(f.steps[1].id.clone(), vec![sample("ValidForm { email: \"a@b.io\", age: 30 }")]);
        }
        cx.emit(crate::state::AppEvent::LiveValues);
        cx.notify();
    });
    s.select("signup.check_age");
    s.save("02-signup-status-def-selected");
    // a node at full detail (§15.7): kind stripe, ports on the border, chips
    s.flow_canvas(|c, cx| {
        c.zoom_to_selection();
        cx.notify();
    });
    s.save("02b-signup-node-full-detail");
    // the data model's cardinality control on User.email
    s.data_canvas(|c, cx| {
        c.selected_node = None;
        c.selected_edge = c.graph.edges.iter().position(|e| e.field.as_ref().is_some_and(|(o, f)| o == "signup.User" && f == "email"));
        cx.notify();
    });
    s.save("02c-signup-cardinality");
    s.palette(crate::palette::Mode::Nodes);
    s.save("02d-signup-go-to-node");
    s.close_dialog();
    s.flow_canvas(|c, cx| {
        c.fit();
        cx.notify();
    });
    let step = s.state(|st, _| {
        let reg = st.project.model.nodes.get("signup.register").unwrap();
        match &reg.kind {
            wh_model::NodeKind::Flow(f) => f.steps[1].id.clone(),
            _ => unreachable!(),
        }
    });
    s.select(&step);
    s.save("03-signup-step-selected");
    // an unresolved step stays in the graph as an error node
    s.state(|st, cx| {
        let path = "src/signup.wh.md".to_string();
        let text = st.project.text(&path).to_string();
        let at = text.find("|> validate\n").unwrap() + 3;
        st.edit_text(&path, at..at + "validate".len(), "validatexx", cx);
    });
    let bad = s.state(|st, _| {
        let at = st.project.text("src/signup.wh.md").find("validatexx").unwrap();
        format!("signup.register#?validatexx@{at}")
    });
    s.select(&bad);
    s.save("04-signup-unresolved-step");
    drop(s);

    for p in ["todo", "pong"] {
        let mut s = open(p, false);
        s.save(&format!("05-{p}-overview"));
    }
    let mut s = open("signup", true);
    s.select("signup.User");
    s.save("06-signup-light");
    let _ = &s.shot;
    println!("snapshots written to {}", root().join("target/ide-ux/shots/after").display());
}
