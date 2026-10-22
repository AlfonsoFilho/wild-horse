//! Performance gates (`complexity`, `budget`, `alloc`) and the comparison of
//! two sets of measurements (variant vs incumbent, current vs baseline).

use serde::{Deserialize, Serialize};

use crate::benchgen::AUTO;
use crate::measure::{Measurement, fmt_ns};
use crate::spec::{AllocLimit, PerfSpec, Target};
use crate::stats::{self, ComplexityFit};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GateStatus {
    Pass,
    Fail,
    /// Could not be checked (no size sweep, tool missing, …).
    Skipped,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GateResult {
    /// The clause, as written (`complexity O(n)`).
    pub gate: String,
    pub status: GateStatus,
    pub detail: String,
}

impl GateResult {
    pub fn failed(&self) -> bool {
        self.status == GateStatus::Fail
    }
}

/// Fit the auto-bench growth curve of a node (needs a size sweep).
pub fn complexity_fit(measurements: &[Measurement]) -> Option<ComplexityFit> {
    let pts: Vec<(u64, f64)> = measurements.iter().filter(|m| m.bench == AUTO).map(|m| (m.size, m.mean.point)).collect();
    stats::fit_complexity(&pts)
}

/// Evaluate a node's performance clauses against its measurements.
pub fn evaluate(spec: &PerfSpec, measurements: &[Measurement], sized: bool) -> (Option<ComplexityFit>, Vec<GateResult>) {
    let auto: Vec<&Measurement> = measurements.iter().filter(|m| m.bench == AUTO).collect();
    let fit = if sized { complexity_fit(measurements) } else { None };
    let mut gates = Vec::new();
    if let Some((declared, text)) = &spec.complexity {
        let r = match (declared, &fit) {
            (None, _) => GateResult { gate: text.clone(), status: GateStatus::Skipped, detail: "unrecognized complexity class".into() },
            (Some(_), None) => GateResult {
                gate: text.clone(),
                status: GateStatus::Skipped,
                detail: if sized { "not enough sizes measured to fit a growth curve".into() } else { "the def has no size parameter".into() },
            },
            (Some(d), Some(f)) => {
                let ok = stats::within(f, *d);
                GateResult {
                    gate: text.clone(),
                    status: if ok { GateStatus::Pass } else { GateStatus::Fail },
                    detail: format!(
                        "measured growth {} (log-log slope {:.2}) {} declared {}",
                        f.class.label(),
                        f.exponent,
                        if ok { "within" } else { "exceeds" },
                        d.label()
                    ),
                }
            }
        };
        gates.push(r);
    }
    for b in &spec.budgets {
        let r = match auto.iter().find(|m| m.size == b.size) {
            None => GateResult { gate: b.text.clone(), status: GateStatus::Skipped, detail: format!("no measurement at n={}", b.size) },
            Some(m) => {
                let ok = m.mean.point <= b.time_ns;
                GateResult {
                    gate: b.text.clone(),
                    status: if ok { GateStatus::Pass } else { GateStatus::Fail },
                    detail: format!(
                        "mean {} (95% CI {} – {}) at n={} {} budget {}",
                        fmt_ns(m.mean.point),
                        fmt_ns(m.mean.lower),
                        fmt_ns(m.mean.upper),
                        b.size,
                        if ok { "within" } else { "over" },
                        fmt_ns(b.time_ns)
                    ),
                }
            }
        };
        gates.push(r);
    }
    if let Some((limit, text)) = &spec.alloc {
        let counted: Vec<&&Measurement> = auto.iter().filter(|m| m.allocs.is_some()).collect();
        let r = if counted.is_empty() {
            GateResult { gate: text.clone(), status: GateStatus::Skipped, detail: "no allocation counts".into() }
        } else {
            let mut worst: Option<String> = None;
            for m in &counted {
                let n = m.allocs.unwrap_or(0) as i128;
                let bound = match limit {
                    AllocLimit::None => 0,
                    AllocLimit::Bounded(e) => e.eval(m.size),
                };
                if n > bound {
                    worst = Some(format!("{n} allocations per call at n={} (limit {bound})", m.size));
                    break;
                }
            }
            match worst {
                Some(w) => GateResult { gate: text.clone(), status: GateStatus::Fail, detail: w },
                None => {
                    let max = counted.iter().filter_map(|m| m.allocs).max().unwrap_or(0);
                    GateResult { gate: text.clone(), status: GateStatus::Pass, detail: format!("at most {max} allocations per call") }
                }
            }
        };
        gates.push(r);
    }
    (fit, gates)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Change {
    Improved,
    Regressed,
    /// Within noise, or not statistically significant.
    Same,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Delta {
    pub bench: String,
    pub size: u64,
    pub before: f64,
    pub after: f64,
    /// after / before (lower is better for every target).
    pub ratio: f64,
    /// One-sided p-value of the observed direction (None for exact counts).
    pub p_value: Option<f64>,
    pub change: Change,
}

/// `after` compared with `before` on one metric.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Comparison {
    pub target: Target,
    pub deltas: Vec<Delta>,
    /// Geometric mean of the ratios (lower is better).
    pub geomean_ratio: f64,
    /// Significantly better overall and nowhere significantly worse.
    pub improved: bool,
    /// Worse overall: the geometric mean beyond the threshold *and* at least
    /// one benchmark significantly worse (one noisy size is not enough).
    pub regressed: bool,
    /// At least one benchmark is significantly worse (vetoes `improved`).
    #[serde(default)]
    pub worse_somewhere: bool,
    pub summary: String,
}

/// Compare two measurement sets benchmark by benchmark. A benchmark counts
/// as changed only when the difference exceeds `threshold` (relative) and is
/// significant at level `alpha` (Welch's t-test on criterion's samples).
pub fn compare(before: &[Measurement], after: &[Measurement], target: Target, threshold: f64, alpha: f64) -> Comparison {
    let mut deltas = Vec::new();
    for a in after {
        let Some(b) = before.iter().find(|b| b.node == a.node && b.bench == a.bench && b.size == a.size) else { continue };
        let (bv, av, p) = match target {
            Target::Memory => match (b.alloc_bytes, a.alloc_bytes) {
                (Some(x), Some(y)) => (x as f64 + 1.0, y as f64 + 1.0, None),
                _ => continue,
            },
            Target::Latency => (b.median.point, a.median.point, stats::welch(&b.samples, &a.samples)),
            Target::Speed | Target::Throughput | Target::Size => (b.mean.point, a.mean.point, stats::welch(&b.samples, &a.samples)),
        };
        if bv <= 0.0 || av <= 0.0 {
            continue;
        }
        let ratio = av / bv;
        let (p_value, significant) = match p {
            Some(w) => {
                let pv = if ratio < 1.0 { w.p_less } else { w.p_greater };
                (Some(pv), pv < alpha)
            }
            // exact counts (or too few samples): the threshold decides
            None => (None, true),
        };
        let change = if significant && ratio < 1.0 - threshold {
            Change::Improved
        } else if significant && ratio > 1.0 + threshold {
            Change::Regressed
        } else {
            Change::Same
        };
        deltas.push(Delta { bench: a.bench.clone(), size: a.size, before: bv, after: av, ratio, p_value, change });
    }
    let geomean_ratio =
        if deltas.is_empty() { 1.0 } else { (deltas.iter().map(|d| d.ratio.ln()).sum::<f64>() / deltas.len() as f64).exp() };
    let worse_somewhere = deltas.iter().any(|d| d.change == Change::Regressed);
    let regressed = worse_somewhere && geomean_ratio > 1.0 + threshold;
    let improved = !worse_somewhere && geomean_ratio < 1.0 - threshold && deltas.iter().any(|d| d.change == Change::Improved);
    let summary = if deltas.is_empty() {
        "nothing comparable was measured".to_string()
    } else {
        let worst = deltas.iter().max_by(|a, b| a.ratio.total_cmp(&b.ratio)).expect("non-empty");
        format!(
            "{} on {}: geometric mean ×{:.3} ({:+.1}%); {} of {} benchmarks significantly better, {} worse (worst ×{:.2} at {}/{})",
            if improved {
                "improved"
            } else if regressed {
                "regressed"
            } else if worse_somewhere {
                "slower at some sizes, not overall"
            } else {
                "no significant change"
            },
            target.label(),
            geomean_ratio,
            (geomean_ratio - 1.0) * 100.0,
            deltas.iter().filter(|d| d.change == Change::Improved).count(),
            deltas.len(),
            deltas.iter().filter(|d| d.change == Change::Regressed).count(),
            worst.ratio,
            worst.bench,
            worst.size,
        )
    };
    Comparison { target, deltas, geomean_ratio, improved, regressed, worse_somewhere, summary }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::measure::Estimate;

    fn m(size: u64, ns: f64, jitter: f64) -> Measurement {
        let samples: Vec<f64> = (0..12).map(|i| ns * (1.0 + jitter * ((i % 3) as f64 - 1.0))).collect();
        let e = Estimate { point: ns, lower: ns * 0.99, upper: ns * 1.01 };
        Measurement { node: "m.f".into(), bench: AUTO.into(), size, mean: e, median: e, std_dev: 0.0, samples, allocs: Some(size), alloc_bytes: Some(size * 8) }
    }

    #[test]
    fn significant_improvement_only() {
        let before: Vec<Measurement> = [64, 256, 1024].iter().map(|&n| m(n, n as f64 * 10.0, 0.02)).collect();
        let faster: Vec<Measurement> = [64, 256, 1024].iter().map(|&n| m(n, n as f64 * 5.0, 0.02)).collect();
        let c = compare(&before, &faster, Target::Speed, 0.03, 0.05);
        assert!(c.improved && !c.regressed, "{}", c.summary);
        let noise: Vec<Measurement> = [64, 256, 1024].iter().map(|&n| m(n, n as f64 * 10.1, 0.02)).collect();
        let c = compare(&before, &noise, Target::Speed, 0.03, 0.05);
        assert!(!c.improved && !c.regressed, "{}", c.summary);
        // faster on small inputs but slower on large ones is not an improvement
        let mixed: Vec<Measurement> = [(64, 100.0), (256, 600.0), (1024, 20_000.0)].iter().map(|&(n, t)| m(n, t, 0.02)).collect();
        let c = compare(&before, &mixed, Target::Speed, 0.03, 0.05);
        assert!(!c.improved && c.worse_somewhere && !c.regressed, "{}", c.summary);
        // one noisy size does not make a regression; a slower whole curve does
        let noisy: Vec<Measurement> = [(64, 640.0), (256, 2560.0), (1024, 10240.0 * 1.07)].iter().map(|&(n, t)| m(n, t, 0.005)).collect();
        let c = compare(&before, &noisy, Target::Speed, 0.03, 0.05);
        assert!(c.worse_somewhere && !c.regressed, "{}", c.summary);
        let slower: Vec<Measurement> = [64, 256, 1024].iter().map(|&n| m(n, n as f64 * 12.0, 0.005)).collect();
        let c = compare(&before, &slower, Target::Speed, 0.03, 0.05);
        assert!(c.regressed, "{}", c.summary);
    }
}
