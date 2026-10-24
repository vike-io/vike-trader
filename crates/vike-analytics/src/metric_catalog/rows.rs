//! The catalog's DATA: the `METRICS` roster in render order and the `ABSENT` refusals.

use super::{MetricHome, MetricSpec, MetricUnit};

// One `METRICS` row on one line: `row!(id, home, unit, what)`, where `home` names a `MetricHome`
// variant and `unit` a `MetricUnit` one. `METRICS` is `#[rustfmt::skip]` so the rows stay one
// line each: rustfmt would re-wrap every call longer than `max_width` one argument per line.
macro_rules! row {
    ($id:literal, $home:ident, $unit:ident, $what:literal) => {
        MetricSpec { id: $id, home: MetricHome::$home, unit: MetricUnit::$unit, what: $what }
    };
}

/// The roster. **Declaration order is render order**, so this array is also the answer to "in what
/// order does a report print", and no consumer keeps a second ordering.
///
/// Grouped by what the number is computed FROM — the trade ledger, then the equity curve — because
/// that is the grouping that predicts which half of a run record can still answer a question when
/// the other half is missing.
#[rustfmt::skip]
pub const METRICS: &[MetricSpec] = &[
    // --- the compact eight: on `BacktestReport` itself since the document's first version -------
    row!("final_equity", Compact, Money, "account value at the last equity sample"),
    row!("total_return", Compact, Percent, "first-to-last equity change as a fraction of the start"),
    row!("n_trades", Compact, Count, "round trips CLOSED (an open position at the end counts for none)"),
    row!("win_rate", Compact, Percent, "share of closed trades with positive PnL"),
    row!("sharpe", Compact, Ratio, "annualized mean/stdev of per-sample returns"),
    row!("max_drawdown", Compact, Percent, "deepest peak-to-trough fall, as a fraction of that peak"),
    row!("profit_factor", Compact, Ratio, "gross profit / gross loss; inf when nothing lost"),
    row!("funding_paid", Compact, Money, "net perp funding cashflow, received-positive"),
    // --- the trade ledger's own statistics ------------------------------------------------------
    row!("net_profit", Extended, Money, "sum of closed-trade PnL"),
    row!("gross_profit", Extended, Money, "sum of the winning trades alone"),
    row!("gross_loss", Extended, Money, "sum of the losing trades alone"),
    row!("total_fees", Extended, Money, "round-trip fees charged across every closed trade"),
    row!("avg_win", Extended, Money, "mean PnL of the winning trades"),
    row!("avg_loss", Extended, Money, "mean PnL of the losing trades"),
    row!("largest_win", Extended, Money, "best single closed trade"),
    row!("largest_loss", Extended, Money, "worst single closed trade"),
    row!("payoff_ratio", Extended, Ratio, "avg_win over the absolute avg_loss"),
    row!("expected_payoff", Extended, Money, "net profit per closed trade"),
    row!("consecutive_wins", Extended, Count, "longest unbroken run of winning trades"),
    row!("consecutive_losses", Extended, Count, "longest unbroken run of losing trades"),
    row!("sqn", Extended, Ratio, "System Quality Number — trade expectancy over its own noise, scaled by sqrt(n)"),
    row!("long_ratio", Extended, Percent, "share of closed trades entered long"),
    // --- the equity curve's own statistics ------------------------------------------------------
    row!("sortino", Extended, Ratio, "Sharpe with only DOWNSIDE deviation in the denominator"),
    row!("calmar", Extended, Ratio, "annualized return over max drawdown"),
    row!("cagr", Extended, Percent, "compound annual growth rate of the curve"),
    row!("mar_ratio", Extended, Ratio, "CAGR over max drawdown"),
    row!("recovery_factor", Extended, Ratio, "total gain over max drawdown — how many drawdowns the run earned back"),
    row!("ulcer_index", Extended, Ratio, "root-mean-square drawdown depth: how much TIME was spent underwater, not just how deep it went"),
    row!("ulcer_performance_index", Extended, Ratio, "annualized return over the ulcer index"),
    row!("k_ratio", Extended, Ratio, "slope over standard error of the log-equity regression — how STRAIGHT the curve is"),
    row!("risk_return_ratio", Extended, Ratio, "mean return over return stdev, unannualized"),
    row!("returns_volatility", Extended, Ratio, "annualized standard deviation of per-sample returns"),
    row!("returns_skewness", Extended, Ratio, "third moment of the return distribution — negative means rare large losses"),
    row!("returns_kurtosis", Extended, Ratio, "excess fourth moment — how fat the return tails are"),
    row!("tail_ratio", Extended, Ratio, "95th percentile return over the absolute 5th percentile return"),
    row!("omega", Extended, Ratio, "gains above a zero threshold over losses below it, whole-distribution"),
    row!("value_at_risk_95", Extended, Ratio, "historical 95% Value-at-Risk of per-sample returns"),
    row!("expected_shortfall_95", Extended, Ratio, "historical 95% expected shortfall (CVaR) — the mean of the tail VaR cuts off"),
];

/// Metrics this tree COMPUTES and a run record cannot feed, each with the reason.
///
/// ⚠ **A row here is a refusal that explains itself, not a TODO.** An operator who types a name
/// from `crate::metrics` and gets "unknown metric" concludes they typed it wrong; the truth is that
/// the input does not exist on disk, which is a different problem with a different fix. Adding a
/// row is therefore cheap, and removing one means an input genuinely arrived.
pub const ABSENT: &[(&str, &str)] = &[(
    "exposure",
    "it needs the PER-STEP POSITION SIZES, and BacktestResult carries only the equity curve and \
     the closed-trade ledger — no run record can answer it, so it is declared absent rather than \
     computed against a substitute",
)];
