//! `wh-ide [project-dir]`: the Wild Horse IDE. Each project gets its own
//! window; with no project open a welcome window offers New / Open / Recent.

#[macro_use]
mod panels;
mod backend;
mod canvas;
mod capsule;
mod config;
mod document;
mod graph_panel;
mod highlight;
mod inspector;
mod jobs;
mod live_backend;
mod mcp;
mod markdown;
mod palette;
mod project;
mod rustgen;
mod side_panels;
#[cfg(feature = "snapshots")]
mod snapshots;
mod state;
mod theme;
mod tokens;
mod watch;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::dock::{
    BasePanelView, DockArea, DockAreaState, DockEvent, DockLayout, DockPlacement, DockSkin, PanelInfo, PanelState, PanelStyle, panel_handle,
    register_panel,
};
use gpui_kit::component::input::{Copy as InputCopy, Cut as InputCut, Paste as InputPaste, SelectAll as InputSelectAll};
use gpui_kit::component::{ActiveTheme, Disableable, Sizable, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;

use crate::backend::{JobKind, MockBackend};
use crate::document::DocumentPanel;
use crate::graph_panel::{DataModelPanel, FlowGraphPanel};
use crate::inspector::InspectorPanel;
use crate::project::Project;
use crate::side_panels::{AgentPanel, LogPanel};
use crate::state::{AppEvent, AppState};

actions!(
    wh,
    [
        GlobalUndo,
        GlobalRedo,
        Save,
        SaveAll,
        SaveAs,
        NewProject,
        OpenProject,
        CloseProject,
        NewFile,
        OpenFile,
        CloseFile,
        RevertFile,
        ClearRecent,
        ToggleDocument,
        ToggleSidebar,
        ToggleLog,
        ResetLayout,
        CheckProject,
        RunApp,
        Build,
        BuildRelease,
        SynthesizeAll,
        TestProject,
        Optimize,
        CancelJob,
        NextProblem,
        PreviousProblem,
        ToggleLiveValues,
        PresetWrite,
        PresetDesign,
        PresetDebug,
        About,
        Hide,
        HideOthers,
        ShowAll,
        Minimize,
        Zoom,
        Quit
    ]
);

/// File → Open Recent → a project folder.
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = wh, no_json)]
pub struct OpenRecent {
    pub path: PathBuf,
}

/// Every open window: project workspaces and the welcome window.
#[derive(Default)]
struct Windows {
    projects: Vec<(AnyWindowHandle, WeakEntity<Workspace>)>,
    welcome: Option<AnyWindowHandle>,
    quitting: bool,
}
impl Global for Windows {}

/// The dock panel names, in default-layout order.
const PANEL_NAMES: [&str; 6] = ["wh.document", "wh.flow", "wh.data", "wh.inspector", "wh.agent", "wh.log"];

