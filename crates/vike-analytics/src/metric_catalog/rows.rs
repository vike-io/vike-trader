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
    // --- the compact eight: on `TradingResults` itself since the document's first version -------
    row!("final_equity", Compact, Money, "account value at the last equity sample"),
    row!("total_return", Compact, Percent, "first-to-last equity change as a fraction of the start"),
    row!("n_trades", Compact, Count, "round trips CLOSED (an open position at the end counts for none)"),
    row!("win_rate", Compact, Percent, "share of closed trades with positive PnL before fees"),
    row!("sharpe", Compact, Ratio, "annualized mean/stdev of per-sample returns"),
    row!("max_drawdown", Compact, Percent, "deepest peak-to-trough fall, as a fraction of that peak"),
    row!("profit_factor", Compact, Ratio, "gross profit / gross loss, both before fees; inf when nothing lost"),
    row!("funding_paid", Compact, Money, "net perp funding cashflow, received-positive"),
    // --- the trade ledger's own statistics ------------------------------------------------------
    row!("net_profit", Extended, Money, "sum of closed-trade PnL before fees (total_fees is apart)"),
    row!("gross_profit", Extended, Money, "sum of the winning trades alone, before fees"),
    row!("gross_loss", Extended, Money, "sum of the losing trades alone, before fees"),
    row!("total_fees", Extended, Money, "round-trip fees charged across every closed trade"),
    row!("average_win", Extended, Money, "mean PnL of the winning trades, before fees"),
    row!("average_loss", Extended, Money, "mean PnL of the losing trades, before fees"),
    row!("largest_win", Extended, Money, "best single closed trade, before fees"),
    row!("largest_loss", Extended, Money, "worst single closed trade, before fees"),
    row!("payoff_ratio", Extended, Ratio, "average_win over the absolute average_loss, both before fees"),
    row!("expected_payoff", Extended, Money, "net_profit per closed trade, before fees"),
    row!("consecutive_wins", Extended, Count, "longest unbroken run of winning trades (PnL before fees)"),
    row!("consecutive_losses", Extended, Count, "longest unbroken run of losing trades (PnL before fees)"),
    row!("sqn", Extended, Ratio, "System Quality Number — trade expectancy before fees over its own noise, scaled by sqrt(n)"),
    row!("long_ratio", Extended, Percent, "share of closed trades entered long"),
    // --- the equity curve's own statistics ------------------------------------------------------
    row!("sortino", Extended, Ratio, "Sharpe with only DOWNSIDE deviation in the denominator"),
    row!("calmar", Extended, Ratio, "annualized return (CAGR) over max drawdown; the MAR ratio is the same number"),
    row!("cagr", Extended, Percent, "compound annual growth rate of the curve"),
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
    // --- closed-trade accounting: one lot per entry, first in first out, every row net of both commissions -----
    // Only on a run that armed `engine.closed_trades` (`MetricHome::ClosedTrades`); the ALL column of its block.
    row!("closed_trades", ClosedTrades, Count, "closed rows: one per closing fill and entry lot it reaches"),
    row!("closed_winners", ClosedTrades, Count, "closed rows with a positive net"),
    row!("closed_losers", ClosedTrades, Count, "closed rows with a negative net"),
    row!("closed_even", ClosedTrades, Count, "closed rows with a net of exactly zero"),
    row!("closed_percent_profitable", ClosedTrades, Percent, "winning rows over all closed rows, even ones included"),
    row!("closed_net_profit", ClosedTrades, Money, "sum of the closed rows' net, after commission"),
    row!("closed_gross_profit", ClosedTrades, Money, "sum of the winning rows' net"),
    row!("closed_gross_loss", ClosedTrades, Money, "sum of the losing rows' net, as a positive amount"),
    row!("closed_profit_factor", ClosedTrades, Ratio, "the winning rows' net over the losing rows' net, both after commission; inf when nothing lost"),
    row!("closed_expected_payoff", ClosedTrades, Money, "closed net profit per closed row"),
    row!("closed_average_win", ClosedTrades, Money, "mean net of the winning rows"),
    row!("closed_average_loss", ClosedTrades, Money, "mean net of the losing rows"),
    row!("closed_win_loss_ratio", ClosedTrades, Ratio, "closed_average_win over the absolute closed_average_loss"),
    row!("closed_largest_win", ClosedTrades, Money, "best closed row's net"),
    row!("closed_largest_loss", ClosedTrades, Money, "worst closed row's net"),
    row!("closed_largest_win_percent", ClosedTrades, Percent, "the best row as a share of the closed gross profit"),
    row!("closed_largest_loss_percent", ClosedTrades, Percent, "the worst row as a share of the closed gross loss"),
    row!("closed_average_bars", ClosedTrades, Ratio, "mean bars from entry to exit over the closed rows"),
    row!("closed_average_bars_win", ClosedTrades, Ratio, "mean bars of the winning rows"),
    row!("closed_average_bars_loss", ClosedTrades, Ratio, "mean bars of the losing rows"),
    row!("closed_max_held", ClosedTrades, Ratio, "the most quantity held at once"),
    row!("closed_commission", ClosedTrades, Money, "commission charged to the closed rows, entry and exit shares"),
    row!("closed_open_trades", ClosedTrades, Count, "entry lots still open at the end, in no other closed_ row"),
    row!("closed_open_pnl", ClosedTrades, Money, "price P&L of the open lots at the last price, before commission"),
    row!("closed_expectancy_percent", ClosedTrades, Percent, "mean of the closed rows' net percents, each its net over its entry's value"),
    row!("closed_outliers", ClosedTrades, Count, "closed rows whose net percent lies over two deviations from the mean percent"),
    row!("closed_outliers_pnl", ClosedTrades, Money, "the outlier rows' net"),
    row!("closed_outliers_pnl_percent", ClosedTrades, Percent, "closed_outliers_pnl over the initial capital"),
    row!("closed_average_win_percent", ClosedTrades, Percent, "mean net percent of the winning rows, outliers left out"),
    row!("closed_average_loss_percent", ClosedTrades, Percent, "mean net percent of the losing rows, outliers left out"),
    row!("closed_win_streak", ClosedTrades, Count, "the most winning closed rows in a row"),
    row!("closed_loss_streak", ClosedTrades, Count, "the most losing closed rows in a row"),
    row!("closed_average_win_streak", ClosedTrades, Ratio, "mean length of the runs of winning rows"),
    row!("closed_average_loss_streak", ClosedTrades, Ratio, "mean length of the runs of losing rows"),
    // The block's account-wide figures (`closed_trades.account`). The equity at a bar: the initial capital, the closed
    // rows' net, the open lots' P&L less the entry commission they carry. The closed-trade equity: after each closed row.
    row!("closed_total_pnl", ClosedTrades, Money, "closed net profit plus the open lots' P&L at the last price, less their entry commission"),
    row!("closed_total_return", ClosedTrades, Percent, "closed_total_pnl over the initial capital"),
    row!("closed_net_profit_percent", ClosedTrades, Percent, "closed net profit over the initial capital"),
    row!("closed_drawdown", ClosedTrades, Money, "max drawdown, intrabar: from the highest closed-trade equity to a later bar's adverse extreme"),
    row!("closed_drawdown_percent", ClosedTrades, Percent, "closed_drawdown over the equity peak it fell from"),
    row!("closed_drawdown_init_percent", ClosedTrades, Percent, "closed_drawdown over the initial capital"),
    row!("closed_runup", ClosedTrades, Money, "max run-up, intrabar: from the lowest closed-trade equity to a later bar's favourable extreme"),
    row!("closed_runup_percent", ClosedTrades, Percent, "closed_runup over the equity trough it rose from"),
    row!("closed_runup_init_percent", ClosedTrades, Percent, "closed_runup over the initial capital"),
    row!("closed_drawdown_close", ClosedTrades, Money, "max drawdown, close-to-close: the deepest fall of the closed-trade equity below its peak"),
    row!("closed_drawdown_close_percent", ClosedTrades, Percent, "closed_drawdown_close over that peak"),
    row!("closed_runup_close", ClosedTrades, Money, "max run-up, close-to-close: the highest rise of the closed-trade equity above its trough"),
    row!("closed_runup_close_percent", ClosedTrades, Percent, "closed_runup_close over that trough"),
    row!("closed_average_drawdown_percent", ClosedTrades, Percent, "mean depth of the closed-trade equity's drawdowns, each over its peak"),
    row!("closed_average_drawdown_days", ClosedTrades, Ratio, "mean calendar days of those drawdowns, peak to lowest point"),
    row!("closed_average_runup_percent", ClosedTrades, Percent, "mean height of the closed-trade equity's run-ups (trough to the next peak), each over its trough"),
    row!("closed_average_runup_days", ClosedTrades, Ratio, "mean calendar days of those run-ups, trough to peak"),
    row!("closed_runup_now_percent", ClosedTrades, Percent, "the run-up of the closed-trade equity still in progress, over its trough"),
    row!("closed_sharpe", ClosedTrades, Ratio, "monthly Sharpe of the equity at bar closes: mean monthly return less the risk-free rate's month, over the returns' deviation"),
    row!("closed_sortino", ClosedTrades, Ratio, "monthly Sortino: as closed_sharpe, over the deviation below the risk-free rate's month"),
    row!("closed_cagr", ClosedTrades, Percent, "closed_net_profit_percent compounded to a year of 365 calendar days, first bar to last"),
    row!("closed_buy_hold", ClosedTrades, Money, "the initial capital held in the first entry's symbol from that entry's price to the last"),
    row!("closed_buy_hold_percent", ClosedTrades, Percent, "closed_buy_hold over the initial capital"),
    row!("closed_buy_hold_chart", ClosedTrades, Money, "buy and hold at the last closed row's exit when nothing is open, else closed_buy_hold"),
    row!("closed_outperformance", ClosedTrades, Percent, "closed_total_return less closed_buy_hold_percent"),
    row!("closed_margin_calls", ClosedTrades, Count, "the engine's margin calls: cuts a position short of margin was forced into"),
    row!("closed_average_margin", ClosedTrades, Money, "margin the open lots used at each bar's close, averaged from the first entry's bar"),
    row!("closed_margin_eff", ClosedTrades, Ratio, "closed net profit over closed_average_margin: the margin efficiency"),
];

/// Metrics this tree COMPUTES and a run record cannot feed, each with the reason.
///
/// ⚠ **A row here is a refusal that explains itself, not a TODO.** An operator who types a name
/// from `crate::metrics` and gets "unknown metric" concludes they typed it wrong; the truth is that
/// the input does not exist on disk, which is a different problem with a different fix. Adding a
/// row is therefore cheap, and removing one means an input genuinely arrived.
pub const ABSENT: &[(&str, &str)] = &[(
    "exposure",
    "it needs the PER-STEP POSITION SIZES, and TradingRecord carries only the equity curve and \
     the closed-trade ledger — no run record can answer it, so it is declared absent rather than \
     computed against a substitute",
)];
