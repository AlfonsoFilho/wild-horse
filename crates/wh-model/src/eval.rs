//! Check-time evaluation of refinement predicates on literals (spec §13
//! rule 9): where a spec passes a literal to a refined type (`Email("x")`,
//! `normalize_email("A@B.com")`, a field default), the refinement is
//! evaluated on the literal and a violation is reported at the literal.
//!
//! Only a decidable subset is evaluated: literals, `self`, arithmetic and
//! comparisons on numbers and strings, `and`/`or`/`not`, `in`, `len`,
//! `abs`, `min`/`max`, `matches` (Rust `regex` semantics, as in generated
//! code) and the pure string methods. Anything else makes the predicate
//! undecidable and the literal is accepted (it is still checked at run time).

use wh_syntax::ast::{BinOp, CmpOp, Expr, ExprKind, ItemKind, StrKind, UnaryOp};

use crate::model::{Model, NodeKind};
use crate::ty::{IntTy, Ty};

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Int(i128),
    Float(f64),
    Str(String),
    Bool(bool),
    List(Vec<Value>),
    None,
}

impl Value {
    fn num(&self) -> Option<f64> {
        match self {
            Value::Int(i) => Some(*i as f64),
            Value::Float(f) => Some(*f),
            _ => None,
        }
    }

    fn truth(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }
}

/// The value of a literal expression (`3`, `-1`, `"a@b"`, `[1, 2]`, `None`).
pub fn literal(e: &Expr) -> Option<Value> {
    Some(match &e.kind {
        ExprKind::Int(v) => Value::Int(parse_int(v)?),
        ExprKind::Float(v) => Value::Float(v.parse().ok()?),
        ExprKind::Str { value, kind: StrKind::Normal | StrKind::Raw } => Value::Str(value.clone()),
        ExprKind::Bool(b) => Value::Bool(*b),
        ExprKind::None => Value::None,
        ExprKind::Unary { op: UnaryOp::Neg, expr } => match literal(expr)? {
            Value::Int(i) => Value::Int(-i),
            Value::Float(f) => Value::Float(-f),
            _ => return None,
        },
        ExprKind::List(items) => Value::List(items.iter().map(literal).collect::<Option<_>>()?),
        _ => return None,
    })
}

fn parse_int(v: &str) -> Option<i128> {
    let digits = strip_suffix(v);
    if let Some(h) = digits.strip_prefix("0x") {
        i128::from_str_radix(h, 16).ok()
    } else if let Some(b) = digits.strip_prefix("0b") {
        i128::from_str_radix(b, 2).ok()
    } else if let Some(o) = digits.strip_prefix("0o") {
        i128::from_str_radix(o, 8).ok()
    } else {
        digits.parse().ok()
    }
}

/// `300u8` → `300`
fn strip_suffix(v: &str) -> &str {
    for s in ["u128", "i128", "usize", "isize", "u64", "i64", "u32", "i32", "u16", "i16", "u8", "i8"] {
        if let Some(x) = v.strip_suffix(s) {
            return x;
        }
    }
    v
}

/// Evaluate a predicate with `self` bound to `this`; `None` = undecidable.
pub fn eval(e: &Expr, this: &Value) -> Option<Value> {
    eval_with(e, &|n| (n == "self").then(|| this.clone()))
}

/// Evaluate an expression whose free names are looked up in `vars`.
pub fn eval_with(e: &Expr, vars: &dyn Fn(&str) -> Option<Value>) -> Option<Value> {
    let this = vars;
    if let Some(v) = literal(e) {
        return Some(v);
    }
    match &e.kind {
        ExprKind::Name(n) => vars(n),
        ExprKind::Unary { op: UnaryOp::Not, expr } => Some(Value::Bool(!eval_with(expr, this)?.truth()?)),
        ExprKind::Unary { op: UnaryOp::Neg, expr } => match eval_with(expr, this)? {
            Value::Int(i) => Some(Value::Int(-i)),
            Value::Float(f) => Some(Value::Float(-f)),
            _ => None,
        },
        ExprKind::Binary { op: BinOp::And, lhs, rhs } => {
            let l = eval_with(lhs, this).and_then(|v| v.truth());
            if l == Some(false) {
                return Some(Value::Bool(false));
            }
            let r = eval_with(rhs, this).and_then(|v| v.truth());
            match (l, r) {
                (_, Some(false)) => Some(Value::Bool(false)),
                (Some(true), Some(true)) => Some(Value::Bool(true)),
                _ => None,
            }
        }
        ExprKind::Binary { op: BinOp::Or, lhs, rhs } => {
            let l = eval_with(lhs, this).and_then(|v| v.truth());
            if l == Some(true) {
                return Some(Value::Bool(true));
            }
            let r = eval_with(rhs, this).and_then(|v| v.truth());
            match (l, r) {
                (_, Some(true)) => Some(Value::Bool(true)),
                (Some(false), Some(false)) => Some(Value::Bool(false)),
                _ => None,
            }
        }
        ExprKind::Binary { op, lhs, rhs } => arith(*op, eval_with(lhs, this)?, eval_with(rhs, this)?),
        ExprKind::Compare { first, rest } => {
            let mut prev = eval_with(first, this)?;
            for (op, rhs) in rest {
                let r = eval_with(rhs, this)?;
                if !compare(*op, &prev, &r)? {
                    return Some(Value::Bool(false));
                }
                prev = r;
            }
            Some(Value::Bool(true))
        }
        ExprKind::IfElse { then, cond, otherwise } => {
            if eval_with(cond, this)?.truth()? { eval_with(then, this) } else { eval_with(otherwise, this) }
        }
        ExprKind::Call { func, args } => {
            let vals: Vec<Value> = args.iter().map(|a| eval_with(&a.value, this)).collect::<Option<_>>()?;
            match &func.kind {
                ExprKind::Name(n) => builtin(n, &vals),
                ExprKind::Attr { base, attr } => method(&eval_with(base, this)?, &attr.name, &vals),
                _ => None,
            }
        }
        _ => None,
    }
}

