//! A reusable node-graph canvas: nodes with typed ports (one row per port),
//! bezier edges, section frames, pan (drag background / scroll), zoom
//! (pinch, ⌘-scroll), selection, node dragging and port-to-port connecting.
//!
//! Styling follows the UX review's §15.7: a kind stripe, glyph and 10% tint
//! in the header; ports on the node border, coloured by type family and
//! shaped by structure; quiet ✓ and loud ✗ status chips; an accent selection
//! ring. Below [`FULL_LOD`] zoom nodes collapse to their header with the name
//! kept at a readable size.
//!
//! The canvas knows nothing about Wild Horse ops: it renders a
//! [`wh_graph::Graph`] and emits [`CanvasEvent`]s; the owning panel turns
//! those into structured edits.

use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use wh_graph::{Cardinality, EdgeKind, GNodeKind, Graph, HEADER_H, PortDir, PortRef, PortShape, ROW_H, TypeFamily};

use crate::capsule::{self, Chip};
use crate::tokens::{Tokens, Tone, tokens};

actions!(canvas, [FitView, ZoomToSelection, ZoomIn, ZoomOut]);

/// Zoom at and above which nodes show their port rows.
pub const FULL_LOD: f32 = 0.7;
/// The smallest on-screen size of a node name.
const MIN_TITLE_PX: f32 = 11.0;
const MIN_ZOOM: f32 = 0.05;
const MAX_ZOOM: f32 = 2.5;

#[derive(Debug, Clone)]
#[allow(dead_code)] // payloads are part of the widget's API
pub enum CanvasEvent {
    SelectNode(usize),
    SelectEdge(usize),
    ClearSelection,
    /// A node was dragged to a new world position.
    Moved(usize, wh_graph::Point),
    /// The user dragged from one port to another.
    Connect { from: PortRef, to: PortRef },
    DoubleClick(usize),
    /// A node was dragged and dropped onto another one that accepts it
    /// (see [`GraphCanvas::can_drop`]), e.g. to reorder flow steps.
    DropOnto { node: usize, target: usize },
}

/// Decides whether a connection from `from` may end at `to` (for greying
/// out incompatible ports while dragging).
pub type Accepts = Rc<dyn Fn(&Graph, PortRef, PortRef) -> bool>;

/// Decides whether node `a` may be dropped onto node `b`.
pub type CanDrop = Rc<dyn Fn(&Graph, usize, usize) -> bool>;

enum Drag {
    Pan { start: Point<Pixels>, origin: (f32, f32) },
    Node { node: usize, grab: (f32, f32), moved: bool, over: Option<usize> },
    Connect { from: PortRef, to: Point<Pixels> },
}

pub struct GraphCanvas {
    pub graph: Graph,
    pub pos: Vec<wh_graph::Point>,
    pub offset: (f32, f32),
    pub zoom: f32,
    pub selected_node: Option<usize>,
    pub selected_edge: Option<usize>,
    /// Graph node ids drawn greyed (last-good copies of unparsable blocks).
    pub ghosts: BTreeSet<String>,
    /// Graph node ids highlighted as executed by the last run.
    pub executed: BTreeSet<String>,
    /// Status chip per graph node id (synthesis and spec results; a step shows its callee's).
    pub chips: BTreeMap<String, Chip>,
    /// Live value seen on the edge leaving a step, by step id: (value, is an error).
    pub edge_values: BTreeMap<String, (String, bool)>,
    pub accepts: Accepts,
    pub can_drop: CanDrop,
    pub focus: FocusHandle,
    bounds: Rc<Cell<Bounds<Pixels>>>,
    drag: Option<Drag>,
    hovered: Option<usize>,
    fitted: bool,
    /// Keep the graph fitted when the canvas is resized, until the user pans or zooms.
    auto_fit: bool,
    fitted_size: (f32, f32),
}

impl EventEmitter<CanvasEvent> for GraphCanvas {}

impl GraphCanvas {
    pub fn new(cx: &mut Context<Self>) -> Self {
        GraphCanvas {
            graph: Graph::default(),
            pos: vec![],
            offset: (24.0, 24.0),
            zoom: 0.85,
            selected_node: None,
            selected_edge: None,
            ghosts: BTreeSet::new(),
            executed: BTreeSet::new(),
            chips: Default::default(),
            edge_values: Default::default(),
            accepts: Rc::new(|_, _, _| true),
            can_drop: Rc::new(|_, _, _| false),
            focus: cx.focus_handle(),
            bounds: Rc::new(Cell::new(Bounds::default())),
            drag: None,
            hovered: None,
            fitted: false,
            auto_fit: true,
            fitted_size: (0.0, 0.0),
        }
    }

    pub fn set_graph(&mut self, graph: Graph, pos: Vec<wh_graph::Point>, cx: &mut Context<Self>) {
        // keep the selection on the same node id across rebuilds
        let sel = self.selected_node.and_then(|i| self.graph.nodes.get(i)).map(|n| n.id.clone());
        let sel_edge = self.selected_edge.and_then(|i| self.graph.edges.get(i)).map(|e| self.edge_key(e));
        let hov = self.hovered.and_then(|i| self.graph.nodes.get(i)).map(|n| n.id.clone());
        self.graph = graph;
        self.pos = pos;
        self.selected_node = sel.and_then(|id| self.graph.index_of(&id));
        self.selected_edge = sel_edge.and_then(|k| self.graph.edges.iter().position(|e| self.edge_key(e) == k));
        self.hovered = hov.and_then(|id| self.graph.index_of(&id));
        cx.notify();
    }

    fn edge_key(&self, e: &wh_graph::GEdge) -> (String, Option<usize>, String, EdgeKind, Option<String>) {
        (self.graph.nodes[e.from.node].id.clone(), e.from.row, self.graph.nodes[e.to.node].id.clone(), e.kind, e.label.clone())
    }

