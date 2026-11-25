//! Anti-overfitting statistics (López de Prado, *Advances in Financial ML*).
//!
//! Port of vike-trader-app `analysis/overfit.py`. Is the observed Sharpe significant
//! once you account for track-record length, return non-normality, and the number of
//! strategy configurations tried? PSR / deflated Sharpe / PBO-via-CSCV + a plain verdict.
//!
//! ⚠ **"Port" here means PORTED FROM, not bit-identical to CPython — and this module is where the
//! difference is largest.** The root `CLAUDE.md` says a site ported from a call Python writes as
//! builtin `sum()` must use `vike_math::summation::compensated_sum`, the Neumaier-compensated fold. MEASURED
//! 2026-09-19: this file holds fifteen explicit naive `.sum::<f64>()` folds in production code
//! (`col_means`, `pearson`, `effective_n_trials`, `sample_variance`), and the Python twin does
//! write several of them as builtin `sum()`. Two are compensated since 2026-11-18, by the owner's
//! ruling: the Pearson correlation and the sample variance are `vike-math`'s one `pearson` and one
//! `mean_variance` (decision 0294's amendment of that day lists the values that moved). The column
//! means and `effective_n_trials`' average stay the plain fold (`vike_math::statistics::plain_mean`),
//! deliberately:
//!
//! * `docs/decisions/0021-python-oracle-retired-vike-is-the-reference.md` retired the oracle. vike
//!   IS the reference, so "does this match CPython" is not a requirement anywhere any more.
//! * **Nothing compares this crate to Python, and nothing has for some time.** The fixture that
//!   could have — `fixtures/r2/metrics.json` — was deleted as an ORPHAN in #1277 ("a consumption
//!   gate, and the six orphans it found"), i.e. no test was replaying it even before it went.
//!   `crates/vike-analytics/tests/` holds `libm_platform_probe.rs` and `research_property_tests.rs`,
//!   neither of which compares to Python.
//! * Switching them would MOVE every number this module produces, in the last bits, with no test
//!   to catch a mistake and no oracle to say which answer is right. That is risk without payoff.
//!
//! `crates/vike-ops/tests/architecture/parity_fold_gate.rs` pins the count so it cannot grow quietly; the same
//! reconciliation, for the same reasons, is in `crates/vike-indicators/src/lib.rs`. What would
//! reopen this: somebody wanting these numbers compared to CPython again, which needs a fixture
//! first.

use std::f64::consts::{E, EULER_GAMMA};

use vike_math::distributions::{normal_cdf_complementary_form, normal_quantile};
use vike_math::special_functions::logit;
use vike_math::statistics::{mean_variance, pearson, plain_mean};

use crate::metrics;
use crate::validation::combinations;

/// Sample variance, `n−1` denominator, the two-pass compensated fold
/// (`vike_math::statistics::mean_variance`, the one variance since 2026-11-18), with this module's
/// `0.0` under two values.
///
/// ⚠ **This said "matching Python `statistics.variance`" until 2026-09-19, and that was FALSE — not
/// approximately, but unachievably.** CPython's `statistics.variance` does not sum in floating
/// point at all: it goes through `_ss` to `_sum`, which accumulates exact `Fraction`s over
/// `_exact_ratio` and only rounds at the end. No `f64` fold reproduces that — **neither this naive
/// one NOR `vike_math::summation::compensated_sum`**, so the usual repair for a parity claim does not apply here and
/// the claim had to go rather than be honoured.
///
/// What it does instead is the ordinary two-pass definition, which is what every caller in this
/// module actually needs.
fn sample_variance(xs: &[f64]) -> f64 {
    if xs.len() < 2 {
        return 0.0;
    }
    mean_variance(xs.iter().copied(), 1.0).1
}

/// The four statistical inputs every `overfit::` entry point below derives from ONE equity
/// curve — see [`sharpe_moments`], which is the only supported way to build one from a curve
/// ([`OverfitSharpeMoments::of`] builds one from moments a report already computed).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OverfitSharpeMoments {
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

