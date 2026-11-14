//! The comparison itself: every difference between two runs, field by field and bit for bit.

use vike_analytics::{BacktestResult, metrics};
use vike_model::{OrderDrop, OrderResting, Trade};

use super::PERIODS_PER_YEAR;

// ---------------------------------------------------------------------------------------------
// Comparison — field by field, value by value, bit for bit
// ---------------------------------------------------------------------------------------------

/// `None` when the two `f64`s are the SAME BIT PATTERN, a described difference otherwise.
///
/// Bits rather than `==` for two reasons this tree already relies on
/// (`crates/vike-simulator/src/engine/tests.rs`'s own cross-order equality tests do the same): `==` calls
/// two `NaN`s different when they are the identical computed answer, and it calls `0.0` and
/// `-0.0` the same when they are not. Neither tolerance nor rounding appears anywhere in this
/// file on purpose.
fn f64_diff(label: &str, compiled: f64, strategy_file: f64) -> Option<String> {
    if compiled.to_bits() == strategy_file.to_bits() {
        return None;
    }
    Some(format!(
        "{label}: compiled {compiled:?} (bits {:#018x}) vs strategy file {strategy_file:?} (bits {:#018x}), \
         difference {:?}",
        compiled.to_bits(),
        strategy_file.to_bits(),
        strategy_file - compiled
    ))
}

/// Every difference between the two trade lists, described — not the first one, and never a
/// length check standing in for a comparison. `assert_eq!(a.len(), b.len())` passes for two runs
/// that traded completely differently, which is the failure mode this function exists to refuse.
pub(super) fn trade_diffs(compiled: &[Trade], strategy_file: &[Trade]) -> Vec<String> {
    let mut out = Vec::new();
    if compiled.len() != strategy_file.len() {
        out.push(format!(
            "trade COUNT: compiled {} vs strategy file {}",
            compiled.len(),
            strategy_file.len()
        ));
    }
    for (i, (c, p)) in compiled.iter().zip(strategy_file.iter()).enumerate() {
        for (name, cv, pv) in [
            ("entry_price", c.entry_price, p.entry_price),
            ("exit_price", c.exit_price, p.exit_price),
            ("size", c.size, p.size),
            ("pnl", c.pnl, p.pnl),
            ("fees", c.fees, p.fees),
            ("mae", c.mae, p.mae),
            ("mfe", c.mfe, p.mfe),
        ] {
            if let Some(d) = f64_diff(&format!("trade[{i}].{name}"), cv, pv) {
                out.push(d);
            }
        }
        if c.entry_timestamp != p.entry_timestamp {
            out.push(format!(
                "trade[{i}].entry_timestamp: compiled {} vs strategy file {}",
                c.entry_timestamp, p.entry_timestamp
            ));
        }
        if c.exit_timestamp != p.exit_timestamp {
            out.push(format!(
                "trade[{i}].exit_timestamp: compiled {} vs strategy file {}",
                c.exit_timestamp, p.exit_timestamp
            ));
        }
        if c.symbol != p.symbol {
            out.push(format!(
                "trade[{i}].symbol: compiled {:?} vs strategy file {:?}",
                c.symbol, p.symbol
            ));
        }
        if c.is_long != p.is_long {
            out.push(format!(
                "trade[{i}].is_long: compiled {} vs strategy file {}",
                c.is_long, p.is_long
            ));
        }
    }
    out
}

