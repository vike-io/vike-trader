//! Anti-overfitting statistics (López de Prado, *Advances in Financial ML*).
//!
//! Port of vike-trader-app `analysis/overfit.py`. Is the observed Sharpe significant
//! once you account for track-record length, return non-normality, and the number of
//! strategy configurations tried? PSR / deflated Sharpe / PBO-via-CSCV + a plain verdict.

use std::f64::consts::{E, SQRT_2};

use crate::metrics;
use crate::validation::combinations;

const EULER: f64 = 0.5772156649015329;

/// Standard normal CDF Φ, via `libm::erfc` (`vike-options` libm precedent).
pub(crate) fn normal_cdf(x: f64) -> f64 {
    0.5 * libm::erfc(-x / SQRT_2)
}

/// Inverse standard normal CDF Φ⁻¹ — Acklam's rational approximation (|rel err| ≲ 1.15e-9 on
/// its own), refined by one step of Halley's rational method — Acklam's own documented
/// enhancement (<http://home.online.no/~pjacklam/notes/invnorm/>) — to push accuracy to ~1e-15
/// (full double precision). The bare rational approximation alone is "ample" in the sense the
/// original doc note intended for `expected_max_sharpe`'s single `Φ⁻¹(1 - 1/N)` call, but is a
/// *relative*-error bound: at |z| appreciably above 1 that is a few ULP shy of 1e-9 in absolute
/// terms, which is too tight for the Φ/Φ⁻¹ roundtrip gate below (measured -2.57e-9 at x=-2.5
/// pre-refinement) — the refinement closes that gap without touching the gate's tolerance.
fn normal_inv_cdf(p: f64) -> f64 {
    const A: [f64; 6] = [
        -3.969683028665376e+01,
        2.209460984245205e+02,
        -2.759285104469687e+02,
        1.38357751867269e+02,
        -3.066479806614716e+01,
        2.506628277459239e+00,
    ];
    const B: [f64; 5] = [
        -5.447609879822406e+01,
        1.615858368580409e+02,
        -1.556989798598866e+02,
        6.680131188771972e+01,
        -1.328068155288572e+01,
    ];
    const C: [f64; 6] = [
        -7.784894002430293e-03,
        -3.223964580411365e-01,
        -2.400758277161838e+00,
        -2.549732539343734e+00,
        4.374664141464968e+00,
        2.938163982698783e+00,
    ];
    const D: [f64; 4] = [
        7.784695709041462e-03,
        3.224671290700398e-01,
        2.445134137142996e+00,
        3.754408661907416e+00,
    ];
    const PLOW: f64 = 0.02425;
    const PHIGH: f64 = 1.0 - PLOW;
    if p <= 0.0 {
        return f64::NEG_INFINITY;
    }
    if p >= 1.0 {
        return f64::INFINITY;
    }
    let x = if p < PLOW {
        let q = (-2.0 * libm::log(p)).sqrt();
        (((((C[0] * q + C[1]) * q + C[2]) * q + C[3]) * q + C[4]) * q + C[5])
            / ((((D[0] * q + D[1]) * q + D[2]) * q + D[3]) * q + 1.0)
    } else if p <= PHIGH {
        let q = p - 0.5;
        let r = q * q;
        (((((A[0] * r + A[1]) * r + A[2]) * r + A[3]) * r + A[4]) * r + A[5]) * q
            / (((((B[0] * r + B[1]) * r + B[2]) * r + B[3]) * r + B[4]) * r + 1.0)
    } else {
        let q = (-2.0 * libm::log(1.0 - p)).sqrt();
        -(((((C[0] * q + C[1]) * q + C[2]) * q + C[3]) * q + C[4]) * q + C[5])
            / ((((D[0] * q + D[1]) * q + D[2]) * q + D[3]) * q + 1.0)
    };
    // Halley step: e = Φ(x) - p; u = e * sqrt(2π) * exp(x²/2); x -= u / (1 + x*u/2).
    let e = normal_cdf(x) - p;
    let u = e * (2.0 * std::f64::consts::PI).sqrt() * libm::exp(x * x / 2.0);
    x - u / (1.0 + x * u / 2.0)
}

