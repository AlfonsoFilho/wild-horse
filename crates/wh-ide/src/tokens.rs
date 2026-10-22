//! Wild Horse colour roles (UX review §15.3) that gpui-kit's `ThemeColor` has
//! no slot for: canvas surfaces, node kinds, port type families, edge kinds,
//! status and AI provenance, each with a dark and a light value.
//!
//! Chrome colours (panel backgrounds, borders, buttons, tabs) come from the
//! gpui-kit theme loaded from `themes/wild-horse.json` (see `theme.rs`); views
//! read those through `cx.theme()` and use this module for the roles below.
//! This is the only file in the crate that writes colour literals.

use std::sync::LazyLock;

use gpui_kit::component::ActiveTheme;
use gpui_kit::{App, Hsla, rgb};
use wh_graph::{EdgeKind, GNodeKind, TypeFamily};

/// The full role table of §15.3; a few roles (input borders, button fill,
/// text selection) are applied through the gpui-kit theme instead.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy)]
pub struct Tokens {
    pub canvas: Hsla,
    pub panel: Hsla,
    pub sunken: Hsla,
    pub raised: Hsla,
    pub overlay: Hsla,
    pub hover: Hsla,
    pub border_subtle: Hsla,
    pub border_strong: Hsla,
    pub border_input: Hsla,
    pub text_primary: Hsla,
    pub text_secondary: Hsla,
    pub text_tertiary: Hsla,
    pub accent: Hsla,
    pub accent_button: Hsla,
    pub selection_text: Hsla,
    pub success: Hsla,
    pub danger: Hsla,
    pub warning: Hsla,
    pub info: Hsla,
    pub muted: Hsla,
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
    pub ty_error: Hsla,
    pub ty_unit: Hsla,
    pub edge_data: Hsla,
    pub edge_calls: Hsla,
    /// Section frame fill: 2.5% white (dark) or black (light).
    pub frame: Hsla,
    pub dark: bool,
}

fn c(hex: u32) -> Hsla {
    rgb(hex).into()
}

static DARK: LazyLock<Tokens> = LazyLock::new(|| Tokens {
    canvas: c(0x0F1012),
    panel: c(0x16171A),
    sunken: c(0x121316),
    raised: c(0x1D1F23),
    overlay: c(0x24272C),
    hover: c(0x23262B),
    border_subtle: c(0x2A2D33),
    border_strong: c(0x3A3E46),
    border_input: c(0x666C76),
    text_primary: c(0xE6E8EB),
    text_secondary: c(0xA9AEB7),
    text_tertiary: c(0x8A909A),
    accent: c(0x6EA0FF),
    accent_button: c(0x3D6FE0),
    selection_text: c(0x2B3F66),
    success: c(0x4CC38A),
    danger: c(0xF2676B),
    warning: c(0xE8B44C),
    info: c(0x6EA0FF),
    muted: c(0x8A909A),
    agent: c(0xC08CFF),
    kind_def: c(0x56C2E6),
    kind_flow: c(0x9AA7FF),
    kind_effect: c(0xF0A35E),
    kind_data: c(0xC3A6FF),
    kind_error: c(0xF47FA0),
    kind_param: c(0xA9AEB7),
    ty_scalar: c(0x7FD8B4),
    ty_text: c(0xF59EC4),
    ty_record: c(0x7DB3FF),
    ty_enum: c(0xC3A6FF),
    ty_capability: c(0xF0A35E),
    ty_error: c(0xF2676B),
    ty_unit: c(0x8A909A),
    edge_data: c(0x8E96A3),
    edge_calls: c(0x6B717C),
    frame: c(0xFFFFFF).opacity(0.025),
    dark: true,
});

