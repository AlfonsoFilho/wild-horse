//! Document-level parsing: Markdown structure (front matter, headings, prose,
//! references) plus the `wh` blocks inside it.

use pulldown_cmark::{CodeBlockKind, Event, Options, Parser as MdParser, Tag, TagEnd};

use crate::ast::*;
use crate::diag::Diagnostic;
use crate::parser::parse_block;
use crate::span::Span;

pub struct Parsed {
    pub doc: Document,
    pub diagnostics: Vec<Diagnostic>,
}

pub fn parse_document(src: &str) -> Parsed {
    let mut diagnostics = Vec::new();
    let front_matter = front_matter(src);

    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_YAML_STYLE_METADATA_BLOCKS);

    struct RawHeading {
        level: u8,
        title: String,
        span: Span,
    }
    let mut headings: Vec<RawHeading> = Vec::new();
    let mut fences: Vec<(Span, Span)> = Vec::new(); // (whole block, content)
    let mut other_code: Vec<Span> = Vec::new();
    let mut in_heading: Option<(u8, String, Span)> = None;

    for (event, range) in MdParser::new_ext(src, opts).into_offset_iter() {
        let span = Span::new(range.start as u32, range.end as u32);
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                in_heading = Some((level as u8, String::new(), span));
            }
            Event::Text(t) | Event::Code(t) if in_heading.is_some() => {
                in_heading.as_mut().unwrap().1.push_str(&t);
            }
            Event::End(TagEnd::Heading(_)) => {
                if let Some((level, title, span)) = in_heading.take() {
                    headings.push(RawHeading { level, title: title.trim().to_string(), span });
                }
            }
            Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(info))) => {
                let lang = info.split_whitespace().next().unwrap_or("");
                if lang == "wh" {
                    fences.push((span, fence_content(src, span)));
                } else {
                    other_code.push(span);
                }
            }
            Event::Start(Tag::CodeBlock(CodeBlockKind::Indented)) => other_code.push(span),
            _ => {}
        }
    }

    // sections: a heading governs until the next heading of the same or higher level
    let mut sections: Vec<Section> = Vec::new();
    for (i, h) in headings.iter().enumerate() {
        let end = headings[i + 1..]
            .iter()
            .find(|n| n.level <= h.level)
            .map_or(src.len() as u32, |n| n.span.start);
        let parent = (0..i).rev().find(|&j| headings[j].level < h.level && sections[j].span.end >= end);
        sections.push(Section {
            level: h.level,
            title: h.title.clone(),
            heading_span: h.span,
            span: Span::new(h.span.start, end),
            parent,
        });
    }
    let innermost_section = |offset: u32| -> Option<usize> {
        sections
            .iter()
            .enumerate()
            .filter(|(_, s)| s.span.start <= offset && offset < s.span.end.max(s.span.start + 1))
            .max_by_key(|(_, s)| s.level)
            .map(|(i, _)| i)
    };

    // prose: the gaps between front matter, headings and wh blocks
    let mut hard: Vec<Span> = Vec::new();
    if let Some(fm) = &front_matter {
        hard.push(fm.span);
    }
    hard.extend(headings.iter().map(|h| h.span));
    hard.extend(fences.iter().map(|f| f.0));
    hard.sort();
    let mut prose: Vec<Prose> = Vec::new();
    let mut cursor = 0u32;
    let mut gaps = Vec::new();
    for h in &hard {
        if h.start > cursor {
            gaps.push(Span::new(cursor, h.start));
        }
        cursor = cursor.max(h.end);
    }
    if (cursor as usize) < src.len() {
        gaps.push(Span::new(cursor, src.len() as u32));
    }
    for g in gaps {
        let text = &src[g.range()];
        let trimmed = text.trim();
        if trimmed.is_empty() {
            continue;
        }
        let lead = (text.len() - text.trim_start().len()) as u32;
        let span = Span::new(g.start + lead, g.start + lead + trimmed.len() as u32);
        prose.push(Prose { text: trimmed.to_string(), span });
    }

    // references `[[name]]` in prose, skipping non-wh code
    let mut refs = Vec::new();
    for p in &prose {
        let text = &src[p.span.range()];
        let mut from = 0;
        while let Some(open) = text[from..].find("[[") {
            let open = from + open;
            let Some(close) = text[open + 2..].find("]]") else { break };
            let close = open + 2 + close;
            let target = text[open + 2..close].trim();
            let span = Span::new(p.span.start + open as u32, p.span.start + close as u32 + 2);
            let in_code = other_code.iter().any(|c| c.start <= span.start && span.end <= c.end);
            if !target.is_empty() && !target.contains('\n') && !in_code {
                refs.push(DocRef { target: target.to_string(), span });
            }
            from = close + 2;
        }
    }

    // wh blocks
    let mut blocks = Vec::new();
    for (span, content) in fences {
        let (items, diags) = parse_block(src, content);
        diagnostics.extend(diags);
        let prose_idx = prose.iter().rposition(|p| {
            p.span.end <= span.start && src[p.span.end as usize..span.start as usize].trim().is_empty()
        });
        blocks.push(WhBlock { span, content_span: content, items, section: innermost_section(span.start), prose: prose_idx });
    }

    let doc = Document { front_matter, sections, prose, blocks, refs, span: Span::new(0, src.len() as u32) };
    Parsed { doc, diagnostics }
}

/// The code between the opening and closing fence lines of a fenced block.
fn fence_content(src: &str, block: Span) -> Span {
    let text = &src[block.range()];
    let open_end = text.find('\n').map_or(text.len(), |i| i + 1);
    let body = text.trim_end_matches(['\n', '\r']);
    let last_line_start = body.rfind('\n').map_or(0, |i| i + 1);
    let last = body[last_line_start..].trim_start();
    let close_start = if last_line_start >= open_end && (last.starts_with("```") || last.starts_with("~~~")) {
        last_line_start
    } else {
        text.len()
    };
    Span::new(block.start + open_end as u32, block.start + close_start.max(open_end) as u32)
}

fn front_matter(src: &str) -> Option<FrontMatter> {
    let rest = src.strip_prefix("---\n").or_else(|| src.strip_prefix("---\r\n"))?;
    let header_len = src.len() - rest.len();
    let mut offset = 0;
    for line in rest.split_inclusive('\n') {
        if line.trim_end() == "---" || line.trim_end() == "..." {
            let raw = rest[..offset].to_string();
            let module = raw.lines().find_map(|l| {
                let (k, v) = l.split_once(':')?;
                (k.trim() == "module").then(|| v.trim().trim_matches('"').to_string())
            });
            let end = header_len + offset + line.len();
            return Some(FrontMatter { module, raw, span: Span::new(0, end as u32) });
        }
        offset += line.len();
    }
    None
}
