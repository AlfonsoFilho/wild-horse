//! Prose rendering for the document panel (UX review D1, D2): Markdown via
//! `pulldown-cmark` into styled text runs, the front matter as property
//! pills, and `[[refs]]` as kind-tinted links that select their node.

use std::ops::Range;
use std::rc::Rc;

use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use wh_model::Model;

use crate::tokens::Tokens;

/// Called with the node id of a clicked `[[ref]]`.
pub type OnRef = Rc<dyn Fn(String, &mut Window, &mut App)>;

pub struct Style {
    pub font: SharedString,
    pub mono: SharedString,
    pub t: &'static Tokens,
}

const REF_OPEN: char = '\u{E000}';
const REF_CLOSE: char = '\u{E001}';

/// A ref target: the node id it names, or the unresolved name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Node(String, wh_graph::GNodeKind),
    Missing(String),
}

pub fn resolve_ref(model: &Model, module: &str, name: &str) -> Target {
    match model.resolve_path(module, name).and_then(|id| model.nodes.get(&id)) {
        Some(n) => Target::Node(n.id.clone(), crate::capsule::kind_of(n)),
        None => Target::Missing(name.to_string()),
    }
}

/// Split `---` front matter off the top: `(key, value)` pairs and the body offset.
pub fn front_matter(text: &str) -> (Vec<(String, String)>, usize) {
    let Some(rest) = text.strip_prefix("---\n") else { return (vec![], 0) };
    let Some(end) = rest.find("\n---") else { return (vec![], 0) };
    let props = rest[..end]
        .lines()
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_string(), v.trim().trim_matches('"').to_string()))
        .collect();
    let after = 4 + end + 4;
    (props, after.min(text.len()))
}

#[derive(Default)]
struct Inline {
    text: String,
    runs: Vec<(usize, Run)>,
    refs: Vec<(Range<usize>, Target)>,
}

#[derive(Clone, Copy, Default, PartialEq)]
struct Run {
    bold: bool,
    italic: bool,
    code: bool,
    link: bool,
    strike: bool,
    reference: Option<RefLook>,
}

#[derive(Clone, Copy, PartialEq)]
struct RefLook {
    color: Hsla,
    missing: bool,
}

struct Builder<'a> {
    model: &'a Model,
    module: &'a str,
    s: &'a Style,
    on_ref: OnRef,
    id: usize,
    out: Vec<AnyElement>,
    inl: Inline,
    run: Run,
    in_ref: bool,
    heading: Option<HeadingLevel>,
    lists: Vec<Option<u64>>,
    marker: Option<String>,
    quote: usize,
    code: Option<String>,
    table: Option<(Vec<AnyElement>, Vec<AnyElement>, bool)>,
}

/// Render one prose block. `id` keeps element ids unique per block.
pub fn render(text: &str, id: usize, model: &Model, module: &str, s: &Style, on_ref: OnRef) -> Vec<AnyElement> {
    let (props, body) = front_matter(text);
    let mut b = Builder {
        model,
        module,
        s,
        on_ref,
        id: id * 1000,
        out: vec![],
        inl: Inline::default(),
        run: Run::default(),
        in_ref: false,
        heading: None,
        lists: vec![],
        marker: None,
        quote: 0,
        code: None,
        table: None,
    };
    if !props.is_empty() {
        b.out.push(pills(&props, s).into_any_element());
    }
    let src = mark_refs(&text[body..]);
    for ev in Parser::new_ext(&src, Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH) {
        b.event(ev);
    }
    b.flush();
    b.out
}