    pub fn select_id(&mut self, id: Option<&str>, center: bool, cx: &mut Context<Self>) {
        self.selected_node = id.and_then(|id| self.graph.index_of(id));
        if self.selected_node.is_some() {
            self.selected_edge = None;
        }
        if center {
            if let Some(i) = self.selected_node {
                self.center_on(i);
            }
        }
        cx.notify();
    }

    fn compact(&self) -> bool {
        self.zoom < FULL_LOD
    }

    /// World-space box of node `i` as drawn.
    fn node_box(&self, i: usize) -> (f32, f32, f32, f32) {
        let (w, h) = self.graph.nodes[i].size();
        let p = self.pos[i];
        // compact nodes keep a readable minimum height on screen
        let h = if self.compact() { h.max(22.0 / self.zoom).min(h.max(HEADER_H)) } else { h };
        (p.x, p.y, w, h)
    }

    /// Scroll so node `i` is visible (centred if it was off-screen).
    pub fn center_on(&mut self, i: usize) {
        let b = self.bounds.get();
        let (w, h) = (f32::from(b.size.width), f32::from(b.size.height));
        if w <= 0.0 {
            return;
        }
        let (x, y, nw, nh) = self.node_box(i);
        let sx = x * self.zoom + self.offset.0;
        let sy = y * self.zoom + self.offset.1;
        if sx < 0.0 || sy < 0.0 || sx + nw * self.zoom > w || sy + nh * self.zoom > h {
            self.offset = (w / 2.0 - (x + nw / 2.0) * self.zoom, h / 2.0 - (y + nh / 2.0) * self.zoom);
            self.auto_fit = false;
        }
    }

