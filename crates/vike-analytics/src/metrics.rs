//! Performance metrics computed from an equity curve and the trade list.
//! Exact port of `analysis/metrics.py` — pure functions over slices.
//!
//! PARITY: metrics composed of {+,−,×,÷,abs,compare} are bit-gated; anything through
//! sqrt/pow/log (sharpe, sortino, calmar, cagr, ulcer, mar, k_ratio) is gated at ≤1e-12
//! relative (CPython libm vs this crate's `libm` may differ in the last ulp — plan §5).
//! Python `x ** 2` / `x ** e` are mirrored as a `pow` CALL (never `x*x`) to minimize divergence.
//!
//! ⚠ That call is `libm::pow`, NOT `f64::powf`, and every `ln`/`exp` here is `libm::log`/
//! `libm::exp` for the same reason: `f64`'s transcendentals are the PLATFORM's libm, which IEEE
//! 754 does not require to be correctly rounded, so the same curve produced different numbers on
//! Windows and Linux. The crate doc's "Cross-platform determinism" section carries the decision,
//! what it costs, and the gate that holds it.

use vike_model::{Trade, py_sum};

/// Fractional return from first to last equity point (0.01 == 1%).
pub fn total_return(equity_curve: &[f64]) -> f64 {
    if equity_curve.len() < 2 || equity_curve[0] == 0.0 {
        return 0.0;
    }
    equity_curve[equity_curve.len() - 1] / equity_curve[0] - 1.0
}

/// Per-bar simple returns (`e[i]/e[i-1] - 1`), SKIPPING zero-denominator steps.
/// The canonical primitive behind sharpe / sortino / omega.
pub fn returns(equity_curve: &[f64]) -> Vec<f64> {
    let mut out = Vec::new();
    for i in 1..equity_curve.len() {
        if equity_curve[i - 1] != 0.0 {
            out.push(equity_curve[i] / equity_curve[i - 1] - 1.0);
        }
    }
    out
}

/// Fraction of trades with positive PnL (0..1).
pub fn win_rate(trades: &[Trade]) -> f64 {
    if trades.is_empty() {
        return 0.0;
    }
    let wins = trades.iter().filter(|t| t.pnl > 0.0).count();
    wins as f64 / trades.len() as f64
}

/// Largest peak-to-trough drop as a positive fraction of the peak (0.2 == 20%).
pub fn max_drawdown(equity_curve: &[f64]) -> f64 {
    if equity_curve.is_empty() {
        return 0.0;
    }
    let mut peak = equity_curve[0];
    let mut worst = 0.0;
    for &v in equity_curve {
        peak = peak.max(v);
        if peak > 0.0 {
            worst = f64::max(worst, (peak - v) / peak);
        }
    }
    worst
}

/// Gross profit / gross loss. `inf` when there are no losing trades.
pub fn profit_factor(trades: &[Trade]) -> f64 {
    let gross_profit: f64 = py_sum(trades.iter().filter(|t| t.pnl > 0.0).map(|t| t.pnl));
    let gross_loss: f64 = -py_sum(trades.iter().filter(|t| t.pnl < 0.0).map(|t| t.pnl));
    if gross_loss == 0.0 {
        return if gross_profit > 0.0 { f64::INFINITY } else { 0.0 };
    }
    gross_profit / gross_loss
}

/// Annualized Sortino ratio (target = 0, risk-free = 0).
pub fn sortino(equity_curve: &[f64], periods_per_year: f64) -> f64 {
    if equity_curve.len() < 2 {
        return 0.0;
    }
    let rets = returns(equity_curve);
    if rets.len() < 2 {
        return 0.0;
    }
    let mean: f64 = py_sum(rets.iter().copied()) / rets.len() as f64;
    let downside_var: f64 =
        py_sum(rets.iter().map(|&r| libm::pow(r.min(0.0), 2.0))) / (rets.len() - 1) as f64;
    let downside_dev = downside_var.sqrt();
    if downside_dev == 0.0 {
        return 0.0;
    }
    (mean / downside_dev) * periods_per_year.sqrt()
}