/// Derive the [`OverfitSharpeMoments`] an `overfit::` call needs from an equity curve. THE single home
/// for two unit conventions, each of which has its own footgun — one of which already shipped as
/// a real bug (pinned by `vike-ai`'s `deflate_batch_pins_per_period_dsr_magnitude`):
///
/// - **Sharpe frequency — `sr_per_obs` is per-period, never annualized.** The textbook Bailey &
///   López de Prado PSR is `Φ((sharpe - sharpe_star)·sqrt(n-1)/denom)` with NO periods-per-year factor
///   and a `denominator = sqrt(1 - skew·sharpe + (kurt-1)/4·sharpe²)` (clamped to 1e-12) that uses `sharpe`
///   directly — so an ANNUALIZED `sharpe` double-counts annualization through `sqrt(n-1)` and blows
///   up `denominator`, saturating PSR/DSR to ~1.0 and making any significance test vacuous. Computed
///   here via [`metrics::risk_return_ratio`] (= `mean/std`), the direct per-period twin of
///   `metrics::sharpe(curve, periods_per_year)` (= `mean/std · sqrt(periods_per_year)`). A caller that also DISPLAYS an
///   annualized Sharpe keeps computing that separately with `metrics::sharpe` — the two are
///   different quantities for different consumers, and only this one may reach `overfit::`.
/// - **Kurtosis rebase — `kurt` is non-excess (normal == 3).** [`metrics::returns_kurtosis`]
///   reports EXCESS kurtosis (pandas `.kurt()` convention, normal == 0) while the `kurt`
///   parameter is documented non-excess, so it is rebased by `+3.0` exactly once, here.
///
/// `n_obs` is deliberately UNCLAMPED: a curve with fewer than 2 returns yields `n_obs < 2`, which
/// [`probabilistic_sharpe_ratio`]'s own `n < 2` guard turns into an honest `0.0` rather than a
/// number derived from a degenerate curve.
pub fn sharpe_moments(equity_curve: &[f64]) -> OverfitSharpeMoments {
    OverfitSharpeMoments::of(
        metrics::risk_return_ratio(equity_curve),
        metrics::returns(equity_curve).len(),
        metrics::returns_skewness(equity_curve),
        metrics::returns_kurtosis(equity_curve),
    )
}

impl OverfitSharpeMoments {
    /// The moments of `n_obs` returns whose per-period Sharpe, skewness and EXCESS kurtosis are already in hand
    /// (`crate::results::TradingResultsExtendedMetrics::sharpe_moments`, [`sharpe_moments`]): the one place the
    /// kurtosis is rebased to non-excess.
    pub fn of(sr_per_obs: f64, n_obs: usize, skew: f64, excess_kurtosis: f64) -> Self {
        Self { sr_per_obs, n_obs, skew, kurt: 3.0 + excess_kurtosis }
    }

    /// P(true Sharpe > 0) under these moments: [`psr_of_curve`]'s number.
    pub fn psr(&self) -> f64 {
        probabilistic_sharpe_ratio(self.sr_per_obs, self.n_obs, 0.0, self.skew, self.kurt)
    }
}

/// P(true Sharpe > `sharpe_star`) given observed per-observation Sharpe `sharpe` over `n`
/// observations; `kurt` is non-excess (normal = 3). Python `probabilistic_sharpe_ratio`.
pub fn probabilistic_sharpe_ratio(
    sharpe: f64,
    n: usize,
    sharpe_star: f64,
    skew: f64,
    kurt: f64,
) -> f64 {
    if n < 2 {
        return 0.0;
    }
    let denominator = sharpe_variance(sharpe, skew, kurt).sqrt();
    normal_cdf_complementary_form((sharpe - sharpe_star) * ((n - 1) as f64).sqrt() / denominator)
}

/// The variance term of a per-observation Sharpe, `1 − skew·SR + (kurtosis − 1)/4·SR²` (non-excess
/// `kurtosis`), clamped at `1e-12`: the ONE copy [`probabilistic_sharpe_ratio`] and
/// [`minimum_track_record_length`] read.
fn sharpe_variance(sharpe: f64, skew: f64, kurtosis: f64) -> f64 {
    (1.0 - skew * sharpe + ((kurtosis - 1.0) / 4.0) * sharpe * sharpe).max(1e-12)
}

/// P(true Sharpe > 0) of ONE equity curve: [`probabilistic_sharpe_ratio`] over that curve's
/// [`sharpe_moments`], so a single-run surface (the Studio's Validation tab) asks for the number
/// instead of assembling the per-observation inputs, and their non-excess kurtosis, itself.
pub fn psr_of_curve(equity_curve: &[f64]) -> f64 {
    sharpe_moments(equity_curve).psr()
}