static LIGHT: LazyLock<Tokens> = LazyLock::new(|| Tokens {
    canvas: c(0xF6F7F9),
    panel: c(0xFFFFFF),
    sunken: c(0xF3F4F6),
    raised: c(0xFFFFFF),
    overlay: c(0xFFFFFF),
    hover: c(0xEEF0F3),
    border_subtle: c(0xE3E5E9),
    border_strong: c(0xC9CDD4),
    border_input: c(0x868D97),
    text_primary: c(0x1C1F24),
    text_secondary: c(0x4E5560),
    text_tertiary: c(0x656C77),
    accent: c(0x2F6BE4),
    accent_button: c(0x2F6BE4),
    selection_text: c(0xD6E4FF),
    success: c(0x1E7F4E),
    danger: c(0xC8323A),
    warning: c(0x8A5A00),
    info: c(0x2F6BE4),
    muted: c(0x656C77),
    agent: c(0x7C3AED),
    kind_def: c(0x0E7490),
    kind_flow: c(0x4F46E5),
    kind_effect: c(0xB4530F),
    kind_data: c(0x7C3AED),
    kind_error: c(0xBE185D),
    kind_param: c(0x4E5560),
    ty_scalar: c(0x0F7A55),
    ty_text: c(0xB4236E),
    ty_record: c(0x2563EB),
    ty_enum: c(0x7C3AED),
    ty_capability: c(0xB4530F),
    ty_error: c(0xC8323A),
    ty_unit: c(0x656C77),
    edge_data: c(0x6B7380),
    edge_calls: c(0x858C96),
    frame: c(0x000000).opacity(0.025),
    dark: false,
});

/// The token set for the active appearance.
pub fn tokens(cx: &App) -> &'static Tokens {
    if cx.theme().is_dark() { &DARK } else { &LIGHT }
}

impl Tokens {
    /// Kind colour: header stripe, glyph and (at 10%) header tint.
    pub fn kind(&self, k: GNodeKind) -> Hsla {
        match k {
            GNodeKind::Def | GNodeKind::Step => self.kind_def,
            GNodeKind::Flow | GNodeKind::App => self.kind_flow,
            GNodeKind::Capability | GNodeKind::Adapter => self.kind_effect,
            GNodeKind::Struct | GNodeKind::Enum | GNodeKind::Refined | GNodeKind::Alias => self.kind_data,
            GNodeKind::ErrorEnum => self.kind_error,
            GNodeKind::Param => self.kind_param,
        }
    }

    /// Port fill and data-wire colour for a type family.
    pub fn family(&self, f: TypeFamily) -> Hsla {
        match f {
            TypeFamily::Scalar => self.ty_scalar,
            TypeFamily::Text => self.ty_text,
            TypeFamily::Record => self.ty_record,
            TypeFamily::Enum => self.ty_enum,
            TypeFamily::Capability => self.ty_capability,
            TypeFamily::Error => self.ty_error,
            TypeFamily::Unit | TypeFamily::Other => self.ty_unit,
        }
    }

    /// Edge stroke: (colour, width in px, dash pattern in px; empty = solid).
    pub fn edge(&self, k: EdgeKind) -> (Hsla, f32, &'static [f32]) {
        match k {
            EdgeKind::Data => (self.edge_data, 1.75, &[]),
            EdgeKind::Has => (self.text_secondary, 1.5, &[]),
            EdgeKind::Result => (self.kind_flow, 2.25, &[]),
            EdgeKind::Uses | EdgeKind::Provides => (self.kind_effect, 1.25, &[6.0, 4.0]),
            EdgeKind::Implements => (self.kind_effect.opacity(0.7), 1.25, &[1.5, 3.5]),
            EdgeKind::Calls => (self.edge_calls, 1.0, &[1.0, 3.0]),
            EdgeKind::References => (self.ty_record, 1.5, &[6.0, 4.0]),
            EdgeKind::VariantOf => (self.kind_data, 1.5, &[]),
            EdgeKind::ConvertsFrom => (self.danger, 1.5, &[5.0, 4.0]),
            EdgeKind::Refines | EdgeKind::AliasOf => (self.kind_data.opacity(0.7), 1.25, &[1.5, 3.5]),
            EdgeKind::Produces | EdgeKind::Consumes => (self.edge_calls, 1.0, &[4.0, 4.0]),
        }
    }
}

/// Status chip roles (§9 A1): one map used by the canvas, the document gutter,
/// the inspector and the status bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Muted,
    Info,
    Success,
    Warning,
    Danger,
}

impl Tone {
    pub fn color(self, t: &Tokens) -> Hsla {
        match self {
            Tone::Muted => t.muted,
            Tone::Info => t.info,
            Tone::Success => t.success,
            Tone::Warning => t.warning,
            Tone::Danger => t.danger,
        }
    }
}