/// Sample variance (n−1 denominator), matching Python `statistics.variance`.
fn sample_variance(xs: &[f64]) -> f64 {
    let n = xs.len();
    if n < 2 {
        return 0.0;
    }
    let mean = xs.iter().sum::<f64>() / n as f64;
    xs.iter().map(|x| (x - mean) * (x - mean)).sum::<f64>() / (n as f64 - 1.0)
}

/// The four statistical inputs every `overfit::` entry point below derives from ONE equity
/// curve — see [`sharpe_moments`], which is the only supported way to build one.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SharpeMoments {
    /// **PER-PERIOD** (per-observation) Sharpe — NOT annualized. Feed this to
    /// [`probabilistic_sharpe_ratio`] / [`deflated_sharpe_ratio`] /
    /// [`deflated_sharpe_with_effective_n`] as both the observed value and each trial.
    pub sr_per_obs: f64,
    /// Number of per-bar returns behind `sr_per_obs` (== `metrics::returns(curve).len()`), the
    /// `n` those functions want. Unclamped — see [`sharpe_moments`].
    pub n_obs: usize,
    /// Sample skewness of the same returns (`metrics::returns_skewness`), same convention the
    /// `skew` parameter expects — passed through as-is.
    pub skew: f64,
    /// **NON-EXCESS** kurtosis (normal == 3), already rebased — see [`sharpe_moments`].
    pub kurt: f64,
}

/// Derive the [`SharpeMoments`] an `overfit::` call needs from an equity curve. THE single home
/// for two unit conventions, each of which has its own footgun — one of which already shipped as
/// a real bug (pinned by `vike-ai`'s `deflate_batch_pins_per_period_dsr_magnitude`):
///
/// - **Sharpe frequency — `sr_per_obs` is per-period, never annualized.** The textbook Bailey &
///   López de Prado PSR is `Φ((sr - sr_star)·sqrt(n-1)/denom)` with NO periods-per-year factor
///   and a `denom = sqrt(1 - skew·sr + (kurt-1)/4·sr²)` (clamped to 1e-12) that uses `sr`
///   directly — so an ANNUALIZED `sr` double-counts annualization through `sqrt(n-1)` and blows
///   up `denom`, saturating PSR/DSR to ~1.0 and making any significance test vacuous. Computed
///   here via [`metrics::risk_return_ratio`] (= `mean/std`), the direct per-period twin of
///   `metrics::sharpe(curve, ppy)` (= `mean/std · sqrt(ppy)`). A caller that also DISPLAYS an
///   annualized Sharpe keeps computing that separately with `metrics::sharpe` — the two are
///   different quantities for different consumers, and only this one may reach `overfit::`.
/// - **Kurtosis rebase — `kurt` is non-excess (normal == 3).** [`metrics::returns_kurtosis`]
///   reports EXCESS kurtosis (pandas `.kurt()` convention, normal == 0) while the `kurt`
///   parameter is documented non-excess, so it is rebased by `+3.0` exactly once, here.
///
/// `n_obs` is deliberately UNCLAMPED: a curve with fewer than 2 returns yields `n_obs < 2`, which
/// [`probabilistic_sharpe_ratio`]'s own `n < 2` guard turns into an honest `0.0` rather than a
/// number derived from a degenerate curve.
pub fn sharpe_moments(equity_curve: &[f64]) -> SharpeMoments {
    SharpeMoments {
        sr_per_obs: metrics::risk_return_ratio(equity_curve),
        n_obs: metrics::returns(equity_curve).len(),
        skew: metrics::returns_skewness(equity_curve),
        kurt: 3.0 + metrics::returns_kurtosis(equity_curve),
    }
}

/// P(true Sharpe > `sr_star`) given observed per-observation Sharpe `sr` over `n`
/// observations; `kurt` is non-excess (normal = 3). Python `probabilistic_sharpe_ratio`.
pub fn probabilistic_sharpe_ratio(sr: f64, n: usize, sr_star: f64, skew: f64, kurt: f64) -> f64 {
    if n < 2 {
        return 0.0;
    }
    let denom = (1.0 - skew * sr + ((kurt - 1.0) / 4.0) * sr * sr).max(1e-12).sqrt();
    normal_cdf((sr - sr_star) * ((n - 1) as f64).sqrt() / denom)
}