/// Annualized return (CAGR) divided by max drawdown.
pub fn calmar(equity_curve: &[f64], periods_per_year: f64) -> f64 {
    if equity_curve.len() < 3 || equity_curve[0] <= 0.0 {
        return 0.0;
    }
    let n = (equity_curve.len() - 1) as f64;
    let growth = equity_curve[equity_curve.len() - 1] / equity_curve[0];
    if growth <= 0.0 {
        return 0.0;
    }
    let exponent = periods_per_year / n;
    if exponent > 1000.0 {
        return 0.0;
    }
    let c = libm::pow(growth, exponent) - 1.0; // Python catches OverflowError; f64 overflows to inf
    if !c.is_finite() {
        return 0.0;
    }
    let mdd = max_drawdown(equity_curve);
    if mdd == 0.0 {
        return if c > 0.0 { f64::INFINITY } else { 0.0 };
    }
    c / mdd
}

/// Omega ratio of per-bar returns: gains above `threshold` / losses below it.
pub fn omega(equity_curve: &[f64], threshold: f64) -> f64 {
    if equity_curve.len() < 2 {
        return 0.0;
    }
    let rets = returns(equity_curve);
    let gains: f64 = py_sum(rets.iter().filter(|&&r| r > threshold).map(|&r| r - threshold));
    let losses: f64 = py_sum(rets.iter().filter(|&&r| r < threshold).map(|&r| threshold - r));
    if losses == 0.0 {
        return if gains > 0.0 { f64::INFINITY } else { 0.0 };
    }
    gains / losses
}

/// Sample mean and variance of `xs` (`ddof` degrees-of-freedom correction), both folded via
/// `py_sum` (Neumaier-compensated, matching CPython's builtin `sum()`) — the shared core of
/// every mean/std pair in this crate (`sharpe`, `risk_return_ratio`, `returns_volatility`,
/// `returns_skewness`, `returns_kurtosis`, `sqn`, and `benchmark::variance`). Every current
/// caller passes `ddof = 1.0` (sample variance). Guards (`n < k`, zero dispersion, empty
/// input, ...) differ per caller and stay at each call site — this performs only the raw
/// mean+variance arithmetic, moved verbatim from each former call site.
///
/// `pub` rather than the `pub(crate)` it carried before the vike-analytics extraction, for exactly
/// ONE reason: `vike_sim`'s impact model (`vike_sim::AlmgrenChriss`) folds its return
/// series through this same function so the two cannot drift on `py_sum` discipline, and that call
/// became cross-crate when this cluster moved. A shared numeric primitive gaining a wider
/// visibility — not new public surface with a new contract; the body is untouched.
pub fn mean_variance<I>(xs: I, ddof: f64) -> (f64, f64)
where
    I: ExactSizeIterator<Item = f64> + Clone,
{
    let n = xs.len() as f64;
    let mean = py_sum(xs.clone()) / n;
    let var = py_sum(xs.map(|x| libm::pow(x - mean, 2.0))) / (n - ddof);
    (mean, var)
}

/// Annualized Sharpe of per-bar returns (risk-free = 0). 0.0 if variance is 0.
pub fn sharpe(equity_curve: &[f64], periods_per_year: f64) -> f64 {
    if equity_curve.len() < 2 {
        return 0.0;
    }
    let rets = returns(equity_curve);
    if rets.len() < 2 {
        return 0.0;
    }
    let (mean, var) = mean_variance(rets.iter().copied(), 1.0);
    let std = var.sqrt();
    if std == 0.0 {
        return 0.0;
    }
    (mean / std) * periods_per_year.sqrt()
}

pub fn net_profit(trades: &[Trade]) -> f64 {
    py_sum(trades.iter().map(|t| t.pnl))
}

pub fn gross_profit(trades: &[Trade]) -> f64 {
    py_sum(trades.iter().filter(|t| t.pnl > 0.0).map(|t| t.pnl))
}

pub fn gross_loss(trades: &[Trade]) -> f64 {
    -py_sum(trades.iter().filter(|t| t.pnl < 0.0).map(|t| t.pnl))
}

pub fn total_fees(trades: &[Trade]) -> f64 {
    py_sum(trades.iter().map(|t| t.fees))
}

/// Average gross PnL per trade (0.0 if there are no trades).
pub fn expected_payoff(trades: &[Trade]) -> f64 {
    if trades.is_empty() { 0.0 } else { net_profit(trades) / trades.len() as f64 }
}

