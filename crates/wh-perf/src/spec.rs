//! The performance clauses of a def (language spec §5): `optimize`,
//! `complexity`, `budget`, `alloc`, `mutate`, `bench`, `no_panic`.

use serde::{Deserialize, Serialize};
use wh_model::{Model, Ty};
use wh_syntax::ast::{AllocSpec, BinOp, ClauseKind, CmpOp, Expr, ExprKind, Param};

/// Growth classes the complexity fit distinguishes, simplest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Complexity {
    Constant,
    Log,
    Linear,
    NLogN,
    Quadratic,
    Cubic,
}

impl Complexity {
    pub const ALL: [Complexity; 6] =
        [Complexity::Constant, Complexity::Log, Complexity::Linear, Complexity::NLogN, Complexity::Quadratic, Complexity::Cubic];

    /// Parse the text of a `complexity` clause: `O(1)`, `O(log n)`, `O(log(n))`,
    /// `O(n)`, `O(n log n)`, `O(n * log(n))`, `O(n**2)`, `O(n^2)`, `O(n*n)`, `O(n**3)`.
    pub fn parse(text: &str) -> Option<Self> {
        let t: String = text.chars().filter(|c| !c.is_whitespace() && *c != '*' && *c != '(' && *c != ')').collect();
        let t = t.strip_prefix("complexity").unwrap_or(&t);
        let t = t.strip_prefix('O').or_else(|| t.strip_prefix('o')).unwrap_or(t);
        // `n**2` lost its stars: `n2`; `n*n` → `nn`; `n^2` → `n^2`
        Some(match t {
            "1" | "c" => Complexity::Constant,
            "logn" | "lgn" | "log2n" => Complexity::Log,
            "n" => Complexity::Linear,
            "nlogn" | "nlgn" | "nlog2n" | "lognn" => Complexity::NLogN,
            "n2" | "n^2" | "nn" | "n²" => Complexity::Quadratic,
            "n3" | "n^3" | "nnn" | "n³" => Complexity::Cubic,
            _ => return None,
        })
    }

    /// The growth function.
    pub fn f(self, n: f64) -> f64 {
        let n = n.max(1.0);
        match self {
            Complexity::Constant => 1.0,
            Complexity::Log => n.log2().max(1.0),
            Complexity::Linear => n,
            Complexity::NLogN => n * n.log2().max(1.0),
            Complexity::Quadratic => n * n,
            Complexity::Cubic => n * n * n,
        }
    }

    /// Typical log-log slope of the class over practical sizes.
    pub fn exponent(self) -> f64 {
        match self {
            Complexity::Constant => 0.0,
            Complexity::Log => 0.15,
            Complexity::Linear => 1.0,
            Complexity::NLogN => 1.15,
            Complexity::Quadratic => 2.0,
            Complexity::Cubic => 3.0,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Complexity::Constant => "O(1)",
            Complexity::Log => "O(log n)",
            Complexity::Linear => "O(n)",
            Complexity::NLogN => "O(n log n)",
            Complexity::Quadratic => "O(n²)",
            Complexity::Cubic => "O(n³)",
        }
    }
}

/// The metric an `optimize` clause targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Target {
    /// Mean time per call.
    Speed,
    /// Median time per call.
    Latency,
    /// Calls per second (the inverse of the mean time).
    Throughput,
    /// Bytes allocated per call.
    Memory,
    /// Code size (not measured yet).
    Size,
}

impl Target {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "speed" => Target::Speed,
            "latency" => Target::Latency,
            "throughput" => Target::Throughput,
            "memory" => Target::Memory,
            "size" => Target::Size,
            _ => return None,
        })
    }

    pub fn label(self) -> &'static str {
        match self {
            Target::Speed => "speed (mean time per call)",
            Target::Latency => "latency (median time per call)",
            Target::Throughput => "throughput (calls per second)",
            Target::Memory => "memory (bytes allocated per call)",
            Target::Size => "size (code size)",
        }
    }
}