/// Panels of the workspace being restored by `DockArea::load`, handed to the
/// registered panel builders by name.
#[derive(Default)]
struct LoadingPanels(Vec<(&'static str, Arc<dyn BasePanelView>)>);
impl Global for LoadingPanels {}

/// Workspace presets (UX review L1): one stored arrangement each.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
enum Preset {
    Write,
    #[default]
    Design,
    Debug,
}

impl Preset {
    const ALL: [Preset; 3] = [Preset::Write, Preset::Design, Preset::Debug];

    fn key(self) -> &'static str {
        match self {
            Preset::Write => "write",
            Preset::Design => "design",
            Preset::Debug => "debug",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Preset::Write => "Write",
            Preset::Design => "Design",
            Preset::Debug => "Debug",
        }
    }
}

/// `.wh/presets.json`: the current preset and each preset's last dock arrangement.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
struct Presets {
    #[serde(default)]
    current: Preset,
    #[serde(default)]
    layouts: std::collections::BTreeMap<String, serde_json::Value>,
}

impl Presets {
    fn path(dir: &Path) -> PathBuf {
        dir.join(".wh").join("presets.json")
    }

    fn load(dir: &Path) -> Presets {
        std::fs::read_to_string(Self::path(dir)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }

    fn save(&self, dir: &Path) {
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let _ = std::fs::create_dir_all(dir.join(".wh"));
            let _ = std::fs::write(Self::path(dir), json);
        }
    }
}

struct Workspace {
    state: Entity<AppState>,
    presets: Presets,
    dock: Entity<DockArea>,
    panels: Vec<(&'static str, Arc<dyn BasePanelView>)>,
    focus: FocusHandle,
    /// Set once the user agreed to close; the next close request goes through.
    closing: bool,
    /// Bumped on each dock change; the debounced save writes the newest one only.
    layout_gen: u64,
    /// The MCP bridge socket; dropping it (closing the window) removes the socket.
    _bridge: Option<wh_mcp::bridge::BridgeHost>,
    _subs: Vec<Subscription>,
}

impl Workspace {
    fn new(project: Project, window: &mut Window, cx: &mut Context<Self>) -> Self {
        // the `wh` MCP bridge: agents (ours, or any started with `wh mcp --bridge <socket>`)
        // edit this window's live buffers through semantic tools
        let (ui_tx, ui_rx) = std::sync::mpsc::channel();
        let bridge = wh_project::Project::load(&project.dir).ok().and_then(|p| {
            let socket = wh_mcp::bridge::default_socket_path(&p.id());
            match wh_mcp::bridge::serve(&socket, Arc::new(mcp::IdeBridge::new(ui_tx, Arc::new(p)))) {
                Ok(host) => Some(host),
                Err(e) => {
                    eprintln!("wh-ide: no MCP bridge at {}: {e}", socket.display());
                    None
                }
            }
        });
        // the real pipeline and agents, unless WH_IDE_MOCK=1 (scripted events, no agents or cargo)
        let backend: Box<dyn backend::Backend> = match wh_project::Project::load(&project.dir) {
            Ok(p) if std::env::var("WH_IDE_MOCK").is_err() => {
                Box::new(live_backend::LiveBackend::new(p).with_bridge(bridge.as_ref().map(|h| h.socket_path().to_path_buf())))
            }
            _ => Box::new(MockBackend::default()),
        };
        let dir = project.dir.clone();
        let state = cx.new(|cx| {
            let mut s = AppState::new(project, backend, cx);
            s.set_ui_calls(ui_rx);
            if let Some(h) = &bridge {
                s.info("mcp", format!("agents can edit this project through `wh mcp --bridge {}`", h.socket_path().display()));
            }
            s
        });
        let document = cx.new(|cx| DocumentPanel::new(state.clone(), window, cx));
        state.update(cx, |s, _| s.push_live_focus());
        let flow = cx.new(|cx| FlowGraphPanel::new(state.clone(), window, cx));
        let data = cx.new(|cx| DataModelPanel::new(state.clone(), window, cx));
        let inspector = cx.new(|cx| InspectorPanel::new(state.clone(), window, cx));
        let agent = cx.new(|cx| AgentPanel::new(state.clone(), window, cx));
        let log = cx.new(|cx| LogPanel::new(state.clone(), window, cx));
        let panels: Vec<(&'static str, Arc<dyn BasePanelView>)> = vec![
            (PANEL_NAMES[0], panel_handle(document)),
            (PANEL_NAMES[1], panel_handle(flow)),
            (PANEL_NAMES[2], panel_handle(data)),
            (PANEL_NAMES[3], panel_handle(inspector)),
            (PANEL_NAMES[4], panel_handle(agent)),
            (PANEL_NAMES[5], panel_handle(log)),
        ];

        let (dock, skin) = DockSkin::dock_area("wh-ide", Some(1), window, cx);
        skin.set_panel_style(PanelStyle::TabBar, cx);
        let mut subs = vec![
            cx.observe(&state, |_, _, cx| cx.notify()),
            cx.subscribe(&dock, |this, _, ev: &DockEvent, cx| {
                if let DockEvent::LayoutChanged = ev {
                    this.schedule_layout_save(cx);
                }
            }),
            cx.subscribe_in(&state, window, |_, state, ev: &AppEvent, window, cx| match ev {
                // the title shows the dirty state
                AppEvent::Changed(_) | AppEvent::Saved => {
                    let p = &state.read(cx).project;
                    window.set_window_title(&format!("Wild Horse — {}{}", p.name, if p.is_dirty() { " •" } else { "" }));
                }
                AppEvent::JobDone { job, ok, summary } => {
                    use gpui_kit::component::WindowExt;
                    use gpui_kit::component::notification::Notification;
                    let note = if *ok { Notification::success(summary.clone()) } else { Notification::error(summary.clone()).autohide(false) };
                    window.push_notification(note.title(format!("{} {}", job.label(), if *ok { "done" } else { "failed" })), cx);
                }
                _ => {}
            }),
        ];
        subs.push(cx.on_release(|this, cx| this.save_layout(cx)));
        let focus = cx.focus_handle();
        let mut this = Workspace { state, presets: Presets::default(), dock, panels, focus, closing: false, layout_gen: 0, _bridge: bridge, _subs: subs };
        this.default_layout(window, cx);
        this.restore_layout(&dir, window, cx);
        let weak = cx.entity().downgrade();
        window.on_window_should_close(cx, move |window, cx| weak.update(cx, |this, cx| this.should_close(window, cx)).unwrap_or(true));
        this.focus.focus(window, cx);
        this
    }

    fn panel(&self, name: &str) -> Arc<dyn BasePanelView> {
        self.panels.iter().find(|(n, _)| *n == name).map(|(_, p)| p.clone()).expect("every panel exists")
    }

    fn default_layout(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let p: Vec<Arc<dyn BasePanelView>> = PANEL_NAMES.iter().map(|n| self.panel(n)).collect();
        self.dock.update(cx, |area, cx| {
            area.set_center(
                DockLayout::v_split().child(DockLayout::tabs().panel_view(p[1].clone(), cx), None).child(DockLayout::tabs().panel_view(p[2].clone(), cx), None),
                window,
                cx,
            );
            area.set_dock(DockPlacement::Left, DockLayout::tabs().panel_view(p[0].clone(), cx), window, cx);
            area.set_dock(DockPlacement::Right, DockLayout::tabs().panel_view(p[3].clone(), cx).panel_view(p[4].clone(), cx), window, cx);
            area.set_dock(DockPlacement::Bottom, DockLayout::tabs().panel_view(p[5].clone(), cx), window, cx);
            area.set_dock_size(DockPlacement::Left, px(560.), window, cx);
            area.set_dock_size(DockPlacement::Right, px(380.), window, cx);
            area.set_dock_size(DockPlacement::Bottom, px(190.), window, cx);
        });
    }

    /// Load `.wh/dock.json` when it names exactly our panels.
    /// Load `.wh/dock.json` (the last arrangement) when it names exactly our panels.
    fn restore_layout(&mut self, dir: &Path, window: &mut Window, cx: &mut Context<Self>) {
        self.presets = Presets::load(dir);
        let Ok(text) = std::fs::read_to_string(config::dock_path(dir)) else { return };
        let Ok(state) = serde_json::from_str::<DockAreaState>(&text) else { return };
        self.load_state(state, window, cx);
    }

    /// Load a dumped layout if it holds exactly our six panels; false otherwise.
    fn load_state(&mut self, state: DockAreaState, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let mut names = Vec::new();
        fn leaves(p: &PanelState, out: &mut Vec<String>) {
            if let PanelInfo::Panel(_) = p.info {
                out.push(p.panel_name.clone());
            }
            for c in &p.children {
                leaves(c, out);
            }
        }
        leaves(&state.center, &mut names);
        for d in [&state.left_dock, &state.right_dock, &state.bottom_dock].into_iter().flatten() {
            leaves(d.panel(), &mut names);
        }
        names.sort();
        let mut expected: Vec<String> = PANEL_NAMES.iter().map(|s| s.to_string()).collect();
        expected.sort();
        if names != expected {
            return false;
        }
        cx.set_global(LoadingPanels(self.panels.clone()));
        let r = self.dock.update(cx, |area, cx| area.load(state, window, cx));
        cx.set_global(LoadingPanels::default());
        if r.is_err() {
            self.default_layout(window, cx);
            return false;
        }
        true
    }

    /// The preset's built-in arrangement (UX review L1).
    fn preset_layout(&mut self, preset: Preset, window: &mut Window, cx: &mut Context<Self>) {
        if preset == Preset::Design {
            return self.default_layout(window, cx);
        }
        let p: Vec<Arc<dyn BasePanelView>> = PANEL_NAMES.iter().map(|n| self.panel(n)).collect();
        let (doc, flow, data, inspector, agent, log) = (&p[0], &p[1], &p[2], &p[3], &p[4], &p[5]);
        self.dock.update(cx, |area, cx| {
            match preset {
                // the document wide, the inspector beside it, the graphs one click away
                Preset::Write => {
                    area.set_center(DockLayout::tabs().panel_view(doc.clone(), cx), window, cx);
                    area.set_dock(DockPlacement::Left, DockLayout::tabs().panel_view(flow.clone(), cx).panel_view(data.clone(), cx), window, cx);
                    area.set_dock(DockPlacement::Right, DockLayout::tabs().panel_view(inspector.clone(), cx).panel_view(agent.clone(), cx), window, cx);
                    area.set_dock_size(DockPlacement::Left, px(520.), window, cx);
                    area.set_dock_size(DockPlacement::Right, px(420.), window, cx);
                }
                // the flow graph wide, the log (trace, problems) tall, the inspector beside it
                Preset::Debug => {
                    area.set_center(DockLayout::tabs().panel_view(flow.clone(), cx).panel_view(data.clone(), cx), window, cx);
                    area.set_dock(DockPlacement::Left, DockLayout::tabs().panel_view(doc.clone(), cx), window, cx);
                    area.set_dock(DockPlacement::Right, DockLayout::tabs().panel_view(inspector.clone(), cx).panel_view(agent.clone(), cx), window, cx);
                    area.set_dock_size(DockPlacement::Left, px(480.), window, cx);
                    area.set_dock_size(DockPlacement::Right, px(400.), window, cx);
                }
                Preset::Design => unreachable!(),
            }
            area.set_dock(DockPlacement::Bottom, DockLayout::tabs().panel_view(log.clone(), cx), window, cx);
            area.set_dock_size(DockPlacement::Bottom, px(if preset == Preset::Debug { 300. } else { 170. }), window, cx);
        });
        let open = |pl: DockPlacement| match (preset, pl) {
            (Preset::Write, DockPlacement::Left) | (Preset::Debug, DockPlacement::Left) => false,
            (Preset::Write, DockPlacement::Bottom) => false,
            _ => true,
        };
        for pl in [DockPlacement::Left, DockPlacement::Right, DockPlacement::Bottom] {
            if self.dock.read(cx).is_dock_open(pl) != open(pl) {
                self.toggle_dock(pl, window, cx);
            }
        }
    }

    /// Switch workspace preset: remember this one's arrangement, bring back the other's.
    fn switch_preset(&mut self, preset: Preset, window: &mut Window, cx: &mut Context<Self>) {
        let dir = self.dir(cx);
        let current = self.presets.current;
        if let Ok(v) = serde_json::to_value(self.dock.read(cx).dump(cx)) {
            self.presets.layouts.insert(current.key().to_string(), v);
        }
        self.presets.current = preset;
        let saved = self.presets.layouts.get(preset.key()).cloned().and_then(|v| serde_json::from_value::<DockAreaState>(v).ok());
        if !saved.is_some_and(|s| self.load_state(s, window, cx)) {
            self.preset_layout(preset, window, cx);
        }
        self.presets.save(&dir);
        self.save_layout(cx);
        cx.notify();
    }

    fn schedule_layout_save(&mut self, cx: &mut Context<Self>) {
        self.layout_gen += 1;
        let generation = self.layout_gen;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(500)).await;
            let _ = this.update(cx, |this, cx| {
                if this.layout_gen == generation {
                    this.save_layout(cx);
                }
            });
        })
        .detach();
    }

    fn save_layout(&mut self, cx: &mut App) {
        let dir = self.state.read(cx).project.dir.clone();
        let state = self.dock.read(cx).dump(cx);
        if let Ok(json) = serde_json::to_string_pretty(&state) {
            let p = config::dock_path(&dir);
            let _ = std::fs::create_dir_all(p.parent().unwrap());
            if std::fs::read_to_string(&p).ok().as_deref() != Some(json.as_str()) {
                let _ = std::fs::write(p, json);
            }
        }
    }

    fn toggle_dock(&mut self, placement: DockPlacement, window: &mut Window, cx: &mut Context<Self>) {
        self.dock.update(cx, |area, cx| area.toggle_dock(placement, window, cx));
    }

    fn dir(&self, cx: &App) -> PathBuf {
        self.state.read(cx).project.dir.clone()
    }

    /// Ask before closing a window with unsaved files; true closes now.
    fn should_close(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let dirty = self.state.read(cx).project.dirty_files();
        if self.closing || dirty.is_empty() {
            self.save_layout(cx);
            return true;
        }
        let rx = window.prompt(
            PromptLevel::Warning,
            &format!("Save changes to {} file(s) before closing {}?", dirty.len(), self.state.read(cx).project.name),
            Some(&format!("{}\n\nYour changes are lost if you don't save them.", dirty.join("\n"))),
            &["Save", "Don't Save", "Cancel"],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            let Ok(answer) = rx.await else { return };
            let _ = this.update_in(cx, |this, window, cx| {
                let close = match answer {
                    0 => this.state.update(cx, |s, cx| s.save_all(cx)),
                    1 => true,
                    _ => false,
                };
                if close {
                    this.closing = true;
                    this.save_layout(cx);
                    window.remove_window();
                }
            });
        })
        .detach();
        false
    }

    fn close_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.should_close(window, cx) {
            window.remove_window();
        }
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        let active = self.state.read(cx).active_path();
        self.state.update(cx, |s, cx| match active {
            Some(p) => {
                s.save_file(&p, cx);
            }
            None => {
                s.save_all(cx);
            }
        });
    }

    fn new_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let dir = self.state.read(cx).project.source_dir();
        let rx = cx.prompt_for_new_path(&dir, Some("untitled.wh.md"));
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(mut path))) = rx.await else { return };
            let _ = this.update_in(cx, |this, window, cx| {
                let name = path.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
                if !name.ends_with(".wh.md") {
                    path.set_file_name(format!("{}.wh.md", name.trim_end_matches(".md")));
                }
                let rel = this.state.read(cx).project.rel_path(&path).filter(|r| this.state.read(cx).project.is_source(r));
                let Some(rel) = rel else {
                    let src = this.state.read(cx).project.sources.join(", ");
                    return alert(window, cx, "That is not in the project's sources", &format!("New files go under {src} inside the project."));
                };
                if this.state.read(cx).project.files.contains(&rel) {
                    return alert(window, cx, "That file is already part of the project", &rel);
                }
                let stem = name.trim_end_matches(".wh.md").trim_end_matches(".md");
                let text = config::new_file_text(&config::snake_ident(stem));
                if let Some(p) = path.parent() {
                    let _ = std::fs::create_dir_all(p);
                }
                match std::fs::write(&path, &text) {
                    Ok(()) => this.state.update(cx, |s, cx| s.add_file(&rel, &text, cx)),
                    Err(e) => alert(window, cx, "Could not create the file", &e.to_string()),
                }
            });
        })
        .detach();
    }

    fn open_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: true, prompt: Some("Open".into()) });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else { return };
            let _ = this.update_in(cx, |this, window, cx| {
                for path in paths {
                    let p = &this.state.read(cx).project;
                    let rel = p.rel_path(&path).filter(|r| p.is_source(r));
                    let known = rel.as_ref().is_some_and(|r| p.files.contains(r));
                    match rel {
                        Some(rel) if known => this.state.update(cx, |s, cx| s.open_tab(&rel, cx)),
                        Some(rel) => match std::fs::read_to_string(&path) {
                            Ok(text) => this.state.update(cx, |s, cx| s.add_file(&rel, &text, cx)),
                            Err(e) => alert(window, cx, "Could not read the file", &e.to_string()),
                        },
                        None => alert(
                            window,
                            cx,
                            "Not a source file of this project",
                            &format!("{}\n\nOpen a .wh.md file under {} of {}.", path.display(), p.sources.join(", "), p.dir.display()),
                        ),
                    }
                }
            });
        })
        .detach();
    }

    fn save_as(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(from) = self.state.read(cx).active_path() else { return };
        let dir = self.dir(cx).join(&from);
        let (folder, name) = (dir.parent().map(Path::to_path_buf).unwrap_or_default(), dir.file_name().map(|f| f.to_string_lossy().into_owned()));
        let rx = cx.prompt_for_new_path(&folder, name.as_deref());
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(path))) = rx.await else { return };
            let _ = this.update_in(cx, |this, _, cx| this.state.update(cx, |s, cx| s.save_as(&from, &path, cx)));
        })
        .detach();
    }

    /// ⌘W: close the active tab (asking about unsaved changes), or the window when no tab is open.
    fn close_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.state.read(cx).active_path() else { return self.close_project(window, cx) };
        if !self.state.read(cx).project.docs.is_dirty(&path) {
            return self.state.update(cx, |s, cx| s.close_tab(&path, cx));
        }
        let rx = window.prompt(
            PromptLevel::Warning,
            &format!("Save changes to {path}?"),
            Some("If you don't save, the file goes back to its saved text (⌘Z brings the edits back)."),
            &["Save", "Don't Save", "Cancel"],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            let Ok(answer) = rx.await else { return };
            let _ = this.update_in(cx, |this, _, cx| {
                this.state.update(cx, |s, cx| match answer {
                    0 => {
                        if s.save_file(&path, cx) {
                            s.close_tab(&path, cx);
                        }
                    }
                    1 => {
                        s.revert_file(&path, cx);
                        s.close_tab(&path, cx);
                    }
                    _ => {}
                })
            });
        })
        .detach();
    }

    fn revert_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.state.read(cx).active_path() else { return };
        if !self.state.read(cx).project.docs.is_dirty(&path) {
            return self.state.update(cx, |s, cx| s.revert_file(&path, cx));
        }
        let rx = window.prompt(
            PromptLevel::Warning,
            &format!("Revert {path} to its saved text?"),
            Some("Unsaved changes are discarded; ⌘Z brings them back."),
            &["Revert", "Cancel"],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(0) = rx.await {
                let _ = this.update_in(cx, |this, _, cx| this.state.update(cx, |s, cx| s.revert_file(&path, cx)));
            }
        })
        .detach();
    }

    /// Project → Optimize: the selected node must be a hole.
    fn optimize_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let hole = self.state.read(cx).project.selected_node().filter(|n| n.is_hole()).map(|n| n.id.clone());
        match hole {
            Some(id) => self.state.update(cx, |s, cx| s.run_job(JobKind::Optimize(id), cx)),
            None => alert(window, cx, "Select a hole to optimize", "Optimize runs a variant tournament for one synthesized definition (a def without an extern body)."),
        }
    }

    /// Route a menu / key action to this workspace.
    fn handle(&mut self, action: &dyn Action, window: &mut Window, cx: &mut Context<Self>) {
        // compare by name: the routed actions are unit structs
        let is = |other: &dyn Action| action.name() == other.name();
        if is(&GlobalUndo) {
            self.state.update(cx, |s, cx| s.undo(cx));
        } else if is(&GlobalRedo) {
            self.state.update(cx, |s, cx| s.redo(cx));
        } else if is(&Save) {
            self.save(cx);
        } else if is(&SaveAll) {
            self.state.update(cx, |s, cx| {
                s.save_all(cx);
            });
        } else if is(&SaveAs) {
            self.save_as(window, cx);
        } else if is(&NewFile) {
            self.new_file(window, cx);
        } else if is(&OpenFile) {
            self.open_file(window, cx);
        } else if is(&CloseFile) {
            self.close_file(window, cx);
        } else if is(&RevertFile) {
            self.revert_file(window, cx);
        } else if is(&CloseProject) {
            self.close_project(window, cx);
        } else if is(&ToggleDocument) {
            self.toggle_dock(DockPlacement::Left, window, cx);
        } else if is(&ToggleSidebar) {
            self.toggle_dock(DockPlacement::Right, window, cx);
        } else if is(&ToggleLog) {
            self.toggle_dock(DockPlacement::Bottom, window, cx);
        } else if is(&ResetLayout) {
            let p = self.presets.current;
            self.presets.layouts.remove(p.key());
            self.preset_layout(p, window, cx);
        } else if is(&CheckProject) {
            self.state.update(cx, |s, cx| s.check(cx));
        } else if is(&Build) {
            self.state.update(cx, |s, cx| s.run_job(JobKind::Build { release: false }, cx));
        } else if is(&BuildRelease) {
            self.state.update(cx, |s, cx| s.run_job(JobKind::Build { release: true }, cx));
        } else if is(&SynthesizeAll) {
            self.state.update(cx, |s, cx| s.run_job(JobKind::SynthesizeAll, cx));
        } else if is(&TestProject) {
            self.state.update(cx, |s, cx| s.run_job(JobKind::Test, cx));
        } else if is(&Optimize) {
            self.optimize_selected(window, cx);
        } else if is(&ToggleLiveValues) {
            self.state.update(cx, |s, cx| {
                let on = !s.live.on;
                s.set_live(on, cx)
            });
        } else if is(&PresetWrite) {
            self.switch_preset(Preset::Write, window, cx);
        } else if is(&PresetDesign) {
            self.switch_preset(Preset::Design, window, cx);
        } else if is(&PresetDebug) {
            self.switch_preset(Preset::Debug, window, cx);
        } else if is(&NextProblem) {
            self.state.update(cx, |s, cx| s.step_problem(true, cx));
        } else if is(&PreviousProblem) {
            self.state.update(cx, |s, cx| s.step_problem(false, cx));
        } else if is(&CancelJob) {
            self.state.update(cx, |s, cx| s.cancel_job(cx));
        } else if is(&RunApp) {
            self.state.update(cx, |s, cx| s.run_app(cx));
        }
    }
}