/// How many observations a track record needs before its Sharpe tells `sharpe` apart from
/// `sharpe_star` at `confidence`, with probability `power` (the forward test's question: how many days,
/// or trades, before luck and edge can be told apart). Bailey & López de Prado (2012), "The Sharpe ratio
/// efficient frontier", the minimum track record length, generalised by a power term:
///
/// `n = 1 + (1 − skew·SR + (kurtosis − 1)/4·SR²) · ((z(confidence) + z(power)) / (SR − SR*))²`
///
/// - Per-observation Sharpe, NOT annualized, with non-excess `kurtosis` (normal = 3): the units of
///   [`probabilistic_sharpe_ratio`], whose test this inverts. At `power = 0.5` this IS their MinTRL, and
///   [`probabilistic_sharpe_ratio`] at that `n` equals `confidence`.
/// - `power` is the chance a record of that length rejects `sharpe_star` when the true Sharpe is
///   `sharpe`: the variance term is taken at `sharpe`, as [`probabilistic_sharpe_ratio`] takes it at the
///   observed one.
/// - One-sided in the direction of the difference: `sharpe` below `sharpe_star` asks how long until a
///   record shows it is WORSE. Equal Sharpes never part: `f64::INFINITY`.
/// - `NaN` for a `confidence` or `power` outside (0, 1) or a non-finite input.
pub fn minimum_track_record_length(
    sharpe: f64,
    sharpe_star: f64,
    skew: f64,
    kurtosis: f64,
    confidence: f64,
    power: f64,
) -> f64 {
    let probability = |p: f64| p > 0.0 && p < 1.0;
    let finite = [sharpe, sharpe_star, skew, kurtosis].iter().all(|v| v.is_finite());
    if !finite || !probability(confidence) || !probability(power) {
        return f64::NAN;
    }
    let gap = (sharpe - sharpe_star).abs();
    if gap == 0.0 {
        return f64::INFINITY;
    }
    let variance = sharpe_variance(sharpe, skew, kurtosis);
    let z = (normal_quantile(confidence) + normal_quantile(power)) / gap;
    1.0 + variance * z * z
}

/// Expected maximum Sharpe across `n_trials` independent trials (the DSR benchmark).
/// Python `expected_max_sharpe`.
pub fn expected_max_sharpe(var_trials: f64, n_trials: usize) -> f64 {
    if n_trials < 2 || var_trials <= 0.0 {
        return 0.0;
    }
    let nt = n_trials as f64;
    let z1 = normal_quantile(1.0 - 1.0 / nt);
    let z2 = normal_quantile(1.0 - 1.0 / (nt * E));
    var_trials.sqrt() * ((1.0 - EULER_GAMMA) * z1 + EULER_GAMMA * z2)
}

/// Deflated Sharpe: PSR benchmarked against the expected max Sharpe of the trials.
/// Python `deflated_sharpe_ratio`.
pub fn deflated_sharpe_ratio(
    observed_sharpe: f64,
    trial_sharpes: &[f64],
    n_obs: usize,
    skew: f64,
    kurt: f64,
) -> f64 {
    let var_trials = sample_variance(trial_sharpes);
    let sharpe_star = expected_max_sharpe(var_trials, trial_sharpes.len());
    probabilistic_sharpe_ratio(observed_sharpe, n_obs, sharpe_star, skew, kurt)
}

/// Column means over the given rows of a T×N matrix: the plain twin
/// (`vike_math::statistics::plain_mean`) the `overfit::pbo_cscv` probe row pins.
fn col_means(matrix: &[Vec<f64>], rows: &[usize], n_cols: usize) -> Vec<f64> {
    (0..n_cols).map(|j| plain_mean(rows.iter().map(|&t| matrix[t][j]))).collect()
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
        let out_of_sample_rows: Vec<usize> = (0..n_splits)
            .filter(|g| !combo_set.contains(g))
            .flat_map(|g| groups[g].iter().copied())
            .collect();
        if is_rows.is_empty() || out_of_sample_rows.is_empty() {
            continue;
        }
        let is_perf = col_means(matrix, &is_rows, n_cols);
        let out_of_sample_perf = col_means(matrix, &out_of_sample_rows, n_cols);
        // first-argmax (mirror Python `max(range, key=...)`, which returns the first max).
        let mut best = 0usize;
        for j in 1..n_cols {
            if is_perf[j] > is_perf[best] {
                best = j;
            }
        }
        let rank =
            (0..n_cols).filter(|&j| out_of_sample_perf[j] <= out_of_sample_perf[best]).count();
        let omega = rank as f64 / (n_cols as f64 + 1.0);
        if !(0.0 < omega && omega < 1.0) {
            continue;
        }
        logits.push(logit(omega));
    }
    if logits.is_empty() {
        return f64::NAN;
    }
    logits.iter().filter(|&&lam| lam <= 0.0).count() as f64 / logits.len() as f64
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
    let avg = if corrs.is_empty() { 0.0 } else { plain_mean(corrs.iter().copied()) };
    let r = avg.max(0.0);
    let eff = n as f64 / (1.0 + (n as f64 - 1.0) * r);
    eff.clamp(1.0, n as f64)
}

