//! Benchmark input generation: proptest strategies that build values of a
//! requested size `n`, emitted as Rust source for the bench crate.
//!
//! The generated crate's property strategies (`wh_codegen::tests::TestGen`)
//! draw collections of up to 8 elements; a size sweep needs exactly `n`, so
//! the bench crate gets its own generator that mirrors them: refined types
//! filter their base through `parse` (regex refinements generate matching
//! strings), structs and enums are built field by field.

use wh_codegen::rust::{Names, module_ident, rust_keyword_safe};
use wh_model::{Model, NodeKind, Ty};
use wh_syntax::ast::{ExprKind, ItemKind};

/// Emits strategy expressions in terms of a size variable `n: usize`.
pub struct InputGen<'m> {
    pub model: &'m Model,
    /// Rust name of the generated library crate (`signup`).
    pub lib: String,
    stack: Vec<String>,
}

/// Does a value of this type have a size (a length) the sweep can vary?
pub fn is_sized(model: &Model, t: &Ty) -> bool {
    match t {
        Ty::Str | Ty::Bytes | Ty::List(_) | Ty::Set(_) | Ty::SortedSet(_) | Ty::Dict(..) | Ty::SortedDict(..) => true,
        Ty::Option(e) => is_sized(model, e),
        Ty::Tuple(ts) => ts.iter().any(|t| is_sized(model, t)),
        Ty::Named(id, _) => match model.nodes.get(id).map(|n| &n.kind) {
            Some(NodeKind::Refined { base, .. }) => {
                // a regex-refined string has the regex's own lengths
                regex_of(model, id).is_none() && is_sized(model, base)
            }
            Some(NodeKind::Struct(s)) => s.fields.iter().any(|f| is_sized(model, &f.ty)),
            _ => false,
        },
        _ => false,
    }
}

/// The regex of a `matches(self, r"...")` refinement.
pub fn regex_of(model: &Model, id: &str) -> Option<String> {
    let node = model.nodes.get(id)?;
    let ItemKind::TypeDef(td) = &node.item.kind else { return None };
    fn find(e: &wh_syntax::ast::Expr) -> Option<String> {
        match &e.kind {
            ExprKind::Call { func, args } if func.callee_name() == Some("matches") && args.len() == 2 => match &args[1].value.kind {
                ExprKind::Str { value, .. } => Some(value.clone()),
                _ => None,
            },
            ExprKind::Binary { op: wh_syntax::ast::BinOp::And, lhs, rhs } => find(lhs).or_else(|| find(rhs)),
            _ => None,
        }
    }
    find(td.refinement.as_ref()?)
}

impl<'m> InputGen<'m> {
    pub fn new(model: &'m Model, lib: &str) -> Self {
        Self { model, lib: lib.to_string(), stack: vec![] }
    }

    /// Fully qualified Rust type as seen from the bench crate.
    pub fn rust_ty(&self, t: &Ty) -> String {
        // an empty module never matches a node's module: every path is qualified
        Names::new(self.model, "").ty(t).replace("crate::", &format!("{}::", self.lib))
    }

    /// Path of a definition (type or function) from the bench crate.
    pub fn path(&self, id: &str) -> String {
        let n = &self.model.nodes[id];
        format!("{}::{}::{}", self.lib, module_ident(&n.module), rust_keyword_safe(&n.name))
    }

    /// A strategy expression for `t`. With `sized`, collections get exactly
    /// `n` elements, strings `n` characters and integers the value `n`;
    /// otherwise values stay small.
    pub fn strategy(&mut self, t: &Ty, sized: bool) -> Result<String, String> {
        let len = if sized { "n" } else { "0..8usize" };
        Ok(match t {
            Ty::Bool => "proptest::prelude::any::<bool>()".into(),
            Ty::Int(i) if sized => format!("proptest::strategy::Just({0}::try_from(n).unwrap_or({0}::MAX))", i.rust()),
            Ty::Int(i) => format!("proptest::prelude::any::<{}>()", i.rust()),
            Ty::Float64 if sized => "proptest::strategy::Just(n as f64)".into(),
            Ty::Float64 => "(-1.0e9f64..1.0e9f64)".into(),
            Ty::Float32 if sized => "proptest::strategy::Just(n as f32)".into(),
            Ty::Float32 => "(-1.0e6f32..1.0e6f32)".into(),
            Ty::Str if sized => {
                "proptest::collection::vec(proptest::char::range('a', 'z'), n).prop_map(|cs| cs.into_iter().collect::<String>())".into()
            }
            Ty::Str => "\"[a-zA-Z0-9 @._-]{0,16}\"".into(),
            Ty::Char => "proptest::prelude::any::<char>()".into(),
            Ty::Bytes => format!("proptest::collection::vec(proptest::prelude::any::<u8>(), {len})"),
            Ty::Unit => "proptest::strategy::Just(())".into(),
            Ty::Duration => "(0u64..1000).prop_map(std::time::Duration::from_millis)".into(),
            Ty::List(e) => format!("proptest::collection::vec({}, {len})", self.strategy(e, false)?),
            Ty::Set(e) => format!(
                "proptest::collection::vec({}, {len}).prop_map(|v| v.into_iter().collect::<std::collections::HashSet<_>>())",
                self.strategy(e, false)?
            ),
            Ty::SortedSet(e) => format!(
                "proptest::collection::vec({}, {len}).prop_map(|v| v.into_iter().collect::<std::collections::BTreeSet<_>>())",
                self.strategy(e, false)?
            ),
            Ty::Dict(k, v) => format!(
                "proptest::collection::vec(({}, {}), {len}).prop_map(|v| v.into_iter().collect::<std::collections::HashMap<_, _>>())",
                self.strategy(k, false)?,
                self.strategy(v, false)?
            ),
            Ty::SortedDict(k, v) => format!(
                "proptest::collection::vec(({}, {}), {len}).prop_map(|v| v.into_iter().collect::<std::collections::BTreeMap<_, _>>())",
                self.strategy(k, false)?,
                self.strategy(v, false)?
            ),
            Ty::Option(e) if sized => format!("{}.prop_map(Some)", self.strategy(e, true)?),
            Ty::Option(e) => format!("proptest::option::of({})", self.strategy(e, false)?),
            Ty::Tuple(ts) => {
                let s: Vec<String> = ts.iter().map(|t| self.strategy(t, sized && is_sized(self.model, t))).collect::<Result<_, _>>()?;
                format!("({},)", s.join(", "))
            }
            Ty::Array(e, len) => format!(
                "proptest::collection::vec({}, {len}usize).prop_map(|v| <[{}; {len}]>::try_from(v).expect(\"array length\"))",
                self.strategy(e, false)?,
                self.rust_ty(e)
            ),
            Ty::Named(id, args) if args.is_empty() => self.named(id, sized)?,
            other => return Err(format!("cannot generate benchmark inputs of type `{other}`")),
        })
    }

