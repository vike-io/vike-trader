//! Performance metrics computed from an equity curve and the trade list.
//! Exact port of `analysis/metrics.py` — pure functions over slices.
//!
//! PARITY: metrics composed of {+,−,×,÷,abs,compare} are bit-gated; anything through
//! sqrt/pow/log (sharpe, sortino, calmar, cagr, ulcer, k_ratio) is gated at ≤1e-12
//! relative (CPython libm vs this crate's `libm` may differ in the last ulp — plan §5).
//! Python `x ** e` is mirrored as a `pow` CALL to minimize divergence. A square is
//! `vike_math::powers::square` since 2026-11-19, the same bits as the `libm::pow(x, 2.0)` it replaced
//! (the `libm` crate's `y == 2` branch is `x * x`).
//!
//! ⚠ That call is `libm::pow`, NOT `f64::powf`, and every `ln`/`exp` here is `libm::log`/
//! `libm::exp` for the same reason: `f64`'s transcendentals are the PLATFORM's libm, which IEEE
//! 754 does not require to be correctly rounded, so the same curve produced different numbers on
//! Windows and Linux. The crate doc's "Cross-platform determinism" section carries the decision,
//! what it costs, and the gate that holds it.

use vike_model::Trade;

use vike_math::statistics::{
    downside_variance, mean, mean_variance, percentile, sample_excess_kurtosis, sample_skewness,
};
use vike_math::summation::compensated_sum;
use vike_math::time_series::drawdown_fraction;

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

/// `values` in ascending [`f64::total_cmp`] order: the one order the tail metrics' percentiles
/// read ([`tail_ratio`], [`value_at_risk`], [`expected_shortfall`]). Unstable, and still the bits a stable sort
/// gives: `total_cmp` calls two values equal only when their bits are equal, so no order among equals can show.
pub(crate) fn ascending(mut values: Vec<f64>) -> Vec<f64> {
    values.sort_unstable_by(f64::total_cmp);
    values
}

/// Fraction of trades with positive PnL (0..1).
pub fn win_rate(trades: &[Trade]) -> f64 {
    if trades.is_empty() {
        return 0.0;
    }
    let wins = trades.iter().filter(|t| t.pnl > 0.0).count();
    wins as f64 / trades.len() as f64
}

/// One closed trade's price move from its entry to its exit, in the trade's own direction, as a fraction of the
/// entry price (0.02 == 2%): what the position earned on the price before fees, whatever money the contract
/// settles in. Not finite for an entry price of zero.
pub fn trade_return(trade: &Trade) -> f64 {
    let moved = trade.exit_price / trade.entry_price - 1.0;
    if trade.is_long { moved } else { -moved }
}

/// Largest peak-to-trough drop as a positive fraction of the peak (0.2 == 20%).
pub fn max_drawdown(equity_curve: &[f64]) -> f64 {
    let mut walk = DrawdownWalk::default();
    for &v in equity_curve {
        walk.push(v);
    }
    walk.worst()
}

/// [`max_drawdown`]'s walk one value at a time, for a producer that sees a curve it does not keep: the tick lane folds
/// every event's equity through it and keeps one close a step (`vike_simulator::EquitySampling::ByTime`), so the
/// drawdown it reports is the whole path's, to the bit. The peak runs from the first value; a drop counts only under a
/// positive peak.
#[derive(Debug, Clone, Copy)]
pub struct DrawdownWalk {
    peak: f64,
    worst: f64,
}

impl Default for DrawdownWalk {
    fn default() -> Self {
        // `NEG_INFINITY.max(v)` is `v`, so the first push starts the peak where the slice walk started it.
        Self { peak: f64::NEG_INFINITY, worst: 0.0 }
    }
}

impl DrawdownWalk {
    /// The next value of the curve, and how far it stands below the peak so far, as a positive fraction of that
    /// peak (`0.0` at a new peak, and while the peak is not positive): the curve's drawdown at this value, the
    /// underwater line a chart draws.
    pub fn push(&mut self, v: f64) -> f64 {
        self.peak = self.peak.max(v);
        // `worst` starts at 0 and only grows, so folding the `0.0` of a non-positive peak in moves nothing.
        let drop = drawdown_fraction(self.peak, v).unwrap_or(0.0);
        self.worst = f64::max(self.worst, drop);
        drop
    }

    /// The largest drop so far, as [`max_drawdown`] answers it over the values pushed.
    pub fn worst(&self) -> f64 {
        self.worst
    }
}

/// Gross profit / gross loss. `inf` when there are no losing trades.
pub fn profit_factor(trades: &[Trade]) -> f64 {
    let gross_profit: f64 = compensated_sum(trades.iter().filter(|t| t.pnl > 0.0).map(|t| t.pnl));
    let gross_loss: f64 = -compensated_sum(trades.iter().filter(|t| t.pnl < 0.0).map(|t| t.pnl));
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
    sortino_of_returns(&returns(equity_curve), periods_per_year)
}