/// Total return / max drawdown. `inf` when there's no drawdown.
pub fn recovery_factor(equity_curve: &[f64]) -> f64 {
    let dd = max_drawdown(equity_curve);
    if dd == 0.0 {
        return if total_return(equity_curve) > 0.0 { f64::INFINITY } else { 0.0 };
    }
    total_return(equity_curve) / dd
}

fn max_run(trades: &[Trade], win: bool) -> usize {
    let mut best = 0usize;
    let mut run = 0usize;
    for t in trades {
        let hit = if win { t.pnl > 0.0 } else { t.pnl < 0.0 };
        run = if hit { run + 1 } else { 0 };
        best = best.max(run);
    }
    best
}

pub fn consecutive_wins(trades: &[Trade]) -> usize {
    max_run(trades, true)
}

pub fn consecutive_losses(trades: &[Trade]) -> usize {
    max_run(trades, false)
}

pub fn largest_win(trades: &[Trade]) -> f64 {
    let wins: Vec<f64> = trades.iter().filter(|t| t.pnl > 0.0).map(|t| t.pnl).collect();
    if wins.is_empty() { 0.0 } else { wins.iter().copied().fold(f64::NEG_INFINITY, f64::max) }
}

pub fn largest_loss(trades: &[Trade]) -> f64 {
    let losses: Vec<f64> = trades.iter().filter(|t| t.pnl < 0.0).map(|t| t.pnl).collect();
    if losses.is_empty() { 0.0 } else { losses.iter().copied().fold(f64::INFINITY, f64::min) }
}

pub fn avg_win(trades: &[Trade]) -> f64 {
    let wins: Vec<f64> = trades.iter().filter(|t| t.pnl > 0.0).map(|t| t.pnl).collect();
    if wins.is_empty() { 0.0 } else { py_sum(wins.iter().copied()) / wins.len() as f64 }
}

pub fn avg_loss(trades: &[Trade]) -> f64 {
    let losses: Vec<f64> = trades.iter().filter(|t| t.pnl < 0.0).map(|t| t.pnl).collect();
    if losses.is_empty() { 0.0 } else { py_sum(losses.iter().copied()) / losses.len() as f64 }
}

/// CAGR: (final/first)^(periods_per_year/n) - 1; 0.0 for flat/short/non-positive/overflow.
pub fn cagr(equity_curve: &[f64], periods_per_year: f64) -> f64 {
    if equity_curve.len() < 2 || equity_curve[0] <= 0.0 {
        return 0.0;
    }
    let n = (equity_curve.len() - 1) as f64;
    let growth = equity_curve[equity_curve.len() - 1] / equity_curve[0];
    if growth <= 0.0 {
        return 0.0;
    }
    let exponent = periods_per_year / n;
    if exponent > 1000.0 {
        return 0.0;
    }
    let c = libm::pow(growth, exponent) - 1.0;
    if c.is_finite() { c } else { 0.0 }
}

/// Ulcer Index: sqrt(mean(drawdown_pct_i^2)).
pub fn ulcer_index(equity_curve: &[f64]) -> f64 {
    if equity_curve.len() < 2 {
        return 0.0;
    }
    let mut peak = equity_curve[0];
    let mut sq_sum = 0.0;
    for &v in equity_curve {
        peak = peak.max(v);
        let dd_pct = if peak > 0.0 { (peak - v) / peak * 100.0 } else { 0.0 };
        sq_sum += libm::pow(dd_pct, 2.0);
    }
    (sq_sum / equity_curve.len() as f64).sqrt()
}

/// MAR Ratio: CAGR / max_drawdown.
pub fn mar_ratio(equity_curve: &[f64], periods_per_year: f64) -> f64 {
    let mdd = max_drawdown(equity_curve);
    let c = cagr(equity_curve, periods_per_year);
    if mdd == 0.0 {
        return if c > 0.0 { f64::INFINITY } else { 0.0 };
    }
    c / mdd
}

