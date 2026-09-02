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

use vike_model::{py_sum, Trade};

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
/// ONE reason: `vike_backtest::impact` (the Almgren-Chriss market-impact model) folds its return
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
    if trades.is_empty() {
        0.0
    } else {
        net_profit(trades) / trades.len() as f64
    }
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
    if wins.is_empty() {
        0.0
    } else {
        wins.iter().copied().fold(f64::NEG_INFINITY, f64::max)
    }
}

pub fn largest_loss(trades: &[Trade]) -> f64 {
    let losses: Vec<f64> = trades.iter().filter(|t| t.pnl < 0.0).map(|t| t.pnl).collect();
    if losses.is_empty() {
        0.0
    } else {
        losses.iter().copied().fold(f64::INFINITY, f64::min)
    }
}

pub fn avg_win(trades: &[Trade]) -> f64 {
    let wins: Vec<f64> = trades.iter().filter(|t| t.pnl > 0.0).map(|t| t.pnl).collect();
    if wins.is_empty() {
        0.0
    } else {
        py_sum(wins.iter().copied()) / wins.len() as f64
    }
}

pub fn avg_loss(trades: &[Trade]) -> f64 {
    let losses: Vec<f64> = trades.iter().filter(|t| t.pnl < 0.0).map(|t| t.pnl).collect();
    if losses.is_empty() {
        0.0
    } else {
        py_sum(losses.iter().copied()) / losses.len() as f64
    }
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
    if c.is_finite() {
        c
    } else {
        0.0
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Build an equity curve whose per-bar returns equal `rets` exactly.
    fn eq_from_returns(rets: &[f64]) -> Vec<f64> {
        let mut eq = vec![1.0];
        for &r in rets {
            eq.push(eq.last().unwrap() * (1.0 + r));
        }
        eq
    }

    fn t(pnl: f64) -> Trade {
        Trade {
            entry_price: 1.0,
            exit_price: 1.0,
            size: 1.0,
            pnl,
            fees: 0.0,
            entry_ts: 0,
            exit_ts: 0,
            symbol: String::new(),
            mae: 0.0,
            mfe: 0.0,
            is_long: false,
        }
    }

    // Reference values verified against NautilusTrader's crates/analysis statistics
    // (nautilus_trader develop, 2026-07-08) — same math, house 0.0-sentinel convention
    // instead of NaN for degenerate inputs.

    #[test]
    fn risk_return_ratio_known_value() {
        let eq = eq_from_returns(&[0.1, -0.05, 0.2, -0.1, 0.15]);
        assert!((risk_return_ratio(&eq) - 0.463_600_445_571_753_45).abs() < 1e-9);
    }

    #[test]
    fn risk_return_ratio_zero_std_is_zero() {
        assert_eq!(risk_return_ratio(&[100.0; 10]), 0.0);
    }

    #[test]
    fn risk_return_ratio_empty_is_zero() {
        assert_eq!(risk_return_ratio(&[]), 0.0);
    }

    #[test]
    fn returns_volatility_known_value() {
        let eq = eq_from_returns(&[0.01, -0.02, 0.03, -0.01, 0.02, 0.04, -0.03, 0.05, -0.04, 0.02]);
        assert!((returns_volatility(&eq, 252.0) - 0.485_262_815_389_763_96).abs() < 1e-9);
    }

    #[test]
    fn returns_volatility_empty_is_zero() {
        assert_eq!(returns_volatility(&[], 252.0), 0.0);
    }

    #[test]
    fn returns_skewness_known_value() {
        let eq = eq_from_returns(&[0.01, -0.02, 0.03, -0.01, 0.02, 0.04, -0.03, 0.05, -0.04, 0.02]);
        assert!((returns_skewness(&eq) - (-0.228_720_234_225_963_13)).abs() < 1e-9);
    }

    #[test]
    fn returns_skewness_insufficient_data_is_zero() {
        assert_eq!(returns_skewness(&[100.0, 101.0, 99.0]), 0.0);
    }

    #[test]
    fn returns_skewness_zero_dispersion_is_zero() {
        assert_eq!(returns_skewness(&[100.0; 5]), 0.0);
    }

    #[test]
    fn returns_kurtosis_known_value() {
        let eq = eq_from_returns(&[0.01, -0.02, 0.03, -0.01, 0.02, 0.04, -0.03, 0.05, -0.04, 0.02]);
        assert!((returns_kurtosis(&eq) - (-1.262_244_325_199_502_8)).abs() < 1e-9);
    }

    #[test]
    fn returns_kurtosis_insufficient_data_is_zero() {
        assert_eq!(returns_kurtosis(&[100.0, 101.0, 99.0, 102.0]), 0.0);
    }

    /// `percentile` is INTERPOLATING (numpy's default `method="linear"`), and this pins it against
    /// the NEAREST-RANK reading — `sorted[round((n - 1) * q)]` — that a second implementation in
    /// `crates/vike-backtest/src/bin/cheap_np_depth.rs` had grown independently. The three
    /// even-length assertions are values nearest-rank CANNOT produce — it can only ever return an
    /// element of the input — so a "simplification" back to indexing reddens here rather than
    /// silently moving an operator-facing number. The rest pin the cases where the two conventions
    /// agree (the ends, an integer rank, and the two degenerate lengths), so the agreement is a
    /// recorded claim rather than an assumption.
    #[test]
    fn percentile_interpolates_and_is_not_nearest_rank() {
        let v = [0.0, 1.0, 2.0, 3.0];
        assert_eq!(percentile(&v, 0.5), 1.5, "nearest-rank would read 2.0 here");
        assert_eq!(percentile(&v, 0.25), 0.75);
        assert_eq!(percentile(&v, 0.75), 2.25);
        // The two ends are exact, and `q == 1.0` is the `else` arm (there is no `lo + 1`).
        assert_eq!(percentile(&v, 0.0), 0.0);
        assert_eq!(percentile(&v, 1.0), 3.0);
        // An odd length AGREES with nearest-rank wherever `(n - 1) * q` lands on an integer.
        let odd = [0.0, 1.0, 2.0, 3.0, 4.0];
        assert_eq!(percentile(&odd, 0.5), 2.0);
        assert_eq!(percentile(&odd, 0.25), 1.0);
        // Degenerate inputs: the documented empty answer, and the no-special-case single element.
        assert_eq!(percentile(&[], 0.5), 0.0);
        for q in [0.0, 0.1, 0.5, 0.9, 1.0] {
            assert_eq!(percentile(&[7.0], q), 7.0, "n == 1 needs no special case");
        }
    }

    /// **The ONE literal pin of [`PERCENTILE_METHOD`], and it sits beside the algorithm it names.**
    /// Every artifact-side test compares its stamp to the constant rather than to a second copy of
    /// this string, so there is exactly one place a convention change has to be typed — and it is
    /// this one, next to the body being changed.
    ///
    /// It reddens on a RENAME as well as on a real change of convention, deliberately: a rename is
    /// exactly as invisible to a reader holding two JSON files as a silent algorithm swap, and the
    /// author who lands here is the author who has to decide which of the two they are doing.
    ///
    /// The shape assertions are not decoration either — the value is grepped out of pasted JSON, so
    /// it must stay one lowercase ASCII token with no whitespace.
    #[test]
    fn the_published_method_name_is_pinned_and_stays_greppable() {
        assert_eq!(
            PERCENTILE_METHOD, "numpy_linear",
            "an artifact stamped with a DIFFERENT name than an operator was told to look for is \
             the defect this constant exists to prevent — if `percentile`'s body really changed, \
             change this string AND say so where operators read (this crate's own doc, \
             `crates/vike-backtest/src/binutil.rs`'s `PERCENTILE_NOTE`, and \
             `crates/vike-backtest/CLAUDE.md`)"
        );
        assert!(!PERCENTILE_METHOD.is_empty());
        assert!(
            PERCENTILE_METHOD
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'),
            "one lowercase ASCII token, greppable out of pasted JSON: {PERCENTILE_METHOD:?}"
        );
    }

    #[test]
    fn tail_ratio_known_value() {
        let eq = eq_from_returns(&[0.01, -0.02, 0.03, -0.01, 0.02, 0.04, -0.03, 0.05, -0.04, 0.02]);
        assert!((tail_ratio(&eq) - 1.281_690_140_845_070_4).abs() < 1e-9);
    }

    #[test]
    fn tail_ratio_symmetric_is_near_one() {
        let eq = eq_from_returns(&[-0.03, -0.02, -0.01, 0.0, 0.01, 0.02, 0.03]);
        assert!((tail_ratio(&eq) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn tail_ratio_insufficient_data_is_zero() {
        assert_eq!(tail_ratio(&[100.0, 101.0]), 0.0);
    }

    #[test]
    fn value_at_risk_known_value() {
        let eq =
            eq_from_returns(&[0.02, -0.05, 0.01, -0.08, 0.03, -0.02, 0.04, -0.10, 0.015, -0.03]);
        assert!((value_at_risk(&eq, 0.95) - (-0.091)).abs() < 1e-9);
    }

    #[test]
    fn value_at_risk_empty_is_zero() {
        assert_eq!(value_at_risk(&[], 0.95), 0.0);
    }

    #[test]
    fn expected_shortfall_known_value() {
        let eq =
            eq_from_returns(&[0.02, -0.05, 0.01, -0.08, 0.03, -0.02, 0.04, -0.10, 0.015, -0.03]);
        assert!((expected_shortfall(&eq, 0.95) - (-0.10)).abs() < 1e-9);
    }

    #[test]
    fn expected_shortfall_multi_element_tail() {
        let eq =
            eq_from_returns(&[0.02, -0.05, 0.01, -0.08, 0.03, -0.02, 0.04, -0.10, 0.015, -0.03]);
        assert!((expected_shortfall(&eq, 0.60) - (-0.065)).abs() < 1e-9);
    }

    #[test]
    fn expected_shortfall_at_most_value_at_risk() {
        let eq =
            eq_from_returns(&[0.02, -0.05, 0.01, -0.08, 0.03, -0.02, 0.04, -0.10, 0.015, -0.03]);
        assert!(expected_shortfall(&eq, 0.90) <= value_at_risk(&eq, 0.90));
    }

    #[test]
    fn expected_shortfall_empty_is_zero() {
        assert_eq!(expected_shortfall(&[], 0.95), 0.0);
    }

    #[test]
    fn sqn_known_value() {
        let pnls = [10.0, -5.0, 20.0, -10.0, 15.0];
        let trades: Vec<Trade> = pnls.iter().map(|&p| t(p)).collect();
        let mean = pnls.iter().sum::<f64>() / pnls.len() as f64;
        let var = pnls.iter().map(|p| (p - mean).powf(2.0)).sum::<f64>() / (pnls.len() - 1) as f64;
        let expected = (pnls.len() as f64).sqrt() * mean / var.sqrt();
        assert!((sqn(&trades) - expected).abs() < 1e-9);
    }

    #[test]
    fn sqn_no_trades_is_zero() {
        assert_eq!(sqn(&[]), 0.0);
    }

    #[test]
    fn sqn_one_trade_is_zero() {
        assert_eq!(sqn(&[t(10.0)]), 0.0);
    }

    #[test]
    fn sqn_zero_std_is_zero() {
        assert_eq!(sqn(&[t(10.0), t(10.0), t(10.0)]), 0.0);
    }

    #[test]
    fn ulcer_performance_index_positive_for_curve_with_drawdown() {
        let eq = [10_000.0, 12_000.0, 9_000.0, 13_000.0];
        assert!(ulcer_performance_index(&eq, 252.0) > 0.0);
    }

    #[test]
    fn ulcer_performance_index_zero_ulcer_and_positive_cagr_is_inf() {
        let eq: Vec<f64> = (0..252).map(|i| 10_000.0 + i as f64 * 50.0).collect();
        assert_eq!(ulcer_performance_index(&eq, 252.0), f64::INFINITY);
    }

    #[test]
    fn ulcer_performance_index_flat_is_zero() {
        assert_eq!(ulcer_performance_index(&[100.0; 10], 252.0), 0.0);
    }

    fn tl(is_long: bool) -> Trade {
        Trade { is_long, ..t(0.0) }
    }

    #[test]
    fn long_ratio_all_long() {
        assert_eq!(long_ratio(&[tl(true), tl(true), tl(true)]), 1.0);
    }

    #[test]
    fn long_ratio_all_short() {
        assert_eq!(long_ratio(&[tl(false), tl(false)]), 0.0);
    }

    #[test]
    fn long_ratio_mixed() {
        let trades = [tl(true), tl(false), tl(true), tl(false)];
        assert_eq!(long_ratio(&trades), 0.5);
    }

    #[test]
    fn long_ratio_no_trades_is_zero() {
        assert_eq!(long_ratio(&[]), 0.0);
    }
}