fn alert<T: 'static>(window: &mut Window, cx: &mut Context<T>, message: &str, detail: &str) {
    let rx = window.prompt(PromptLevel::Warning, message, Some(detail), &["OK"], cx);
    cx.spawn(async move |_, _| {
        let _ = rx.await;
    })
    .detach();
}

/// A warning on the active window (or stderr when there is none).
fn alert_app(cx: &mut App, message: &str, detail: &str) {
    let Some(h) = cx.active_window().or_else(|| cx.windows().first().copied()) else {
        eprintln!("wh-ide: {message}: {detail}");
        return;
    };
    let (message, detail) = (message.to_string(), detail.to_string());
    let _ = h.update(cx, |_, window, cx| {
        let rx = window.prompt(PromptLevel::Warning, &message, Some(&detail), &["OK"], cx);
        cx.spawn(async move |_| {
            let _ = rx.await;
        })
        .detach();
    });
}

impl Render for Workspace {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let st = self.state.read(cx);
        let p = &st.project;
        let errors = p.model.diagnostics.iter().filter(|d| d.diag.severity == wh_syntax::Severity::Error).count();
        let failing = st.tests.values().filter(|r| !r.ok()).count();
        let tests = if st.tests_running {
            "  ·  specs running…".to_string()
        } else if st.tests.is_empty() {
            String::new()
        } else {
            format!("  ·  specs: {} ✓ {} ✗", st.tests.len() - failing, failing)
        };
        let job = st.job.as_ref().map(|j| {
            let count = if j.total > 0 { format!(" {}/{}", j.done, j.total) } else { String::new() };
            let phase = j.phase.map(|p| p.label()).unwrap_or("starting");
            let detail = if j.detail.is_empty() { String::new() } else { format!(" — {}", j.detail) };
            format!("{}: {phase}{count}{detail} ({:.0} s)", j.kind.label(), j.started.elapsed().as_secs_f32())
        });
        let running = job.is_some();
        let problems = st.job_problems.len();
        let toolbar = h_flex()
            .gap_1()
            .px_2()
            .py_0p5()
            .border_b_1()
            .border_color(theme.border)
            .child(Button::new("tb-build").xsmall().primary().label("Build").disabled(running).on_click(cx.listener(|this, _, w, cx| this.handle(&Build, w, cx))))
            .child(Button::new("tb-test").xsmall().outline().label("Test").disabled(running).on_click(cx.listener(|this, _, w, cx| this.handle(&TestProject, w, cx))))
            .child(Button::new("tb-run").xsmall().outline().label("Run").on_click(cx.listener(|this, _, w, cx| this.handle(&RunApp, w, cx))))
            .child(Button::new("tb-synth").xsmall().ghost().label("Synthesize All").disabled(running).on_click(cx.listener(|this, _, w, cx| this.handle(&SynthesizeAll, w, cx))))
            .child(Button::new("tb-release").xsmall().ghost().label("Build Release").disabled(running).on_click(cx.listener(|this, _, w, cx| this.handle(&BuildRelease, w, cx))))
            .when(running, |d| d.child(Button::new("tb-cancel").xsmall().danger().label("Cancel").on_click(cx.listener(|this, _, w, cx| this.handle(&CancelJob, w, cx)))))
            .child(div().flex_1().text_xs().text_color(theme.muted_foreground).overflow_hidden().whitespace_nowrap().child(job.clone().unwrap_or_default()))
            .children(Preset::ALL.into_iter().map(|p| {
                Button::new(SharedString::from(format!("preset-{}", p.key()))).xsmall().ghost().toggled(self.presets.current == p).label(p.label()).on_click(
                    cx.listener(move |this, _, w, cx| this.switch_preset(p, w, cx)),
                )
            }))
            .when(problems > 0 && !running, |d| d.child(div().text_xs().text_color(theme.danger).child(format!("{problems} problem(s) from the last job — see the Log"))));
        if running {
            // the elapsed time ticks while a job runs
            cx.on_next_frame(_window, |_, _, cx| cx.notify());
        }
        let warnings = p.model.diagnostics.iter().filter(|d| d.diag.severity == wh_syntax::Severity::Warning).count();
        let live = if !st.live.on {
            String::new()
        } else if st.live.running {
            "  ·  live …".to_string()
        } else if st.live.nodes.is_empty() {
            if st.live.stale.is_some() { "  ·  live …".to_string() } else { String::new() }
        } else {
            let (pass, fail, other) = st.live.counts();
            format!("  ·  live {pass} ✓ {fail} ✗{}", if other > 0 { format!(" {other} ⏱") } else { String::new() })
        };
        let rest = format!(
            "{}{}{}{}  ·  {}",
            live,
            tests,
            if p.ghosts.is_empty() { String::new() } else { format!("  ·  {} stale node(s) from unparsable blocks", p.ghosts.len()) },
            if st.pending.is_empty() { String::new() } else { format!("  ·  {} author edit(s) to review", st.pending.len()) },
            p.selection.as_deref().and_then(|id| crate::capsule::selection_label(&p.display, id)).unwrap_or_default(),
        );
        let head = format!("{}{}  ·  {} nodes  ·  ", p.name, if p.is_dirty() { " •" } else { "" }, p.model.nodes.len());
        let counts = div()
            .id("problem-count")
            .cursor_pointer()
            .hover(|d| d.text_color(theme.foreground))
            .when(errors > 0, |d| d.text_color(theme.danger))
            .on_click(cx.listener(|this, _, window, cx| {
                // open the bottom dock on the Problems tab
                if !this.dock.read(cx).is_dock_open(DockPlacement::Bottom) {
                    this.toggle_dock(DockPlacement::Bottom, window, cx);
                }
                this.state.update(cx, |s, cx| s.show_problems(cx));
            }))
            .child(format!("{errors} error(s), {warnings} warning(s)"));
        let status = h_flex().child(head).child(counts).child(rest);
        // ⇧⌘P / ⌘P / ⌘. work wherever focus is in the window (ide-ux's palette)
        crate::palette::on_palette(v_flex(), &self.state)
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            .key_context("Workspace")
            .track_focus(&self.focus)
            .child(toolbar)
            .child(div().flex_1().min_h_0().child(self.dock.clone()))
            .child(
                h_flex()
                    .px_3()
                    .py_0p5()
                    .text_xs()
                    .bg(theme.status_bar)
                    .border_t_1()
                    .border_color(theme.border)
                    .text_color(theme.muted_foreground)
                    .child(status),
            )
    }
}