/// The metric catalog computed over a result — the numbers an operator reads off a run, as
/// VALUES rather than as a count of them.
fn metric_values(r: &BacktestResult) -> Vec<(&'static str, f64)> {
    let eq = &r.equity_curve;
    let tr = &r.trades;
    vec![
        ("final_equity", r.final_equity),
        ("total_return", metrics::total_return(eq)),
        ("max_drawdown", metrics::max_drawdown(eq)),
        ("sharpe", metrics::sharpe(eq, PERIODS_PER_YEAR)),
        ("sortino", metrics::sortino(eq, PERIODS_PER_YEAR)),
        ("calmar", metrics::calmar(eq, PERIODS_PER_YEAR)),
        ("cagr", metrics::cagr(eq, PERIODS_PER_YEAR)),
        ("omega", metrics::omega(eq, 0.0)),
        ("ulcer_index", metrics::ulcer_index(eq)),
        ("ulcer_performance_index", metrics::ulcer_performance_index(eq, PERIODS_PER_YEAR)),
        ("recovery_factor", metrics::recovery_factor(eq)),
        ("risk_return_ratio", metrics::risk_return_ratio(eq)),
        ("returns_volatility", metrics::returns_volatility(eq, PERIODS_PER_YEAR)),
        ("returns_skewness", metrics::returns_skewness(eq)),
        ("returns_kurtosis", metrics::returns_kurtosis(eq)),
        ("tail_ratio", metrics::tail_ratio(eq)),
        ("value_at_risk", metrics::value_at_risk(eq, 0.95)),
        ("expected_shortfall", metrics::expected_shortfall(eq, 0.95)),
        ("k_ratio", metrics::k_ratio(eq)),
        ("win_rate", metrics::win_rate(tr)),
        ("profit_factor", metrics::profit_factor(tr)),
        ("net_profit", metrics::net_profit(tr)),
        ("gross_profit", metrics::gross_profit(tr)),
        ("gross_loss", metrics::gross_loss(tr)),
        ("total_fees", metrics::total_fees(tr)),
        ("expected_payoff", metrics::expected_payoff(tr)),
        ("largest_win", metrics::largest_win(tr)),
        ("largest_loss", metrics::largest_loss(tr)),
        ("average_win", metrics::average_win(tr)),
        ("average_loss", metrics::average_loss(tr)),
        ("payoff_ratio", metrics::payoff_ratio(tr)),
        ("long_ratio", metrics::long_ratio(tr)),
        ("sqn", metrics::sqn(tr)),
    ]
}

pub(super) fn metric_diffs(
    compiled: &BacktestResult,
    strategy_file: &BacktestResult,
) -> Vec<String> {
    let (a, b) = (metric_values(compiled), metric_values(strategy_file));
    let mut out = Vec::new();
    for ((name, cv), (_, pv)) in a.into_iter().zip(b) {
        if let Some(d) = f64_diff(&format!("metric {name}"), cv, pv) {
            out.push(d);
        }
    }
    out
}

/// Everything on the result that is NOT a trade or a metric: the equity curve point by point,
/// the timestamps, and the engine's own diagnostic counters. A strategy that traded identically
/// but was fed a different bar index would show up here first.
pub fn result_shape_diffs(
    compiled: &BacktestResult,
    strategy_file: &BacktestResult,
) -> Vec<String> {
    let mut out = Vec::new();
    if compiled.n_trades != strategy_file.n_trades {
        out.push(format!(
            "n_trades: compiled {} vs strategy file {}",
            compiled.n_trades, strategy_file.n_trades
        ));
    }
    if compiled.equity_curve.len() != strategy_file.equity_curve.len() {
        out.push(format!(
            "equity_curve LENGTH: compiled {} vs strategy file {}",
            compiled.equity_curve.len(),
            strategy_file.equity_curve.len()
        ));
    }
    for (i, (c, p)) in compiled.equity_curve.iter().zip(&strategy_file.equity_curve).enumerate() {
        if let Some(d) = f64_diff(&format!("equity_curve[{i}]"), *c, *p) {
            out.push(d);
        }
    }
    // Element by element with its index, not a whole-vector `!=`. A bare inequality was the one
    // detail-free message left in this file: it said the timestamps differed and nothing about
    // WHERE or BY HOW MUCH, which is the shape of report this whole comparison exists to avoid.
    if compiled.equity_timestamp.len() != strategy_file.equity_timestamp.len() {
        out.push(format!(
            "equity_timestamp LENGTH: compiled {} vs strategy file {}",
            compiled.equity_timestamp.len(),
            strategy_file.equity_timestamp.len()
        ));
    }
    for (i, (c, p)) in
        compiled.equity_timestamp.iter().zip(&strategy_file.equity_timestamp).enumerate()
    {
        if c != p {
            out.push(format!(
                "equity_timestamp[{i}]: compiled {c} vs strategy file {p} (difference {})",
                p - c
            ));
        }
    }
    if compiled.per_symbol_pnl.len() != strategy_file.per_symbol_pnl.len() {
        out.push("per_symbol_pnl LENGTH differs".to_string());
    }
    for (i, ((cs, cv), (ps, pv))) in
        compiled.per_symbol_pnl.iter().zip(&strategy_file.per_symbol_pnl).enumerate()
    {
        if cs != ps {
            out.push(format!(
                "per_symbol_pnl[{i}] symbol: compiled {cs:?} vs strategy file {ps:?}"
            ));
        }
        if let Some(d) = f64_diff(&format!("per_symbol_pnl[{i}] value"), *cv, *pv) {
            out.push(d);
        }
    }
    for (name, c, p) in [
        ("stale_deferrals", compiled.stale_deferrals, strategy_file.stale_deferrals),
        ("session_deferrals", compiled.session_deferrals, strategy_file.session_deferrals),
        ("impact_unpriced", compiled.impact_unpriced, strategy_file.impact_unpriced),
    ] {
        if c != p {
            out.push(format!("{name}: compiled {c} vs strategy file {p}"));
        }
    }
    // Field by field, like everything else. This was a LENGTH check, which would have passed two
    // runs whose orders were refused for entirely different reasons — and the gate reason is
    // exactly the interesting part of a dropped order.
    if compiled.dropped.len() != strategy_file.dropped.len() {
        out.push(format!(
            "dropped ORDER COUNT: compiled {} vs strategy file {}",
            compiled.dropped.len(),
            strategy_file.dropped.len()
        ));
    }
    for (
        i,
        (
            OrderDrop { symbol: cs, reason: cr, size: csz, weight: cw },
            OrderDrop { symbol: ps, reason: pr, size: psz, weight: pw },
        ),
    ) in compiled.dropped.iter().zip(&strategy_file.dropped).enumerate()
    {
        if cs != ps {
            out.push(format!("dropped[{i}].symbol: compiled {cs:?} vs strategy file {ps:?}"));
        }
        if cr != pr {
            out.push(format!("dropped[{i}].reason: compiled {cr:?} vs strategy file {pr:?}"));
        }
        if let Some(d) = f64_diff(&format!("dropped[{i}].size"), *csz, *psz) {
            out.push(d);
        }
        if let Some(d) = f64_diff(&format!("dropped[{i}].weight"), *cw, *pw) {
            out.push(d);
        }
    }
    out
}