    /// Section frames in world space: (x0, y0, x1, y1, title, depth).
    fn frames(&self) -> Vec<(f32, f32, f32, f32, String)> {
        let mut out = Vec::new();
        for (ci, c) in self.graph.clusters.iter().enumerate() {
            let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
            let mut any = false;
            for i in 0..self.graph.nodes.len() {
                if self.graph.nodes[i].cluster != Some(ci) || i >= self.pos.len() {
                    continue;
                }
                any = true;
                let (x, y, w, h) = self.node_box(i);
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x + w);
                y1 = y1.max(y + h);
            }
            if any {
                let (pad, title_h) = (wh_graph::CLUSTER_PAD, wh_graph::CLUSTER_TITLE_H);
                out.push((x0 - pad, y0 - title_h, x1 + pad, y1 + pad, c.title.clone()));
            }
        }
        out
    }

    /// Everything that is drawn, in world space: nodes and section frames.
    fn content_bounds(&self) -> Option<(f32, f32, f32, f32)> {
        if self.pos.is_empty() {
            return None;
        }
        let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for i in 0..self.graph.nodes.len().min(self.pos.len()) {
            let (x, y, w, h) = self.node_box(i);
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x + w + 6.0);
            y1 = y1.max(y + h + 6.0);
        }
        for (a, b, c, d, _) in self.frames() {
            x0 = x0.min(a);
            y0 = y0.min(b);
            x1 = x1.max(c);
            y1 = y1.max(d);
        }
        Some((x0, y0, x1, y1))
    }

    /// Fit the whole graph, centred on both axes (G1).
    pub fn fit(&mut self) {
        let b = self.bounds.get();
        let (w, h) = (f32::from(b.size.width), f32::from(b.size.height));
        if w <= 0.0 {
            return;
        }
        // the LOD changes node heights, so settle the zoom in two passes
        for _ in 0..2 {
            let Some((x0, y0, x1, y1)) = self.content_bounds() else { return };
            let m = 24.0;
            let z = ((w - 2.0 * m) / (x1 - x0).max(1.0)).min((h - 2.0 * m) / (y1 - y0).max(1.0)).clamp(MIN_ZOOM, 1.2);
            self.zoom = z;
            self.offset = (w / 2.0 - (x0 + x1) / 2.0 * z, h / 2.0 - (y0 + y1) / 2.0 * z);
        }
        self.fitted = true;
        self.auto_fit = true;
        self.fitted_size = (w, h);
    }

    /// Zoom to the selected node at a readable size (`F`).
    pub fn zoom_to_selection(&mut self) {
        let Some(i) = self.selected_node else { return self.fit() };
        let b = self.bounds.get();
        let (w, h) = (f32::from(b.size.width), f32::from(b.size.height));
        let (x, y, nw, nh) = self.node_box(i);
        let fit = ((w - 80.0) / nw).min((h - 80.0) / nh);
        self.zoom = fit.clamp(FULL_LOD, 1.4);
        let (x, y, nw, nh) = (x, y, nw, self.node_box(i).3.max(nh));
        self.offset = (w / 2.0 - (x + nw / 2.0) * self.zoom, h / 2.0 - (y + nh / 2.0) * self.zoom);
        self.auto_fit = false;
    }

    pub fn zoom_by(&mut self, factor: f32, around: Option<Point<Pixels>>) {
        let b = self.bounds.get();
        let (cx_, cy_) = match around {
            Some(p) => (f32::from(p.x - b.origin.x), f32::from(p.y - b.origin.y)),
            None => (f32::from(b.size.width) / 2.0, f32::from(b.size.height) / 2.0),
        };
        let nz = (self.zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);
        let k = nz / self.zoom;
        self.offset = (cx_ - (cx_ - self.offset.0) * k, cy_ - (cy_ - self.offset.1) * k);
        self.zoom = nz;
        self.auto_fit = false;
    }

    fn to_world(&self, p: Point<Pixels>) -> (f32, f32) {
        let b = self.bounds.get();
        let x = f32::from(p.x - b.origin.x);
        let y = f32::from(p.y - b.origin.y);
        ((x - self.offset.0) / self.zoom, (y - self.offset.1) / self.zoom)
    }

    /// World position of a port (or the header anchor) on side `dir`.
    fn anchor(&self, r: PortRef, dir: PortDir) -> (f32, f32) {
        let (x, y, w, h) = self.node_box(r.node);
        let y = match r.row {
            _ if self.compact() => y + h / 2.0,
            Some(row) => y + HEADER_H + row as f32 * ROW_H + ROW_H / 2.0,
            None => y + HEADER_H / 2.0,
        };
        let x = if dir == PortDir::Out { x + w } else { x };
        (x, y)
    }

    fn screen(&self, (x, y): (f32, f32)) -> Point<Pixels> {
        let b = self.bounds.get();
        point(b.origin.x + px(x * self.zoom + self.offset.0), b.origin.y + px(y * self.zoom + self.offset.1))
    }

    /// The port (or header) under a world position: `(node, row)`.
    fn hit(&self, w: (f32, f32)) -> Option<PortRef> {
        self.hit_except(w, None)
    }

    fn hit_except(&self, (x, y): (f32, f32), skip: Option<usize>) -> Option<PortRef> {
        // ports sit on the border: 8 screen px of slack either side
        let slack = 8.0 / self.zoom;
        for i in (0..self.graph.nodes.len().min(self.pos.len())).rev() {
            if Some(i) == skip {
                continue;
            }
            let (px_, py, w, h) = self.node_box(i);
            if x >= px_ - slack && x <= px_ + w + slack && y >= py && y <= py + h {
                let row = if self.compact() || y < py + HEADER_H { None } else { Some(((y - py - HEADER_H) / ROW_H) as usize) };
                let row = row.filter(|r| *r < self.graph.nodes[i].rows.len());
                return Some(PortRef { node: i, row });
            }
        }
        None
    }

    fn bezier(&self, a: (f32, f32), b: (f32, f32)) -> [(f32, f32); 4] {
        let dx = ((b.0 - a.0).abs() / 2.0).max(40.0);
        [a, (a.0 + dx, a.1), (b.0 - dx, b.1), b]
    }

    fn edge_hit(&self, w: (f32, f32)) -> Option<usize> {
        let mut best = None;
        let tol = 6.0 / self.zoom;
        for (i, e) in self.graph.edges.iter().enumerate() {
            let c = self.bezier(self.anchor(e.from, PortDir::Out), self.anchor(e.to, PortDir::In));
            for k in 0..=24 {
                let t = k as f32 / 24.0;
                let p = cubic(&c, t);
                let d = ((p.0 - w.0).powi(2) + (p.1 - w.1).powi(2)).sqrt();
                if d < tol && best.is_none_or(|(_, bd)| d < bd) {
                    best = Some((i, d));
                }
            }
        }
        best.map(|(i, _)| i)
    }

    fn port_ok(&self, from: PortRef, to: PortRef) -> bool {
        (self.accepts)(&self.graph, from, to)
    }

    fn on_down(&mut self, ev: &MouseDownEvent, cx: &mut Context<Self>) {
        let w = self.to_world(ev.position);
        if let Some(hit) = self.hit(w) {
            let i = hit.node;
            let n = &self.graph.nodes[i];
            let (x, _, nw, _) = self.node_box(i);
            // grabbing an output port (right border) starts a connection
            let near_right = w.0 >= x + nw - 10.0f32.max(10.0 / self.zoom);
            let compact = self.compact();
            let out_row = if compact {
                n.rows.iter().rposition(|r| r.port == Some(PortDir::Out))
            } else {
                hit.row.filter(|r| n.rows[*r].port == Some(PortDir::Out))
            };
            let header_out = matches!(n.kind, GNodeKind::Struct) && (hit.row.is_none() || compact);
            if near_right && (out_row.is_some() || header_out) {
                let row = if header_out { None } else { out_row };
                self.drag = Some(Drag::Connect { from: PortRef { node: i, row }, to: ev.position });
                cx.notify();
                return;
            }
            // clicking a data-model field row selects that field's relationship
            if let Some(r) = hit.row.filter(|r| n.rows[*r].field.is_some()) {
                if let Some(e) = self.graph.edges.iter().position(|e| e.from == PortRef { node: i, row: Some(r) }) {
                    self.selected_edge = Some(e);
                    self.selected_node = None;
                    cx.emit(CanvasEvent::SelectEdge(e));
                    cx.notify();
                    return;
                }
            }
            if ev.click_count >= 2 {
                cx.emit(CanvasEvent::DoubleClick(i));
            }
            self.selected_node = Some(i);
            self.selected_edge = None;
            cx.emit(CanvasEvent::SelectNode(i));
            let p = self.pos[i];
            self.drag = Some(Drag::Node { node: i, grab: (w.0 - p.x, w.1 - p.y), moved: false, over: None });
        } else if let Some(e) = self.edge_hit(w) {
            self.selected_edge = Some(e);
            self.selected_node = None;
            cx.emit(CanvasEvent::SelectEdge(e));
        } else {
            self.drag = Some(Drag::Pan { start: ev.position, origin: self.offset });
            if self.selected_node.is_some() || self.selected_edge.is_some() {
                self.selected_node = None;
                self.selected_edge = None;
                cx.emit(CanvasEvent::ClearSelection);
            }
        }
        cx.notify();
    }

    fn on_move(&mut self, ev: &MouseMoveEvent, cx: &mut Context<Self>) {
        if ev.pressed_button != Some(MouseButton::Left) {
            if self.drag.is_some() {
                self.finish(ev.position, cx);
            }
            let h = self.hit(self.to_world(ev.position)).map(|h| h.node);
            if h != self.hovered {
                self.hovered = h;
                cx.notify();
            }
            return;
        }
        let w = self.to_world(ev.position);
        match &mut self.drag {
            Some(Drag::Pan { start, origin }) => {
                self.offset = (origin.0 + f32::from(ev.position.x - start.x), origin.1 + f32::from(ev.position.y - start.y));
                self.auto_fit = false;
            }
            Some(Drag::Node { node, grab, moved, .. }) => {
                let n = *node;
                self.pos[n] = wh_graph::Point::new(w.0 - grab.0, w.1 - grab.1);
                *moved = true;
                let target = self.hit_except(w, Some(n)).map(|h| h.node).filter(|&t| (self.can_drop)(&self.graph, n, t));
                if let Some(Drag::Node { over, .. }) = &mut self.drag {
                    *over = target;
                }
            }
            Some(Drag::Connect { to, .. }) => *to = ev.position,
            None => return,
        }
        cx.notify();
    }

    fn finish(&mut self, at: Point<Pixels>, cx: &mut Context<Self>) {
        match self.drag.take() {
            Some(Drag::Node { node, moved: true, over: Some(target), .. }) => cx.emit(CanvasEvent::DropOnto { node, target }),
            Some(Drag::Node { node, moved: true, .. }) => cx.emit(CanvasEvent::Moved(node, self.pos[node])),
            Some(Drag::Connect { from, .. }) => {
                if let Some(to) = self.hit(self.to_world(at)) {
                    if to.node != from.node && self.port_ok(from, to) {
                        cx.emit(CanvasEvent::Connect { from, to });
                    }
                }
            }
            _ => {}
        }
        cx.notify();
    }

    /// Screen width node `i` may take in compact LOD: its name, up to the
    /// next node to its right in the same row (labels never cover neighbours).
    fn compact_width(&self, i: usize, title_px: f32) -> f32 {
        let (x, y, w, h) = self.node_box(i);
        let z = self.zoom;
        let n = &self.graph.nodes[i];
        let glyph = if z >= 0.4 { title_px + 5.0 } else { 0.0 };
        let chip = if self.chip_for(i).is_some() || n.error.is_some() { title_px + 4.0 } else { 0.0 };
        let want = n.title.chars().count() as f32 * title_px * 0.6 + glyph + chip + 20.0;
        let mut avail = f32::MAX;
        for j in 0..self.graph.nodes.len().min(self.pos.len()) {
            if j == i {
                continue;
            }
            let (xj, yj, _, hj) = self.node_box(j);
            if xj > x && yj < y + h && yj + hj > y {
                avail = avail.min((xj - x) * z - 6.0);
            }
        }
        want.min(avail).max(w * z)
    }

    fn chip_for(&self, i: usize) -> Option<&Chip> {
        let n = &self.graph.nodes[i];
        self.chips.get(&n.id)
    }

    fn render_node(&self, i: usize, cx: &mut Context<Self>) -> AnyElement {
        let t = tokens(cx);
        let n = &self.graph.nodes[i];
        let z = self.zoom;
        let compact = self.compact();
        let (wx, wy, w, h) = self.node_box(i);
        let (sx, sy, mut sw, sh) = (wx * z + self.offset.0, wy * z + self.offset.1, w * z, h * z);
        let selected = self.selected_node == Some(i);
        let hovered = self.hovered == Some(i);
        let ghost = self.ghosts.contains(&n.id) || n.model_node.as_ref().is_some_and(|m| self.ghosts.contains(m));
        let executed = self.executed.contains(&n.id);
        let connecting = match &self.drag {
            Some(Drag::Connect { from, .. }) => Some(*from),
            _ => None,
        };
        let header_ok = connecting.map(|f| f.node == i || self.port_ok(f, PortRef { node: i, row: None }));
        let drop_target = matches!(&self.drag, Some(Drag::Node { over: Some(tg), .. }) if *tg == i);
        let chip = self.chip_for(i).cloned();
        let failing = n.error.is_some() || chip.as_ref().is_some_and(|c| c.tone == Tone::Danger);
        let kind = t.kind(n.kind);
        let border = if drop_target {
            t.accent
        } else if failing {
            t.danger.opacity(0.8)
        } else if ghost {
            t.warning.opacity(0.8)
        } else if executed {
            t.accent.opacity(0.7)
        } else if hovered {
            t.border_strong
        } else {
            t.border_subtle
        };
        let header_h = if compact { sh } else { HEADER_H * z };
        let title_px = (12.5 * z).max(MIN_TITLE_PX).min(header_h.max(MIN_TITLE_PX + 2.0) * 0.75).max(MIN_TITLE_PX);
        if compact {
            sw = self.compact_width(i, title_px);
        }
        let meta_px = (11.0 * z).max(10.0);
        let meta = match n.kind {
            GNodeKind::Step if n.error.is_none() => n.subtitle.split(" of ").next().unwrap_or("").to_string(),
            GNodeKind::Step => "unresolved".into(),
            GNodeKind::Param => "input".into(),
            _ if n.badges.iter().any(|b| b == "external") => n.subtitle.clone(),
            _ => String::new(),
        };
        let tip = {
            let mut s = format!("{} {}", n.kind.label(), n.title);
            if !n.subtitle.is_empty() {
                s.push_str(&format!("\n{}", n.subtitle));
            }
            if let Some(r) = &n.raises {
                s.push_str(&format!("\nraises {r}"));
            }
            if !n.badges.is_empty() {
                s.push_str(&format!("\n{}", n.badges.join(" · ")));
            }
            if let Some(e) = &n.error {
                s.push_str(&format!("\n✗ {e}"));
            }
            if let Some(c) = &chip {
                s.push_str(&format!("\n{} {}", c.glyph, c.detail));
            }
            if ghost {
                s.push_str("\n⚠ its block does not parse; showing the last valid version");
            }
            s
        };
        let tint = if n.error.is_some() { t.danger } else { kind };
        let header = h_flex()
            .id(("node-head", i))
            .relative()
            .h(px(header_h))
            .flex_none()
            .pl(px(6.0 + 3.0 * z.min(1.0)))
            .pr(px(6.0))
            .gap(px(5.0 * z.clamp(0.6, 1.0)))
            .bg(tint.opacity(0.10))
            .when(!compact, |d| d.border_b_1().border_color(t.border_subtle))
            .child(div().absolute().left_0().top(px(header_h * 0.25)).bottom(px(header_h * 0.25)).w(px(3.)).rounded_r(px(2.)).bg(tint))
            .when(z >= 0.4, |d| d.child(div().flex_none().text_size(px(title_px)).font_weight(FontWeight::BOLD).text_color(kind).child(capsule::kind_glyph(n.kind))))
            .child(
                div()
                    .min_w_0()
                    .flex_shrink(1.0)
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_size(px(title_px))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(t.text_primary)
                    .child(n.title.clone()),
            )
            .when(!meta.is_empty() && !compact, |d| {
                d.child(div().flex_none().whitespace_nowrap().text_size(px(meta_px)).text_color(t.text_tertiary).child(meta.clone()))
            })
            .child(div().flex_1())
            .when(ghost, |d| d.child(div().flex_none().text_size(px(title_px)).text_color(t.warning).child("⚠")))
            .when_some(n.error.as_ref().filter(|_| chip.is_none()), |d, _| {
                d.child(capsule::chip(&Chip { glyph: "✗", tone: Tone::Danger, text: "unresolved".into(), detail: String::new() }, t, px(meta_px), compact))
            })
            .when_some(chip.clone(), |d, c| d.child(capsule::chip(&c, t, px(meta_px), compact || z < 0.9)))
            .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx));
        let mut body = v_flex()
            .size_full()
            .rounded(px(8.0 * z.min(1.0)))
            .border_1()
            .border_color(border)
            .when(ghost, |d| d.border_dashed())
            .bg(t.raised)
            .shadow_sm()
            .overflow_hidden()
            .child(header);
        if !compact {
            let row_px = (12.0 * z).max(9.0).min(ROW_H * z * 0.8);
            for (ri, r) in n.rows.iter().enumerate() {
                let ok = connecting.map(|f| f.node == i || r.port != Some(PortDir::In) || self.port_ok(f, PortRef { node: i, row: Some(ri) }));
                let is_err = n.error.is_some();
                let mut row = h_flex()
                    .h(px(ROW_H * z))
                    .flex_none()
                    .px(px(10.0 * z))
                    .gap(px(6.0 * z))
                    .whitespace_nowrap()
                    .text_size(px(row_px))
                    .when(ok == Some(false), |d| d.opacity(0.3));
                if r.port == Some(PortDir::Out) && r.label != "out" {
                    row = row.child(div().flex_1());
                }
                row = row.child(div().flex_none().text_color(if is_err { t.danger } else { t.text_primary }).child(r.label.clone())).child(
                    div()
                        .when(r.port != Some(PortDir::Out) || r.label == "out", |d| d.flex_1())
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .font_family(cx.theme().mono_font_family.clone())
                        .text_size(px(row_px * 0.95))
                        .text_color(if is_err { t.text_secondary } else { t.text_secondary })
                        .when(r.port == Some(PortDir::Out), |d| d.text_right())
                        .child(r.detail.clone()),
                );
                body = body.child(row);
            }
        }
        let mut wrap = div()
            .absolute()
            .left(px(sx))
            .top(px(sy))
            .w(px(sw))
            .h(px(sh))
            .when(ghost, |d| d.opacity(0.45))
            .when(header_ok == Some(false), |d| d.opacity(0.3))
            .child(body);
        // selection ring: 2 px accent outside the border, with a 1 px gap
        if selected {
            wrap = wrap.child(div().absolute().top(px(-3.)).left(px(-3.)).right(px(-3.)).bottom(px(-3.)).rounded(px(8.0 * z.min(1.0) + 3.0)).border_2().border_color(t.accent));
        }
        // ports on the border, by type family (fill) and structure (shape)
        let port_d = (10.0 * z).clamp(6.0, 10.0);
        if !compact {
            for (ri, r) in n.rows.iter().enumerate() {
                let Some(dir) = r.port else { continue };
                let cy = (HEADER_H + ri as f32 * ROW_H + ROW_H / 2.0) * z;
                let color = if n.error.is_some() { t.danger } else { t.family(r.family) };
                let el = port(r.shape, r.family, color, port_d, t);
                let x = if dir == PortDir::In { -port_d / 2.0 } else { sw - port_d / 2.0 };
                let (pw, _) = port_size(r.shape, port_d);
                let x = x - (pw - port_d) / 2.0;
                wrap = wrap.child(div().absolute().left(px(x)).top(px(cy - port_d / 2.0)).child(el));
            }
            if n.raises.is_some() {
                let r = n.raises.clone().unwrap_or_default();
                wrap = wrap.child(
                    div()
                        .id(("raises", i))
                        .absolute()
                        .left(px(sw / 2.0 - 6.0))
                        .bottom(px(-8.0))
                        .w(px(12.))
                        .h(px(12.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_size(px(10.))
                        .text_color(t.ty_error)
                        .child("▼")
                        .tooltip(move |window, cx| Tooltip::new(format!("raises {r}")).build(window, cx)),
                );
            }
        } else {
            // one in / one out at header height
            let has_in = n.header_in || n.rows.iter().any(|r| r.port == Some(PortDir::In));
            let has_out = n.rows.iter().any(|r| r.port == Some(PortDir::Out)) || n.kind == GNodeKind::Struct;
            let d = 6.0;
            if has_in {
                wrap = wrap.child(div().absolute().left(px(-d / 2.0)).top(px(sh / 2.0 - d / 2.0)).size(px(d)).rounded_full().bg(t.edge_data).border_1().border_color(t.raised));
            }
            if has_out {
                wrap = wrap.child(div().absolute().left(px(sw - d / 2.0)).top(px(sh / 2.0 - d / 2.0)).size(px(d)).rounded_full().bg(t.edge_data).border_1().border_color(t.raised));
            }
        }
        wrap.into_any_element()
    }
}