    fn named(&mut self, id: &str, sized: bool) -> Result<String, String> {
        if self.stack.iter().any(|s| s == id) {
            return Err(format!("cannot generate benchmark inputs of the recursive type `{id}`"));
        }
        self.stack.push(id.to_string());
        let r = self.named_inner(id, sized);
        self.stack.pop();
        r
    }

    fn named_inner(&mut self, id: &str, sized: bool) -> Result<String, String> {
        let node = self.model.nodes.get(id).ok_or_else(|| format!("unknown type `{id}`"))?;
        let ty = self.path(id);
        Ok(match &node.kind {
            NodeKind::Refined { base, generics } if generics.is_empty() => {
                let base_s = match (regex_of(self.model, id), base) {
                    (Some(re), Ty::Str) => {
                        let re = re.trim_start_matches('^').trim_end_matches('$');
                        format!("proptest::string::string_regex({re:?}).expect(\"valid regex\")")
                    }
                    _ => self.strategy(base, sized && is_sized(self.model, base))?,
                };
                format!("{base_s}.prop_filter_map(\"refinement\", |v| {ty}::parse(v).ok()).boxed()")
            }
            NodeKind::Struct(s) if s.generics.is_empty() => {
                let fields = s.fields.clone();
                if fields.is_empty() {
                    return Ok(format!("proptest::strategy::Just({ty} {{}}).boxed()"));
                }
                let strats: Vec<String> =
                    fields.iter().map(|f| self.strategy(&f.ty, sized && is_sized(self.model, &f.ty))).collect::<Result<_, _>>()?;
                let names: Vec<String> = fields.iter().map(|f| rust_keyword_safe(&f.name)).collect();
                let check = if s.has_invariants { ".prop_filter(\"invariant\", |v| v.wh_check().is_ok())" } else { "" };
                format!("{}.prop_map(|{}| {ty} {{ {} }}){check}.boxed()", nest(&strats), nest(&names), names.join(", "))
            }
            NodeKind::Enum(e) if e.generics.is_empty() => {
                let variants = e.variants.clone();
                let mut arms = Vec::new();
                for v in &variants {
                    if v.items.is_empty() {
                        arms.push(format!("proptest::strategy::Just({ty}::{}).boxed()", v.name));
                        continue;
                    }
                    let strats: Vec<String> =
                        v.items.iter().map(|i| self.strategy(&i.ty, sized && is_sized(self.model, &i.ty))).collect::<Result<_, _>>()?;
                    let vars: Vec<String> = (0..v.items.len()).map(|i| format!("a{i}")).collect();
                    let ctor = if v.items.iter().all(|i| i.name.is_some()) {
                        let fs: Vec<String> =
                            v.items.iter().zip(&vars).map(|(i, a)| format!("{}: {a}", rust_keyword_safe(i.name.as_deref().unwrap_or("")))).collect();
                        format!("{ty}::{} {{ {} }}", v.name, fs.join(", "))
                    } else {
                        format!("{ty}::{}({})", v.name, vars.join(", "))
                    };
                    arms.push(format!("{}.prop_map(|{}| {ctor}).boxed()", nest(&strats), nest(&vars)));
                }
                if arms.is_empty() {
                    return Err(format!("the enum `{id}` has no variants"));
                }
                format!("proptest::strategy::Union::new(vec![{}]).boxed()", arms.join(", "))
            }
            NodeKind::Alias(t) => {
                let t = t.clone();
                format!("{}.boxed()", self.strategy(&t, sized)?)
            }
            _ => return Err(format!("cannot generate benchmark inputs of type `{id}`")),
        })
    }
}

/// proptest implements `Strategy` for tuples of up to 12 elements.
fn nest(items: &[String]) -> String {
    if items.len() == 1 {
        return format!("({},)", items[0]);
    }
    if items.len() <= 10 {
        return format!("({})", items.join(", "));
    }
    let (a, b) = items.split_at(10);
    format!("({}, {})", a.join(", "), nest(b))
}