/// K-Ratio (Lars Kestner): slope / stderr of OLS log-equity vs bar-index.
pub fn k_ratio(equity_curve: &[f64]) -> f64 {
    if equity_curve.len() < 2 {
        return 0.0;
    }
    let mut log_eq = Vec::new();
    let mut xs = Vec::new();
    for (i, &v) in equity_curve.iter().enumerate() {
        if v > 0.0 {
            log_eq.push(libm::log(v));
            xs.push(i as f64);
        }
    }
    let n = xs.len();
    if n < 2 {
        return 0.0;
    }
    let nf = n as f64;
    let mean_x: f64 = py_sum(xs.iter().copied()) / nf;
    let mean_y: f64 = py_sum(log_eq.iter().copied()) / nf;
    let ss_xx: f64 = py_sum(xs.iter().map(|&x| libm::pow(x - mean_x, 2.0)));
    if ss_xx == 0.0 {
        return 0.0;
    }
    let ss_xy: f64 = py_sum((0..n).map(|i| (xs[i] - mean_x) * (log_eq[i] - mean_y)));
    let slope = ss_xy / ss_xx;
    let sse: f64 = py_sum((0..n).map(|i| {
        let y_hat = mean_y + slope * (xs[i] - mean_x);
        libm::pow(log_eq[i] - y_hat, 2.0)
    }));
    if n < 3 {
        return 0.0;
    }
    let s2 = sse / (nf - 2.0);
    if s2 <= 0.0 {
        return 0.0;
    }
    let se_slope = (s2 / ss_xx).sqrt();
    if se_slope == 0.0 {
        return 0.0;
    }
    slope / se_slope
}

/// Average win PnL / |average loss PnL|; 0.0 when there are no wins or no losses.
pub fn payoff_ratio(trades: &[Trade]) -> f64 {
    let wins: Vec<f64> = trades.iter().filter(|t| t.pnl > 0.0).map(|t| t.pnl).collect();
    let losses: Vec<f64> = trades.iter().filter(|t| t.pnl < 0.0).map(|t| t.pnl).collect();
    if wins.is_empty() || losses.is_empty() {
        return 0.0;
    }
    let avg_w = py_sum(wins.iter().copied()) / wins.len() as f64;
    let avg_l = py_sum(losses.iter().copied()) / losses.len() as f64; // negative
    if avg_l == 0.0 {
        return 0.0;
    }
    avg_w / avg_l.abs()
}

/// Fraction of trades whose opening side was long (0..1). 0.0 for an empty trade list.
///
/// NB: `Trade::is_long` is only populated by the event-driven engine (`engine.rs`) and the
/// vectorized kernel (`vector_engine.rs`) — both compute it from the position's prior sign
/// before it's overwritten by the closing fill, so it is always meaningful for Rust-produced
/// trades. That was NOT true of the Python app this was ported from, whose legacy vectorized
/// kernels returned no per-trade side array and defaulted `is_long` to `false` — recorded
/// because it explains why old numbers read 0.0, not because anything still compares the two.
pub fn long_ratio(trades: &[Trade]) -> f64 {
    if trades.is_empty() {
        return 0.0;
    }
    let longs = trades.iter().filter(|t| t.is_long).count();
    longs as f64 / trades.len() as f64
}

/// Fraction of bars with a non-zero position; 0.0 if lists are empty/mismatched.
pub fn exposure(equity_curve: &[f64], position_sizes: &[f64]) -> f64 {
    let n = equity_curve.len();
    if n == 0 || position_sizes.len() != n {
        return 0.0;
    }
    let active = position_sizes.iter().filter(|&&s| s != 0.0).count();
    active as f64 / n as f64
}

/// Non-annualized mean(returns) / std(returns) (ddof=1) — Sharpe without the
/// sqrt(periods_per_year) annualization factor. 0.0 if variance is 0 or fewer than 2 returns.
pub fn risk_return_ratio(equity_curve: &[f64]) -> f64 {
    let rets = returns(equity_curve);
    if rets.len() < 2 {
        return 0.0;
    }
    let (mean, var) = mean_variance(rets.iter().copied(), 1.0);
    let std = var.sqrt();
    if std == 0.0 {
        return 0.0;
    }
    mean / std
}

/// Annualized standard deviation of per-bar returns (ddof=1). 0.0 for fewer than 2 returns.
pub fn returns_volatility(equity_curve: &[f64], periods_per_year: f64) -> f64 {
    let rets = returns(equity_curve);
    if rets.len() < 2 {
        return 0.0;
    }
    let (_, var) = mean_variance(rets.iter().copied(), 1.0);
    var.sqrt() * periods_per_year.sqrt()
}