/// Expected maximum Sharpe across `n_trials` independent trials (the DSR benchmark).
/// Python `expected_max_sharpe`.
pub fn expected_max_sharpe(var_trials: f64, n_trials: usize) -> f64 {
    if n_trials < 2 || var_trials <= 0.0 {
        return 0.0;
    }
    let nt = n_trials as f64;
    let z1 = normal_inv_cdf(1.0 - 1.0 / nt);
    let z2 = normal_inv_cdf(1.0 - 1.0 / (nt * E));
    var_trials.sqrt() * ((1.0 - EULER) * z1 + EULER * z2)
}

/// Deflated Sharpe: PSR benchmarked against the expected max Sharpe of the trials.
/// Python `deflated_sharpe_ratio`.
pub fn deflated_sharpe_ratio(
    observed_sr: f64,
    trial_sharpes: &[f64],
    n_obs: usize,
    skew: f64,
    kurt: f64,
) -> f64 {
    let var_trials = sample_variance(trial_sharpes);
    let sr_star = expected_max_sharpe(var_trials, trial_sharpes.len());
    probabilistic_sharpe_ratio(observed_sr, n_obs, sr_star, skew, kurt)
}

/// Column means over the given rows of a T×N matrix.
fn col_means(matrix: &[Vec<f64>], rows: &[usize], n_cols: usize) -> Vec<f64> {
    let denom = rows.len() as f64;
    (0..n_cols).map(|j| rows.iter().map(|&t| matrix[t][j]).sum::<f64>() / denom).collect()
}

/// Probability of Backtest Overfitting via CSCV. `matrix` is T observations × N trials of
/// per-observation performance. Python `pbo_cscv`. Non-finite anywhere -> `NaN` (uncomputable,
/// honest — NOT 0.0, which reads as "no overfit"). Panics if `n_splits` is odd.
pub fn pbo_cscv(matrix: &[Vec<f64>], n_splits: usize) -> f64 {
    assert!(n_splits.is_multiple_of(2), "n_splits must be even for CSCV");
    let t = matrix.len();
    if t == 0 {
        return f64::NAN;
    }
    let n_cols = matrix[0].len();
    if matrix.iter().flatten().any(|v| !v.is_finite()) {
        return f64::NAN;
    }
    let bounds: Vec<(usize, usize)> =
        (0..n_splits).map(|g| (g * t / n_splits, (g + 1) * t / n_splits)).collect();
    let groups: Vec<Vec<usize>> = bounds.iter().map(|&(a, b)| (a..b).collect()).collect();

    let mut logits: Vec<f64> = Vec::new();
    for combo in combinations(n_splits, n_splits / 2) {
        let combo_set: std::collections::HashSet<usize> = combo.iter().copied().collect();
        let is_rows: Vec<usize> = combo.iter().flat_map(|&g| groups[g].iter().copied()).collect();
        let oos_rows: Vec<usize> = (0..n_splits)
            .filter(|g| !combo_set.contains(g))
            .flat_map(|g| groups[g].iter().copied())
            .collect();
        if is_rows.is_empty() || oos_rows.is_empty() {
            continue;
        }
        let is_perf = col_means(matrix, &is_rows, n_cols);
        let oos_perf = col_means(matrix, &oos_rows, n_cols);
        // first-argmax (mirror Python `max(range, key=...)`, which returns the first max).
        let mut best = 0usize;
        for j in 1..n_cols {
            if is_perf[j] > is_perf[best] {
                best = j;
            }
        }
        let rank = (0..n_cols).filter(|&j| oos_perf[j] <= oos_perf[best]).count();
        let omega = rank as f64 / (n_cols as f64 + 1.0);
        if !(0.0 < omega && omega < 1.0) {
            continue;
        }
        logits.push(libm::log(omega / (1.0 - omega)));
    }
    if logits.is_empty() {
        return f64::NAN;
    }
    logits.iter().filter(|&&lam| lam <= 0.0).count() as f64 / logits.len() as f64
}