fn arith(op: BinOp, l: Value, r: Value) -> Option<Value> {
    match (&l, &r) {
        (Value::Int(a), Value::Int(b)) if op != BinOp::Div => Some(Value::Int(match op {
            BinOp::Add => a.checked_add(*b)?,
            BinOp::Sub => a.checked_sub(*b)?,
            BinOp::Mul => a.checked_mul(*b)?,
            BinOp::FloorDiv => a.checked_div_euclid(*b)?,
            BinOp::Mod => a.checked_rem_euclid(*b)?,
            BinOp::Pow => a.checked_pow(u32::try_from(*b).ok()?)?,
            _ => return None,
        })),
        (Value::Str(a), Value::Str(b)) if op == BinOp::Add => Some(Value::Str(format!("{a}{b}"))),
        _ => {
            let (a, b) = (l.num()?, r.num()?);
            Some(Value::Float(match op {
                BinOp::Add => a + b,
                BinOp::Sub => a - b,
                BinOp::Mul => a * b,
                BinOp::Div => a / b,
                BinOp::FloorDiv => (a / b).floor(),
                BinOp::Mod => a.rem_euclid(b),
                BinOp::Pow => a.powf(b),
                _ => return None,
            }))
        }
    }
}

fn compare(op: CmpOp, l: &Value, r: &Value) -> Option<bool> {
    use std::cmp::Ordering;
    let ord = |l: &Value, r: &Value| -> Option<Ordering> {
        match (l, r) {
            (Value::Int(a), Value::Int(b)) => Some(a.cmp(b)),
            (Value::Str(a), Value::Str(b)) => Some(a.cmp(b)),
            (Value::Bool(a), Value::Bool(b)) => Some(a.cmp(b)),
            _ => l.num()?.partial_cmp(&r.num()?),
        }
    };
    Some(match op {
        CmpOp::Eq => match (l, r) {
            (Value::None, Value::None) => true,
            (Value::None, _) | (_, Value::None) => false,
            (Value::List(a), Value::List(b)) => a == b,
            _ => ord(l, r)? == Ordering::Equal,
        },
        CmpOp::NotEq => !compare(CmpOp::Eq, l, r)?,
        CmpOp::Lt => ord(l, r)? == Ordering::Less,
        CmpOp::Le => ord(l, r)? != Ordering::Greater,
        CmpOp::Gt => ord(l, r)? == Ordering::Greater,
        CmpOp::Ge => ord(l, r)? != Ordering::Less,
        CmpOp::In | CmpOp::NotIn => {
            let found = match (l, r) {
                (Value::Str(a), Value::Str(b)) => b.contains(a.as_str()),
                (x, Value::List(xs)) => xs.iter().any(|y| compare(CmpOp::Eq, x, y) == Some(true)),
                _ => return None,
            };
            found == (op == CmpOp::In)
        }
        CmpOp::Is => matches!((l, r), (Value::None, Value::None)),
        CmpOp::IsNot => !matches!((l, r), (Value::None, Value::None)),
    })
}

fn builtin(n: &str, args: &[Value]) -> Option<Value> {
    Some(match (n, args) {
        ("len", [Value::Str(s)]) => Value::Int(s.chars().count() as i128),
        ("len", [Value::List(xs)]) => Value::Int(xs.len() as i128),
        ("matches", [Value::Str(s), Value::Str(re)]) => Value::Bool(regex::Regex::new(re).ok()?.is_match(s)),
        ("abs", [Value::Int(i)]) => Value::Int(i.abs()),
        ("abs", [Value::Float(f)]) => Value::Float(f.abs()),
        ("min" | "max", [_, _, ..]) => {
            let mut best = args[0].clone();
            for a in &args[1..] {
                let less = compare(CmpOp::Lt, a, &best)?;
                if less == (n == "min") && compare(CmpOp::Eq, a, &best) != Some(true) {
                    best = a.clone();
                }
            }
            best
        }
        _ => return None,
    })
}