/// `[[name]]` → private-use markers, which Markdown leaves alone.
fn mark_refs(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find("[[") {
        match rest[i + 2..].find("]]") {
            Some(j) if !rest[i + 2..i + 2 + j].contains('\n') => {
                out.push_str(&rest[..i]);
                out.push(REF_OPEN);
                out.push_str(&rest[i + 2..i + 2 + j]);
                out.push(REF_CLOSE);
                rest = &rest[i + 2 + j + 2..];
            }
            _ => {
                out.push_str(&rest[..i + 2]);
                rest = &rest[i + 2..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn pills(props: &[(String, String)], s: &Style) -> Div {
    let t = s.t;
    h_flex().flex_wrap().gap_1p5().children(props.iter().map(|(k, v)| {
        h_flex()
            .gap_1()
            .px_2()
            .py_0p5()
            .rounded_full()
            .bg(t.sunken)
            .border_1()
            .border_color(t.border_subtle)
            .text_xs()
            .child(div().text_color(t.text_tertiary).child(k.clone()))
            .child(div().text_color(t.text_secondary).child(v.clone()))
    }))
}

impl Builder<'_> {
    fn event(&mut self, ev: Event<'_>) {
        match ev {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(t) => match &mut self.code {
                Some(code) => code.push_str(&t),
                None => self.text(&t),
            },
            Event::Code(t) => {
                let saved = self.run;
                self.run.code = true;
                self.push(&t);
                self.run = saved;
            }
            Event::SoftBreak => self.push(" "),
            Event::HardBreak => self.push("\n"),
            Event::Rule => {
                self.flush();
                self.out.push(div().my_2().h(px(1.)).bg(self.s.t.border_subtle).into_any_element());
            }
            Event::Html(h) | Event::InlineHtml(h) => self.push(&h),
            Event::TaskListMarker(done) => self.push(if done { "☑ " } else { "☐ " }),
            _ => {}
        }
    }

    fn start(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph => {}
            Tag::Heading { level, .. } => {
                self.flush();
                self.heading = Some(level);
            }
            Tag::BlockQuote(_) => {
                self.flush();
                self.quote += 1;
            }
            Tag::CodeBlock(kind) => {
                self.flush();
                let _ = matches!(kind, CodeBlockKind::Fenced(_));
                self.code = Some(String::new());
            }
            Tag::List(start) => {
                self.flush();
                self.lists.push(start);
            }
            Tag::Item => {
                self.flush();
                let depth = self.lists.len();
                self.marker = Some(match self.lists.last_mut() {
                    Some(Some(n)) => {
                        *n += 1;
                        format!("{}.", *n - 1)
                    }
                    _ => if depth > 1 { "◦" } else { "•" }.to_string(),
                });
            }
            Tag::Emphasis => self.run.italic = true,
            Tag::Strong => self.run.bold = true,
            Tag::Strikethrough => self.run.strike = true,
            Tag::Link { .. } => self.run.link = true,
            Tag::Table(_) => {
                self.flush();
                self.table = Some((vec![], vec![], false));
            }
            Tag::TableHead => {
                if let Some(t) = &mut self.table {
                    t.2 = true;
                }
            }
            Tag::TableRow | Tag::TableCell => {}
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph | TagEnd::Item => self.flush(),
            TagEnd::Heading(_) => {
                self.flush();
                self.heading = None;
            }
            TagEnd::BlockQuote(_) => {
                self.flush();
                self.quote = self.quote.saturating_sub(1);
            }
            TagEnd::CodeBlock => {
                let code = self.code.take().unwrap_or_default();
                let t = self.s.t;
                self.out.push(
                    div()
                        .ml(px(self.indent()))
                        .px_3()
                        .py_2()
                        .rounded(px(8.))
                        .bg(t.sunken)
                        .border_1()
                        .border_color(t.border_subtle)
                        .font_family(self.s.mono.clone())
                        .text_size(px(12.5))
                        .text_color(t.text_secondary)
                        .child(code.trim_end().to_string())
                        .into_any_element(),
                );
            }
            TagEnd::List(_) => {
                self.flush();
                self.lists.pop();
            }
            TagEnd::Emphasis => self.run.italic = false,
            TagEnd::Strong => self.run.bold = false,
            TagEnd::Strikethrough => self.run.strike = false,
            TagEnd::Link => self.run.link = false,
            TagEnd::TableCell => {
                let cell = self.take_inline();
                let head = self.table.as_ref().is_some_and(|t| t.2);
                if let Some((_, row, _)) = &mut self.table {
                    row.push(div().flex_1().min_w_0().px_2().py_1().when(head, |d| d.font_weight(FontWeight::SEMIBOLD)).child(cell).into_any_element());
                }
            }
            TagEnd::TableHead | TagEnd::TableRow => {
                let t = self.s.t;
                if let Some((rows, row, head)) = &mut self.table {
                    let cells = std::mem::take(row);
                    rows.push(h_flex().border_b_1().border_color(t.border_subtle).when(*head, |d| d.bg(t.sunken)).children(cells).into_any_element());
                    *head = false;
                }
            }
            TagEnd::Table => {
                if let Some((rows, _, _)) = self.table.take() {
                    let t = self.s.t;
                    self.out.push(v_flex().rounded(px(6.)).border_1().border_color(t.border_subtle).overflow_hidden().text_sm().children(rows).into_any_element());
                }
            }
            _ => {}
        }
    }

    fn indent(&self) -> f32 {
        (self.lists.len().saturating_sub(1)) as f32 * 18.0
    }

    /// Text with `[[ref]]` markers.
    fn text(&mut self, t: &str) {
        let mut rest = t;
        loop {
            let marker = if self.in_ref { REF_CLOSE } else { REF_OPEN };
            match rest.find(marker) {
                Some(i) => {
                    if self.in_ref {
                        self.push_ref_text(&rest[..i]);
                    } else {
                        self.push(&rest[..i]);
                    }
                    self.in_ref = !self.in_ref;
                    rest = &rest[i + marker.len_utf8()..];
                }
                None => {
                    if self.in_ref {
                        self.push_ref_text(rest);
                    } else {
                        self.push(rest);
                    }
                    return;
                }
            }
        }
    }

    fn push_ref_text(&mut self, name: &str) {
        if name.is_empty() {
            return;
        }
        let target = resolve_ref(self.model, self.module, name.trim());
        let look = match &target {
            Target::Node(_, k) => RefLook { color: self.s.t.kind(*k), missing: false },
            Target::Missing(_) => RefLook { color: self.s.t.danger, missing: true },
        };
        let start = self.inl.text.len();
        let saved = self.run;
        self.run.reference = Some(look);
        self.push(name);
        self.run = saved;
        self.inl.refs.push((start..self.inl.text.len(), target));
    }

    fn push(&mut self, s: &str) {
        if s.is_empty() {
            return;
        }
        self.inl.text.push_str(s);
        match self.inl.runs.last_mut() {
            Some((len, r)) if *r == self.run => *len += s.len(),
            _ => self.inl.runs.push((s.len(), self.run)),
        }
    }

    fn take_inline(&mut self) -> AnyElement {
        let inl = std::mem::take(&mut self.inl);
        self.id += 1;
        let t = self.s.t;
        let base = self.heading.is_some();
        let runs: Vec<TextRun> = inl
            .runs
            .iter()
            .map(|(len, r)| {
                let mut font = gpui_kit::font(if r.code { self.s.mono.clone() } else { self.s.font.clone() });
                font.weight = if r.bold || base { FontWeight::SEMIBOLD } else if r.reference.is_some() { FontWeight::MEDIUM } else { FontWeight::NORMAL };
                if r.italic {
                    font.style = FontStyle::Italic;
                }
                let color = match (r.reference, r.link, r.code) {
                    (Some(l), _, _) => l.color,
                    (None, true, _) => t.accent,
                    (None, false, true) => t.text_primary,
                    _ => if self.quote > 0 { t.text_secondary } else { t.text_primary },
                };
                let background_color = match (r.reference, r.code) {
                    (Some(l), _) if !l.missing => Some(l.color.opacity(0.10)),
                    (_, true) => Some(t.hover),
                    _ => None,
                };
                let underline = match r.reference {
                    Some(l) if l.missing => Some(UnderlineStyle { thickness: px(1.), color: Some(t.danger), wavy: true }),
                    _ if r.link => Some(UnderlineStyle { thickness: px(1.), color: Some(t.accent.opacity(0.5)), wavy: false }),
                    _ => None,
                };
                let strikethrough = r.strike.then(|| StrikethroughStyle { thickness: px(1.), color: Some(t.text_secondary) });
                TextRun { len: *len, font, color, background_color, underline, strikethrough }
            })
            .collect();
        let ranges: Vec<Range<usize>> = inl.refs.iter().map(|(r, _)| r.clone()).collect();
        let targets: Vec<Target> = inl.refs.into_iter().map(|(_, t)| t).collect();
        let tips = targets.clone();
        let on_ref = self.on_ref.clone();
        let styled = StyledText::new(inl.text).with_runs(runs);
        let tip_ranges = ranges.clone();
        InteractiveText::new(("md", self.id), styled)
            .on_click(ranges, move |ix, window, cx| {
                if let Some(Target::Node(id, _)) = targets.get(ix) {
                    cx.stop_propagation();
                    on_ref(id.clone(), window, cx);
                }
            })
            .tooltip(move |at, window, cx| {
                let ix = tip_ranges.iter().position(|r| r.contains(&at))?;
                let text = match &tips[ix] {
                    Target::Node(id, _) => format!("{id} — click to select"),
                    Target::Missing(n) => format!("`{n}` does not name a definition"),
                };
                Some(gpui_kit::component::tooltip::Tooltip::new(text).build(window, cx))
            })
            .into_any_element()
    }

    fn flush(&mut self) {
        if self.inl.text.trim().is_empty() {
            self.inl = Inline::default();
            if self.marker.is_none() {
                return;
            }
        }
        let indent = self.indent();
        let quote = self.quote > 0;
        let heading = self.heading;
        let marker = self.marker.take();
        let t = self.s.t;
        let body = self.take_inline();
        let el = div().min_w_0().flex_1().child(body);
        let el = match heading {
            Some(HeadingLevel::H1) => el.text_size(px(26.)).line_height(px(32.)).mt(px(12.)),
            Some(HeadingLevel::H2) => el.text_size(px(20.)).line_height(px(28.)).mt(px(10.)),
            Some(HeadingLevel::H3) => el.text_size(px(16.)).line_height(px(24.)).mt(px(6.)),
            Some(_) => el.text_size(px(15.)).line_height(px(24.)),
            None => el.text_size(px(15.)).line_height(px(24.)),
        };
        let row = h_flex()
            .items_start()
            .ml(px(indent))
            .when_some(marker, |d, m| d.child(div().w(px(18.)).flex_none().text_size(px(15.)).line_height(px(24.)).text_color(t.text_tertiary).child(m)))
            .child(el)
            .when(quote, |d| d.pl_3().border_l_2().border_color(t.border_strong));
        self.out.push(row.into_any_element());
    }
}

#[cfg(test)]
mod tests {
    use super::{REF_CLOSE, REF_OPEN, front_matter, mark_refs};

    #[test]
    fn refs_and_front_matter() {
        assert_eq!(mark_refs("see [[a.b]] and [x]"), format!("see {REF_OPEN}a.b{REF_CLOSE} and [x]"));
        assert_eq!(mark_refs("[[ broken"), "[[ broken");
        let (props, at) = front_matter("---\nmodule: signup\n---\n# Title\n");
        assert_eq!(props, vec![("module".to_string(), "signup".to_string())]);
        assert!("---\nmodule: signup\n---\n# Title\n"[at..].trim_start().starts_with("# Title"));
    }
}
