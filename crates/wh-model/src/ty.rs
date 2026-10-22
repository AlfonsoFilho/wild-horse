//! Semantic types.

use std::fmt;

use serde::Serialize;

/// Fully qualified id of a definition: `module.Name`.
pub type NodeId = String;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub enum IntTy {
    I8,
    I16,
    I32,
    I64,
    I128,
    U8,
    U16,
    U32,
    U64,
    U128,
    Usize,
    Isize,
}

impl IntTy {
    pub fn rust(self) -> &'static str {
        match self {
            IntTy::I8 => "i8",
            IntTy::I16 => "i16",
            IntTy::I32 => "i32",
            IntTy::I64 => "i64",
            IntTy::I128 => "i128",
            IntTy::U8 => "u8",
            IntTy::U16 => "u16",
            IntTy::U32 => "u32",
            IntTy::U64 => "u64",
            IntTy::U128 => "u128",
            IntTy::Usize => "usize",
            IntTy::Isize => "isize",
        }
    }

    pub fn from_name(s: &str) -> Option<Self> {
        Some(match s {
            "i8" => IntTy::I8,
            "i16" => IntTy::I16,
            "i32" => IntTy::I32,
            "i64" | "int" => IntTy::I64,
            "i128" => IntTy::I128,
            "u8" => IntTy::U8,
            "u16" => IntTy::U16,
            "u32" => IntTy::U32,
            "u64" => IntTy::U64,
            "u128" => IntTy::U128,
            "usize" => IntTy::Usize,
            "isize" => IntTy::Isize,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub enum Ty {
    Bool,
    Int(IntTy),
    Float32,
    Float64,
    Str,
    Char,
    Bytes,
    Unit,
    Duration,
    Instant,
    List(Box<Ty>),
    Dict(Box<Ty>, Box<Ty>),
    SortedDict(Box<Ty>, Box<Ty>),
    Set(Box<Ty>),
    SortedSet(Box<Ty>),
    Tuple(Vec<Ty>),
    Array(Box<Ty>, u64),
    Option(Box<Ty>),
    Fn(Vec<Ty>, Box<Ty>),
    Stream(Box<Ty>),
    /// A user definition (struct, enum, refined type, capability, adapter)
    /// with generic arguments. Aliases are expanded during resolution.
    Named(NodeId, Vec<Ty>),
    /// A generic parameter in scope.
    Param(String),
    /// Unknown / not inferred; compatible with everything (errors were
    /// already reported where it originated).
    Unknown,
}

impl Ty {
    pub fn is_unknown(&self) -> bool {
        matches!(self, Ty::Unknown)
    }

    pub fn is_int(&self) -> bool {
        matches!(self, Ty::Int(_))
    }

    pub fn is_numeric(&self) -> bool {
        matches!(self, Ty::Int(_) | Ty::Float32 | Ty::Float64)
    }

    pub fn named_id(&self) -> Option<&str> {
        match self {
            Ty::Named(id, _) => Some(id),
            _ => None,
        }
    }

    /// Element type for iteration in spec expressions.
    pub fn element(&self) -> Option<Ty> {
        match self {
            Ty::List(t) | Ty::Set(t) | Ty::SortedSet(t) | Ty::Array(t, _) | Ty::Stream(t) => Some((**t).clone()),
            Ty::Dict(k, _) | Ty::SortedDict(k, _) => Some((**k).clone()),
            Ty::Str => Some(Ty::Char),
            Ty::Bytes => Some(Ty::Int(IntTy::U8)),
            Ty::Unknown => Some(Ty::Unknown),
            _ => None,
        }
    }

    /// Visit every named type mentioned.
    pub fn walk_named(&self, f: &mut impl FnMut(&str)) {
        match self {
            Ty::Named(id, args) => {
                f(id);
                for a in args {
                    a.walk_named(f);
                }
            }
            Ty::List(t) | Ty::Set(t) | Ty::SortedSet(t) | Ty::Array(t, _) | Ty::Option(t) | Ty::Stream(t) => {
                t.walk_named(f)
            }
            Ty::Dict(k, v) | Ty::SortedDict(k, v) => {
                k.walk_named(f);
                v.walk_named(f);
            }
            Ty::Tuple(ts) => ts.iter().for_each(|t| t.walk_named(f)),
            Ty::Fn(ps, r) => {
                ps.iter().for_each(|t| t.walk_named(f));
                r.walk_named(f);
            }
            _ => {}
        }
    }

    pub fn substitute(&self, params: &[String], args: &[Ty]) -> Ty {
        let sub = |t: &Ty| t.substitute(params, args);
        match self {
            Ty::Param(p) => params.iter().position(|q| q == p).and_then(|i| args.get(i).cloned()).unwrap_or(Ty::Unknown),
            Ty::List(t) => Ty::List(Box::new(sub(t))),
            Ty::Set(t) => Ty::Set(Box::new(sub(t))),
            Ty::SortedSet(t) => Ty::SortedSet(Box::new(sub(t))),
            Ty::Option(t) => Ty::Option(Box::new(sub(t))),
            Ty::Stream(t) => Ty::Stream(Box::new(sub(t))),
            Ty::Array(t, n) => Ty::Array(Box::new(sub(t)), *n),
            Ty::Dict(k, v) => Ty::Dict(Box::new(sub(k)), Box::new(sub(v))),
            Ty::SortedDict(k, v) => Ty::SortedDict(Box::new(sub(k)), Box::new(sub(v))),
            Ty::Tuple(ts) => Ty::Tuple(ts.iter().map(sub).collect()),
            Ty::Fn(ps, r) => Ty::Fn(ps.iter().map(sub).collect(), Box::new(sub(r))),
            Ty::Named(id, a) => Ty::Named(id.clone(), a.iter().map(sub).collect()),
            other => other.clone(),
        }
    }
}

impl fmt::Display for Ty {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let list = |f: &mut fmt::Formatter<'_>, ts: &[Ty]| -> fmt::Result {
            for (i, t) in ts.iter().enumerate() {
                if i > 0 {
                    write!(f, ", ")?;
                }
                write!(f, "{t}")?;
            }
            Ok(())
        };
        match self {
            Ty::Bool => write!(f, "bool"),
            Ty::Int(IntTy::I64) => write!(f, "int"),
            Ty::Int(i) => write!(f, "{}", i.rust()),
            Ty::Float32 => write!(f, "f32"),
            Ty::Float64 => write!(f, "float"),
            Ty::Str => write!(f, "str"),
            Ty::Char => write!(f, "char"),
            Ty::Bytes => write!(f, "bytes"),
            Ty::Unit => write!(f, "unit"),
            Ty::Duration => write!(f, "duration"),
            Ty::Instant => write!(f, "instant"),
            Ty::List(t) => write!(f, "list[{t}]"),
            Ty::Dict(k, v) => write!(f, "dict[{k}, {v}]"),
            Ty::SortedDict(k, v) => write!(f, "sorted_dict[{k}, {v}]"),
            Ty::Set(t) => write!(f, "set[{t}]"),
            Ty::SortedSet(t) => write!(f, "sorted_set[{t}]"),
            Ty::Tuple(ts) => {
                write!(f, "tuple[")?;
                list(f, ts)?;
                write!(f, "]")
            }
            Ty::Array(t, n) => write!(f, "array[{t}, {n}]"),
            Ty::Option(t) => write!(f, "{t} | None"),
            Ty::Fn(ps, r) => {
                write!(f, "(")?;
                list(f, ps)?;
                write!(f, ") -> {r}")
            }
            Ty::Stream(t) => write!(f, "Stream[{t}]"),
            Ty::Named(id, args) => {
                let short = id.rsplit('.').next().unwrap_or(id);
                write!(f, "{short}")?;
                if !args.is_empty() {
                    write!(f, "[")?;
                    list(f, args)?;
                    write!(f, "]")?;
                }
                Ok(())
            }
            Ty::Param(p) => write!(f, "{p}"),
            Ty::Unknown => write!(f, "?"),
        }
    }
}