fn method(base: &Value, m: &str, args: &[Value]) -> Option<Value> {
    let Value::Str(s) = base else {
        return match (base, m) {
            (Value::List(xs), "is_empty") => Some(Value::Bool(xs.is_empty())),
            (Value::List(xs), "contains") => Some(Value::Bool(xs.iter().any(|x| Some(x) == args.first()))),
            _ => None,
        };
    };
    let arg = |i: usize| match args.get(i) {
        Some(Value::Str(a)) => Some(a.as_str()),
        _ => None,
    };
    Some(match m {
        "lower" => Value::Str(s.to_lowercase()),
        "upper" => Value::Str(s.to_uppercase()),
        "strip" => Value::Str(s.trim().to_string()),
        "lstrip" => Value::Str(s.trim_start().to_string()),
        "rstrip" => Value::Str(s.trim_end().to_string()),
        "startswith" => Value::Bool(s.starts_with(arg(0)?)),
        "endswith" => Value::Bool(s.ends_with(arg(0)?)),
        "contains" => Value::Bool(s.contains(arg(0)?)),
        "isdigit" => Value::Bool(!s.is_empty() && s.chars().all(|c| c.is_ascii_digit())),
        "isalpha" => Value::Bool(!s.is_empty() && s.chars().all(char::is_alphabetic)),
        "is_empty" => Value::Bool(s.is_empty()),
        "len" => Value::Int(s.chars().count() as i128),
        _ => return None,
    })
}

fn int_range(i: IntTy) -> (i128, i128) {
    match i {
        IntTy::I8 => (i8::MIN as i128, i8::MAX as i128),
        IntTy::I16 => (i16::MIN as i128, i16::MAX as i128),
        IntTy::I32 => (i32::MIN as i128, i32::MAX as i128),
        IntTy::I64 | IntTy::Isize => (i64::MIN as i128, i64::MAX as i128),
        IntTy::I128 => (i128::MIN, i128::MAX),
        IntTy::U8 => (0, u8::MAX as i128),
        IntTy::U16 => (0, u16::MAX as i128),
        IntTy::U32 => (0, u32::MAX as i128),
        IntTy::U64 | IntTy::Usize => (0, u64::MAX as i128),
        IntTy::U128 => (0, i128::MAX),
    }
}

fn show(v: &Value, src: &Expr) -> String {
    match v {
        Value::Str(s) => format!("{s:?}"),
        Value::Int(i) => i.to_string(),
        _ => match &src.kind {
            ExprKind::Float(f) => f.clone(),
            _ => format!("{v:?}"),
        },
    }
}

/// Does the literal `e`, used where `expected` is wanted, violate a
/// refinement (or the range of an integer type)? Returns the message.
pub fn check_literal(model: &Model, expected: &Ty, e: &Expr) -> Option<String> {
    let v = literal(e)?;
    check_value(model, expected, &v, e, 0)
}

fn check_value(model: &Model, expected: &Ty, v: &Value, src: &Expr, depth: u32) -> Option<String> {
    if depth > 8 {
        return None;
    }
    match (expected, v) {
        (_, Value::None) => None,
        (Ty::Option(inner), _) => check_value(model, inner, v, src, depth + 1),
        (Ty::List(inner) | Ty::Set(inner) | Ty::SortedSet(inner), Value::List(xs)) => {
            let items: Vec<&Expr> = match &src.kind {
                ExprKind::List(items) => items.iter().collect(),
                _ => vec![src; xs.len()],
            };
            xs.iter().zip(items).find_map(|(x, s)| check_value(model, inner, x, s, depth + 1))
        }
        (Ty::Int(i), Value::Int(n)) => {
            let (lo, hi) = int_range(*i);
            (*n < lo || *n > hi).then(|| format!("`{n}` does not fit in `{}`", i.rust()))
        }
        (Ty::Named(id, _), _) => {
            let node = model.nodes.get(id)?;
            let NodeKind::Refined { base, .. } = &node.kind else { return None };
            if let Some(msg) = check_value(model, base, v, src, depth + 1) {
                return Some(msg);
            }
            let ItemKind::TypeDef(td) = &node.item.kind else { return None };
            let rule = td.refinement.as_ref()?;
            match eval(rule, v) {
                Some(Value::Bool(false)) => {
                    let text = &model.files[node.file].text[rule.span.range()];
                    Some(format!("{} does not satisfy `{}`: `{text}`", show(v, src), node.name))
                }
                _ => None,
            }
        }
        _ => None,
    }
}