fn port_size(shape: PortShape, d: f32) -> (f32, f32) {
    match shape {
        PortShape::Collection => (d * 1.5, d * 0.8),
        _ => (d, d),
    }
}

/// One port glyph: ● single, ◐ optional, ▬ collection, ◆ stream, ○ unit.
fn port(shape: PortShape, family: TypeFamily, color: Hsla, d: f32, t: &Tokens) -> AnyElement {
    let _ = family;
    let ring = t.raised;
    match shape {
        PortShape::Single => div().size(px(d)).rounded_full().bg(color).border_2().border_color(ring).into_any_element(),
        PortShape::Hollow => div().size(px(d)).rounded_full().bg(ring).border_2().border_color(color).into_any_element(),
        PortShape::Optional => div()
            .size(px(d))
            .rounded_full()
            .border_2()
            .border_color(color)
            .bg(ring)
            .overflow_hidden()
            .child(div().w(px(d / 2.0)).h_full().bg(color))
            .into_any_element(),
        PortShape::Collection => {
            let (w, h) = port_size(shape, d);
            div().mt(px((d - h) / 2.0)).w(px(w)).h(px(h)).rounded(px(h / 2.0)).bg(color).border_1().border_color(ring).into_any_element()
        }
        PortShape::Stream => div()
            .size(px(d))
            .flex()
            .items_center()
            .justify_center()
            .text_size(px(d * 1.2))
            .line_height(px(d))
            .text_color(color)
            .child("◆")
            .into_any_element(),
    }
}

