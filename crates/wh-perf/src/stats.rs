//! Statistics: Welch's t-test for "is this variant really faster", and the
//! growth-curve fit behind `complexity` gates.

use serde::{Deserialize, Serialize};

use crate::spec::Complexity;

pub fn mean_var(xs: &[f64]) -> (f64, f64) {
    let n = xs.len() as f64;
    if xs.is_empty() {
        return (f64::NAN, f64::NAN);
    }
    let m = xs.iter().sum::<f64>() / n;
    let v = if xs.len() > 1 { xs.iter().map(|x| (x - m).powi(2)).sum::<f64>() / (n - 1.0) } else { 0.0 };
    (m, v)
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Welch {
    pub t: f64,
    pub df: f64,
    /// One-sided p-value for "mean(b) < mean(a)".
    pub p_less: f64,
    /// One-sided p-value for "mean(b) > mean(a)".
    pub p_greater: f64,
}

/// Welch's unequal-variance t-test of sample `b` against sample `a`.
pub fn welch(a: &[f64], b: &[f64]) -> Option<Welch> {
    if a.len() < 2 || b.len() < 2 {
        return None;
    }
    let (ma, va) = mean_var(a);
    let (mb, vb) = mean_var(b);
    let (na, nb) = (a.len() as f64, b.len() as f64);
    let se2 = va / na + vb / nb;
    if se2 <= 0.0 {
        // identical constant samples: decide by the means alone
        let p = if mb < ma { (0.0, 1.0) } else if mb > ma { (1.0, 0.0) } else { (0.5, 0.5) };
        return Some(Welch { t: 0.0, df: na + nb - 2.0, p_less: p.0, p_greater: p.1 });
    }
    let t = (mb - ma) / se2.sqrt();
    let df = se2.powi(2) / ((va / na).powi(2) / (na - 1.0) + (vb / nb).powi(2) / (nb - 1.0));
    let cdf = student_t_cdf(t, df);
    Some(Welch { t, df, p_less: cdf, p_greater: 1.0 - cdf })
}

/// CDF of Student's t distribution.
pub fn student_t_cdf(t: f64, df: f64) -> f64 {
    let x = df / (df + t * t);
    let tail = 0.5 * betai(df / 2.0, 0.5, x);
    if t > 0.0 { 1.0 - tail } else { tail }
}

fn ln_gamma(x: f64) -> f64 {
    // Lanczos approximation (g = 7, n = 9)
    const C: [f64; 9] = [
        0.999_999_999_999_809_9,
        676.520_368_121_885_1,
        -1_259.139_216_722_402_8,
        771.323_428_777_653_1,
        -176.615_029_162_140_6,
        12.507_343_278_686_905,
        -0.138_571_095_265_720_12,
        9.984_369_578_019_572e-6,
        1.505_632_735_149_311_6e-7,
    ];
    if x < 0.5 {
        return (std::f64::consts::PI / (std::f64::consts::PI * x).sin()).ln() - ln_gamma(1.0 - x);
    }
    let x = x - 1.0;
    let mut a = C[0];
    let t = x + 7.5;
    for (i, c) in C.iter().enumerate().skip(1) {
        a += c / (x + i as f64);
    }
    0.5 * (2.0 * std::f64::consts::PI).ln() + (x + 0.5) * t.ln() - t + a.ln()
}

/// Regularized incomplete beta function I_x(a, b).
fn betai(a: f64, b: f64, x: f64) -> f64 {
    if x <= 0.0 {
        return 0.0;
    }
    if x >= 1.0 {
        return 1.0;
    }
    let bt = (ln_gamma(a + b) - ln_gamma(a) - ln_gamma(b) + a * x.ln() + b * (1.0 - x).ln()).exp();
    if x < (a + 1.0) / (a + b + 2.0) { bt * betacf(a, b, x) / a } else { 1.0 - bt * betacf(b, a, 1.0 - x) / b }
}

/// Continued fraction for the incomplete beta function (Numerical Recipes).
fn betacf(a: f64, b: f64, x: f64) -> f64 {
    const EPS: f64 = 1e-12;
    const FPMIN: f64 = 1e-300;
    let (qab, qap, qam) = (a + b, a + 1.0, a - 1.0);
    let mut c = 1.0;
    let mut d = 1.0 - qab * x / qap;
    if d.abs() < FPMIN {
        d = FPMIN;
    }
    d = 1.0 / d;
    let mut h = d;
    for m in 1..300 {
        let m = m as f64;
        let m2 = 2.0 * m;
        let aa = m * (b - m) * x / ((qam + m2) * (a + m2));
        d = 1.0 + aa * d;
        if d.abs() < FPMIN {
            d = FPMIN;
        }
        c = 1.0 + aa / c;
        if c.abs() < FPMIN {
            c = FPMIN;
        }
        d = 1.0 / d;
        h *= d * c;
        let aa = -(a + m) * (qab + m) * x / ((a + m2) * (qap + m2));
        d = 1.0 + aa * d;
        if d.abs() < FPMIN {
            d = FPMIN;
        }
        c = 1.0 + aa / c;
        if c.abs() < FPMIN {
            c = FPMIN;
        }
        d = 1.0 / d;
        let del = d * c;
        h *= del;
        if (del - 1.0).abs() < EPS {
            break;
        }
    }
    h
}

/// Result of fitting time-vs-size measurements to the growth classes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ComplexityFit {
    /// The simplest class that fits about as well as the best one.
    pub class: Complexity,
    /// Log-log slope over the larger sizes (≈1 linear, ≈2 quadratic).
    pub exponent: f64,
    /// Mean squared relative residual of each class's fit.
    pub residuals: Vec<(Complexity, f64)>,
    /// (size, mean ns per call) the fit used.
    pub points: Vec<(u64, f64)>,
}

