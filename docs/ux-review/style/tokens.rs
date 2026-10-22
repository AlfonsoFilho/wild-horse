//! Proposed `crates/wh-ide/src/tokens.rs` (UX review sketch, not compiled into the app).
//!
//! Wild Horse roles that gpui-kit's `ThemeColor` has no slot for: node kinds,
//! type families for ports, edge kinds, synthesis status and AI provenance.
//! Chrome colours (backgrounds, borders, buttons, tabs, focus ring) come from
//! the gpui-kit theme loaded from `wild-horse-theme.json`; read them through
//! `cx.theme()` and use this module only for the roles below.

use gpui::{App, Hsla, rgb};
use gpui_kit::component::ActiveTheme;
use wh_graph::{EdgeKind, GNodeKind};

pub struct WhTokens {
    pub canvas: Hsla,
    pub raised: Hsla,
    pub overlay: Hsla,
    pub text_tertiary: Hsla,
    pub accent: Hsla,
    pub success: Hsla,
    pub danger: Hsla,
    pub warning: Hsla,
    pub agent: Hsla,
    pub kind_def: Hsla,
    pub kind_flow: Hsla,
    pub kind_effect: Hsla,
    pub kind_data: Hsla,
    pub kind_error: Hsla,
    pub kind_param: Hsla,
    pub ty_scalar: Hsla,
    pub ty_text: Hsla,
    pub ty_record: Hsla,
    pub ty_enum: Hsla,
    pub ty_capability: Hsla,
    pub ty_unit: Hsla,
    pub edge_data: Hsla,
    pub edge_calls: Hsla,
}

pub fn dark() -> WhTokens {
    WhTokens {
    canvas: rgb(0x0F1012).into(),
    raised: rgb(0x1D1F23).into(),
    overlay: rgb(0x24272C).into(),
    text_tertiary: rgb(0x8A909A).into(),
    accent: rgb(0x6EA0FF).into(),
    success: rgb(0x4CC38A).into(),
    danger: rgb(0xF2676B).into(),
    warning: rgb(0xE8B44C).into(),
    agent: rgb(0xC08CFF).into(),
    kind_def: rgb(0x56C2E6).into(),
    kind_flow: rgb(0x9AA7FF).into(),
    kind_effect: rgb(0xF0A35E).into(),
    kind_data: rgb(0xC3A6FF).into(),
    kind_error: rgb(0xF47FA0).into(),
    kind_param: rgb(0xA9AEB7).into(),
    ty_scalar: rgb(0x7FD8B4).into(),
    ty_text: rgb(0xF59EC4).into(),
    ty_record: rgb(0x7DB3FF).into(),
    ty_enum: rgb(0xC3A6FF).into(),
    ty_capability: rgb(0xF0A35E).into(),
    ty_unit: rgb(0x8A909A).into(),
    edge_data: rgb(0x8E96A3).into(),
    edge_calls: rgb(0x6B717C).into(),
    }
}

pub fn light() -> WhTokens {
    WhTokens {
    canvas: rgb(0xF6F7F9).into(),
    raised: rgb(0xFFFFFF).into(),
    overlay: rgb(0xFFFFFF).into(),
    text_tertiary: rgb(0x656C77).into(),
    accent: rgb(0x2F6BE4).into(),
    success: rgb(0x1E7F4E).into(),
    danger: rgb(0xC8323A).into(),
    warning: rgb(0x8A5A00).into(),
    agent: rgb(0x7C3AED).into(),
    kind_def: rgb(0x0E7490).into(),
    kind_flow: rgb(0x4F46E5).into(),
    kind_effect: rgb(0xB4530F).into(),
    kind_data: rgb(0x7C3AED).into(),
    kind_error: rgb(0xBE185D).into(),
    kind_param: rgb(0x4E5560).into(),
    ty_scalar: rgb(0x0F7A55).into(),
    ty_text: rgb(0xB4236E).into(),
    ty_record: rgb(0x2563EB).into(),
    ty_enum: rgb(0x7C3AED).into(),
    ty_capability: rgb(0xB4530F).into(),
    ty_unit: rgb(0x656C77).into(),
    edge_data: rgb(0x6B7380).into(),
    edge_calls: rgb(0x858C96).into(),
    }
}

/// The token set for the active appearance.
pub fn tokens(cx: &App) -> WhTokens {
    if cx.theme().is_dark() { dark() } else { light() }
}

/// Replaces `canvas::kind_color`: header stripe and glyph colour (header tint = 10% alpha).
pub fn kind(t: &WhTokens, k: GNodeKind) -> Hsla {
    match k {
        GNodeKind::Def | GNodeKind::Step => t.kind_def,
        GNodeKind::Flow | GNodeKind::App => t.kind_flow,
        GNodeKind::Capability | GNodeKind::Adapter => t.kind_effect,
        GNodeKind::Struct | GNodeKind::Enum | GNodeKind::Refined | GNodeKind::Alias => t.kind_data,
        GNodeKind::ErrorEnum => t.kind_error,
        GNodeKind::Param => t.kind_param,
    }
}

/// Edge stroke: (colour, width px, dash pattern in px; empty = solid). Replaces `canvas::edge_color`.
pub fn edge(t: &WhTokens, k: EdgeKind) -> (Hsla, f32, &'static [f32]) {
    match k {
        EdgeKind::Data | EdgeKind::Has => (t.edge_data, 1.75, &[]), // or the source port's type colour
        EdgeKind::Result => (t.kind_flow, 2.25, &[]),
        EdgeKind::Uses | EdgeKind::Provides => (t.kind_effect, 1.25, &[6.0, 4.0]),
        EdgeKind::Implements => (t.kind_effect.opacity(0.7), 1.25, &[1.5, 3.5]),
        EdgeKind::Calls => (t.edge_calls, 1.0, &[1.0, 3.0]),
        EdgeKind::References => (t.ty_record, 1.5, &[6.0, 4.0]),
        EdgeKind::VariantOf => (t.kind_data, 1.5, &[]),
        EdgeKind::ConvertsFrom => (t.danger, 1.5, &[5.0, 4.0]),
        EdgeKind::Refines | EdgeKind::AliasOf => (t.kind_data.opacity(0.7), 1.25, &[1.5, 3.5]),
        EdgeKind::Produces | EdgeKind::Consumes => (t.edge_calls, 1.0, &[4.0, 4.0]),
    }
}

/// Header tint for a node of kind `k`.
pub fn header_tint(t: &WhTokens, k: GNodeKind) -> Hsla {
    kind(t, k).opacity(0.10)
}