/// Shown when no project is open.
struct Welcome {
    focus: FocusHandle,
}

impl Render for Welcome {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let mut recent = v_flex().gap_1().w(px(460.));
        let list = config::recent_projects();
        if list.is_empty() {
            recent = recent.child(div().text_xs().text_color(theme.muted_foreground).child("No recent projects."));
        }
        for (i, p) in list.into_iter().enumerate() {
            let name = p.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
            let path = p.clone();
            recent = recent.child(
                h_flex()
                    .id(("recent", i))
                    .gap_2()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .cursor_pointer()
                    .hover(|d| d.bg(theme.list_hover))
                    .on_click(cx.listener(move |_, _, _, cx| {
                        let path = path.clone();
                        cx.defer(move |cx| open_project(path, cx));
                    }))
                    .child(div().font_weight(FontWeight::SEMIBOLD).child(name))
                    .child(div().flex_1().text_xs().text_color(theme.muted_foreground).overflow_hidden().whitespace_nowrap().child(p.display().to_string())),
            );
        }
        v_flex()
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            .track_focus(&self.focus)
            .items_center()
            .justify_center()
            .gap_4()
            .child(div().text_3xl().font_weight(FontWeight::BOLD).child("Wild Horse"))
            .child(div().text_color(theme.muted_foreground).child("Literate programs: the .wh.md document is the source of truth."))
            .child(
                h_flex()
                    .gap_2()
                    .child(Button::new("new").primary().label("New Project…").on_click(cx.listener(|_, _, _, cx| cx.defer(new_project))))
                    .child(Button::new("open").outline().label("Open Project…").on_click(cx.listener(|_, _, _, cx| cx.defer(open_project_dialog)))),
            )
            .child(div().mt_4().text_xs().font_weight(FontWeight::SEMIBOLD).text_color(theme.muted_foreground).child("RECENT"))
            .child(recent)
    }
}