/// An integer expression over the input size `n` (`alloc bounded(...)`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SizeExpr {
    Const(i128),
    /// The input size (any name, or `len(x)`).
    N,
    Bin(Box<SizeExpr>, char, Box<SizeExpr>),
}

impl SizeExpr {
    pub fn from_expr(e: &Expr) -> Option<Self> {
        Some(match &e.kind {
            ExprKind::Int(v) => SizeExpr::Const(v.replace('_', "").parse().ok()?),
            ExprKind::Name(_) => SizeExpr::N,
            ExprKind::Call { func, args } if func.callee_name() == Some("len") && args.len() == 1 => SizeExpr::N,
            ExprKind::Binary { op, lhs, rhs } => {
                let op = match op {
                    BinOp::Add => '+',
                    BinOp::Sub => '-',
                    BinOp::Mul => '*',
                    BinOp::Div | BinOp::FloorDiv => '/',
                    _ => return None,
                };
                SizeExpr::Bin(Box::new(Self::from_expr(lhs)?), op, Box::new(Self::from_expr(rhs)?))
            }
            _ => return None,
        })
    }

    pub fn eval(&self, n: u64) -> i128 {
        match self {
            SizeExpr::Const(v) => *v,
            SizeExpr::N => n as i128,
            SizeExpr::Bin(a, op, b) => {
                let (a, b) = (a.eval(n), b.eval(n));
                match op {
                    '+' => a.saturating_add(b),
                    '-' => a.saturating_sub(b),
                    '*' => a.saturating_mul(b),
                    _ => a.checked_div(b).unwrap_or(i128::MAX),
                }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum AllocLimit {
    None,
    Bounded(SizeExpr),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Budget {
    /// Time budget per call, in nanoseconds.
    pub time_ns: f64,
    /// Input size the budget applies to.
    pub size: u64,
    /// Source text of the clause.
    pub text: String,
}

/// `mutate score >= x` (or `>`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MutateGate {
    pub threshold: f64,
    pub strict: bool,
    pub text: String,
}

impl MutateGate {
    pub fn passes(&self, score: f64) -> bool {
        if self.strict { score > self.threshold } else { score >= self.threshold }
    }
}

/// A `bench name(params): call` clause.
#[derive(Debug, Clone)]
pub struct BenchClause {
    pub name: String,
    pub params: Vec<(Param, Ty)>,
    pub body: Expr,
}

/// Every performance-related clause of one def.
#[derive(Debug, Clone, Default)]
pub struct PerfSpec {
    pub node: String,
    pub optimize: Option<Target>,
    /// (parsed class, clause text)
    pub complexity: Option<(Option<Complexity>, String)>,
    pub budgets: Vec<Budget>,
    pub alloc: Option<(AllocLimit, String)>,
    pub mutate: Option<MutateGate>,
    pub benches: Vec<BenchClause>,
    pub no_panic: bool,
}

pub fn duration_ns(value: &str, unit: &str) -> Option<f64> {
    let v: f64 = value.replace('_', "").parse().ok()?;
    let scale = match unit {
        "ns" => 1.0,
        "us" | "µs" => 1e3,
        "ms" => 1e6,
        "s" => 1e9,
        _ => return None,
    };
    Some(v * scale)
}

impl PerfSpec {
    /// The performance clauses of a def node (`None` for other nodes).
    pub fn of(model: &Model, node: &str) -> Option<PerfSpec> {
        let n = model.nodes.get(node)?;
        let def = n.def()?;
        let text = &model.files[n.file].text;
        let mut spec = PerfSpec { node: node.to_string(), ..Default::default() };
        for c in &def.clauses {
            let src = text.get(c.span.range()).unwrap_or("").trim().to_string();
            match &c.kind {
                ClauseKind::Optimize(t) => spec.optimize = Target::parse(&t.name),
                ClauseKind::Complexity(e) => {
                    let et = text.get(e.span.range()).unwrap_or("");
                    spec.complexity = Some((Complexity::parse(et), src));
                }
                ClauseKind::Budget { time, size, .. } => {
                    let ns = match &time.kind {
                        ExprKind::Duration { value, unit } => duration_ns(value, unit),
                        _ => None,
                    };
                    let size = SizeExpr::from_expr(size).map(|s| s.eval(0)).filter(|s| *s > 0);
                    if let (Some(time_ns), Some(size)) = (ns, size) {
                        spec.budgets.push(Budget { time_ns, size: size as u64, text: src });
                    }
                }
                ClauseKind::Alloc(a) => {
                    let limit = match a {
                        AllocSpec::None => Some(AllocLimit::None),
                        AllocSpec::Bounded(e) => SizeExpr::from_expr(e).map(AllocLimit::Bounded),
                    };
                    if let Some(l) = limit {
                        spec.alloc = Some((l, src));
                    }
                }
                ClauseKind::Mutate(e) => spec.mutate = mutate_gate(e, src),
                ClauseKind::Bench { name, params, body } => {
                    let mut d = Vec::new();
                    let params = params.iter().map(|p| (p.clone(), model.resolve_ty(&n.module, &p.ty, &[], &mut d))).collect();
                    spec.benches.push(BenchClause { name: name.name.clone(), params, body: body.clone() });
                }
                ClauseKind::NoPanic => spec.no_panic = true,
                _ => {}
            }
        }
        Some(spec)
    }

    /// Input sizes the spec itself needs measured (budget sizes).
    pub fn required_sizes(&self) -> Vec<u64> {
        let mut s: Vec<u64> = self.budgets.iter().map(|b| b.size).collect();
        s.sort_unstable();
        s.dedup();
        s
    }
}

fn number(e: &Expr) -> Option<f64> {
    match &e.kind {
        ExprKind::Int(v) | ExprKind::Float(v) => v.replace('_', "").parse().ok(),
        _ => None,
    }
}

fn mutate_gate(e: &Expr, text: String) -> Option<MutateGate> {
    let ExprKind::Compare { first, rest } = &e.kind else { return None };
    let [(op, rhs)] = rest.as_slice() else { return None };
    let is_score = |e: &Expr| matches!(&e.kind, ExprKind::Name(n) if n == "score");
    match (is_score(first), op) {
        (true, CmpOp::Ge) => Some(MutateGate { threshold: number(rhs)?, strict: false, text }),
        (true, CmpOp::Gt) => Some(MutateGate { threshold: number(rhs)?, strict: true, text }),
        (false, CmpOp::Le) if is_score(rhs) => Some(MutateGate { threshold: number(first)?, strict: false, text }),
        (false, CmpOp::Lt) if is_score(rhs) => Some(MutateGate { threshold: number(first)?, strict: true, text }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn complexity_texts() {
        assert_eq!(Complexity::parse("O(1)"), Some(Complexity::Constant));
        assert_eq!(Complexity::parse("O(n)"), Some(Complexity::Linear));
        assert_eq!(Complexity::parse("O(log(n))"), Some(Complexity::Log));
        assert_eq!(Complexity::parse("O(n * log(n))"), Some(Complexity::NLogN));
        assert_eq!(Complexity::parse("O(n log n)"), Some(Complexity::NLogN));
        assert_eq!(Complexity::parse("O(n**2)"), Some(Complexity::Quadratic));
        assert_eq!(Complexity::parse("O(n^2)"), Some(Complexity::Quadratic));
        assert_eq!(Complexity::parse("O(2**n)"), None);
    }

    #[test]
    fn size_exprs() {
        let e = SizeExpr::Bin(Box::new(SizeExpr::N), '*', Box::new(SizeExpr::Const(2)));
        assert_eq!(e.eval(10), 20);
        assert_eq!(duration_ns("50", "us"), Some(50_000.0));
        assert_eq!(duration_ns("1_000", "ns"), Some(1000.0));
    }
}
