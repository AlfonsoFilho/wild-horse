//! A node's identity in human terms, shared by every view (UX review §3, §9 A1):
//! one status vocabulary (glyph + tone + text) for synthesis and spec results,
//! the chip that renders it, and readable selection labels.

use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use wh_model::{Model, NodeKind};

use crate::backend::{NodeResult, SynthStatus};
use crate::tokens::{Tokens, Tone};

/// One status as shown in a chip, a gutter or the status bar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chip {
    pub glyph: &'static str,
    pub tone: Tone,
    /// Short text shown in the chip.
    pub text: String,
    /// Full explanation for the tooltip.
    pub detail: String,
}

impl Chip {
    fn new(glyph: &'static str, tone: Tone, text: impl Into<String>, detail: impl Into<String>) -> Chip {
        Chip { glyph, tone, text: text.into(), detail: detail.into() }
    }

    /// Quiet when good: verified shows only its glyph.
    pub fn is_quiet(&self) -> bool {
        self.tone == Tone::Success
    }
}

/// The synthesis state of a hole. `queue_pos` is its 1-based place among
/// queued holes, when known.
pub fn synth_chip(s: &SynthStatus, queue_pos: Option<usize>) -> Chip {
    match s {
        SynthStatus::Stale => Chip::new("◌", Tone::Warning, "stale", "stale: the spec changed since the last synthesis"),
        SynthStatus::Queued => {
            let text = queue_pos.map_or("queued".to_string(), |k| format!("queued ({})", ordinal(k)));
            Chip::new("⋯", Tone::Muted, text.clone(), format!("{text}: waiting for the compiler agent"))
        }
        SynthStatus::Synthesizing => Chip::new("◐", Tone::Info, "writing", "the compiler agent is writing the implementation"),
        SynthStatus::Verifying { round, of } => {
            Chip::new("◔", Tone::Info, format!("verifying {round}/{of}"), format!("verifying the implementation, round {round} of {of}"))
        }
        SynthStatus::Accepted => Chip::new("✓", Tone::Success, "verified", "synthesized and verified"),
        SynthStatus::Restored => Chip::new("✓", Tone::Success, "verified", "restored from the synthesis cache and verified"),
        SynthStatus::SoftStale => {
            Chip::new("✓", Tone::Success, "verified", "verified; the prose changed since, which never re-synthesizes on its own")
        }
        SynthStatus::Failed(m) => Chip::new("✗", Tone::Danger, short_reason(m), format!("synthesis failed: {m}")),
    }
}

/// Spec results from the latest test run.
pub fn test_chip(r: &NodeResult) -> Chip {
    if r.ok() {
        Chip::new("✓", Tone::Success, format!("{}/{}", r.passed, r.passed), r.summary())
    } else {
        Chip::new("✗", Tone::Danger, format!("{}/{} failing", r.failed, r.passed + r.failed), r.summary())
    }
}

/// The one status to show for a node: failures first, then work in progress,
/// then success.
pub fn node_chip(synth: Option<&SynthStatus>, queue_pos: Option<usize>, tests: Option<&NodeResult>) -> Option<Chip> {
    let s = synth.map(|s| synth_chip(s, queue_pos));
    let t = tests.map(test_chip);
    match (s, t) {
        (Some(s), _) if s.tone == Tone::Danger => Some(s),
        (_, Some(t)) if t.tone == Tone::Danger => Some(t),
        (Some(s), _) if s.tone != Tone::Success => Some(s),
        (_, Some(t)) => Some(t),
        (s, None) => s,
    }
}

/// 1-based queue positions of the queued holes, by node id.
pub fn queue_positions(synthesis: &[(String, SynthStatus)]) -> std::collections::BTreeMap<String, usize> {
    synthesis.iter().filter(|(_, s)| *s == SynthStatus::Queued).enumerate().map(|(k, (n, _))| (n.clone(), k + 1)).collect()
}

fn ordinal(k: usize) -> String {
    let suffix = match (k % 10, k % 100) {
        (1, r) if r != 11 => "st",
        (2, r) if r != 12 => "nd",
        (3, r) if r != 13 => "rd",
        _ => "th",
    };
    format!("{k}{suffix}")
}

/// The first clause of a failure message, short enough for a chip.
fn short_reason(m: &str) -> String {
    let first = m.split(['\n', ';']).next().unwrap_or(m).trim();
    if first.chars().count() > 40 { format!("{}…", first.chars().take(39).collect::<String>()) } else { first.to_string() }
}

/// A status chip: tinted pill with glyph and text, or just the glyph when
/// `quiet` (verified) or `compact`.
pub fn chip(c: &Chip, t: &Tokens, size: Pixels, compact: bool) -> Div {
    let color = c.tone.color(t);
    let glyph_only = compact || c.is_quiet();
    div()
        .flex_none()
        .flex()
        .items_center()
        .gap(size * 0.3)
        .text_size(size)
        .line_height(size * 1.4)
        .text_color(color)
        .when(!glyph_only, |d| d.px(size * 0.45).rounded(size * 0.7).bg(color.opacity(0.15)).border_1().border_color(color.opacity(0.45)))
        .child(div().font_weight(FontWeight::BOLD).child(c.glyph))
        .when(!glyph_only, |d| d.child(div().whitespace_nowrap().child(c.text.clone())))
}