fn window_options(title: String, cx: &mut App) -> WindowOptions {
    let n = cx.windows().len() as f32;
    let mut bounds = Bounds::centered(None, size(px(1600.), px(1000.)), cx);
    bounds.origin.x += px(28. * n);
    bounds.origin.y += px(28. * n);
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: Some(TitlebarOptions { title: Some(title.into()), ..Default::default() }),
        ..Default::default()
    }
}

fn show_welcome(cx: &mut App) {
    if let Some(h) = cx.default_global::<Windows>().welcome {
        let _ = h.update(cx, |_, window, _| window.activate_window());
        return;
    }
    let mut options = window_options("Wild Horse".into(), cx);
    options.window_bounds = Some(WindowBounds::Windowed(Bounds::centered(None, size(px(720.), px(520.)), cx)));
    if let Ok((h, _)) = gpui_kit::open_window(options, cx, |window, cx| {
        theme::follow_system(window);
        cx.new(|cx| Welcome { focus: cx.focus_handle() })
    }) {
        cx.default_global::<Windows>().welcome = Some(h);
    }
}

/// Open `dir` in its own window (or bring its window to the front).
fn open_project(dir: PathBuf, cx: &mut App) {
    let dir = dir.canonicalize().unwrap_or(dir);
    let open: Vec<(AnyWindowHandle, Entity<Workspace>)> =
        cx.default_global::<Windows>().projects.iter().filter_map(|(h, w)| Some((*h, w.upgrade()?))).collect();
    if let Some((h, _)) = open.iter().find(|(_, w)| w.read(cx).dir(cx) == dir) {
        let _ = h.update(cx, |_, window, _| window.activate_window());
        return;
    }
    let project = match Project::open(&dir) {
        Ok(p) => p,
        Err(e) => return alert_app(cx, "Cannot open the project", &format!("{}: {e}", dir.display())),
    };
    config::add_recent(&dir);
    set_menus(cx);
    let title = format!("Wild Horse — {}", project.name);
    let options = window_options(title, cx);
    let project = std::cell::RefCell::new(Some(project));
    match gpui_kit::open_window(options, cx, |window, cx| {
        theme::follow_system(window);
        cx.new(|cx| Workspace::new(project.borrow_mut().take().unwrap(), window, cx))
    }) {
        Ok((h, ws)) => {
            let w = cx.default_global::<Windows>();
            w.projects.push((h, ws.downgrade()));
            if let Some(welcome) = w.welcome.take() {
                let _ = welcome.update(cx, |_, window, _| window.remove_window());
            }
        }
        Err(e) => alert_app(cx, "Cannot open a window", &e.to_string()),
    }
}