fn cubic(c: &[(f32, f32); 4], t: f32) -> (f32, f32) {
    let u = 1.0 - t;
    let a = u * u * u;
    let b = 3.0 * u * u * t;
    let d = 3.0 * u * t * t;
    let e = t * t * t;
    (a * c[0].0 + b * c[1].0 + d * c[2].0 + e * c[3].0, a * c[0].1 + b * c[1].1 + d * c[2].1 + e * c[3].1)
}

/// Screen-space polyline pieces of a cubic for a dash pattern (`[]` = solid).
fn dashes(c: &[Point<Pixels>; 4], pattern: &[f32]) -> Vec<Vec<Point<Pixels>>> {
    let w: [(f32, f32); 4] = c.map(|p| (f32::from(p.x), f32::from(p.y)));
    let steps = 48;
    let pts: Vec<(f32, f32)> = (0..=steps).map(|k| cubic(&w, k as f32 / steps as f32)).collect();
    if pattern.is_empty() {
        return vec![pts.iter().map(|&(x, y)| point(px(x), px(y))).collect()];
    }
    let mut out = Vec::new();
    let mut cur: Vec<Point<Pixels>> = Vec::new();
    let (mut k, mut left, mut on) = (0usize, pattern[0], true);
    for win in pts.windows(2) {
        let (a, b) = (win[0], win[1]);
        let len = ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt();
        let mut done = 0.0;
        while done < len {
            let step = (len - done).min(left);
            let p0 = (a.0 + (b.0 - a.0) * done / len, a.1 + (b.1 - a.1) * done / len);
            let p1 = (a.0 + (b.0 - a.0) * (done + step) / len, a.1 + (b.1 - a.1) * (done + step) / len);
            if on {
                if cur.is_empty() {
                    cur.push(point(px(p0.0), px(p0.1)));
                }
                cur.push(point(px(p1.0), px(p1.1)));
            }
            done += step;
            left -= step;
            if left <= 1e-3 {
                if on && cur.len() > 1 {
                    out.push(std::mem::take(&mut cur));
                }
                cur.clear();
                on = !on;
                k = (k + 1) % pattern.len();
                left = pattern[k];
            }
        }
    }
    if cur.len() > 1 {
        out.push(cur);
    }
    out
}

