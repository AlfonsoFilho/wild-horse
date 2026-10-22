//! The Document panel: a block editor over one `.wh.md` buffer per tab.
//! Prose blocks render as styled Markdown and turn into a Markdown source
//! editor when clicked; `wh` blocks are gpui-kit code editors highlighted
//! from `wh_syntax::lexer` tokens. Typing emits `Origin::Text` transactions;
//! regions written by other origins flash.

use std::ops::Range;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui_kit::component::input::{
    Editor, EditorState, FoldRange, HighlightStyleResolver, InputEdit, InputEvent, InputHighlighter, RangeDecoration,
    RangeDecorationCollection, RangeDecorationStyle, Rope, TabSize, Textarea, TextareaState,
};
use gpui_kit::component::highlighter::{Diagnostic, DiagnosticSeverity};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme, WindowExt, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use wh_doc::Origin;
use wh_syntax::Severity;

use crate::capsule;
use crate::highlight::{Class, highlight};
use crate::state::{AppEvent, AppState, SelectionSource};
use crate::tokens::tokens;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Prose,
    Code,
}

struct Seg {
    kind: Kind,
    /// Prose: the text. Code: the content between the fences.
    range: Range<usize>,
    /// Code: the whole fenced block.
    fence: Range<usize>,
    text: String,
    editor: Option<Entity<EditorState>>,
    flash: Option<RangeDecorationCollection>,
    mark: Option<RangeDecorationCollection>,
    prose_edit: Option<Entity<TextareaState>>,
    flash_until: Option<Instant>,
    _subs: Vec<Subscription>,
}

pub struct DocumentPanel {
    state: Entity<AppState>,
    focus: FocusHandle,
    /// The file shown (the active tab), `None` when every tab is closed.
    shown: Option<String>,
    segs: Vec<Seg>,
    scroll: ScrollHandle,
    /// A selection to bring to ⅓ of the viewport once its block is laid out: (segment, line).
    pending_reveal: Option<(usize, usize)>,
    /// An edit from another view that landed off-screen: (segment, label, below the viewport).
    jump: Option<(usize, String, bool)>,
    _subs: Vec<Subscription>,
}

/// Toast id, so a new structural edit replaces the previous toast.
struct EditToast;

crate::dock_panel!(DocumentPanel, "wh.document", "Document");

/// `(kind, range, fence)` for each block of a document.
fn segment(text: &str) -> Vec<(Kind, Range<usize>, Range<usize>)> {
    let doc = wh_syntax::parse_document(text).doc;
    let mut out = Vec::new();
    let mut cursor = 0;
    let prose = |a: usize, b: usize, out: &mut Vec<_>| {
        if text[a..b].trim().is_empty() {
            return;
        }
        let lead = text[a..b].len() - text[a..b].trim_start().len();
        let tail = text[a..b].len() - text[a..b].trim_end().len();
        out.push((Kind::Prose, a + lead..b - tail, a + lead..b - tail));
    };
    for b in &doc.blocks {
        prose(cursor, b.span.start as usize, &mut out);
        out.push((Kind::Code, b.content_span.range(), b.span.range()));
        cursor = (b.span.end as usize).min(text.len());
    }
    prose(cursor, text.len(), &mut out);
    out
}

struct WhHighlighter {
    runs: Vec<(Range<usize>, Class)>,
}

/// The theme's `highlight.syntax` key for a token class (literals share their port colours, §15.3).
fn class_key(c: Class) -> &'static str {
    match c {
        Class::Keyword => "keyword",
        Class::Type => "type",
        Class::Function => "function",
        Class::String => "string",
        Class::Number => "number",
        Class::Constant => "constant",
        Class::Comment => "comment",
        Class::Operator => "operator",
        Class::Decorator => "attribute",
    }
}

impl InputHighlighter for WhHighlighter {
    fn language(&self) -> SharedString {
        "wh".into()
    }

    fn update(&mut self, _edit: Option<InputEdit>, text: &Rope, _folding: bool, _window: &mut Window, _cx: &mut Context<EditorState>) {
        self.runs = highlight(&text.to_string());
    }

    fn styles(&self, range: &Range<usize>, resolver: &dyn HighlightStyleResolver) -> Vec<(Range<usize>, HighlightStyle)> {
        let mut out = Vec::new();
        let mut at = range.start;
        for (r, c) in &self.runs {
            if r.end <= range.start || r.start >= range.end {
                continue;
            }
            let s = r.start.max(range.start);
            let e = r.end.min(range.end);
            if s < at {
                continue;
            }
            if s > at {
                out.push((at..s, HighlightStyle::default()));
            }
            out.push((s..e, resolver.style(class_key(*c)).unwrap_or_default()));
            at = e;
        }
        if at < range.end {
            out.push((at..range.end, HighlightStyle::default()));
        }
        out
    }

