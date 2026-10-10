//! The comparison itself: every difference between two runs, field by field and bit for bit.

use vike_analytics::{BacktestResult, metrics};
use vike_model::{Trade, WorkingOrder};

use super::PERIODS_PER_YEAR;

// ---------------------------------------------------------------------------------------------
// Comparison — field by field, value by value, bit for bit
// ---------------------------------------------------------------------------------------------

/// `None` when the two `f64`s are the SAME BIT PATTERN, a described difference otherwise.
///
/// Bits rather than `==` for two reasons this tree already relies on
/// (`crates/vike-sim/src/engine.rs`'s own cross-order equality tests do the same): `==` calls
/// two `NaN`s different when they are the identical computed answer, and it calls `0.0` and
/// `-0.0` the same when they are not. Neither tolerance nor rounding appears anywhere in this
/// file on purpose.
fn f64_diff(label: &str, compiled: f64, plugin: f64) -> Option<String> {
    if compiled.to_bits() == plugin.to_bits() {
        return None;
    }
    Some(format!(
        "{label}: compiled {compiled:?} (bits {:#018x}) vs plugin {plugin:?} (bits {:#018x}), \
         difference {:?}",
        compiled.to_bits(),
        plugin.to_bits(),
        plugin - compiled
    ))
}

/// Every difference between the two trade lists, described — not the first one, and never a
/// length check standing in for a comparison. `assert_eq!(a.len(), b.len())` passes for two runs
/// that traded completely differently, which is the failure mode this function exists to refuse.
pub(super) fn trade_diffs(compiled: &[Trade], plugin: &[Trade]) -> Vec<String> {
    let mut out = Vec::new();
    if compiled.len() != plugin.len() {
        out.push(format!("trade COUNT: compiled {} vs plugin {}", compiled.len(), plugin.len()));
    }
    for (i, (c, p)) in compiled.iter().zip(plugin.iter()).enumerate() {
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
        if c.entry_ts != p.entry_ts {
            out.push(format!(
                "trade[{i}].entry_ts: compiled {} vs plugin {}",
                c.entry_ts, p.entry_ts
            ));
        }
        if c.exit_ts != p.exit_ts {
            out.push(format!("trade[{i}].exit_ts: compiled {} vs plugin {}", c.exit_ts, p.exit_ts));
        }
        if c.symbol != p.symbol {
            out.push(format!(
                "trade[{i}].symbol: compiled {:?} vs plugin {:?}",
                c.symbol, p.symbol
            ));
        }
        if c.is_long != p.is_long {
            out.push(format!("trade[{i}].is_long: compiled {} vs plugin {}", c.is_long, p.is_long));
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
        ("mar_ratio", metrics::mar_ratio(eq, PERIODS_PER_YEAR)),
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
        ("avg_win", metrics::avg_win(tr)),
        ("avg_loss", metrics::avg_loss(tr)),
        ("payoff_ratio", metrics::payoff_ratio(tr)),
        ("long_ratio", metrics::long_ratio(tr)),
        ("sqn", metrics::sqn(tr)),
    ]
}

pub(super) fn metric_diffs(compiled: &BacktestResult, plugin: &BacktestResult) -> Vec<String> {
    let (a, b) = (metric_values(compiled), metric_values(plugin));
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
pub fn result_shape_diffs(compiled: &BacktestResult, plugin: &BacktestResult) -> Vec<String> {
    let mut out = Vec::new();
    if compiled.n_trades != plugin.n_trades {
        out.push(format!("n_trades: compiled {} vs plugin {}", compiled.n_trades, plugin.n_trades));
    }
    if compiled.equity_curve.len() != plugin.equity_curve.len() {
        out.push(format!(
            "equity_curve LENGTH: compiled {} vs plugin {}",
            compiled.equity_curve.len(),
            plugin.equity_curve.len()
        ));
    }
    for (i, (c, p)) in compiled.equity_curve.iter().zip(&plugin.equity_curve).enumerate() {
        if let Some(d) = f64_diff(&format!("equity_curve[{i}]"), *c, *p) {
            out.push(d);
        }
    }
    // Element by element with its index, not a whole-vector `!=`. A bare inequality was the one
    // detail-free message left in this file: it said the timestamps differed and nothing about
    // WHERE or BY HOW MUCH, which is the shape of report this whole comparison exists to avoid.
    if compiled.equity_ts.len() != plugin.equity_ts.len() {
        out.push(format!(
            "equity_ts LENGTH: compiled {} vs plugin {}",
            compiled.equity_ts.len(),
            plugin.equity_ts.len()
        ));
    }
    for (i, (c, p)) in compiled.equity_ts.iter().zip(&plugin.equity_ts).enumerate() {
        if c != p {
            out.push(format!("equity_ts[{i}]: compiled {c} vs plugin {p} (difference {})", p - c));
        }
    }
    if compiled.per_symbol_pnl.len() != plugin.per_symbol_pnl.len() {
        out.push("per_symbol_pnl LENGTH differs".to_string());
    }
    for (i, ((cs, cv), (ps, pv))) in
        compiled.per_symbol_pnl.iter().zip(&plugin.per_symbol_pnl).enumerate()
    {
        if cs != ps {
            out.push(format!("per_symbol_pnl[{i}] symbol: compiled {cs:?} vs plugin {ps:?}"));
        }
        if let Some(d) = f64_diff(&format!("per_symbol_pnl[{i}] value"), *cv, *pv) {
            out.push(d);
        }
    }
    for (name, c, p) in [
        ("stale_deferrals", compiled.stale_deferrals, plugin.stale_deferrals),
        ("session_deferrals", compiled.session_deferrals, plugin.session_deferrals),
        ("impact_unpriced", compiled.impact_unpriced, plugin.impact_unpriced),
    ] {
        if c != p {
            out.push(format!("{name}: compiled {c} vs plugin {p}"));
        }
    }
    // Field by field, like everything else. This was a LENGTH check, which would have passed two
    // runs whose orders were refused for entirely different reasons — and the gate reason is
    // exactly the interesting part of a dropped order.
    if compiled.dropped.len() != plugin.dropped.len() {
        out.push(format!(
            "dropped ORDER COUNT: compiled {} vs plugin {}",
            compiled.dropped.len(),
            plugin.dropped.len()
        ));
    }
    for (i, ((cs, cr, csz, cw), (ps, pr, psz, pw))) in
        compiled.dropped.iter().zip(&plugin.dropped).enumerate()
    {
        if cs != ps {
            out.push(format!("dropped[{i}].symbol: compiled {cs:?} vs plugin {ps:?}"));
        }
        if cr != pr {
            out.push(format!("dropped[{i}].reason: compiled {cr:?} vs plugin {pr:?}"));
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
/// Field by field rather than by `PartialEq` on the whole vector (which `WorkingOrder` does
/// derive) for the reason every other comparison in this file is: `assert_eq!` on two vectors of
/// f64-carrying structs reports "not equal" and nothing about WHERE, and an f64 `==` calls two
/// NaNs different and `0.0`/`-0.0` the same. The bit comparison is `f64_diff`'s, shared with the
/// trade and metric comparisons.
pub(super) fn pending_diffs(compiled: &[WorkingOrder], plugin: &[WorkingOrder]) -> Vec<String> {
    let mut out = Vec::new();
    if compiled.len() != plugin.len() {
        out.push(format!(
            "pending ORDER COUNT: compiled {} vs plugin {}",
            compiled.len(),
            plugin.len()
        ));
    }
    for (i, (c, p)) in compiled.iter().zip(plugin.iter()).enumerate() {
        if c.kind != p.kind {
            out.push(format!("pending[{i}].kind: compiled {:?} vs plugin {:?}", c.kind, p.kind));
        }
        if c.side != p.side {
            out.push(format!("pending[{i}].side: compiled {} vs plugin {}", c.side, p.side));
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
                    "pending[{i}].{name}: compiled {a:?} vs plugin {b:?} (presence differs)"
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
