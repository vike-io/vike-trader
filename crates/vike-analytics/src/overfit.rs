//! Anti-overfitting statistics (López de Prado, *Advances in Financial ML*).
//!
//! Port of vike-trader-app `analysis/overfit.py`. Is the observed Sharpe significant
//! once you account for track-record length, return non-normality, and the number of
//! strategy configurations tried? PSR / deflated Sharpe / PBO-via-CSCV + a plain verdict.
//!
//! ⚠ **"Port" here means PORTED FROM, not bit-identical to CPython — and this module is where the
//! difference is largest.** The root `CLAUDE.md` says a site ported from a call Python writes as
//! builtin `sum()` must use `vike_model::py_sum`, the Neumaier-compensated fold. MEASURED
//! 2026-09-19: this file holds fifteen explicit naive `.sum::<f64>()` folds in production code
//! (`col_means`, `pearson`, `effective_n_trials`, `sample_variance`), and the Python twin does
//! write several of them as builtin `sum()`. They stay naive, deliberately:
//!
//! * `docs/decisions/0021-python-oracle-retired-vike-is-the-reference.md` retired the oracle. vike
//!   IS the reference, so "does this match CPython" is not a requirement anywhere any more.
//! * **Nothing compares this crate to Python, and nothing has for some time.** The fixture that
//!   could have — `fixtures/r2/metrics.json` — was deleted as an ORPHAN in #1277 ("a consumption
//!   gate, and the six orphans it found"), i.e. no test was replaying it even before it went.
//!   `crates/vike-analytics/tests/` holds `libm_platform_probe.rs` and `sizing_props.rs` and
//!   nothing else.
//! * Switching them would MOVE every number this module produces, in the last bits, with no test
//!   to catch a mistake and no oracle to say which answer is right. That is risk without payoff.
//!
//! `crates/vike-ops/tests/parity_fold_gate.rs` pins the count so it cannot grow quietly; the same
//! reconciliation, for the same reasons, is in `crates/vike-indicators/src/lib.rs`. What would
//! reopen this: somebody wanting these numbers compared to CPython again, which needs a fixture
//! first.

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

/// Sample variance, `n−1` denominator, as a two-pass `f64` fold.
///
/// ⚠ **This said "matching Python `statistics.variance`" until 2026-09-19, and that was FALSE — not
/// approximately, but unachievably.** CPython's `statistics.variance` does not sum in floating
/// point at all: it goes through `_ss` to `_sum`, which accumulates exact `Fraction`s over
/// `_exact_ratio` and only rounds at the end. No `f64` fold reproduces that — **neither this naive
/// one NOR `vike_model::py_sum`**, so the usual repair for a parity claim does not apply here and
/// the claim had to go rather than be honoured.
///
/// What it does instead is the ordinary two-pass definition, which is what every caller in this
/// module actually needs. See this file's module doc for why the folds here stay naive.
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

    if let Some(wf) = wf_consistency
        && wf < 0.5
    {
        points += 1;
        reasons.push(format!(
            "Only {:.0}% of walk-forward windows were profitable out-of-sample.",
            wf * 100.0
        ));
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

#[path = "overfit_tests.rs"]
#[cfg(test)]
mod overfit_tests;