/// The end decoration of an edge, at its target, in screen px.
#[derive(Clone)]
enum End {
    Arrow,
    None,
    /// Crow's foot notation for a relationship's cardinality.
    Crow(Cardinality),
}

struct EdgePaint {
    pts: [Point<Pixels>; 4],
    color: Hsla,
    width: f32,
    dash: &'static [f32],
    end: End,
    label: Option<(Point<Pixels>, SharedString)>,
}

impl Render for GraphCanvas {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = tokens(cx);
        let theme = cx.theme().clone();
        let b = self.bounds.get().size;
        let now = (f32::from(b.width), f32::from(b.height));
        if now.0 > 0.0 && !self.pos.is_empty() && (!self.fitted || (self.auto_fit && now != self.fitted_size)) {
            self.fit();
        }
        // edges and frames are painted on a canvas under the node divs
        let mut edges = Vec::new();
        let z = self.zoom;
        let lit = self.selected_node.or(self.hovered);
        for (ei, e) in self.graph.edges.iter().enumerate() {
            if e.from.node >= self.pos.len() || e.to.node >= self.pos.len() {
                continue;
            }
            let c = self.bezier(self.anchor(e.from, PortDir::Out), self.anchor(e.to, PortDir::In));
            let sel = self.selected_edge == Some(ei) || lit.is_some_and(|s| s == e.from.node || s == e.to.node);
            let (mut color, mut width, mut dash) = t.edge(e.kind);
            if e.kind == EdgeKind::Data {
                // data wires take the source port's type colour
                let fam = e.from.row.and_then(|r| self.graph.nodes[e.from.node].rows.get(r)).map(|r| r.family).unwrap_or_default();
                if fam != TypeFamily::Other {
                    color = t.family(fam).opacity(0.85);
                }
                if self.graph.nodes[e.to.node].error.is_some() || self.graph.nodes[e.from.node].error.is_some() {
                    color = t.danger;
                    dash = &[5.0, 4.0];
                }
            }
            let executed = self.executed.contains(&self.graph.nodes[e.from.node].id) && self.executed.contains(&self.graph.nodes[e.to.node].id);
            if executed && matches!(e.kind, EdgeKind::Data | EdgeKind::Result) {
                color = t.accent;
                width = 2.0;
                dash = &[2.0, 6.0];
            }
            if sel {
                color = if self.selected_edge == Some(ei) { t.accent } else { color.opacity(1.0) };
                width += 1.0;
            }
            let end = match (&e.cardinality, e.kind) {
                (Some(card), _) => End::Crow(card.clone()),
                (None, EdgeKind::Calls | EdgeKind::Uses | EdgeKind::Provides | EdgeKind::Produces | EdgeKind::Consumes) => End::None,
                _ => End::Arrow,
            };
            let live = self.edge_values.get(&self.graph.nodes[e.from.node].id).filter(|_| matches!(e.kind, EdgeKind::Data | EdgeKind::Result) && z > 0.45);
            if let Some((_, err)) = live {
                color = if *err { t.danger } else { color };
            }
            let label = match (&e.cardinality, &e.label) {
                _ if live.is_some() => live.map(|(v, _)| if v.chars().count() > 28 { format!("{}…", v.chars().take(27).collect::<String>()) } else { v.clone() }),
                (Some(card), _) if z > 0.45 => Some(card.to_string()),
                (None, Some(l)) if z > 0.6 && !matches!(e.kind, EdgeKind::Has | EdgeKind::References) => Some(l.clone()),
                _ => None,
            };
            let mid = cubic(&c, 0.5);
            edges.push(EdgePaint {
                pts: c.map(|p| self.screen(p)),
                color,
                width,
                dash,
                end,
                label: label.map(|l| (self.screen(mid), SharedString::from(l))),
            });
        }
        let frames: Vec<(Point<Pixels>, Point<Pixels>, SharedString)> =
            self.frames().into_iter().map(|(a, b, c, d, title)| (self.screen((a, b)), self.screen((c, d)), SharedString::from(title))).collect();
        let pending = match &self.drag {
            Some(Drag::Connect { from, to }) => {
                let target = self.hit(self.to_world(*to)).filter(|h| h.node != from.node);
                let bad = target.is_some_and(|h| !self.port_ok(*from, h));
                Some((self.screen(self.anchor(*from, PortDir::Out)), *to, if bad { t.danger } else { t.accent }))
            }
            _ => None,
        };
        let bounds = self.bounds.clone();
        let font = theme.font_family.clone();
        let painter = canvas(
            move |b, window, _| {
                if bounds.get().size != b.size {
                    window.refresh();
                }
                bounds.set(b)
            },
            move |_b, _, window, cx| {
                for (a, b, title) in &frames {
                    window.paint_quad(
                        fill(Bounds::from_corners(*a, *b), t.frame).corner_radii(px(10.0 * z.min(1.0))).border_widths(px(1.)).border_color(t.border_strong),
                    );
                    // titles stay readable: inside the reserved band when it fits, above the frame otherwise
                    let band = wh_graph::CLUSTER_TITLE_H * z;
                    let size = (12.5 * z).clamp(11.0, 14.0);
                    if z >= 0.12 {
                        let at = if band >= size * 1.25 { point(a.x + px(10.0 * z.min(1.0)), a.y + px((band - size * 1.3) / 2.0)) } else { point(a.x + px(2.), a.y - px(size * 1.35)) };
                        paint_text(window, cx, title, at, px(size), t.text_secondary, &font, FontWeight::SEMIBOLD);
                    }
                }
                for e in &edges {
                    let mut pb = PathBuilder::stroke(px(e.width));
                    for piece in dashes(&e.pts, e.dash) {
                        pb.move_to(piece[0]);
                        for p in &piece[1..] {
                            pb.line_to(*p);
                        }
                    }
                    if let Ok(path) = pb.build() {
                        window.paint_path(path, e.color);
                    }
                    let tip = e.pts[3];
                    match &e.end {
                        End::Arrow => {
                            let mut ah = PathBuilder::fill();
                            ah.move_to(tip);
                            ah.line_to(point(tip.x - px(7.0), tip.y - px(4.0)));
                            ah.line_to(point(tip.x - px(7.0), tip.y + px(4.0)));
                            ah.close();
                            if let Ok(path) = ah.build() {
                                window.paint_path(path, e.color);
                            }
                        }
                        End::Crow(card) => crow_foot(window, cx, tip, card, e.color, t.canvas, &font),
                        End::None => {}
                    }
                    if let Some((p, l)) = &e.label {
                        let w = text_width(window, l, px(11.), &font);
                        let at = point(p.x - w / 2.0, p.y - px(16.0));
                        window.paint_quad(fill(Bounds::new(point(at.x - px(4.), at.y - px(1.)), size(w + px(8.), px(16.))), t.canvas.opacity(0.9)).corner_radii(px(4.)));
                        paint_text(window, cx, l, at, px(11.), e.color, &font, FontWeight::MEDIUM);
                    }
                }
                if let Some((a, b, color)) = pending {
                    let dx = (f32::from(b.x - a.x).abs() / 2.0).max(40.0);
                    let mut pb = PathBuilder::stroke(px(2.0));
                    pb.move_to(a);
                    pb.cubic_bezier_to(b, point(a.x + px(dx), a.y), point(b.x - px(dx), b.y));
                    if let Ok(path) = pb.build() {
                        window.paint_path(path, color);
                    }
                }
            },
        )
        .absolute()
        .size_full();