    fn fold_ranges(&self, _text: &Rope) -> Vec<FoldRange> {
        vec![]
    }
}

/// Where the first text line of a code editor starts (its top padding).
const GUTTER_TOP: f32 = 6.0;

/// Offset → LSP-style position (line, char column) within `text`.
fn position(text: &str, offset: usize) -> gpui_kit::component::input::Position {
    let offset = offset.min(text.len());
    let (l, c) = wh_syntax::span::line_col(text, offset as u32);
    gpui_kit::component::input::Position::new(l as u32, c as u32)
}

impl DocumentPanel {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let sub = cx.subscribe_in(&state, window, |this, _, ev: &AppEvent, window, cx| this.on_app_event(ev, window, cx));
        let active = {
            let p = &state.read(cx).project;
            // start on the module with the most definitions
            (0..p.files.len()).max_by_key(|&i| p.model.nodes.values().filter(|n| n.file == i).count()).unwrap_or(0)
        };
        let mut this = DocumentPanel {
            state,
            focus: cx.focus_handle(),
            shown: None,
            segs: vec![],
            scroll: ScrollHandle::new(),
            pending_reveal: None,
            jump: None,
            _subs: vec![sub],
        };
        this.state.update(cx, |s, _| s.active_file = active);
        this.rebuild(window, cx);
        this
    }

    fn path(&self, _cx: &App) -> String {
        self.shown.clone().unwrap_or_default()
    }

    fn rebuild(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.shown = self.state.read(cx).active_path();
        let path = self.path(cx);
        let text = self.state.read(cx).project.text(&path).to_string();
        let specs = if self.shown.is_some() { segment(&text) } else { vec![] };
        self.segs = specs.into_iter().enumerate().map(|(i, (k, r, f))| self.make_seg(i, k, r, f, &text, window, cx)).collect();
        self.refresh_diagnostics(cx);
        cx.notify();
    }

    fn make_seg(&mut self, i: usize, kind: Kind, range: Range<usize>, fence: Range<usize>, text: &str, window: &mut Window, cx: &mut Context<Self>) -> Seg {
        let content = text[range.clone()].to_string();
        let mut seg = Seg { kind, range, fence, text: content.clone(), editor: None, flash: None, mark: None, prose_edit: None, flash_until: None, _subs: vec![] };
        if kind == Kind::Code {
            let editor = cx.new(|cx| {
                let mut s = EditorState::new(window, cx)
                    .language("wh")
                    .tab_size(TabSize { tab_size: 4, hard_tabs: false })
                    .folding(false)
                    .default_value(content.trim_end_matches('\n').to_string());
                s.set_highlighter_factory(Rc::new(|_| Some(Box::new(WhHighlighter { runs: vec![] }) as Box<dyn InputHighlighter>)), cx);
                s
            });
            // the editor holds the block without its final newline
            seg.text = content.trim_end_matches('\n').to_string();
            seg.flash = Some(editor.update(cx, |s, cx| s.create_range_decorations_collection(vec![], cx)));
            seg.mark = Some(editor.update(cx, |s, cx| s.create_range_decorations_collection(vec![], cx)));
            seg._subs.push(cx.subscribe_in(&editor, window, move |this, ed, ev: &InputEvent, window, cx| {
                if let InputEvent::Change = ev {
                    let v = ed.read(cx).value().to_string();
                    this.on_seg_typed(i, v, window, cx);
                }
            }));
            seg.editor = Some(editor);
        }
        seg
    }

    /// The user typed in segment `i`; turn the difference into one text edit.
    fn on_seg_typed(&mut self, i: usize, new: String, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(seg) = self.segs.get_mut(i) else { return };
        if seg.text == new {
            return;
        }
        let old = std::mem::replace(&mut seg.text, new.clone());
        let pre = old.char_indices().zip(new.chars()).take_while(|((_, a), b)| a == b).map(|((_, a), _)| a.len_utf8()).sum::<usize>();
        let suf = old[pre..].chars().rev().zip(new[pre..].chars().rev()).take_while(|(a, b)| a == b).map(|(a, _)| a.len_utf8()).sum::<usize>();
        let start = seg.range.start + pre;
        let end = seg.range.start + old.len() - suf;
        let ins = new[pre..new.len() - suf].to_string();
        let path = self.path(cx);
        self.state.update(cx, |s, cx| s.edit_text(&path, start..end, &ins, cx));
    }

    fn on_app_event(&mut self, ev: &AppEvent, window: &mut Window, cx: &mut Context<Self>) {
        match ev {
            AppEvent::Changed(change) => {
                if self.state.read(cx).active_path() != self.shown {
                    return self.rebuild(window, cx);
                }
                self.sync(window, cx);
                if change.origin != Origin::Text {
                    let path = self.path(cx);
                    if let Some(ranges) = change.ranges.get(&path) {
                        self.flash(ranges.clone(), change.origin, cx);
                        self.note_offscreen(ranges, &change.label);
                    }
                    if matches!(change.origin, Origin::Graph | Origin::Inspector) {
                        self.toast(&change.label, window, cx);
                    }
                }
            }
            AppEvent::Selected { id: Some(id), reveal: true, from } if *from != SelectionSource::Document => {
                self.reveal(id.clone(), window, cx);
            }
            AppEvent::ActiveFile | AppEvent::Files => {
                if self.state.read(cx).active_path() != self.shown {
                    self.rebuild(window, cx);
                } else {
                    cx.notify();
                }
            }
            AppEvent::Results | AppEvent::LiveValues => cx.notify(),
            AppEvent::RevealAt { path, offset } => self.reveal_offset(path.clone(), *offset, window, cx),
            _ => {}
        }
    }

    /// Bring segments in line with the buffer, keeping editors when the
    /// block structure did not change.
    fn sync(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let path = self.path(cx);
        let text = self.state.read(cx).project.text(&path).to_string();
        let specs = segment(&text);
        let same = specs.len() == self.segs.len() && specs.iter().zip(&self.segs).all(|(s, g)| s.0 == g.kind);
        if !same {
            self.rebuild(window, cx);
            return;
        }
        for ((_, range, fence), seg) in specs.into_iter().zip(self.segs.iter_mut()) {
            let content = text[range.clone()].to_string();
            seg.range = range;
            seg.fence = fence;
            match seg.kind {
                Kind::Code => {
                    let content = content.trim_end_matches('\n').to_string();
                    if seg.text != content {
                        seg.text = content.clone();
                        if let Some(ed) = &seg.editor {
                            ed.update(cx, |s, cx| s.set_value(content, window, cx));
                        }
                    }
                }
                Kind::Prose => {
                    if seg.text != content {
                        seg.text = content.clone();
                        if let Some(ed) = &seg.prose_edit {
                            if ed.read(cx).value().as_ref() != content {
                                ed.update(cx, |s, cx| s.set_value(content, window, cx));
                            }
                        }
                    }
                }
            }
        }
        self.refresh_diagnostics(cx);
        cx.notify();
    }

    fn refresh_diagnostics(&mut self, cx: &mut Context<Self>) {
        let st = self.state.read(cx);
        let Some(fi) = st.project.file_index(&self.path(cx)) else { return };
        let diags: Vec<wh_model::FileDiagnostic> = st.project.model.diagnostics.iter().filter(|d| d.file == fi).cloned().collect();
        for seg in &self.segs {
            let Some(ed) = &seg.editor else { continue };
            let base = seg.range.start;
            let local = seg.text.clone();
            let mine: Vec<Diagnostic> = diags
                .iter()
                .filter(|d| d.diag.severity != Severity::Lint)
                .filter(|d| (d.diag.span.start as usize) >= base && (d.diag.span.start as usize) <= base + local.len())
                .map(|d| {
                    let s = d.diag.span.start as usize - base;
                    let e = (d.diag.span.end as usize).saturating_sub(base).max(s + 1).min(local.len().max(s));
                    let sev = match d.diag.severity {
                        Severity::Error => DiagnosticSeverity::Error,
                        Severity::Warning => DiagnosticSeverity::Warning,
                        Severity::Lint => DiagnosticSeverity::Hint, // filtered above: lints are gutter dots
                    };
                    Diagnostic::new(position(&local, s)..position(&local, e), d.diag.message.clone()).with_severity(sev)
                })
                .collect();
            ed.update(cx, |s, cx| {
                if let Some(set) = s.diagnostics_mut() {
                    set.clear();
                    set.extend(mine);
                }
                cx.notify();
            });
        }
    }

    /// "Inserted step check_age · ⌘Z to undo" (S3); clicking it undoes.
    fn toast(&self, label: &str, window: &mut Window, cx: &mut Context<Self>) {
        let mut text = label.to_string();
        if let Some(c) = text.get(..1) {
            text = c.to_uppercase() + &text[1..];
        }
        let state = self.state.clone();
        window.push_notification(
            Notification::new().id::<EditToast>().message(format!("{text}  ·  ⌘Z or click to undo")).on_click(move |_, _, cx| {
                state.update(cx, |s, cx| s.undo(cx));
            }),
            cx,
        );
    }

    /// Remember an edit that landed outside the viewport, for the jump pill (D6).
    fn note_offscreen(&mut self, ranges: &[Range<usize>], label: &str) {
        let Some(r) = ranges.first() else { return };
        let Some(i) = self.segs.iter().position(|s| r.start <= s.fence.end && r.end >= s.fence.start) else { return };
        let (Some(item), view) = (self.scroll.bounds_for_item(i), self.scroll.bounds()) else { return };
        let dy = self.scroll.offset().y;
        let (top, bottom) = (item.top() + dy, item.bottom() + dy);
        if bottom < view.top() + px(8.) || top > view.bottom() - px(8.) {
            self.jump = Some((i, label.to_string(), top > view.bottom() - px(8.)));
        } else {
            self.jump = None;
        }
    }

    /// Scroll so that `line` of segment `i` sits at ⅓ of the viewport (D5).
    fn scroll_to_line(&mut self, i: usize, line: usize, line_h: f32) -> bool {
        let (Some(item), view) = (self.scroll.bounds_for_item(i), self.scroll.bounds()) else { return false };
        if view.size.height <= px(0.) {
            return false;
        }
        let header = if self.segs.get(i).is_some_and(|s| s.kind == Kind::Code) { BLOCK_HEADER_H + GUTTER_TOP } else { 0.0 };
        let y = item.top() - view.top() + px(header + line as f32 * line_h);
        let max = self.scroll.max_offset().y;
        let target = (view.size.height / 3.0 - y).min(px(0.)).max(-max);
        self.scroll.set_offset(point(self.scroll.offset().x, target));
        true
    }

    fn flash(&mut self, ranges: Vec<Range<usize>>, origin: Origin, cx: &mut Context<Self>) {
        let t = tokens(cx);
        let color = if origin == Origin::Agent { t.agent } else { t.accent }.opacity(0.22);
        let until = Instant::now() + Duration::from_millis(1600);
        for seg in &mut self.segs {
            let hits: Vec<Range<usize>> = ranges
                .iter()
                .filter(|r| r.start <= seg.fence.end && r.end >= seg.fence.start)
                .map(|r| r.start.max(seg.range.start).saturating_sub(seg.range.start)..r.end.min(seg.range.start + seg.text.len()).saturating_sub(seg.range.start))
                .collect();
            if hits.is_empty() {
                continue;
            }
            match (&seg.flash, seg.kind) {
                (Some(f), Kind::Code) => {
                    let decos = hits
                        .into_iter()
                        .map(|r| if r.is_empty() { r.start.saturating_sub(1)..r.start + 1 } else { r })
                        .map(|r| RangeDecoration::new(r).with_style(RangeDecorationStyle::Fill).with_color(color))
                        .collect();
                    f.set(decos, cx);
                }
                _ => {}
            }
            seg.flash_until = Some(until);
        }
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(1700)).await;
            let _ = this.update(cx, |this, cx| {
                for seg in &mut this.segs {
                    if seg.flash_until.is_some_and(|t| t <= Instant::now()) {
                        seg.flash_until = None;
                        if let Some(f) = &seg.flash {
                            f.clear(cx);
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn reveal(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some((path, span)) = self.state.read(cx).project.reveal_span(&id) else { return };
        let files = self.state.read(cx).project.files.clone();
        if let Some(fi) = files.iter().position(|f| *f == path) {
            if self.shown.as_deref() != Some(path.as_str()) {
                self.state.update(cx, |s, cx| s.set_active_file(fi, cx));
                self.rebuild(window, cx);
            }
        }
        let mark_color = tokens(cx).accent.opacity(0.16);
        for seg in &self.segs {
            if let Some(m) = &seg.mark {
                m.clear(cx);
            }
        }
        let start = span.start as usize;
        if let Some(i) = self.segs.iter().position(|s| s.fence.start <= start && start <= s.fence.end.max(s.range.end)) {
            let seg = &self.segs[i];
            let line = wh_syntax::span::line_col(&seg.text, start.saturating_sub(seg.range.start).min(seg.text.len()) as u32).0;
            let line_h = f32::from(cx.theme().mono_font_size) * 1.5;
            if !self.scroll_to_line(i, line, line_h) {
                // not laid out yet (e.g. a tab switch): finish after the next layout
                self.scroll.scroll_to_item(i);
                self.pending_reveal = Some((i, line));
            }
            self.jump = None;
            let seg = &self.segs[i];
            if let (Some(ed), Some(m)) = (&seg.editor, &seg.mark) {
                let s = start.saturating_sub(seg.range.start).min(seg.text.len());
                let e = (span.end as usize).saturating_sub(seg.range.start).min(seg.text.len()).max(s);
                m.set(vec![RangeDecoration::new(s..e).with_style(RangeDecorationStyle::Fill).with_color(mark_color)], cx);
                ed.update(cx, |st, cx| st.set_selected_range(s..s, cx));
            }
        }
        cx.notify();
    }

    /// The cursor moved inside a code block: select the definition under it.
    fn cursor_moved(&mut self, i: usize, cx: &mut Context<Self>) {
        let Some(seg) = self.segs.get(i) else { return };
        let Some(ed) = &seg.editor else { return };
        let off = seg.range.start + ed.read(cx).cursor();
        let path = self.path(cx);
        let sel = self.state.read(cx).project.selection_at(&path, off);
        if sel.is_some() && sel != self.state.read(cx).project.selection {
            self.state.update(cx, |s, cx| s.select(sel, false, SelectionSource::Document, cx));
        }
    }

    /// The tab's close button: dirty files go through the same prompt as ⌘W.
    fn close_tab(&mut self, path: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.state.read(cx).project.docs.is_dirty(&path) {
            if let Some(i) = self.state.read(cx).project.files.iter().position(|f| *f == path) {
                self.state.update(cx, |s, cx| s.set_active_file(i, cx));
            }
            window.dispatch_action(Box::new(crate::CloseFile), cx);
        } else {
            self.state.update(cx, |s, cx| s.close_tab(&path, cx));
        }
    }

    fn edit_prose(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(seg) = self.segs.get_mut(i) else { return };
        if seg.prose_edit.is_some() {
            return;
        }
        let text = seg.text.clone();
        let rows = text.lines().count() + 1;
        let ed = cx.new(|cx| TextareaState::new(window, cx).rows(rows).soft_wrap(true).default_value(text));
        let sub = cx.subscribe_in(&ed, window, move |this, ed, ev: &InputEvent, window, cx| match ev {
            InputEvent::Change => {
                let v = ed.read(cx).value().to_string();
                this.on_seg_typed(i, v, window, cx);
            }
            InputEvent::Blur => {
                if let Some(s) = this.segs.get_mut(i) {
                    s.prose_edit = None;
                    cx.notify();
                }
            }
            _ => {}
        });
        ed.update(cx, |s, cx| s.focus(window, cx));
        seg.prose_edit = Some(ed);
        seg._subs.push(sub);
        cx.notify();
    }


    /// Show `offset` of `path` at ⅓ height with the caret on it (Problems rows, F8).
    fn reveal_offset(&mut self, path: String, offset: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.shown.as_deref() != Some(path.as_str()) {
            self.rebuild(window, cx);
        }
        let Some(i) = self.segs.iter().position(|s| s.fence.start <= offset && offset <= s.fence.end.max(s.range.end)) else { return };
        let seg = &self.segs[i];
        let line = wh_syntax::span::line_col(&seg.text, offset.saturating_sub(seg.range.start).min(seg.text.len()) as u32).0;
        let line_h = f32::from(cx.theme().mono_font_size) * 1.5;
        if !self.scroll_to_line(i, line, line_h) {
            self.scroll.scroll_to_item(i);
            self.pending_reveal = Some((i, line));
        }
        self.place_caret(i, offset, window, cx);
        cx.notify();
    }

    /// Put the caret at `offset` (a document byte offset) in segment `i`'s editor.
    fn place_caret(&mut self, i: usize, offset: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(seg) = self.segs.get(i) else { return };
        let Some(ed) = seg.editor.clone() else { return };
        let at = offset.saturating_sub(seg.range.start).min(seg.text.len());
        ed.update(cx, |s, cx| {
            s.focus(window, cx);
            s.set_selected_range(at..at, cx);
        });
    }

    /// A one-click author request scoped to one definition (D3).
    fn draft_prose(&mut self, id: String, name: String, cx: &mut Context<Self>) {
        self.state.update(cx, |s, cx| {
            s.select(Some(id), false, SelectionSource::Document, cx);
            s.ask_author(format!("Draft the prose paragraph that explains `{name}`: what it is for and why, in the document's voice."), cx);
        });
    }

    fn select_ref(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        self.state.update(cx, |s, cx| s.select(Some(id.clone()), true, SelectionSource::Document, cx));
        self.reveal(id, window, cx);
    }

    /// The block header: one capsule per definition in the block (§3, D4).
    fn block_header(&self, i: usize, defs: &[BlockDef], cx: &mut Context<Self>) -> Div {
        let t = tokens(cx);
        let mut row = h_flex().h(px(BLOCK_HEADER_H)).flex_none().px_2().gap_3().bg(t.raised).border_b_1().border_color(t.border_subtle).text_size(px(12.5));
        if defs.is_empty() {
            return row.child(div().text_color(t.text_tertiary).child("wh"));
        }
        for (k, d) in defs.iter().enumerate() {
            let color = t.kind(d.kind);
            let id = d.id.clone();
            row = row.child(
                h_flex()
                    .id(("capsule", i * 1000 + k))
                    .gap_1p5()
                    .px_1p5()
                    .rounded(px(4.))
                    .cursor_pointer()
                    .hover(|s| s.bg(t.hover))
                    .child(div().text_color(color).font_weight(FontWeight::SEMIBOLD).child(capsule::kind_glyph(d.kind)))
                    .child(div().text_color(t.text_primary).font_weight(FontWeight::SEMIBOLD).child(d.name.clone()))
                    .when_some(d.chip.clone(), |r, c| r.child(capsule::chip(&c, t, px(11.), false)))
                    .tooltip({
                        let tip = format!("{} {} — click to show it in the graphs and inspector", d.kind.label(), d.id);
                        move |window, cx| Tooltip::new(tip.clone()).build(window, cx)
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let id = id.clone();
                        this.state.update(cx, |s, cx| s.select(Some(id), false, SelectionSource::Inspector, cx));
                    })),
            );
        }
        row
    }
}

/// A definition shown in a code block's header.
struct BlockDef {
    id: String,
    name: String,
    kind: wh_graph::GNodeKind,
    /// First line within the block.
    line: usize,
    chip: Option<capsule::Chip>,
}

const BLOCK_HEADER_H: f32 = 32.0;
const READING_W: f32 = 760.0;

impl Render for DocumentPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let t = tokens(cx);
        let line_h = f32::from(theme.mono_font_size) * 1.5;
        if let Some((i, line)) = self.pending_reveal {
            if self.scroll_to_line(i, line, line_h) {
                self.pending_reveal = None;
                cx.notify();
            }
        }
        let (tabs_open, dirty, diags, active_fi) = {
            let st = self.state.read(cx);
            let p = &st.project;
            let path = self.path(cx);
            (st.open_tabs.clone(), st.open_tabs.iter().map(|f| p.docs.is_dirty(f)).collect::<Vec<_>>(), p.model.diagnostics.clone(), p.file_index(&path))
        };
        let active_fi = active_fi.unwrap_or(usize::MAX);
        let selected = self.shown.as_ref().and_then(|s| tabs_open.iter().position(|t| t == s)).unwrap_or(0);
        let tabs = TabBar::new("doc-tabs")
            .selected_index(selected)
            .children(tabs_open.iter().zip(&dirty).enumerate().map(|(ti, (f, d))| {
                let name = f.rsplit('/').next().unwrap_or(f).to_string();
                let path = f.clone();
                let close = div()
                    .id(("close-tab", ti))
                    .px_1()
                    .rounded_sm()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .hover(|d| d.bg(theme.muted))
                    .child(if *d { "•" } else { "×" })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.close_tab(path.clone(), window, cx);
                    }));
                Tab::new().label(name).suffix(close)
            }))
            .on_click(cx.listener(|this, ix: &usize, window, cx| {
                let Some(path) = this.state.read(cx).open_tabs.get(*ix).cloned() else { return };
                this.state.update(cx, |s, cx| s.open_tab(&path, cx));
                if this.shown.as_deref() != Some(path.as_str()) {
                    this.rebuild(window, cx);
                }
            }));
        if self.shown.is_none() {
            return v_flex().size_full().track_focus(&self.focus).child(tabs).child(
                div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child("No open files. File → Open File… (⌘O) or select a node in a graph."),
            );
        }
        // the definitions of this file, with their status
        let (defs, module): (Vec<(usize, BlockDef)>, String) = {
            let st = self.state.read(cx);
            let queue = capsule::queue_positions(&st.synthesis);
            let model = &st.project.model;
            let defs = model
                .nodes
                .values()
                .filter(|n| n.file == active_fi)
                .map(|n| {
                    let synth = st.synthesis.iter().find(|(id, _)| *id == n.id).map(|(_, s)| s);
                    let chip = capsule::node_chip(synth, queue.get(&n.id).copied(), st.tests.get(&n.id));
                    (n.span.start as usize, BlockDef { id: n.id.clone(), name: n.name.clone(), kind: capsule::kind_of(n), line: 0, chip })
                })
                .collect();
            (defs, model.files.get(active_fi).map(|f| f.module.clone()).unwrap_or_default())
        };
        let file_text = self.state.read(cx).project.model.files.get(active_fi).map(|f| f.text.clone()).unwrap_or_default();
        let md_style = crate::markdown::Style { font: theme.font_family.clone(), mono: theme.mono_font_family.clone(), t };
        let this = cx.entity().downgrade();
        let on_ref: crate::markdown::OnRef = Rc::new(move |id, window, cx| {
            let _ = this.update(cx, |this, cx| this.select_ref(id, window, cx));
        });
        let mut body = v_flex().id("doc-body").items_center().gap(px(14.)).px_6().pt_4().pb_32();
        for (i, seg) in self.segs.iter().enumerate() {
            let flashing = seg.flash_until.is_some();
            let el: AnyElement = match seg.kind {
                Kind::Prose => {
                    let inner: AnyElement = match &seg.prose_edit {
                        Some(ed) => Textarea::new(ed).into_any_element(),
                        None => {
                            let st = self.state.read(cx);
                            v_flex().gap(px(10.)).children(crate::markdown::render(&seg.text, i, &st.project.model, &module, &md_style, on_ref.clone())).into_any_element()
                        }
                    };
                    div()
                        .id(("prose", i))
                        .flex_none()
                        .w_full()
                        .max_w(px(READING_W))
                        .rounded_md()
                        .pl_2()
                        .pr_2()
                        .py_1()
                        .border_l_2()
                        .border_color(if flashing { t.accent } else { gpui_kit::transparent_black() })
                        .when(flashing, |d| d.bg(t.accent.opacity(0.06)))
                        .hover(|d| d.bg(t.hover.opacity(0.5)))
                        .cursor_text()
                        .on_click(cx.listener(move |this, _, window, cx| this.edit_prose(i, window, cx)))
                        .child(inner)
                        .into_any_element()
                }
                Kind::Code => {
                    let ed = seg.editor.clone().unwrap();
                    let lines = seg.text.lines().count().max(1) + 1;
                    let in_block = |at: usize| at >= seg.fence.start && at <= seg.fence.end;
                    let mut block_defs: Vec<BlockDef> = Vec::new();
                    for (at, d) in defs.iter().filter(|(at, _)| in_block(*at)) {
                        let line = wh_syntax::span::line_col(&seg.text, (at.saturating_sub(seg.range.start)).min(seg.text.len()) as u32).0;
                        block_defs.push(BlockDef { line, chip: d.chip.clone(), id: d.id.clone(), name: d.name.clone(), kind: d.kind });
                    }
                    block_defs.sort_by_key(|d| d.line);
                    // gutter: one status glyph per definition line
                    let mut gutter = div().relative().w(px(22.)).flex_none().h_full().bg(t.sunken);
                    for (k, d) in block_defs.iter().enumerate() {
                        let Some(c) = &d.chip else { continue };
                        let tip = format!("{}: {}", d.name, c.detail);
                        gutter = gutter.child(
                            div()
                                .id(("mark", i * 1000 + k))
                                .absolute()
                                .top(px(GUTTER_TOP + d.line as f32 * line_h))
                                .left_0()
                                .w(px(22.))
                                .h(px(line_h))
                                .flex()
                                .items_center()
                                .justify_center()
                                .text_size(px(12.))
                                .font_weight(FontWeight::BOLD)
                                .text_color(c.tone.color(t))
                                .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                                .child(c.glyph),
                        );
                    }
                    let mine: Vec<&wh_model::FileDiagnostic> = diags
                        .iter()
                        .filter(|d| d.file == active_fi && (d.diag.span.start as usize) >= seg.fence.start && (d.diag.span.start as usize) <= seg.fence.end)
                        .collect();
                    // lints: a dim dot at the right margin of their line, with a fix action
                    let mut lints = div().absolute().top_0().right_0().w(px(18.)).h_full();
                    for (k, d) in mine.iter().filter(|d| d.diag.severity == Severity::Lint).enumerate() {
                        let off = (d.diag.span.start as usize).saturating_sub(seg.range.start).min(seg.text.len());
                        let line = wh_syntax::span::line_col(&seg.text, off as u32).0;
                        let node = self.state.read(cx).project.model.node_at(d.file, d.diag.span.start).map(|n| (n.id.clone(), n.name.clone()));
                        let prose = d.diag.message.contains("no prose");
                        let tip = if prose { format!("{} — click to draft it with the author agent", d.diag.message) } else { d.diag.message.clone() };
                        lints = lints.child(
                            div()
                                .id(("lint", i * 1000 + k))
                                .absolute()
                                .top(px(GUTTER_TOP + line as f32 * line_h))
                                .w(px(18.))
                                .h(px(line_h))
                                .flex()
                                .items_center()
                                .justify_center()
                                .cursor_pointer()
                                .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                                .child(div().size(px(6.)).rounded_full().bg(t.text_tertiary.opacity(0.7)))
                                .when_some(node.filter(|_| prose), |d, (id, name)| {
                                    d.on_click(cx.listener(move |this, _, _, cx| {
                                        cx.stop_propagation();
                                        this.draft_prose(id.clone(), name.clone(), cx)
                                    }))
                                }),
                        );
                    }
                    // live values at the right margin of their line (Quokka-style), dimmed when stale
                    let mut values = div().absolute().top_0().left(px(60.)).right(px(20.)).h_full();
                    {
                        let st = self.state.read(cx);
                        let path = self.path(cx);
                        let mut seen_lines = Vec::new();
                        for (k, v) in st.live.in_file(&path).enumerate() {
                            let at = v.span.start as usize;
                            if at < seg.range.start || at > seg.range.start + seg.text.len() {
                                continue;
                            }
                            let line = wh_syntax::span::line_col(&seg.text, (at - seg.range.start) as u32).0;
                            if seen_lines.contains(&line) {
                                continue;
                            }
                            seen_lines.push(line);
                            let fresh = file_text.get(v.span.start as usize..v.span.end as usize) == Some(v.source.as_str());
                            let c = capsule::live_chip(v);
                            let tip = c.detail.clone();
                            values = values.child(
                                div()
                                    .id(("live", i * 1000 + k))
                                    .absolute()
                                    .right_0()
                                    .top(px(GUTTER_TOP + line as f32 * line_h + 2.0))
                                    .max_w(px(260.))
                                    .px_1p5()
                                    .rounded(px(4.))
                                    .bg(t.raised)
                                    .border_1()
                                    .border_color(c.tone.color(t).opacity(0.35))
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .font_family(theme.mono_font_family.clone())
                                    .text_size(px(11.5))
                                    .line_height(px(line_h - 4.0))
                                    .text_color(c.tone.color(t))
                                    .when(!fresh, |d| d.opacity(0.45))
                                    .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                                    .child(if c.text.is_empty() { c.glyph.to_string() } else { format!("{} {}", c.glyph, c.text) }),
                            );
                        }
                    }
                    let mut col = v_flex()
                        .flex_none()
                        .w_full()
                        .max_w(px(READING_W))
                        .rounded(px(8.))
                        .border_1()
                        .border_color(if flashing { t.accent } else { t.border_subtle })
                        .bg(t.sunken)
                        .overflow_hidden()
                        .child(self.block_header(i, &block_defs, cx))
                        .child(
                            div()
                                .relative()
                                .h(px(lines as f32 * line_h + 12.0))
                                .on_mouse_up(MouseButton::Left, cx.listener(move |this, _, _, cx| this.cursor_moved(i, cx)))
                                .on_key_up(cx.listener(move |this, _, _, cx| this.cursor_moved(i, cx)))
                                .flex()
                                .child(gutter)
                                .child(div().flex_1().h_full().child(Editor::new(&ed).bordered(false).size_full()))
                                .child(values)
                                .child(lints),
                        );
                    // real problems stay inline, and a click puts the caret on them
                    for (k, d) in mine.iter().filter(|d| d.diag.severity != Severity::Lint).enumerate() {
                        let color = if d.diag.severity == Severity::Error { t.danger } else { t.warning };
                        let (l, _) = wh_syntax::span::line_col(&file_text, d.diag.span.start);
                        let at = d.diag.span.start as usize;
                        col = col.child(
                            h_flex()
                                .id(("diag", i * 1000 + k))
                                .gap_2()
                                .px_3()
                                .py_1()
                                .text_size(px(12.))
                                .border_t_1()
                                .border_color(color.opacity(0.3))
                                .bg(color.opacity(0.08))
                                .cursor_pointer()
                                .hover(|s| s.bg(color.opacity(0.14)))
                                .on_click(cx.listener(move |this, _, window, cx| this.place_caret(i, at, window, cx)))
                                .child(div().flex_none().font_weight(FontWeight::BOLD).text_color(color).child(if d.diag.severity == Severity::Error { "✗" } else { "⚠" }))
                                .child(div().flex_none().text_color(t.text_tertiary).child(format!("line {}", l + 1)))
                                .child(div().flex_1().text_color(t.text_primary).child(d.diag.message.clone())),
                        );
                    }
                    col.into_any_element()
                }
            };
            body = body.child(el);
        }
        let jump = self.jump.clone().map(|(i, label, below)| {
            let line = self.segs.get(i).map_or(0, |_| 0);
            h_flex()
                .id("jump-pill")
                .absolute()
                .when(below, |d| d.bottom(px(14.)))
                .when(!below, |d| d.top(px(14.)))
                .left_0()
                .right_0()
                .justify_center()
                .child(
                    h_flex()
                        .id("jump-pill-inner")
                        .gap_2()
                        .px_3()
                        .py_1()
                        .rounded_full()
                        .bg(t.overlay)
                        .border_1()
                        .border_color(t.accent.opacity(0.6))
                        .shadow_md()
                        .text_size(px(12.))
                        .text_color(t.text_primary)
                        .cursor_pointer()
                        .child(div().text_color(t.accent).child(if below { "↓" } else { "↑" }))
                        .child(format!("1 change: {label} — jump"))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let line_h = f32::from(cx.theme().mono_font_size) * 1.5;
                            this.scroll_to_line(i, line, line_h);
                            this.jump = None;
                            cx.notify();
                        })),
                )
        });
        let _ = window;
        crate::palette::on_palette(v_flex(), &self.state)
            .size_full()
            .bg(t.panel)
            .track_focus(&self.focus)
            .child(tabs)
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .child(body.overflow_y_scroll().track_scroll(&self.scroll).size_full())
                    .children(jump),
            )
    }
}