fn pearson(a: &[f64], b: &[f64]) -> f64 {
    let n = a.len().min(b.len());
    if n < 2 {
        return 0.0;
    }
    let (a, b) = (&a[..n], &b[..n]);
    let ma = a.iter().sum::<f64>() / n as f64;
    let mb = b.iter().sum::<f64>() / n as f64;
    let cov: f64 = (0..n).map(|i| (a[i] - ma) * (b[i] - mb)).sum();
    let va: f64 = a.iter().map(|x| (x - ma) * (x - ma)).sum();
    let vb: f64 = b.iter().map(|x| (x - mb) * (x - mb)).sum();
    if va <= 0.0 || vb <= 0.0 {
        return 0.0;
    }
    cov / (va * vb).sqrt()
}

/// Correlation-aware effective trial count: `N / (1 + (N-1)·max(mean_corr, 0))`, clamped `[1, N]`.
/// Perfectly-correlated trials collapse to ~1; uncorrelated/anti-correlated stay ~N. Python
/// `effective_n_trials` (the pure pairwise-Pearson path; the numpy fast-path is perf-only).
pub fn effective_n_trials(return_series: &[Vec<f64>]) -> f64 {
    let n = return_series.len();
    if n == 0 {
        return 0.0;
    }
    if n == 1 {
        return 1.0;
    }
    let mut corrs: Vec<f64> = Vec::new();
    for i in 0..n {
        for j in (i + 1)..n {
            corrs.push(pearson(&return_series[i], &return_series[j]));
        }
    }
    let avg = if corrs.is_empty() { 0.0 } else { corrs.iter().sum::<f64>() / corrs.len() as f64 };
    let r = avg.max(0.0);
    let eff = n as f64 / (1.0 + (n as f64 - 1.0) * r);
    eff.clamp(1.0, n as f64)
}

/// DSR benchmarked against expected-max-Sharpe computed with the EFFECTIVE (correlation-corrected)
/// trial count. Python `deflated_sharpe_with_effective_n`.
pub fn deflated_sharpe_with_effective_n(
    observed_sr: f64,
    trial_sharpes: &[f64],
    trial_return_series: &[Vec<f64>],
    n_obs: usize,
    skew: f64,
    kurt: f64,
) -> f64 {
    let eff = (effective_n_trials(trial_return_series).round() as usize).max(1);
    let var_trials = sample_variance(trial_sharpes);
    let sr_star = expected_max_sharpe(var_trials, eff);
    probabilistic_sharpe_ratio(observed_sr, n_obs, sr_star, skew, kurt)
}

/// Plain-language overfitting risk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OverfitLevel {
    Low,
    Medium,
    High,
}

/// A verdict: a level + the human-readable reasons behind it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Verdict {
    pub level: OverfitLevel,
    pub reasons: Vec<String>,
}

/// Combine PBO, deflated Sharpe, and (optional) walk-forward consistency into a label.
/// Python `overfit_verdict` (same point thresholds + NaN-PBO "not assessed" branch).
pub fn overfit_verdict(pbo: f64, deflated_sr: f64, wf_consistency: Option<f64>) -> Verdict {
    let mut points = 0;
    let mut reasons: Vec<String> = Vec::new();

    if pbo.is_nan() {
        reasons.push("PBO not assessed (degenerate or non-finite returns matrix).".to_string());
    } else if pbo > 0.5 {
        points += 2;
        reasons.push(format!(
            "PBO {:.0}%: the selected configuration is more likely than not overfit.",
            pbo * 100.0
        ));
    } else if pbo > 0.2 {
        points += 1;
        reasons.push(format!("PBO {:.0}%: moderate chance the result is curve-fit.", pbo * 100.0));
    }

    if deflated_sr < 0.5 {
        points += 2;
        reasons.push(format!(
            "Deflated Sharpe {:.0}%: edge not significant after the number of trials.",
            deflated_sr * 100.0
        ));
    } else if deflated_sr < 0.9 {
        points += 1;
        reasons.push(format!(
            "Deflated Sharpe {:.0}%: significance is borderline.",
            deflated_sr * 100.0
        ));
    }

    if let Some(wf) = wf_consistency {
        if wf < 0.5 {
            points += 1;
            reasons.push(format!(
                "Only {:.0}% of walk-forward windows were profitable out-of-sample.",
                wf * 100.0
            ));
        }
    }

    let level = if points >= 3 {
        OverfitLevel::High
    } else if points >= 1 {
        OverfitLevel::Medium
    } else {
        OverfitLevel::Low
    };
    if reasons.is_empty() {
        reasons.push("No major overfitting flags.".to_string());
    }
    Verdict { level, reasons }
}