        let nodes: Vec<_> = (0..self.graph.nodes.len().min(self.pos.len())).map(|i| self.render_node(i, cx)).collect();
        let zoom_label = format!("{:.0}%", self.zoom * 100.0);
        div()
            .id("graph-canvas")
            .key_context("GraphCanvas")
            .track_focus(&self.focus)
            .relative()
            .size_full()
            .overflow_hidden()
            .bg(t.canvas)
            .child(painter)
            .children(nodes)
            .child(div().absolute().right(px(8.)).bottom(px(6.)).text_size(px(11.)).text_color(t.text_tertiary).child(zoom_label))
            .on_action(cx.listener(|this, _: &FitView, _, cx| {
                this.fit();
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &ZoomToSelection, _, cx| {
                this.zoom_to_selection();
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &ZoomIn, _, cx| {
                this.zoom_by(1.2, None);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &ZoomOut, _, cx| {
                this.zoom_by(1.0 / 1.2, None);
                cx.notify();
            }))
            .on_mouse_down(MouseButton::Left, cx.listener(|this, ev: &MouseDownEvent, window, cx| {
                this.focus.focus(window, cx);
                this.on_down(ev, cx)
            }))
            .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, _, cx| this.on_move(ev, cx)))
            .on_mouse_up(MouseButton::Left, cx.listener(|this, ev: &MouseUpEvent, _, cx| this.finish(ev.position, cx)))
            .on_hover(cx.listener(|this, over: &bool, _, cx| {
                if !*over && this.hovered.take().is_some() {
                    cx.notify();
                }
            }))
            .on_scroll_wheel(cx.listener(|this, ev: &ScrollWheelEvent, _, cx| {
                let d = ev.delta.pixel_delta(px(16.0));
                if ev.modifiers.platform || ev.modifiers.control {
                    this.zoom_by((1.0 + f32::from(d.y) / 300.0).clamp(0.5, 1.5), Some(ev.position));
                } else {
                    this.offset.0 += f32::from(d.x);
                    this.offset.1 += f32::from(d.y);
                    this.auto_fit = false;
                }
                cx.notify();
            }))
            .on_pinch(cx.listener(|this, ev: &PinchEvent, _, cx| {
                this.zoom_by(1.0 + ev.delta, Some(ev.position));
                cx.notify();
            }))
    }
}