/// The engine's still-working orders, compared field by field — the channel `on_stop` reaches.
///
/// Field by field rather than by `PartialEq` on the whole vector (which `OrderResting` does
/// derive) for the reason every other comparison in this file is: `assert_eq!` on two vectors of
/// f64-carrying structs reports "not equal" and nothing about WHERE, and an f64 `==` calls two
/// NaNs different and `0.0`/`-0.0` the same. The bit comparison is `f64_diff`'s, shared with the
/// trade and metric comparisons.
pub(super) fn pending_diffs(
    compiled: &[OrderResting],
    strategy_file: &[OrderResting],
) -> Vec<String> {
    let mut out = Vec::new();
    if compiled.len() != strategy_file.len() {
        out.push(format!(
            "pending ORDER COUNT: compiled {} vs strategy file {}",
            compiled.len(),
            strategy_file.len()
        ));
    }
    for (i, (c, p)) in compiled.iter().zip(strategy_file.iter()).enumerate() {
        if c.kind != p.kind {
            out.push(format!(
                "pending[{i}].kind: compiled {:?} vs strategy file {:?}",
                c.kind, p.kind
            ));
        }
        if c.side != p.side {
            out.push(format!("pending[{i}].side: compiled {} vs strategy file {}", c.side, p.side));
        }
        if let Some(d) = f64_diff(&format!("pending[{i}].size"), c.size, p.size) {
            out.push(d);
        }
        // `Option<f64>` compared as a pair: PRESENCE first (a `None` and a `Some` are not a bit
        // difference), then the bits when both are present.
        for (name, cv, pv) in
            [("price", c.price, p.price), ("trail", c.trail, p.trail), ("stop", c.stop, p.stop)]
        {
            match (cv, pv) {
                (Some(a), Some(b)) => {
                    if let Some(d) = f64_diff(&format!("pending[{i}].{name}"), a, b) {
                        out.push(d);
                    }
                }
                (a, b) if a.is_some() != b.is_some() => out.push(format!(
                    "pending[{i}].{name}: compiled {a:?} vs strategy file {b:?} (presence differs)"
                )),
                _ => {}
            }
        }
        if let Some(d) = f64_diff(&format!("pending[{i}].weight"), c.weight, p.weight) {
            out.push(d);
        }
    }
    out
}