/// The multiple-testing audit of a SELECTION: the overfitting gates applied to the FULL
/// candidate set a winner was chosen from — never to the winner alone.
#[derive(Clone, Debug, PartialEq)]
pub struct SelectionAudit {
    /// How many candidates the winner was selected FROM. `1` means no selection was
    /// accounted for — the first number to check when a result looks too good.
    pub n_candidates: usize,
    /// Correlation-corrected trial count over ALL candidates ([`effective_n_trials`]).
    pub effective_trials: f64,
    /// The selected candidate's Sharpe, as supplied.
    pub selected_sharpe: f64,
    /// Deflated Sharpe benchmarked against the effective trial count of the FULL set.
    pub deflated_sr: f64,
    /// Probability of backtest overfitting over the FULL candidate matrix.
    pub pbo: f64,
    /// Trades taken by the selected candidate. Reported ALONGSIDE Sharpe because a
    /// mean-reversion family dies by STARVATION (too few divergences) before it dies by
    /// unprofitability, and a Sharpe-ranked sweep cannot see that.
    pub selected_trades: usize,
    /// The combined plain-language verdict.
    pub verdict: Verdict,
}

/// Audit a selection against the FULL candidate set it was drawn from.
///
/// **Why this exists.** [`effective_n_trials`] derives its count from the trials it is
/// FED, and [`pbo_cscv`]'s CSCV matrix columns must be the full candidate set. Feeding
/// either only the SURVIVORS of a search deflates against an `n_trials` that understates
/// by orders of magnitude, and the gates will happily pass garbage — they are only as good
/// as the trial count they are given.
///
/// That failure is silent and easy to commit: pick the best pair out of fifty, hand its
/// curve alone to [`deflated_sharpe_ratio`], read a reassuring number. This function takes
/// the full candidate arrays PLUS the selected index precisely so the mistake is visible —
/// a survivors-only call reports `n_candidates == 1` and says so in the verdict.
///
/// It matters most for PAIR SELECTION, a maximum over `N(N−1)/2` correlated candidate
/// spreads — a search far larger than any parameter sweep, and one that happens BEFORE the
/// sweep. Published work measures the probability that an in-sample cointegrated pair is
/// still cointegrated out-of-sample at ~4.9% against a ~5.0% unconditional base rate:
/// indistinguishable from a 5%-level test's false-positive rate. Pair search is exactly
/// where a spurious winner enters.
///
/// `None` when the inputs are inconsistent (empty set, length mismatch, out-of-range
/// `selected`) — an unauditable selection is not a passing one.
#[allow(clippy::too_many_arguments)]
pub fn audit_selection(
    trial_sharpes: &[f64],
    trial_returns: &[Vec<f64>],
    selected: usize,
    selected_trades: usize,
    n_obs: usize,
    skew: f64,
    kurt: f64,
    n_splits: usize,
) -> Option<SelectionAudit> {
    if trial_sharpes.is_empty()
        || trial_sharpes.len() != trial_returns.len()
        || selected >= trial_sharpes.len()
    {
        return None;
    }
    let selected_sharpe = trial_sharpes[selected];
    let effective_trials = effective_n_trials(trial_returns);
    let deflated_sr = deflated_sharpe_with_effective_n(
        selected_sharpe,
        trial_sharpes,
        trial_returns,
        n_obs,
        skew,
        kurt,
    );
    let pbo = pbo_cscv(trial_returns, n_splits);
    let mut verdict = overfit_verdict(pbo, deflated_sr, None);
    if trial_sharpes.len() == 1 {
        verdict.reasons.push(
            "Only ONE candidate was audited: if a winner was selected from a larger set, \
             these gates are being fed the survivor and understate the trial count."
                .to_string(),
        );
    }
    Some(SelectionAudit {
        n_candidates: trial_sharpes.len(),
        effective_trials,
        selected_sharpe,
        deflated_sr,
        pbo,
        selected_trades,
        verdict,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) {
        assert!((a - b).abs() <= 1e-12 * b.abs().max(1.0), "{a} vs {b}");
    }

    #[test]
    fn cdf_and_inverse_roundtrip() {
        assert!((normal_cdf(0.0) - 0.5).abs() < 1e-12);
        for &x in &[-2.5, -1.0, -0.3, 0.7, 1.96, 3.0] {
            assert!((normal_inv_cdf(normal_cdf(x)) - x).abs() < 1e-9, "roundtrip {x}");
        }
        // Φ(1.96) ≈ 0.975
        assert!((normal_cdf(1.96) - 0.9750021048517795).abs() < 1e-9);
    }

    /// Build an equity curve (starting at 100.0) whose per-bar simple returns are exactly `rets`.
    fn curve_from_returns(rets: &[f64]) -> Vec<f64> {
        let mut eq = vec![100.0];
        for &r in rets {
            let last = *eq.last().unwrap();
            eq.push(last * (1.0 + r));
        }
        eq
    }

    /// The convention pin for the helper the three DSR/PSR call sites share: `sr_per_obs` must be
    /// the PER-PERIOD Sharpe (never annualized) and `kurt` must be NON-EXCESS. Asserted against
    /// the `metrics::` primitives directly — the two footguns consolidated in the doc comment.
    #[test]
    fn sharpe_moments_are_per_period_and_non_excess() {
        let rets: Vec<f64> = (0..48).map(|i| 0.002 + 0.01 * ((i % 4) as f64 - 1.5)).collect();
        let curve = curve_from_returns(&rets);
        let m = sharpe_moments(&curve);

        // per-period Sharpe == mean/std, NOT the annualized metrics::sharpe.
        assert_eq!(m.sr_per_obs, metrics::risk_return_ratio(&curve));
        assert_eq!(m.n_obs, 48);
        assert_eq!(m.skew, metrics::returns_skewness(&curve));
        // kurt is rebased to non-excess (normal == 3) exactly once.
        assert_eq!(m.kurt, metrics::returns_kurtosis(&curve) + 3.0);

        // The annualized value is a DIFFERENT quantity, larger by ~sqrt(252) — feeding it to
        // overfit:: is the bug this helper exists to prevent (it saturates DSR to ~1.0).
        let annualized = metrics::sharpe(&curve, 252.0);
        assert!((annualized / 252.0_f64.sqrt() - m.sr_per_obs).abs() < 1e-12);
        assert!(annualized.abs() > m.sr_per_obs.abs() * 10.0, "annualized must be far larger");
        assert!(
            probabilistic_sharpe_ratio(annualized, m.n_obs, 0.0, m.skew, m.kurt) > 0.999,
            "the annualized wiring is the saturating one"
        );
    }

    /// `n_obs` is unclamped by design: a degenerate curve falls through to
    /// `probabilistic_sharpe_ratio`'s own `n < 2` guard -> an honest 0.0.
    #[test]
    fn sharpe_moments_leave_a_degenerate_curve_to_the_psr_guard() {
        for curve in [vec![], vec![100.0], vec![100.0, 101.0]] {
            let m = sharpe_moments(&curve);
            assert!(m.n_obs < 2, "curve {curve:?} -> n_obs {}", m.n_obs);
            assert_eq!(probabilistic_sharpe_ratio(m.sr_per_obs, m.n_obs, 0.0, m.skew, m.kurt), 0.0);
        }
    }

    #[test]
    fn psr_degenerate_and_known() {
        assert_eq!(probabilistic_sharpe_ratio(1.0, 1, 0.0, 0.0, 3.0), 0.0); // n<2
                                                                            // sr=0, n>=2, sr_star=0 -> Φ(0) = 0.5
        close(probabilistic_sharpe_ratio(0.0, 100, 0.0, 0.0, 3.0), 0.5);
        // skew=0, kurt=3, sr_star=0: PSR = Φ(sr*sqrt(n-1) / sqrt(1 + (kurt-1)/4*sr^2)).
        // NOTE the denom's (kurt-1)/4*sr^2 term only vanishes at sr=0 (already covered above),
        // not merely at kurt=3 (non-excess-normal) — skew=0 alone only kills the -skew*sr term.
        let sr = 0.5_f64;
        let denom = (1.0 + 0.5 * sr * sr).sqrt();
        let expected = normal_cdf(sr * (100.0 - 1.0f64).sqrt() / denom);
        close(probabilistic_sharpe_ratio(0.5, 100, 0.0, 0.0, 3.0), expected);
    }

    #[test]
    fn expected_max_sharpe_degenerate() {
        assert_eq!(expected_max_sharpe(0.1, 1), 0.0); // n_trials < 2
        assert_eq!(expected_max_sharpe(0.0, 5), 0.0); // var <= 0
        assert!(expected_max_sharpe(0.25, 10) > 0.0); // grows with trials
    }

    #[test]
    fn deflated_sharpe_below_psr_when_many_trials() {
        // Same observed SR, but benchmarking against expected-max of 20 varied trials
        // deflates the significance below the raw PSR (sr_star > 0).
        let trials: Vec<f64> = (0..20).map(|i| 0.1 + 0.02 * i as f64).collect();
        let dsr = deflated_sharpe_ratio(0.8, &trials, 250, 0.0, 3.0);
        let psr = probabilistic_sharpe_ratio(0.8, 250, 0.0, 0.0, 3.0);
        assert!(dsr < psr, "dsr {dsr} should be < psr {psr}");
        assert!((0.0..=1.0).contains(&dsr));
    }

    #[test]
    fn pbo_requires_even_splits_and_flags_nonfinite() {
        let m = vec![vec![0.1, 0.2], vec![0.1, 0.2]];
        assert!(pbo_cscv(&m, 2).is_finite() || pbo_cscv(&m, 2).is_nan());
        let bad = vec![vec![f64::NAN, 0.2], vec![0.1, 0.2]];
        assert!(pbo_cscv(&bad, 2).is_nan());
    }

    #[test]
    #[should_panic(expected = "even")]
    fn pbo_odd_splits_panics() {
        let m = vec![vec![0.1, 0.2]; 4];
        let _ = pbo_cscv(&m, 3);
    }

    #[test]
    fn effective_n_collapses_correlated_trials() {
        // Two identical series -> effective N ~ 1; two anti-correlated -> ~2.
        let same = vec![vec![1.0, 2.0, 3.0, 4.0], vec![1.0, 2.0, 3.0, 4.0]];
        assert!((effective_n_trials(&same) - 1.0).abs() < 1e-9);
        let anti = vec![vec![1.0, 2.0, 3.0, 4.0], vec![4.0, 3.0, 2.0, 1.0]];
        assert!((effective_n_trials(&anti) - 2.0).abs() < 1e-9); // max(neg_corr,0)=0 -> N
        assert_eq!(effective_n_trials(&[]), 0.0);
    }

    #[test]
    fn verdict_levels_and_nan_pbo() {
        // clean: low pbo, high dsr, consistent -> Low
        assert_eq!(overfit_verdict(0.1, 0.95, Some(0.8)).level, OverfitLevel::Low);
        // pbo>0.5 (2) + dsr<0.5 (2) -> High
        assert_eq!(overfit_verdict(0.6, 0.3, None).level, OverfitLevel::High);
        // NaN pbo is surfaced, not treated as "no overfit"
        let v = overfit_verdict(f64::NAN, 0.95, None);
        assert!(v.reasons.iter().any(|r| r.contains("not assessed")));
    }

    // --- selection audit (multiple-testing discipline) ---

    /// Deterministic pseudo-random return series (no `rand` dep, no seeded-RNG drift):
    /// a hash-mixed LCG per candidate so the trials are uncorrelated but reproducible.
    fn synth_trial(seed: u64, n: usize, drift: f64) -> Vec<f64> {
        let mut s = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
        (0..n)
            .map(|_| {
                s = s
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                let u = ((s >> 11) as f64) / ((1u64 << 53) as f64); // [0,1)
                (u - 0.5) * 0.02 + drift
            })
            .collect()
    }

    fn sharpe_of(returns: &[f64]) -> f64 {
        let n = returns.len() as f64;
        let mean = returns.iter().sum::<f64>() / n;
        let var = returns.iter().map(|r| (r - mean) * (r - mean)).sum::<f64>() / n;
        if var <= 0.0 {
            return 0.0;
        }
        mean / var.sqrt()
    }

    /// THE TRAP, DEMONSTRATED — not merely documented.
    ///
    /// Twenty candidates, of which exactly one carries a small drift so it wins the
    /// selection with an unambiguously POSITIVE Sharpe (keeping both PSRs off the
    /// 0.0 floor, so the comparison below is strict and deterministic rather than
    /// dependent on which way a zero-drift sample happened to fall).
    ///
    /// Auditing that winner ALONE (the survivors-only mistake) reports a materially
    /// higher deflated Sharpe than auditing it against the full set it was drawn from
    /// — because a single trial has zero trial-variance, so `expected_max_sharpe`
    /// benchmarks it against 0 instead of against the best-of-20 a selection implies.
    /// If this assertion ever flips, the gates have stopped accounting for selection.
    #[test]
    fn auditing_only_the_survivor_inflates_significance() {
        let n_obs = 250usize;
        let returns: Vec<Vec<f64>> = (0..20)
            .map(|i| synth_trial(i as u64, n_obs, if i == 7 { 0.0012 } else { 0.0 }))
            .collect();
        let sharpes: Vec<f64> = returns.iter().map(|r| sharpe_of(r)).collect();
        let best = sharpes
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .map(|(i, _)| i)
            .unwrap();

        let full = audit_selection(&sharpes, &returns, best, 42, n_obs, 0.0, 3.0, 8).unwrap();
        let survivor_only = audit_selection(
            &sharpes[best..=best],
            &returns[best..=best],
            0,
            42,
            n_obs,
            0.0,
            3.0,
            8,
        )
        .unwrap();

        assert_eq!(best, 7, "the drifted candidate must win the selection");
        assert!(
            full.selected_sharpe > 0.0,
            "winner Sharpe must be positive: {}",
            full.selected_sharpe
        );
        assert_eq!(full.n_candidates, 20);
        assert_eq!(survivor_only.n_candidates, 1);
        assert!(
            full.deflated_sr < survivor_only.deflated_sr,
            "auditing the full candidate set must NOT be more permissive than auditing the \
             survivor alone: full {} vs survivor-only {}",
            full.deflated_sr,
            survivor_only.deflated_sr
        );
    }

    /// A survivors-only audit must SAY so — the failure is silent otherwise.
    #[test]
    fn a_single_candidate_audit_warns_that_selection_is_unaccounted() {
        let r = vec![synth_trial(7, 120, 0.0005)];
        let s = vec![sharpe_of(&r[0])];
        let a = audit_selection(&s, &r, 0, 10, 120, 0.0, 3.0, 4).unwrap();
        assert_eq!(a.n_candidates, 1);
        assert!(
            a.verdict.reasons.iter().any(|m| m.contains("Only ONE candidate")),
            "expected an explicit single-candidate warning, got {:?}",
            a.verdict.reasons
        );
    }

    /// Trade count rides alongside Sharpe: this family starves before it loses money.
    #[test]
    fn trade_count_is_reported_with_the_verdict() {
        let returns: Vec<Vec<f64>> = (0..4).map(|i| synth_trial(i as u64, 100, 0.0)).collect();
        let sharpes: Vec<f64> = returns.iter().map(|r| sharpe_of(r)).collect();
        let a = audit_selection(&sharpes, &returns, 1, 3, 100, 0.0, 3.0, 4).unwrap();
        assert_eq!(a.selected_trades, 3, "a 3-trade winner must surface its trade count");
        assert!(a.effective_trials >= 1.0 && a.effective_trials <= 4.0);
    }

    /// An unauditable selection is not a passing one.
    #[test]
    fn inconsistent_inputs_are_none() {
        let r = vec![synth_trial(1, 50, 0.0)];
        let s = vec![0.5, 0.7]; // length mismatch
        assert!(audit_selection(&s, &r, 0, 1, 50, 0.0, 3.0, 4).is_none());
        assert!(audit_selection(&[], &[], 0, 1, 50, 0.0, 3.0, 4).is_none());
        let s1 = vec![0.5];
        assert!(
            audit_selection(&s1, &r, 5, 1, 50, 0.0, 3.0, 4).is_none(),
            "selected index out of range"
        );
    }
}