/// [`sortino`] over the curve's [`returns`], built once by a caller that reads several metrics of them.
pub(crate) fn sortino_of_returns(rets: &[f64], periods_per_year: f64) -> f64 {
    if rets.len() < 2 {
        return 0.0;
    }
    let average = mean(rets.iter().copied());
    let downside_dev = downside_variance(rets, 0.0, 1.0).sqrt();
    if downside_dev == 0.0 {
        return 0.0;
    }
    (average / downside_dev) * periods_per_year.sqrt()
}

/// Annualized return (CAGR) divided by max drawdown. It is also the MAR ratio: a `mar_ratio` of the
/// same formula stood beside it until 2026-11-09 and was removed, so this is the one spelling.
pub fn calmar(equity_curve: &[f64], periods_per_year: f64) -> f64 {
    calmar_of_drawdown(equity_curve, periods_per_year, max_drawdown(equity_curve))
}

/// [`calmar`] at the maximum drawdown `mdd` the caller holds: a curve kept by time is not where its drawdown was
/// measured (`crate::record::TradingRecord::max_drawdown`).
pub(crate) fn calmar_of_drawdown(equity_curve: &[f64], periods_per_year: f64, mdd: f64) -> f64 {
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
    omega_of_returns(&returns(equity_curve), threshold)
}

/// [`omega`] over the curve's [`returns`].
pub(crate) fn omega_of_returns(rets: &[f64], threshold: f64) -> f64 {
    let gains: f64 =
        compensated_sum(rets.iter().filter(|&&r| r > threshold).map(|&r| r - threshold));
    let losses: f64 =
        compensated_sum(rets.iter().filter(|&&r| r < threshold).map(|&r| threshold - r));
    if losses == 0.0 {
        return if gains > 0.0 { f64::INFINITY } else { 0.0 };
    }
    gains / losses
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
    compensated_sum(trades.iter().map(|t| t.pnl))
}

pub fn gross_profit(trades: &[Trade]) -> f64 {
    compensated_sum(trades.iter().filter(|t| t.pnl > 0.0).map(|t| t.pnl))
}

pub fn gross_loss(trades: &[Trade]) -> f64 {
    -compensated_sum(trades.iter().filter(|t| t.pnl < 0.0).map(|t| t.pnl))
}

pub fn total_fees(trades: &[Trade]) -> f64 {
    compensated_sum(trades.iter().map(|t| t.fees))
}

/// Average gross PnL per trade (0.0 if there are no trades).
pub fn expected_payoff(trades: &[Trade]) -> f64 {
    if trades.is_empty() { 0.0 } else { net_profit(trades) / trades.len() as f64 }
}

/// Total return / max drawdown. `inf` when there's no drawdown.
pub fn recovery_factor(equity_curve: &[f64]) -> f64 {
    recovery_factor_of_drawdown(equity_curve, max_drawdown(equity_curve))
}

/// [`recovery_factor`] at the maximum drawdown the caller holds ([`calmar_of_drawdown`]'s reason).
pub(crate) fn recovery_factor_of_drawdown(equity_curve: &[f64], drawdown: f64) -> f64 {
    if drawdown == 0.0 {
        return if total_return(equity_curve) > 0.0 { f64::INFINITY } else { 0.0 };
    }
    total_return(equity_curve) / drawdown
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

pub fn average_win(trades: &[Trade]) -> f64 {
    let wins: Vec<f64> = trades.iter().filter(|t| t.pnl > 0.0).map(|t| t.pnl).collect();
    if wins.is_empty() { 0.0 } else { mean(wins.iter().copied()) }
}

pub fn average_loss(trades: &[Trade]) -> f64 {
    let losses: Vec<f64> = trades.iter().filter(|t| t.pnl < 0.0).map(|t| t.pnl).collect();
    if losses.is_empty() { 0.0 } else { mean(losses.iter().copied()) }
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
        let drawdown_percent = drawdown_fraction(peak, v).map_or(0.0, |d| d * 100.0);
        sq_sum += vike_math::powers::square(drawdown_percent);
    }
    (sq_sum / equity_curve.len() as f64).sqrt()
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
    vike_math::regression::slope_t_value(&xs, &log_eq)
}

/// Average win PnL / |average loss PnL|; 0.0 when there are no wins or no losses.
pub fn payoff_ratio(trades: &[Trade]) -> f64 {
    let wins: Vec<f64> = trades.iter().filter(|t| t.pnl > 0.0).map(|t| t.pnl).collect();
    let losses: Vec<f64> = trades.iter().filter(|t| t.pnl < 0.0).map(|t| t.pnl).collect();
    if wins.is_empty() || losses.is_empty() {
        return 0.0;
    }
    let average_w = mean(wins.iter().copied());
    let average_l = mean(losses.iter().copied()); // negative
    if average_l == 0.0 {
        return 0.0;
    }
    average_w / average_l.abs()
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
    risk_return_ratio_of_returns(&returns(equity_curve))
}

/// [`risk_return_ratio`] over the curve's [`returns`].
pub(crate) fn risk_return_ratio_of_returns(rets: &[f64]) -> f64 {
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
    volatility_of_returns(&returns(equity_curve), periods_per_year)
}