/// A live value (`wh_live`) as a glyph, tone and short text, e.g. `✓ 40` or
/// `✗ left = 39, right = 40`.
pub fn live_chip(v: &wh_live::LiveValue) -> Chip {
    use wh_live::Status;
    let values = v.values.iter().map(|n| format!("{} = {}", n.label, n.value)).collect::<Vec<_>>().join(", ");
    let short = |s: &str| if s.chars().count() > 48 { format!("{}…", s.chars().take(47).collect::<String>()) } else { s.to_string() };
    let mut detail = match &v.message {
        Some(m) if !values.is_empty() => format!("{m}\n{values}"),
        Some(m) => m.clone(),
        None => values.clone(),
    };
    if let Some(c) = v.cases {
        detail = format!("{detail}\nheld in {c} cases").trim().to_string();
    }
    for s in v.samples.iter().take(3) {
        let ins = s.inputs.iter().map(|n| format!("{} = {}", n.label, n.value)).collect::<Vec<_>>().join(", ");
        let mark = match s.holds {
            Some(true) => "✓",
            Some(false) => "✗",
            None => "◌",
        };
        detail.push_str(&format!("\n{mark} {ins} → {}", s.result.clone().unwrap_or_default()));
    }
    let (glyph, tone) = match v.status {
        Status::Pass => ("✓", Tone::Success),
        Status::Fail => ("✗", Tone::Danger),
        Status::Error | Status::Crashed => ("✗", Tone::Danger),
        Status::Timeout => ("⏱", Tone::Warning),
        Status::NotRun => ("◌", Tone::Muted),
    };
    let text = match (v.status.clone(), v.cases) {
        (Status::Pass, Some(c)) => format!("{c} cases"),
        (Status::Error | Status::Crashed, _) => short(v.message.as_deref().unwrap_or("error")),
        _ => short(&values),
    };
    Chip { glyph, tone, text, detail: detail.trim().to_string() }
}

/// The graph kind (colour and glyph role) of a model node.
pub fn kind_of(n: &wh_model::Node) -> wh_graph::GNodeKind {
    use wh_graph::GNodeKind as K;
    match &n.kind {
        NodeKind::Def(_) => K::Def,
        NodeKind::Flow(_) | NodeKind::Machine(_) => K::Flow,
        NodeKind::App(_) => K::App,
        NodeKind::Capability(_) => K::Capability,
        NodeKind::Adapter(_) => K::Adapter,
        NodeKind::Struct(_) => K::Struct,
        NodeKind::Enum(_) if n.is_error_type => K::ErrorEnum,
        NodeKind::Enum(_) => K::Enum,
        NodeKind::Refined { .. } => K::Refined,
        NodeKind::Alias(_) => K::Alias,
    }
}

/// A text glyph per kind, until the Lucide icons land (St5).
pub fn kind_glyph(k: wh_graph::GNodeKind) -> &'static str {
    use wh_graph::GNodeKind as K;
    match k {
        K::Def | K::Step => "ƒ",
        K::Flow => "⇢",
        K::App => "▸",
        K::Param => "→",
        K::Capability => "⌁",
        K::Adapter => "⊸",
        K::Struct => "{}",
        K::Enum => "≡",
        K::ErrorEnum => "!",
        K::Refined => "⊂",
        K::Alias => "=",
    }
}

/// A readable name for a selection id: "step 2 of register · normalize_form",
/// "def normalize_email", "input form of register". `None` when the id no
/// longer names anything (e.g. after an undo).
pub fn selection_label(model: &Model, id: &str) -> Option<String> {
    if let Some((flow, rest)) = id.split_once('#') {
        let owner = model.nodes.get(flow)?;
        if let Some(p) = rest.strip_prefix("in:") {
            return Some(format!("input {p} of {}", owner.name));
        }
        let steps = match &owner.kind {
            NodeKind::Flow(f) => &f.steps,
            NodeKind::App(a) => &a.entry.as_ref()?.steps,
            _ => return None,
        };
        let k = steps.iter().position(|s| s.id == id)?;
        return Some(format!("step {} of {} · {}", k + 1, owner.name, steps[k].label));
    }
    let n = model.nodes.get(id)?;
    let kind = if n.is_error_type { "@error enum" } else { n.kind.label() };
    Some(format!("{kind} {}", n.name))
}

#[cfg(test)]
mod tests {
    use super::{NodeResult, SynthStatus, Tone, node_chip};

    #[test]
    fn failures_win_and_verified_is_quiet() {
        let failed = SynthStatus::Failed("no compiler agent (mock backend)".into());
        let ok = NodeResult { passed: 2, failed: 0, failures: vec![] };
        let c = node_chip(Some(&failed), None, Some(&ok)).unwrap();
        assert_eq!((c.glyph, c.tone), ("✗", Tone::Danger));
        assert_eq!(c.text, "no compiler agent (mock backend)");
        let c = node_chip(Some(&SynthStatus::Accepted), None, Some(&ok)).unwrap();
        assert!(c.is_quiet());
        let bad = NodeResult { passed: 1, failed: 1, failures: vec!["x".into()] };
        assert_eq!(node_chip(Some(&SynthStatus::Accepted), None, Some(&bad)).unwrap().tone, Tone::Danger);
        assert_eq!(node_chip(Some(&SynthStatus::Queued), Some(3), None).unwrap().text, "queued (3rd)");
        assert!(node_chip(None, None, None).is_none());
    }
}