fn open_project_dialog(cx: &mut App) {
    let rx = cx.prompt_for_paths(PathPromptOptions { files: false, directories: true, multiple: false, prompt: Some("Open Project".into()) });
    cx.spawn(async move |cx| {
        let Ok(Ok(Some(paths))) = rx.await else { return };
        let _ = cx.update(|cx| {
            let Some(dir) = paths.into_iter().next() else { return };
            if config::is_project(&dir) {
                open_project(dir, cx);
            } else {
                alert_app(cx, "Not a Wild Horse project", &format!("{} has no wildhorse.toml. Use New Project… to create one.", dir.display()));
            }
        });
    })
    .detach();
}

/// Ask for a new (or empty) folder, scaffold a project there (`wh new`), open it.
fn new_project(cx: &mut App) {
    let start = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let rx = cx.prompt_for_new_path(&start, Some("my_app"));
    cx.spawn(async move |cx| {
        let Ok(Ok(Some(dir))) = rx.await else { return };
        let _ = cx.update(|cx| {
            if config::is_project(&dir) {
                return open_project(dir, cx);
            }
            let name = config::snake_ident(&dir.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default());
            match wh_project::scaffold::new_project(&dir, &name) {
                Ok(()) => open_project(dir, cx),
                Err(e) => alert_app(cx, "Cannot create the project", &format!("{}: {e:#}", dir.display())),
            }
        });
    })
    .detach();
}

/// Run `f` on the workspace of the active window, or of the frontmost
/// project window when no window is active (app in the background).
fn with_workspace(cx: &mut App, f: impl FnOnce(&mut Workspace, &mut Window, &mut Context<Workspace>)) -> bool {
    let order: Vec<AnyWindowHandle> = cx.active_window().into_iter().chain(cx.window_stack().unwrap_or_default()).collect();
    let projects = cx.default_global::<Windows>().projects.clone();
    let found = order.iter().find_map(|h| projects.iter().find(|(p, _)| p == h).and_then(|(p, w)| Some((*p, w.upgrade()?))));
    let found = found.or_else(|| projects.iter().find_map(|(p, w)| Some((*p, w.upgrade()?))));
    let Some((handle, ws)) = found else { return false };
    handle.update(cx, |_, window, cx| ws.update(cx, |ws, cx| f(ws, window, cx))).is_ok()
}