/// DSR benchmarked against expected-max-Sharpe computed with the EFFECTIVE (correlation-corrected)
/// trial count. Python `deflated_sharpe_with_effective_n`.
pub fn deflated_sharpe_with_effective_n(
    observed_sharpe: f64,
    trial_sharpes: &[f64],
    trial_return_series: &[Vec<f64>],
    n_obs: usize,
    skew: f64,
    kurt: f64,
) -> f64 {
    let eff = (effective_n_trials(trial_return_series).round() as usize).max(1);
    let var_trials = sample_variance(trial_sharpes);
    let sharpe_star = expected_max_sharpe(var_trials, eff);
    probabilistic_sharpe_ratio(observed_sharpe, n_obs, sharpe_star, skew, kurt)
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
pub struct OverfitVerdict {
    pub level: OverfitLevel,
    pub reasons: Vec<String>,
}

/// Combine PBO, deflated Sharpe, and (optional) a walk-forward's profitable step share
/// (`crate::walkforward::WalkforwardResults::profitable_step_share`) into a label.
/// Python `overfit_verdict` (same point thresholds + NaN-PBO "not assessed" branch).
pub fn overfit_verdict(
    pbo: f64,
    deflated_sharpe: f64,
    profitable_step_share: Option<f64>,
) -> OverfitVerdict {
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

    if deflated_sharpe < 0.5 {
        points += 2;
        reasons.push(format!(
            "Deflated Sharpe {:.0}%: edge not significant after the number of trials.",
            deflated_sharpe * 100.0
        ));
    } else if deflated_sharpe < 0.9 {
        points += 1;
        reasons.push(format!(
            "Deflated Sharpe {:.0}%: significance is borderline.",
            deflated_sharpe * 100.0
        ));
    }

    if let Some(walkforward) = profitable_step_share
        && walkforward < 0.5
    {
        points += 1;
        reasons.push(format!(
            "Only {:.0}% of walk-forward windows were profitable out-of-sample.",
            walkforward * 100.0
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
    OverfitVerdict { level, reasons }
}

/// The multiple-testing audit of a SELECTION: the overfitting gates applied to the FULL
/// candidate set a winner was chosen from — never to the winner alone.
#[derive(Clone, Debug, PartialEq)]
pub struct OverfitSelectionAudit {
    /// How many candidates the winner was selected FROM. `1` means no selection was
    /// accounted for — the first number to check when a result looks too good.
    pub n_candidates: usize,
    /// Correlation-corrected trial count over ALL candidates ([`effective_n_trials`]).
    pub effective_trials: f64,
    /// The selected candidate's Sharpe, as supplied.
    pub selected_sharpe: f64,
    /// Deflated Sharpe benchmarked against the effective trial count of the FULL set.
    pub deflated_sharpe: f64,
    /// Probability of backtest overfitting over the FULL candidate matrix.
    pub pbo: f64,
    /// Trades taken by the selected candidate. Reported ALONGSIDE Sharpe because a
    /// mean-reversion family dies by STARVATION (too few divergences) before it dies by
    /// unprofitability, and a Sharpe-ranked sweep cannot see that.
    pub selected_trades: usize,
    /// The combined plain-language verdict.
    pub verdict: OverfitVerdict,
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
/// `trial_returns` holds one return series per candidate (N trials × T observations).
///
/// `None` when the inputs are inconsistent (empty set, length mismatch, series of different
/// lengths, out-of-range `selected`) — an unauditable selection is not a passing one.
pub fn audit_selection(
    trial_sharpes: &[f64],
    trial_returns: &[Vec<f64>],
    selected: usize,
    selected_trades: usize,
    n_obs: usize,
    skew: f64,
    kurt: f64,
    n_splits: usize,
) -> Option<OverfitSelectionAudit> {
    if trial_sharpes.is_empty()
        || trial_sharpes.len() != trial_returns.len()
        || selected >= trial_sharpes.len()
        || trial_returns.iter().any(|r| r.len() != trial_returns[0].len())
    {
        return None;
    }
    let selected_sharpe = trial_sharpes[selected];
    let effective_trials = effective_n_trials(trial_returns);
    let deflated_sharpe = deflated_sharpe_with_effective_n(
        selected_sharpe,
        trial_sharpes,
        trial_returns,
        n_obs,
        skew,
        kurt,
    );
    // `trial_returns` is one series per trial (N × T) and `pbo_cscv` reads T observations × N
    // trials, so it gets the transpose (it was handed N × T until 2026-11-09).
    let by_obs: Vec<Vec<f64>> =
        (0..trial_returns[0].len()).map(|t| trial_returns.iter().map(|r| r[t]).collect()).collect();
    let pbo = pbo_cscv(&by_obs, n_splits);
    let mut verdict = overfit_verdict(pbo, deflated_sharpe, None);
    if trial_sharpes.len() == 1 {
        verdict.reasons.push(
            "Only ONE candidate was audited: if a winner was selected from a larger set, \
             these gates are being fed the survivor and understate the trial count."
                .to_string(),
        );
    }
    Some(OverfitSelectionAudit {
        n_candidates: trial_sharpes.len(),
        effective_trials,
        selected_sharpe,
        deflated_sharpe,
        pbo,
        selected_trades,
        verdict,
    })
}

#[path = "overfit_tests.rs"]
#[cfg(test)]
mod overfit_tests;
