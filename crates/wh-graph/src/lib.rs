//! Graph projections of a Wild Horse [`Model`](wh_model::Model): the flow
//! graph and the data-model graph, a layered (Sugiyama-style) auto-layout,
//! and the `.wh/layout.json` sidecar that persists hand-placed positions.
//!
//! Both projections share one shape: nodes made of a header plus rows, where
//! a row may carry an input port (left) and/or an output port (right), and
//! edges between ports (or node headers). The IDE's canvas renders any
//! [`Graph`] with the metrics below, so the layout and the drawing agree.

pub mod data;
pub mod flow;
pub mod layout;
pub mod store;

use serde::Serialize;
use wh_model::{NodeId, Ty};
use wh_syntax::Span;

pub use data::{DataOptions, data_model_graph};
pub use flow::{FlowOptions, flow_graph};
pub use layout::{Point, layout, place};
pub use store::LayoutStore;

/// Canvas metrics shared by layout and rendering (logical pixels).
pub const HEADER_H: f32 = 30.0;
pub const ROW_H: f32 = 20.0;
pub const CHAR_W: f32 = 7.2;
pub const NODE_PAD: f32 = 12.0;
pub const MIN_W: f32 = 120.0;
pub const MAX_W: f32 = 340.0;
/// Padding around a section cluster's members, and the band above them for its title.
pub const CLUSTER_PAD: f32 = 10.0;
pub const CLUSTER_TITLE_H: f32 = 22.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub enum GNodeKind {
    // flow graph
    Def,
    Flow,
    App,
    Step,
    Param,
    Capability,
    Adapter,
    // data model
    Struct,
    Enum,
    ErrorEnum,
    Refined,
    Alias,
}