/// ⌘Q: one prompt for every project with unsaved files.
fn quit(cx: &mut App) {
    let open: Vec<(AnyWindowHandle, Entity<Workspace>)> =
        cx.default_global::<Windows>().projects.iter().filter_map(|(h, w)| Some((*h, w.upgrade()?))).collect();
    let dirty: Vec<(AnyWindowHandle, Entity<Workspace>)> = open.into_iter().filter(|(_, w)| w.read(cx).state.read(cx).project.is_dirty()).collect();
    let finish = |cx: &mut App| {
        let ws: Vec<Entity<Workspace>> = cx.default_global::<Windows>().projects.iter().filter_map(|(_, w)| w.upgrade()).collect();
        for w in ws {
            w.update(cx, |w, cx| w.save_layout(cx));
        }
        cx.default_global::<Windows>().quitting = true;
        cx.quit();
    };
    if dirty.is_empty() {
        return finish(cx);
    }
    let files: Vec<String> = dirty
        .iter()
        .flat_map(|(_, w)| {
            let p = &w.read(cx).state.read(cx).project;
            p.dirty_files().into_iter().map(move |f| format!("{} — {f}", p.name))
        })
        .collect();
    let (h, _) = dirty[0].clone();
    let _ = h.update(cx, |_, window, cx| {
        window.activate_window();
        let rx = window.prompt(
            PromptLevel::Warning,
            &format!("Save changes to {} file(s) before quitting?", files.len()),
            Some(&files.join("\n")),
            &["Save All", "Don't Save", "Cancel"],
            cx,
        );
        cx.spawn(async move |cx| {
            let Ok(answer) = rx.await else { return };
            let _ = cx.update(|cx| match answer {
                0 => {
                    let ok = dirty.iter().all(|(_, w)| w.read(cx).state.clone().update(cx, |s, cx| s.save_all(cx)));
                    if ok {
                        finish(cx);
                    }
                }
                1 => finish(cx),
                _ => {}
            });
        })
        .detach();
    });
}

fn set_menus(cx: &mut App) {
    let recent: Vec<MenuItem> = config::recent_projects()
        .into_iter()
        .map(|p| MenuItem::action(p.display().to_string(), OpenRecent { path: p }))
        .chain([MenuItem::separator(), MenuItem::action("Clear Recent", ClearRecent)])
        .collect();
    cx.set_menus([
        Menu::new("Wild Horse").items([
            MenuItem::action("About Wild Horse", About),
            MenuItem::separator(),
            MenuItem::os_submenu("Services", SystemMenuType::Services),
            MenuItem::separator(),
            MenuItem::action("Hide Wild Horse", Hide),
            MenuItem::action("Hide Others", HideOthers),
            MenuItem::action("Show All", ShowAll),
            MenuItem::separator(),
            MenuItem::action("Quit Wild Horse", Quit),
        ]),
        Menu::new("File").items([
            MenuItem::action("New Project…", NewProject),
            MenuItem::action("Open Project…", OpenProject),
            MenuItem::submenu(Menu::new("Open Recent").items(recent)),
            MenuItem::action("Close Project", CloseProject),
            MenuItem::separator(),
            MenuItem::action("New File…", NewFile),
            MenuItem::action("Open File…", OpenFile),
            MenuItem::separator(),
            MenuItem::action("Save", Save),
            MenuItem::action("Save All", SaveAll),
            MenuItem::action("Save As…", SaveAs),
            MenuItem::action("Revert File", RevertFile),
            MenuItem::separator(),
            MenuItem::action("Close File", CloseFile),
        ]),
        Menu::new("Edit").items([
            MenuItem::os_action("Undo", GlobalUndo, OsAction::Undo),
            MenuItem::os_action("Redo", GlobalRedo, OsAction::Redo),
            MenuItem::separator(),
            MenuItem::os_action("Cut", InputCut, OsAction::Cut),
            MenuItem::os_action("Copy", InputCopy, OsAction::Copy),
            MenuItem::os_action("Paste", InputPaste, OsAction::Paste),
            MenuItem::os_action("Select All", InputSelectAll, OsAction::SelectAll),
        ]),
        Menu::new("View").items([
            MenuItem::action("Toggle Document", ToggleDocument),
            MenuItem::action("Toggle Inspector & Agent", ToggleSidebar),
            MenuItem::action("Toggle Log", ToggleLog),
            MenuItem::separator(),
            MenuItem::submenu(Menu::new("Workspace").items([
                MenuItem::action("Write", PresetWrite),
                MenuItem::action("Design", PresetDesign),
                MenuItem::action("Debug", PresetDebug),
            ])),
            MenuItem::action("Maximize Panel", gpui_kit::component::dock::ToggleZoom),
            MenuItem::action("Reset Layout", ResetLayout),
            MenuItem::separator(),
            MenuItem::action("Command Palette…", crate::palette::CommandPalette),
            MenuItem::action("Go to Node…", crate::palette::GoToNode),
            MenuItem::action("Quick Actions…", crate::palette::QuickActions),
            MenuItem::separator(),
            MenuItem::action("Live Values", ToggleLiveValues),
            MenuItem::separator(),
            MenuItem::action("Next Problem", NextProblem),
            MenuItem::action("Previous Problem", PreviousProblem),
            MenuItem::separator(),
            MenuItem::submenu(Menu::new("Theme").items([
                MenuItem::action("Follow System", theme::ThemeSystem),
                MenuItem::action("Light", theme::ThemeLight),
                MenuItem::action("Dark", theme::ThemeDark),
            ])),
        ]),
        Menu::new("Project").items([
            MenuItem::action("Build", Build),
            MenuItem::action("Build Release", BuildRelease),
            MenuItem::action("Synthesize All", SynthesizeAll),
            MenuItem::separator(),
            MenuItem::action("Check", CheckProject),
            MenuItem::action("Test", TestProject),
            MenuItem::action("Run", RunApp),
            MenuItem::separator(),
            MenuItem::action("Optimize Selected Hole…", Optimize),
            MenuItem::separator(),
            MenuItem::action("Cancel", CancelJob),
        ]),
        Menu::new("Window").items([MenuItem::action("Minimize", Minimize), MenuItem::action("Zoom", Zoom)]),
    ]);
}

