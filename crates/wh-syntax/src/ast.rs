//! Abstract syntax of a `.wh.md` document. Every node carries the exact span
//! of the source it came from, so edits can be computed as minimal text
//! replacements while the source text stays the lossless representation.

use serde::Serialize;

use crate::span::Span;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Ident {
    pub name: String,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Document {
    pub front_matter: Option<FrontMatter>,
    pub sections: Vec<Section>,
    pub prose: Vec<Prose>,
    pub blocks: Vec<WhBlock>,
    pub refs: Vec<DocRef>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FrontMatter {
    pub module: Option<String>,
    pub raw: String,
    pub span: Span,
}

/// A Markdown heading and the region it governs (up to the next heading of
/// the same or higher level).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Section {
    pub level: u8,
    pub title: String,
    pub heading_span: Span,
    pub span: Span,
    pub parent: Option<usize>,
}

/// A run of Markdown between two `wh` blocks or headings.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Prose {
    pub text: String,
    pub span: Span,
}

/// A `[[name]]` reference in prose.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DocRef {
    pub target: String,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WhBlock {
    /// Whole fenced block including the fences.
    pub span: Span,
    /// The code between the fences.
    pub content_span: Span,
    pub items: Vec<Item>,
    /// Innermost section containing this block.
    pub section: Option<usize>,
    /// Prose immediately preceding the block, attached to its first item.
    pub prose: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Decorator {
    pub name: Ident,
    pub args: Vec<Arg>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Item {
    pub decorators: Vec<Decorator>,
    pub kind: ItemKind,
    pub span: Span,
}

impl Item {
    pub fn name(&self) -> Option<&Ident> {
        match &self.kind {
            ItemKind::Alias(a) => Some(&a.name),
            ItemKind::TypeDef(t) => Some(&t.name),
            ItemKind::Struct(s) => Some(&s.name),
            ItemKind::Enum(e) => Some(&e.name),
            ItemKind::Capability(c) => Some(&c.name),
            ItemKind::Adapter(a) => Some(&a.name),
            ItemKind::Def(d) => Some(&d.sig.name),
            ItemKind::Flow(f) => Some(&f.sig.name),
            ItemKind::Machine(m) => Some(&m.name),
            ItemKind::App(a) => Some(&a.name),
            ItemKind::Import(_) | ItemKind::Depends(_) | ItemKind::Test(_) | ItemKind::Error => None,
        }
    }

    pub fn docstring(&self) -> Option<&str> {
        match &self.kind {
            ItemKind::Struct(s) => s.doc.as_deref(),
            ItemKind::Def(d) => d.doc.as_deref(),
            ItemKind::Flow(f) => f.doc.as_deref(),
            ItemKind::Adapter(a) => a.doc.as_deref(),
            ItemKind::Capability(c) => c.doc.as_deref(),
            ItemKind::Machine(m) => m.doc.as_deref(),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum ItemKind {
    Import(Import),
    Depends(Depends),
    Alias(Alias),
    TypeDef(TypeDef),
    Struct(Struct),
    Enum(Enum),
    Capability(Capability),
    Adapter(Adapter),
    Def(Def),
    Flow(Flow),
    Machine(Machine),
    App(App),
    Test(Test),
    /// An item that failed to parse; diagnostics explain why.
    Error,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum Import {
    /// `from a.b import X, y`
    From { module: Vec<Ident>, names: Vec<Ident> },
    /// `import a.b as c`
    Module { module: Vec<Ident>, alias: Option<Ident> },
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Depends {
    pub crate_name: Ident,
    pub version: String,
    pub features: Vec<String>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Generic {
    pub name: Ident,
    pub bounds: Vec<Ident>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Alias {
    pub name: Ident,
    pub generics: Vec<Generic>,
    pub ty: TypeExpr,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TypeDef {
    pub name: Ident,
    pub generics: Vec<Generic>,
    pub base: TypeExpr,
    pub refinement: Option<Expr>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Field {
    pub is_key: bool,
    /// `sensitive`: redacted from `Debug` output and traces.
    pub is_sensitive: bool,
    pub name: Ident,
    pub ty: TypeExpr,
    pub default: Option<Expr>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Struct {
    pub name: Ident,
    pub generics: Vec<Generic>,
    pub doc: Option<String>,
    pub fields: Vec<Field>,
    pub invariants: Vec<Expr>,
    /// Span of the indented body (for inserting fields).
    pub body_span: Span,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct VariantItem {
    pub is_from: bool,
    pub name: Option<Ident>,
    pub ty: TypeExpr,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Variant {
    pub name: Ident,
    pub items: Vec<VariantItem>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Enum {
    pub name: Ident,
    pub generics: Vec<Generic>,
    pub variants: Vec<Variant>,
    pub body_span: Span,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Capability {
    pub name: Ident,
    pub doc: Option<String>,
    pub generics: Vec<Generic>,
    pub methods: Vec<Def>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Adapter {
    pub name: Ident,
    pub doc: Option<String>,
    pub implements: Vec<TypeExpr>,
    pub fields: Vec<Field>,
    pub clauses: Vec<Clause>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum ParamMode {
    Borrow,
    Own,
    Mut,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Param {
    pub mode: ParamMode,
    /// `sensitive`: the argument is redacted in traces.
    pub is_sensitive: bool,
    pub name: Ident,
    pub ty: TypeExpr,
    pub default: Option<Expr>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Signature {
    pub name: Ident,
    pub generics: Vec<Generic>,
    pub params: Vec<Param>,
    pub ret: Option<TypeExpr>,
    pub raises: Option<TypeExpr>,
    pub uses: Vec<Ident>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Def {
    pub is_async: bool,
    pub extern_path: Option<String>,
    pub sig: Signature,
    pub doc: Option<String>,
    pub clauses: Vec<Clause>,
    /// Span of the indented body, if any (for inserting clauses).
    pub body_span: Option<Span>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Clause {
    pub kind: ClauseKind,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum AllocSpec {
    None,
    Bounded(Expr),
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum ClauseKind {
    Pure,
    Requires(Expr),
    Ensures(Expr),
    RaisesWhen { error: TypeExpr, when: Expr },
    Example { expr: Expr, raises: Option<TypeExpr> },
    Property { name: Ident, params: Vec<Param>, given: Vec<(Ident, Expr)>, body: Expr },
    Bench { name: Ident, params: Vec<Param>, body: Expr },
    Mutate(Expr),
    Complexity(Expr),
    Pattern(TypeExpr),
    Note(String),
    Optimize(Ident),
    Budget { time: Expr, var: Ident, size: Expr },
    Alloc(AllocSpec),
    Unsafe { allow: bool },
    NoPanic,
    /// `trace off|error|warn|info|debug|trace`
    Trace(Ident),
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Flow {
    pub sig: Signature,
    pub doc: Option<String>,
    /// `trace <level>` at the top of the body.
    pub trace: Option<Ident>,
    pub body: Vec<FlowStmt>,
    pub body_span: Span,
}

/// `head |> step |> step`
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FlowExpr {
    pub head: Expr,
    pub steps: Vec<Expr>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MatchArm {
    pub pattern: Expr,
    pub body: Vec<FlowStmt>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum FlowStmtKind {
    Bind { targets: Vec<Ident>, value: FlowExpr },
    Expr(FlowExpr),
    Return(Option<FlowExpr>),
    Match { scrutinee: Expr, arms: Vec<MatchArm> },
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FlowStmt {
    pub kind: FlowStmtKind,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Transition {
    pub from: Expr,
    pub event: Expr,
    pub guard: Option<Expr>,
    pub to: Expr,
    pub emit: Option<Expr>,
    pub span: Span,
}

/// `ignore State on Event`: the pair is deliberately left alone (the state
/// stays, nothing is emitted). `_` matches any state or event.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct IgnoreRule {
    pub state: Expr,
    pub event: Expr,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Machine {
    pub name: Ident,
    pub param: Param,
    pub ret: Option<TypeExpr>,
    pub transitions: Vec<Transition>,
    pub initial: Option<Expr>,
    pub ignores: Vec<IgnoreRule>,
    pub doc: Option<String>,
    /// `trace <level>` in the body: level of the per-transition events.
    pub trace: Option<Ident>,
    /// Span of the indented body (for inserting transitions).
    pub body_span: Span,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Provide {
    pub capability: Ident,
    pub value: Expr,
    pub span: Span,
}

/// `route POST "/users/{id}" -> find_user` in a `server` app.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Route {
    pub method: Ident,
    pub path: String,
    pub path_span: Span,
    /// The flow or def handling the route (`register`, `signup.register`).
    pub target: Expr,
    pub span: Span,
}

/// `status RegisterError.TooYoung = 422`: the HTTP status of an error.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StatusRule {
    pub error: TypeExpr,
    pub code: Expr,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct App {
    pub name: Ident,
    pub targets: Vec<Expr>,
    /// `log level="info" format="pretty"`
    pub log: Vec<(Ident, Expr)>,
    pub depends: Vec<Depends>,
    pub provides: Vec<Provide>,
    pub entry: Option<Flow>,
    /// `server` apps: routes, error statuses and the default address.
    pub routes: Vec<Route>,
    pub statuses: Vec<StatusRule>,
    pub listen: Option<Expr>,
    /// `web` apps: `export normalize_email` (flows or defs).
    pub exports: Vec<Expr>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Expect {
    pub expr: Expr,
    pub raises: Option<TypeExpr>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Test {
    pub name: String,
    pub name_span: Span,
    pub givens: Vec<Provide>,
    pub expects: Vec<Expect>,
}

// ---------------------------------------------------------------- types

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TypeExpr {
    pub kind: TypeExprKind,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum TypeExprKind {
    /// `Name` or `Name[args]`; dotted for imported modules (`billing.Invoice`).
    Named { path: Vec<Ident>, args: Vec<TypeArg> },
    /// `T | None`
    Optional(Box<TypeExpr>),
    /// `(A, B) -> R`
    Fn { params: Vec<TypeExpr>, ret: Box<TypeExpr> },
    Error,
}

impl std::fmt::Display for TypeExpr {
    /// The type as written, with whitespace normalized (`UserId | None`).
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.kind {
            TypeExprKind::Named { path, args } => {
                let p: Vec<&str> = path.iter().map(|i| i.name.as_str()).collect();
                write!(f, "{}", p.join("."))?;
                if !args.is_empty() {
                    write!(f, "[")?;
                    for (i, a) in args.iter().enumerate() {
                        if i > 0 {
                            write!(f, ", ")?;
                        }
                        match a {
                            TypeArg::Type(t) => write!(f, "{t}")?,
                            TypeArg::Const(e) => match &e.kind {
                                ExprKind::Int(v) | ExprKind::Name(v) => write!(f, "{v}")?,
                                _ => write!(f, "_")?,
                            },
                        }
                    }
                    write!(f, "]")?;
                }
                Ok(())
            }
            TypeExprKind::Optional(t) => write!(f, "{t} | None"),
            TypeExprKind::Fn { params, ret } => {
                write!(f, "(")?;
                for (i, p) in params.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{p}")?;
                }
                write!(f, ") -> {ret}")
            }
            TypeExprKind::Error => write!(f, "?"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum TypeArg {
    Type(TypeExpr),
    /// constant argument, e.g. `array[u8, 32]`
    Const(Expr),
}

// ---------------------------------------------------------------- expressions

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    FloorDiv,
    Mod,
    Pow,
    And,
    Or,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum CmpOp {
    Eq,
    NotEq,
    Lt,
    Le,
    Gt,
    Ge,
    In,
    NotIn,
    Is,
    IsNot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum UnaryOp {
    Neg,
    Not,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Quantifier {
    Forall,
    Exists,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Arg {
    pub name: Option<Ident>,
    pub value: Expr,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum StrKind {
    Normal,
    Raw,
    Bytes,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum ExprKind {
    Name(String),
    Int(String),
    Float(String),
    Str { value: String, kind: StrKind },
    Bool(bool),
    None,
    Duration { value: String, unit: String },
    Attr { base: Box<Expr>, attr: Ident },
    Call { func: Box<Expr>, args: Vec<Arg> },
    Index { base: Box<Expr>, index: Box<Expr> },
    Slice { base: Box<Expr>, lo: Option<Box<Expr>>, hi: Option<Box<Expr>> },
    Unary { op: UnaryOp, expr: Box<Expr> },
    Binary { op: BinOp, lhs: Box<Expr>, rhs: Box<Expr> },
    Compare { first: Box<Expr>, rest: Vec<(CmpOp, Expr)> },
    IfElse { then: Box<Expr>, cond: Box<Expr>, otherwise: Box<Expr> },
    Lambda { params: Vec<Ident>, body: Box<Expr> },
    List(Vec<Expr>),
    Tuple(Vec<Expr>),
    /// `[elt for var in iter if cond]` (list) or `elt for var in iter` (generator in a call).
    Comprehension { elt: Box<Expr>, var: Ident, iter: Box<Expr>, cond: Option<Box<Expr>>, generator: bool },
    Quantified { q: Quantifier, var: Ident, iter: Box<Expr>, body: Box<Expr> },
    Error,
}

impl Expr {
    pub fn error(span: Span) -> Self {
        Expr { kind: ExprKind::Error, span }
    }

    /// The callee name if this is `name` or `name(...)`.
    pub fn callee_name(&self) -> Option<&str> {
        match &self.kind {
            ExprKind::Name(n) => Some(n),
            ExprKind::Call { func, .. } => func.callee_name(),
            _ => None,
        }
    }
}