/// Mean squared relative error a fit may exceed the best fit by and still
/// be preferred for being simpler (≈5% per point).
const SIMPLER_SLACK: f64 = 0.0025;

/// Classify measured growth: weighted least squares of `t ≈ a + b·f(n)`
/// (a, b ≥ 0; weights 1/t², i.e. relative error) for every class.
pub fn fit_complexity(points: &[(u64, f64)]) -> Option<ComplexityFit> {
    let mut pts: Vec<(u64, f64)> = points.iter().copied().filter(|(n, t)| *n > 0 && *t > 0.0 && t.is_finite()).collect();
    pts.sort_by_key(|p| p.0);
    pts.dedup_by_key(|p| p.0);
    if pts.len() < 3 {
        return None;
    }
    let mut residuals = Vec::new();
    for c in Complexity::ALL {
        let (mut s, mut sf, mut sff, mut st, mut sft) = (0.0, 0.0, 0.0, 0.0, 0.0);
        for &(n, t) in &pts {
            let w = 1.0 / (t * t);
            let f = c.f(n as f64);
            s += w;
            sf += w * f;
            sff += w * f * f;
            st += w * t;
            sft += w * f * t;
        }
        let det = s * sff - sf * sf;
        let (mut a, mut b) = if det.abs() <= 1e-12 * s * sff { (st / s, 0.0) } else { ((st * sff - sf * sft) / det, (s * sft - sf * st) / det) };
        if b < 0.0 {
            a = st / s;
            b = 0.0;
        }
        if a < 0.0 {
            a = 0.0;
            b = sft / sff;
        }
        let rss: f64 = pts.iter().map(|&(n, t)| ((t - a - b * c.f(n as f64)) / t).powi(2)).sum::<f64>() / pts.len() as f64;
        residuals.push((c, rss));
    }
    let best = residuals.iter().map(|r| r.1).fold(f64::INFINITY, f64::min);
    let class = residuals.iter().find(|r| r.1 <= best + SIMPLER_SLACK).map(|r| r.0)?;
    // slope over the upper sizes, where fixed per-call overhead matters least
    let upper = &pts[pts.len().saturating_sub(3.max(pts.len() / 2))..];
    let exponent = loglog_slope(upper);
    Some(ComplexityFit { class, exponent, residuals, points: pts })
}

fn loglog_slope(pts: &[(u64, f64)]) -> f64 {
    let xs: Vec<f64> = pts.iter().map(|p| (p.0 as f64).ln()).collect();
    let ys: Vec<f64> = pts.iter().map(|p| p.1.ln()).collect();
    let (mx, _) = mean_var(&xs);
    let (my, _) = mean_var(&ys);
    let sxy: f64 = xs.iter().zip(&ys).map(|(x, y)| (x - mx) * (y - my)).sum();
    let sxx: f64 = xs.iter().map(|x| (x - mx).powi(2)).sum();
    if sxx == 0.0 { 0.0 } else { sxy / sxx }
}

/// Does a fit satisfy a declared bound? The fitted class may exceed the
/// bound only when the measured slope still matches it (n log n and n are
/// hard to tell apart over a few sizes).
pub fn within(fit: &ComplexityFit, declared: Complexity) -> bool {
    fit.class <= declared || fit.exponent <= declared.exponent() + 0.3
}

#[cfg(test)]
mod tests {
    use super::*;

    fn curve(f: impl Fn(f64) -> f64) -> Vec<(u64, f64)> {
        [64u64, 256, 1024, 4096].iter().map(|&n| (n, f(n as f64))).collect()
    }

    #[test]
    fn classifies_growth() {
        assert_eq!(fit_complexity(&curve(|_| 50.0)).unwrap().class, Complexity::Constant);
        assert_eq!(fit_complexity(&curve(|n| 200.0 + 3.0 * n)).unwrap().class, Complexity::Linear);
        assert_eq!(fit_complexity(&curve(|n| 0.5 * n * n + 10.0 * n)).unwrap().class, Complexity::Quadratic);
        assert_eq!(fit_complexity(&curve(|n| 20.0 + 5.0 * n.log2())).unwrap().class, Complexity::Log);
        // 3% multiplicative noise does not change the verdict
        let noisy: Vec<(u64, f64)> =
            curve(|n| 3.0 * n).into_iter().enumerate().map(|(i, (n, t))| (n, t * if i % 2 == 0 { 1.03 } else { 0.97 })).collect();
        let f = fit_complexity(&noisy).unwrap();
        assert_eq!(f.class, Complexity::Linear);
        assert!((f.exponent - 1.0).abs() < 0.1, "{f:?}");
    }

    #[test]
    fn complexity_gate() {
        let quad = fit_complexity(&curve(|n| n * n)).unwrap();
        assert!(!within(&quad, Complexity::Linear));
        assert!(within(&quad, Complexity::Quadratic));
        let nlogn = fit_complexity(&curve(|n| n * n.log2())).unwrap();
        assert!(within(&nlogn, Complexity::Linear), "{nlogn:?}");
    }

    #[test]
    fn welch_detects_shifts() {
        let a: Vec<f64> = (0..20).map(|i| 100.0 + (i % 5) as f64).collect();
        let b: Vec<f64> = (0..20).map(|i| 80.0 + (i % 5) as f64).collect();
        let w = welch(&a, &b).unwrap();
        assert!(w.p_less < 1e-6, "{w:?}");
        let same = welch(&a, &a).unwrap();
        assert!((same.p_less - 0.5).abs() < 1e-9);
        // t(10) one-sided 5% critical value ≈ 1.812
        assert!((student_t_cdf(1.812, 10.0) - 0.95).abs() < 1e-3);
    }
}