fn register_actions(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-z", GlobalUndo, None),
        KeyBinding::new("cmd-shift-z", GlobalRedo, None),
        KeyBinding::new("cmd-s", Save, None),
        KeyBinding::new("alt-cmd-s", SaveAll, None),
        KeyBinding::new("cmd-shift-s", SaveAs, None),
        KeyBinding::new("cmd-shift-n", NewProject, None),
        KeyBinding::new("cmd-shift-o", OpenProject, None),
        KeyBinding::new("cmd-n", NewFile, None),
        KeyBinding::new("cmd-o", OpenFile, None),
        KeyBinding::new("cmd-w", CloseFile, None),
        KeyBinding::new("cmd-shift-w", CloseProject, None),
        KeyBinding::new("cmd-1", ToggleDocument, None),
        KeyBinding::new("cmd-2", ToggleSidebar, None),
        KeyBinding::new("cmd-3", ToggleLog, None),
        KeyBinding::new("cmd-shift-b", CheckProject, None),
        KeyBinding::new("cmd-r", RunApp, None),
        KeyBinding::new("cmd-b", Build, None),
        KeyBinding::new("cmd-shift-r", BuildRelease, None),
        KeyBinding::new("cmd-u", TestProject, None),
        // ⌘. is the palette's Quick Actions
        KeyBinding::new("cmd-shift-.", CancelJob, None),
        KeyBinding::new("f8", NextProblem, None),
        KeyBinding::new("alt-cmd-l", ToggleLiveValues, None),
        KeyBinding::new("ctrl-1", PresetWrite, None),
        KeyBinding::new("ctrl-2", PresetDesign, None),
        KeyBinding::new("ctrl-3", PresetDebug, None),
        // maximize the focused panel (its tab group handles the action)
        KeyBinding::new("shift-escape", gpui_kit::component::dock::ToggleZoom, None),
        KeyBinding::new("shift-f8", PreviousProblem, None),
        KeyBinding::new("cmd-h", Hide, None),
        KeyBinding::new("alt-cmd-h", HideOthers, None),
        KeyBinding::new("cmd-m", Minimize, None),
        KeyBinding::new("cmd-q", Quit, None),
        // the document is the undo unit, not a single editor: override the inputs' own history
        KeyBinding::new("cmd-z", GlobalUndo, Some("Input")),
        KeyBinding::new("cmd-shift-z", GlobalRedo, Some("Input")),
        KeyBinding::new("cmd-s", Save, Some("Input")),
        KeyBinding::new("cmd-w", CloseFile, Some("Input")),
    ]);
    graph_panel::bind_keys(cx);
    // workspace actions go to the active project window (focus may be anywhere in it);
    // deferred, because a window is leased while it dispatches and cannot be updated
    macro_rules! route {
        ($($a:ty),*) => {$(
            cx.on_action(|a: &$a, cx| {
                let a = a.clone();
                cx.defer(move |cx| {
                    with_workspace(cx, |ws, window, cx| ws.handle(&a, window, cx));
                });
            });
        )*};
    }
    route!(GlobalUndo, GlobalRedo, Save, SaveAll, SaveAs, NewFile, OpenFile, CloseFile, RevertFile, CloseProject);
    route!(ToggleDocument, ToggleSidebar, ToggleLog, ResetLayout, CheckProject, RunApp);
    route!(Build, BuildRelease, SynthesizeAll, TestProject, Optimize, CancelJob, NextProblem, PreviousProblem);
    route!(PresetWrite, PresetDesign, PresetDebug, ToggleLiveValues);
    // the rest touch windows too: run after the dispatch as well
    fn later<A: Action + Clone>(cx: &mut App, f: fn(&A, &mut App)) {
        cx.on_action(move |a: &A, cx| {
            let a = a.clone();
            cx.defer(move |cx| f(&a, cx));
        });
    }
    later(cx, |_: &NewProject, cx| new_project(cx));
    later(cx, |_: &OpenProject, cx| open_project_dialog(cx));
    later(cx, |a: &OpenRecent, cx| open_project(a.path.clone(), cx));
    later(cx, |_: &ClearRecent, cx| {
        config::clear_recent();
        set_menus(cx);
    });
    later(cx, |_: &Quit, cx| quit(cx));
    later(cx, |_: &Hide, cx| cx.hide());
    later(cx, |_: &HideOthers, cx| cx.hide_other_apps());
    later(cx, |_: &ShowAll, cx| cx.unhide_other_apps());
    later(cx, |_: &About, cx| {
        alert_app(cx, "Wild Horse", &format!("wh-ide {} — an AI-first literate-programming IDE for Rust.", env!("CARGO_PKG_VERSION")))
    });
    later(cx, |_: &Minimize, cx| {
        if let Some(h) = cx.active_window() {
            let _ = h.update(cx, |_, window, _| window.minimize_window());
        }
    });
    later(cx, |_: &Zoom, cx| {
        if let Some(h) = cx.active_window() {
            let _ = h.update(cx, |_, window, _| window.zoom_window());
        }
    });
    // closing the last project window brings back the welcome window
    cx.on_window_closed(|cx, id| {
        let w = cx.default_global::<Windows>();
        w.projects.retain(|(h, _)| h.window_id() != id);
        if w.welcome.is_some_and(|h| h.window_id() == id) {
            w.welcome = None;
            return;
        }
        if w.projects.is_empty() && w.welcome.is_none() && !w.quitting {
            show_welcome(cx);
        }
    })
    .detach();
    // `DockArea::load` rebuilds panels through the registry: hand back the live ones
    for name in PANEL_NAMES {
        register_panel(cx, name, move |_, _, cx| {
            cx.global::<LoadingPanels>().0.iter().find(|(n, _)| *n == name).map(|(_, p)| p.clone()).expect("a validated layout names only our panels")
        });
    }
}

fn main() {
    // offscreen renders for UI checks while the screen is locked (ide-ux; `--features snapshots`)
    #[cfg(feature = "snapshots")]
    if std::env::var_os("WH_IDE_SNAPSHOTS").is_some() {
        return snapshots::run();
    }
    let arg = std::env::args().nth(1).map(PathBuf::from);
    gpui_kit::application().with_assets(gpui_kit::assets::Assets).run(move |cx| {
        gpui_kit::init(cx);
        theme::init(cx);
        cx.set_global(Windows::default());
        cx.set_global(LoadingPanels::default());
        register_actions(cx);
        set_menus(cx);
        match arg {
            Some(dir) => {
                if let Err(e) = Project::open(&dir) {
                    eprintln!("wh-ide: cannot open {}: {e}", dir.display());
                    std::process::exit(1);
                }
                open_project(dir, cx);
            }
            None if config::is_project(Path::new(".")) => open_project(PathBuf::from("."), cx),
            None => show_welcome(cx),
        }
        cx.activate(true);
    });
}