/// [`returns_volatility`] over the curve's [`returns`].
pub(crate) fn volatility_of_returns(rets: &[f64], periods_per_year: f64) -> f64 {
    if rets.len() < 2 {
        return 0.0;
    }
    let (_, var) = mean_variance(rets.iter().copied(), 1.0);
    var.sqrt() * periods_per_year.sqrt()
}

/// Bias-corrected sample skewness of per-bar returns (adjusted Fisher-Pearson, matching
/// `pandas.Series.skew`). 0.0 for fewer than 3 returns or zero dispersion.
pub fn returns_skewness(equity_curve: &[f64]) -> f64 {
    sample_skewness(&returns(equity_curve))
}

/// Bias-corrected sample excess kurtosis of per-bar returns (adjusted Fisher-Pearson,
/// matching `pandas.Series.kurt`; 0.0 for a normal distribution). 0.0 for fewer than 4
/// returns or zero dispersion.
pub fn returns_kurtosis(equity_curve: &[f64]) -> f64 {
    sample_excess_kurtosis(&returns(equity_curve))
}

/// Ratio of the right (gain) tail to the left (loss) tail of per-bar returns:
/// `|percentile(r, 0.95) / percentile(r, 0.05)|`. A value above 1 means a heavier upside
/// tail. 0.0 for fewer than 2 returns or a zero/non-finite 5th percentile.
pub fn tail_ratio(equity_curve: &[f64]) -> f64 {
    tail_ratio_of_sorted(&ascending(returns(equity_curve)))
}

/// [`tail_ratio`] over the curve's [`returns`] in [`ascending`] order.
pub(crate) fn tail_ratio_of_sorted(sorted: &[f64]) -> f64 {
    if sorted.len() < 2 {
        return 0.0;
    }
    let p95 = percentile(sorted, 0.95);
    let p5 = percentile(sorted, 0.05);
    if p5 == 0.0 || !p5.is_finite() {
        return 0.0;
    }
    (p95 / p5).abs()
}

/// Historical (non-parametric) Value at Risk: the `1 - confidence` empirical quantile of
/// per-bar returns. Expressed as a return (more negative = greater risk). 0.0 for an
/// empty return series.
pub fn value_at_risk(equity_curve: &[f64], confidence: f64) -> f64 {
    value_at_risk_of_sorted(&ascending(returns(equity_curve)), confidence)
}

/// [`value_at_risk`] over the curve's [`returns`] in [`ascending`] order.
pub(crate) fn value_at_risk_of_sorted(sorted: &[f64], confidence: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    percentile(sorted, 1.0 - confidence)
}

/// Historical Expected Shortfall (CVaR): mean of the returns at or below the
/// `value_at_risk` threshold — the average of the worst `1 - confidence` tail. Always
/// <= the corresponding VaR. 0.0 for an empty return series.
pub fn expected_shortfall(equity_curve: &[f64], confidence: f64) -> f64 {
    expected_shortfall_of_sorted(&ascending(returns(equity_curve)), confidence)
}

/// [`expected_shortfall`] over the curve's [`returns`] in [`ascending`] order.
pub(crate) fn expected_shortfall_of_sorted(sorted: &[f64], confidence: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let var = percentile(sorted, 1.0 - confidence);
    let tail: Vec<f64> = sorted.iter().copied().filter(|&r| r <= var).collect();
    mean(tail.iter().copied())
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

/// Ulcer Performance Index (Martin Ratio): CAGR / Ulcer Index — like [`calmar`] but
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

/// **The ONE best-first ranking comparator over raw f64 scores** — every score-ranked sort
/// (`vike_backtest`'s objective sweeps, TPE, euler and walk-forward search, the backtesthub's trial
/// listing, the Studio's Saved-pane Sharpe rank) goes through it rather than hand-rolling
/// `partial_cmp(..).unwrap_or(Equal)`, the shape that lets a NaN compare Equal to everything and keep
/// whatever position it started in. Higher is better; a NaN ("unrankable") score sorts LAST among
/// successes — the objective-path twin of `vike_backtest::search::paramscan::RankMetric::cmp_reports`'s NaN
/// rule, so a degenerate point can never rank first here either.
///
/// Here since 2026-11-10 (it was `vike_backtest::search::paramscan`'s): the desktop ranks by it and links
/// no harness (`docs/decisions/0078-one-backtest-path-studios-local-backend-is-deleted.md`'s amendment).
pub fn cmp_scores_desc(a: f64, b: f64) -> std::cmp::Ordering {
    match (a.is_nan(), b.is_nan()) {
        (true, true) => std::cmp::Ordering::Equal,
        (true, false) => std::cmp::Ordering::Greater, // a unrankable → a is worse → sorts last
        (false, true) => std::cmp::Ordering::Less,
        (false, false) => b.partial_cmp(&a).expect("non-NaN f64s compare totally"),
    }
}

#[path = "metrics_tests.rs"]
#[cfg(test)]
mod metrics_tests;