impl GNodeKind {
    pub fn label(self) -> &'static str {
        match self {
            GNodeKind::Def => "def",
            GNodeKind::Flow => "flow",
            GNodeKind::App => "app",
            GNodeKind::Step => "step",
            GNodeKind::Param => "param",
            GNodeKind::Capability => "capability",
            GNodeKind::Adapter => "adapter",
            GNodeKind::Struct => "struct",
            GNodeKind::Enum => "enum",
            GNodeKind::ErrorEnum => "@error enum",
            GNodeKind::Refined => "type",
            GNodeKind::Alias => "alias",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum PortDir {
    In,
    Out,
}

/// Colour family of a port's type (UX review §15.3 `type.*` tokens).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize)]
pub enum TypeFamily {
    /// bool, numbers, durations, refined numbers
    Scalar,
    /// str, char, bytes, refined strings
    Text,
    /// structs and tuples
    Record,
    Enum,
    Capability,
    /// `@error` enums
    Error,
    Unit,
    #[default]
    Other,
}

/// Port shape by structure (UX review §15.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize)]
pub enum PortShape {
    /// `T`
    #[default]
    Single,
    /// `T | None`
    Optional,
    /// list, set, dict, array
    Collection,
    /// `Stream[T]`
    Stream,
    /// unit / None
    Hollow,
}

/// The family and shape for a type.
pub fn type_style(model: &wh_model::Model, ty: &Ty) -> (TypeFamily, PortShape) {
    let shape = match ty {
        Ty::Unit => PortShape::Hollow,
        Ty::Option(_) => PortShape::Optional,
        Ty::List(_) | Ty::Set(_) | Ty::SortedSet(_) | Ty::Dict(..) | Ty::SortedDict(..) | Ty::Array(..) => PortShape::Collection,
        Ty::Stream(_) => PortShape::Stream,
        _ => PortShape::Single,
    };
    (type_family(model, ty, 0), shape)
}

fn type_family(model: &wh_model::Model, ty: &Ty, depth: usize) -> TypeFamily {
    use wh_model::NodeKind;
    if depth > 8 {
        return TypeFamily::Other;
    }
    match ty {
        Ty::Bool | Ty::Int(_) | Ty::Float32 | Ty::Float64 | Ty::Duration | Ty::Instant => TypeFamily::Scalar,
        Ty::Str | Ty::Char | Ty::Bytes => TypeFamily::Text,
        Ty::Unit => TypeFamily::Unit,
        Ty::Tuple(_) => TypeFamily::Record,
        Ty::List(t) | Ty::Set(t) | Ty::SortedSet(t) | Ty::Array(t, _) | Ty::Option(t) | Ty::Stream(t) => type_family(model, t, depth + 1),
        Ty::Dict(_, v) | Ty::SortedDict(_, v) => type_family(model, v, depth + 1),
        Ty::Named(id, _) => match model.nodes.get(id).map(|n| (&n.kind, n.is_error_type)) {
            Some((NodeKind::Struct(_), _)) => TypeFamily::Record,
            Some((NodeKind::Enum(_), true)) => TypeFamily::Error,
            Some((NodeKind::Enum(_) | NodeKind::Machine(_), _)) => TypeFamily::Enum,
            Some((NodeKind::Refined { base, .. }, _)) => type_family(model, base, depth + 1),
            Some((NodeKind::Alias(t), _)) => type_family(model, t, depth + 1),
            Some((NodeKind::Capability(_) | NodeKind::Adapter(_), _)) => TypeFamily::Capability,
            _ => TypeFamily::Other,
        },
        Ty::Fn(..) | Ty::Param(_) | Ty::Unknown => TypeFamily::Other,
    }
}

/// Set every typed row's family and shape from the model.
pub(crate) fn annotate(model: &wh_model::Model, g: &mut Graph) {
    for n in &mut g.nodes {
        for r in &mut n.rows {
            if let Some(ty) = &r.ty {
                (r.family, r.shape) = type_style(model, ty);
            }
        }
    }
}

/// One line of a node body; it may be a port.
#[derive(Debug, Clone, Serialize)]
pub struct Row {
    pub label: String,
    /// Type or extra text shown after the label.
    pub detail: String,
    pub port: Option<PortDir>,
    /// Port type, used to grey out incompatible ports while connecting.
    #[serde(skip)]
    pub ty: Option<Ty>,
    /// For data-model field rows: the field name (for field ops).
    pub field: Option<String>,
    pub family: TypeFamily,
    pub shape: PortShape,
}

impl Row {
    pub fn text(label: impl Into<String>, detail: impl Into<String>) -> Self {
        Row { label: label.into(), detail: detail.into(), port: None, ty: None, field: None, family: TypeFamily::Other, shape: PortShape::Single }
    }

    pub fn port(dir: PortDir, label: impl Into<String>, ty: &Ty) -> Self {
        Row { label: label.into(), detail: ty.to_string(), port: Some(dir), ty: Some(ty.clone()), field: None, family: TypeFamily::Other, shape: PortShape::Single }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct GNode {
    /// Graph-unique id: the model `NodeId` for definitions, the `FlowStep.id`
    /// for steps, `<flow>#in:<param>` for flow parameters.
    pub id: String,
    pub kind: GNodeKind,
    pub title: String,
    pub subtitle: String,
    pub rows: Vec<Row>,
    /// Whether edges may end on the header (data-model types, capabilities, defs).
    pub header_in: bool,
    pub cluster: Option<usize>,
    /// The model node this graph node shows or belongs to (a step's flow).
    pub model_node: Option<NodeId>,
    /// For steps: the callee definition.
    pub callee: Option<NodeId>,
    /// Source location to reveal when selected.
    pub file: usize,
    pub span: Span,
    pub badges: Vec<String>,
    /// The error type the node can raise (drawn as a `raises` port).
    pub raises: Option<String>,
    /// A problem shown on the node itself, e.g. a step whose callee does not resolve.
    pub error: Option<String>,
}

impl GNode {
    pub fn size(&self) -> (f32, f32) {
        let mut chars = self.title.chars().count() + 2 + self.subtitle.chars().count();
        for r in &self.rows {
            chars = chars.max(r.label.chars().count() + r.detail.chars().count() + 3);
        }
        let w = (chars as f32 * CHAR_W + 2.0 * NODE_PAD + 16.0).clamp(MIN_W, MAX_W);
        (w, HEADER_H + self.rows.len() as f32 * ROW_H + 6.0)
    }
}

/// An edge endpoint: a node's row port, or its header (`row: None`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub struct PortRef {
    pub node: usize,
    pub row: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub enum EdgeKind {
    // flow graph
    /// A value flowing from a parameter or step output into a step input.
    Data,
    /// The step whose value the flow returns.
    Result,
    /// A def/flow uses a capability.
    Uses,
    /// An adapter implements a capability.
    Implements,
    /// An app provides a capability with an adapter.
    Provides,
    /// A step instance calls this definition.
    Calls,
    // data model
    Has,
    VariantOf,
    Refines,
    AliasOf,
    ConvertsFrom,
    References,
    Produces,
    Consumes,
}

impl EdgeKind {
    pub fn label(self) -> &'static str {
        match self {
            EdgeKind::Data => "data",
            EdgeKind::Result => "result",
            EdgeKind::Uses => "uses",
            EdgeKind::Implements => "implements",
            EdgeKind::Provides => "provides",
            EdgeKind::Calls => "calls",
            EdgeKind::Has => "has",
            EdgeKind::VariantOf => "variant of",
            EdgeKind::Refines => "refines",
            EdgeKind::AliasOf => "alias of",
            EdgeKind::ConvertsFrom => "converts from",
            EdgeKind::References => "references",
            EdgeKind::Produces => "produces",
            EdgeKind::Consumes => "consumes",
        }
    }

    /// Edges that only annotate (drawn faint, ignored for layering).
    pub fn is_secondary(self) -> bool {
        matches!(self, EdgeKind::Calls | EdgeKind::Produces | EdgeKind::Consumes)
    }
}

/// Relationship multiplicity, derived from a field's type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum Cardinality {
    /// `T`
    One,
    /// `T | None`
    ZeroOrOne,
    /// `list[T]`, `set[T]`
    Many,
    /// `NonEmpty[T]`
    OneOrMore,
    /// `dict[K, V]`: keyed by `K`
    Keyed(String),
    /// `array[T, N]`
    Exactly(u64),
}

impl std::fmt::Display for Cardinality {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Cardinality::One => write!(f, "1"),
            Cardinality::ZeroOrOne => write!(f, "0..1"),
            Cardinality::Many => write!(f, "0..*"),
            Cardinality::OneOrMore => write!(f, "1..*"),
            Cardinality::Keyed(k) => write!(f, "[{k}] 0..*"),
            Cardinality::Exactly(n) => write!(f, "{n}"),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct GEdge {
    pub from: PortRef,
    pub to: PortRef,
    pub kind: EdgeKind,
    pub label: Option<String>,
    pub cardinality: Option<Cardinality>,
    /// Data model: the struct field (owner id, field name) this edge comes from,
    /// so the IDE can rewrite it (`SetCardinality`, `RetypeField`, …).
    pub field: Option<(NodeId, String)>,
    /// Flow graph: `(flow id, step index)` of the target step, for step ops.
    pub step: Option<(NodeId, usize)>,
}

/// A document section shown as a group of nodes.
#[derive(Debug, Clone, Serialize)]
pub struct Cluster {
    pub file: usize,
    pub section: usize,
    pub title: String,
    pub parent: Option<usize>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Graph {
    pub nodes: Vec<GNode>,
    pub edges: Vec<GEdge>,
    pub clusters: Vec<Cluster>,
}

impl Graph {
    pub fn index_of(&self, id: &str) -> Option<usize> {
        self.nodes.iter().position(|n| n.id == id)
    }

    pub fn node(&self, id: &str) -> Option<&GNode> {
        self.nodes.iter().find(|n| n.id == id)
    }

    pub fn edges_of_kind(&self, kind: EdgeKind) -> impl Iterator<Item = &GEdge> {
        self.edges.iter().filter(move |e| e.kind == kind)
    }

    pub(crate) fn push(&mut self, n: GNode) -> usize {
        self.nodes.push(n);
        self.nodes.len() - 1
    }

    pub(crate) fn edge(&mut self, from: PortRef, to: PortRef, kind: EdgeKind) -> &mut GEdge {
        self.edges.push(GEdge { from, to, kind, label: None, cardinality: None, field: None, step: None });
        self.edges.last_mut().unwrap()
    }

    /// Cluster index for a model section, creating it (and its parents) on demand.
    pub(crate) fn cluster_for(&mut self, model: &wh_model::Model, file: usize, section: Option<usize>) -> Option<usize> {
        let section = section?;
        if let Some(i) = self.clusters.iter().position(|c| c.file == file && c.section == section) {
            return Some(i);
        }
        let sec = model.files.get(file)?.doc.sections.get(section)?;
        let parent = self.cluster_for(model, file, sec.parent);
        self.clusters.push(Cluster { file, section, title: sec.title.clone(), parent });
        Some(self.clusters.len() - 1)
    }
}

/// Can a value of type `value` flow into a port of type `port`?
pub fn port_accepts(model: &wh_model::Model, port: &Ty, value: &Ty) -> bool {
    wh_model::assignable(port, value) || wh_model::compatible(model, port, value)
}

#[cfg(test)]
pub(crate) mod testutil {
    use wh_model::{Model, SourceInput};

    pub fn signup() -> Model {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples/signup/src");
        let mut inputs = Vec::new();
        let mut paths: Vec<_> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().path()).collect();
        paths.sort();
        for p in paths {
            if p.to_string_lossy().ends_with(".wh.md") {
                inputs.push(SourceInput { path: p.to_string_lossy().into_owned(), text: std::fs::read_to_string(&p).unwrap() });
            }
        }
        Model::build(&inputs)
    }
}