/// Crow's-foot end glyph at `tip` (the edge arrives travelling +x): │ one,
/// ○│ zero or one, ○< zero or many, │< one or many, a count for arrays and the key for dicts.
fn crow_foot(window: &mut Window, cx: &mut App, tip: Point<Pixels>, card: &Cardinality, color: Hsla, bg: Hsla, font: &SharedString) {
    let line = |window: &mut Window, a: Point<Pixels>, b: Point<Pixels>| {
        let mut pb = PathBuilder::stroke(px(1.5));
        pb.move_to(a);
        pb.line_to(b);
        if let Ok(p) = pb.build() {
            window.paint_path(p, color);
        }
    };
    let bar = |window: &mut Window, x: Pixels| line(window, point(x, tip.y - px(6.)), point(x, tip.y + px(6.)));
    let foot = |window: &mut Window| {
        let root = point(tip.x - px(10.), tip.y);
        line(window, root, point(tip.x, tip.y - px(6.)));
        line(window, root, point(tip.x, tip.y + px(6.)));
    };
    let ring = |window: &mut Window, cxp: Pixels| {
        window.paint_quad(fill(Bounds::new(point(cxp - px(4.), tip.y - px(4.)), size(px(8.), px(8.))), bg).corner_radii(px(4.)).border_widths(px(1.5)).border_color(color));
    };
    match card {
        Cardinality::One => {
            bar(window, tip.x - px(5.));
            bar(window, tip.x - px(9.));
        }
        Cardinality::ZeroOrOne => {
            bar(window, tip.x - px(5.));
            ring(window, tip.x - px(14.));
        }
        Cardinality::Many => {
            foot(window);
            ring(window, tip.x - px(15.));
        }
        Cardinality::OneOrMore => {
            foot(window);
            bar(window, tip.x - px(13.));
        }
        Cardinality::Exactly(n) => {
            bar(window, tip.x - px(5.));
            paint_text(window, cx, &SharedString::from(n.to_string()), point(tip.x - px(18.), tip.y - px(16.)), px(10.), color, font, FontWeight::MEDIUM);
        }
        Cardinality::Keyed(_) => {
            foot(window);
            paint_text(window, cx, &SharedString::from("⚿"), point(tip.x - px(22.), tip.y - px(8.)), px(11.), color, font, FontWeight::NORMAL);
        }
    }
}

fn text_width(window: &mut Window, text: &SharedString, size: Pixels, font: &SharedString) -> Pixels {
    let run = TextRun { len: text.len(), font: gpui_kit::font(font.clone()), color: Hsla::default(), background_color: None, underline: None, strikethrough: None };
    window.text_system().shape_line(text.clone(), size, &[run], None).width
}

#[allow(clippy::too_many_arguments)]
fn paint_text(window: &mut Window, cx: &mut App, text: &SharedString, at: Point<Pixels>, size: Pixels, color: Hsla, font: &SharedString, weight: FontWeight) {
    let mut f = gpui_kit::font(font.clone());
    f.weight = weight;
    let run = TextRun { len: text.len(), font: f, color, background_color: None, underline: None, strikethrough: None };
    let line = window.text_system().shape_line(text.clone(), size, &[run], None);
    let _ = line.paint(at, size * 1.3, TextAlign::Left, None, window, cx);
}