/// Bias-corrected sample skewness of per-bar returns (adjusted Fisher-Pearson, matching
/// `pandas.Series.skew`). 0.0 for fewer than 3 returns or zero dispersion.
pub fn returns_skewness(equity_curve: &[f64]) -> f64 {
    let rets = returns(equity_curve);
    let n = rets.len();
    if n < 3 {
        return 0.0;
    }
    let nf = n as f64;
    let (mean, var) = mean_variance(rets.iter().copied(), 1.0);
    let std = var.sqrt();
    if std == 0.0 {
        return 0.0;
    }
    let sum_cubed = py_sum(rets.iter().map(|&r| libm::pow((r - mean) / std, 3.0)));
    (nf / ((nf - 1.0) * (nf - 2.0))) * sum_cubed
}

/// Bias-corrected sample excess kurtosis of per-bar returns (adjusted Fisher-Pearson,
/// matching `pandas.Series.kurt`; 0.0 for a normal distribution). 0.0 for fewer than 4
/// returns or zero dispersion.
pub fn returns_kurtosis(equity_curve: &[f64]) -> f64 {
    let rets = returns(equity_curve);
    let n = rets.len();
    if n < 4 {
        return 0.0;
    }
    let nf = n as f64;
    let (mean, var) = mean_variance(rets.iter().copied(), 1.0);
    let std = var.sqrt();
    if std == 0.0 {
        return 0.0;
    }
    let sum_quartic = py_sum(rets.iter().map(|&r| libm::pow((r - mean) / std, 4.0)));
    let term1 = (nf * (nf + 1.0)) / ((nf - 1.0) * (nf - 2.0) * (nf - 3.0)) * sum_quartic;
    let term2 = 3.0 * libm::pow(nf - 1.0, 2.0) / ((nf - 2.0) * (nf - 3.0));
    term1 - term2
}

/// `q`-th percentile (`q` in `[0, 1]`) via linear interpolation between closest ranks,
/// matching `numpy.percentile`. `sorted_vals` must be sorted ascending; 0.0 for an empty
/// slice. (`n == 1` needs no special case: the interpolation formula below already falls
/// through to `sorted_vals[0]` bit-identically — `lo == 0`, `frac == 0.0`, `lo + 1 < n` is
/// false, so the `else` arm returns `sorted_vals[0]` directly either way.) Shared with
/// [`crate::montecarlo`] and [`crate::stats`].
///
/// ⚠ **This is the workspace's ONE percentile, and it went `pub` because a second one had already
/// been written.** `crates/vike-backtest/src/bin/cheap_np_depth.rs`'s `pct` computed a
/// NEAREST-RANK answer — `v[round((n - 1) * q)]`, always an observed sample, never an
/// interpolant — and printed it under the same `p10`..`p90` names an operator sizes positions
/// from. The two agree only where `(n - 1) * q` lands on an integer or where the two samples
/// being interpolated between are equal: on `[0, 1, 2, 3]` at `q = 0.5` the nearest-rank reading
/// is `2.0` against this function's `1.5`. Reach for this one; do not write a third.
///
/// ⚠ **Sorting is the CALLER's job and the comparator is part of the contract.** This body does
/// no comparison at all, so it cannot detect an unsorted slice — it will silently interpolate
/// between two arbitrary neighbours. Sort with [`f64::total_cmp`] (as [`tail_ratio`] and
/// [`value_at_risk`] below do), never with `partial_cmp().unwrap_or(Ordering::Equal)`: only the
/// former is a total order, so only the former actually leaves the slice sorted when a NaN is
/// present.
pub fn percentile(sorted_vals: &[f64], q: f64) -> f64 {
    if sorted_vals.is_empty() {
        return 0.0;
    }
    let n = sorted_vals.len();
    let pos = q * (n - 1) as f64;
    let lo = pos as usize;
    let frac = pos - lo as f64;
    if lo + 1 < n {
        sorted_vals[lo] * (1.0 - frac) + sorted_vals[lo + 1] * frac
    } else {
        sorted_vals[lo]
    }
}

/// The NAME of the convention [`percentile`] computes — what an artifact writes down when it has to
/// say WHICH percentile produced its numbers. It lives beside the body rather than beside any
/// artifact, so the string and the algorithm it names cannot come to disagree.
///
/// ⚠ **Change this string in the same edit that changes [`percentile`]'s body, and in no other.**
/// It exists because the numbers moved once already and no artifact said so:
/// `crates/vike-backtest/src/bin/cheap_np_depth.rs`'s `pct` published a NEAREST-RANK reading under
/// the same `p10`..`p90` keys this function now answers, and the two disagree in BOTH directions on
/// one sample — so a file from either side of that change is indistinguishable from a file from the
/// other, and no scale factor converts one into the other.
///
/// **A name rather than a version number, because the discriminating fact is WHICH ALGORITHM, and
/// this tree holds more than two of them.** Besides the nearest-rank body above there is the
/// `d.len() / 2` median `crates/vike-backtest/src/bin/cheap_np_askgate.rs`'s `anchor_json` used to
/// spell, and the floor-rank `pct` over `u64` in `crates/vike-core/tests/runtime_latency.rs`, which
/// is deliberately a convention of its own and stays one — its ceilings are calibrated against it.
/// A third convention arriving here is a third NAME, and a name is a visible diff in every artifact
/// carrying it; a version number would only have said that *something* changed.
pub const PERCENTILE_METHOD: &str = "numpy_linear";

/// Ratio of the right (gain) tail to the left (loss) tail of per-bar returns:
/// `|percentile(r, 0.95) / percentile(r, 0.05)|`. A value above 1 means a heavier upside
/// tail. 0.0 for fewer than 2 returns or a zero/non-finite 5th percentile.
pub fn tail_ratio(equity_curve: &[f64]) -> f64 {
    let rets = returns(equity_curve);
    if rets.len() < 2 {
        return 0.0;
    }
    let mut values = rets.clone();
    values.sort_by(f64::total_cmp);
    let p95 = percentile(&values, 0.95);
    let p5 = percentile(&values, 0.05);
    if p5 == 0.0 || !p5.is_finite() {
        return 0.0;
    }
    (p95 / p5).abs()
}

/// Historical (non-parametric) Value at Risk: the `1 - confidence` empirical quantile of
/// per-bar returns. Expressed as a return (more negative = greater risk). 0.0 for an
/// empty return series.
pub fn value_at_risk(equity_curve: &[f64], confidence: f64) -> f64 {
    let rets = returns(equity_curve);
    if rets.is_empty() {
        return 0.0;
    }
    let mut values = rets;
    values.sort_by(f64::total_cmp);
    percentile(&values, 1.0 - confidence)
}

/// Historical Expected Shortfall (CVaR): mean of the returns at or below the
/// `value_at_risk` threshold — the average of the worst `1 - confidence` tail. Always
/// <= the corresponding VaR. 0.0 for an empty return series.
pub fn expected_shortfall(equity_curve: &[f64], confidence: f64) -> f64 {
    let rets = returns(equity_curve);
    if rets.is_empty() {
        return 0.0;
    }
    let mut values = rets;
    values.sort_by(f64::total_cmp);
    let var = percentile(&values, 1.0 - confidence);
    let tail: Vec<f64> = values.iter().copied().filter(|&r| r <= var).collect();
    py_sum(tail.iter().copied()) / tail.len() as f64
}

/// System Quality Number (Van Tharp): sqrt(n) * mean(pnl) / std(pnl) (ddof=1). 0.0 for
/// fewer than 2 trades or zero dispersion.
pub fn sqn(trades: &[Trade]) -> f64 {
    if trades.len() < 2 {
        return 0.0;
    }
    let n = trades.len();
    let nf = n as f64;
    let (mean, var) = mean_variance(trades.iter().map(|t| t.pnl), 1.0);
    let std = var.sqrt();
    if std == 0.0 {
        return 0.0;
    }
    nf.sqrt() * mean / std
}

/// Ulcer Performance Index (Martin Ratio): CAGR / Ulcer Index — like `mar_ratio` but
/// penalized by drawdown depth AND duration rather than max drawdown alone. `inf` when
/// there is positive CAGR and zero Ulcer Index; 0.0 otherwise.
pub fn ulcer_performance_index(equity_curve: &[f64], periods_per_year: f64) -> f64 {
    let ui = ulcer_index(equity_curve);
    let c = cagr(equity_curve, periods_per_year);
    if ui == 0.0 {
        return if c > 0.0 { f64::INFINITY } else { 0.0 };
    }
    c / ui
}

#[path = "metrics_tests.rs"]
#[cfg(test)]
mod metrics_tests;
