//! `BacktestReport` tests: the fixtures and the old-document builders several topics share.

use super::*;
use vike_model::Trade;

pub(super) fn trade(pnl: f64) -> Trade {
    crate::test_support::trade(pnl, 0.0, true, 1)
}

pub(super) fn sample_result() -> BacktestResult {
    BacktestResult {
        trades: vec![trade(10.0), trade(-5.0)],
        equity_curve: vec![1000.0, 1010.0, 990.0, 1005.0],
        final_equity: 1005.0,
        n_trades: 2,
        intrabar_both_hit: 0,
        per_symbol_pnl: vec![("BTCUSDT".to_string(), 5.0)],
        per_symbol_curves: Vec::new(),
        equity_ts: Vec::new(),
        stale_deferrals: 0,
        impact_unpriced: 0,
        session_deferrals: 0,
        dropped: Vec::new(),
        below_min_reversals: 0,
        warmup: 0,
        funding_paid: 0.0,
        maker_fills: 0,
        taker_fills: 2,
        fees_paid: 0.0,
    }
}

/// A report with nothing degenerate in it — the base the sentinel tests perturb one field of.
pub(super) fn a_finite_report() -> BacktestReport {
    BacktestReport {
        name: None,
        final_equity: 100_000.0,
        total_return: 0.0,
        n_trades: 0,
        win_rate: 0.0,
        sharpe: 0.0,
        max_drawdown: 0.0,
        profit_factor: 0.0,
        funding_paid: 0.0,
        per_symbol_pnl: Vec::new(),
        zero_trade: None,
        extended: None,
        honesty: None,
        realism: None,
    }
}

/// The seven fields that have existed since `report.json`'s first version, as `(key, value)`.
/// The four NOT here — `profit_factor`, `funding_paid`, `per_symbol_pnl`, `zero_trade` — are
/// the ones this file's own field docs record as added later, and the only ones that default.
pub(super) const ORIGINAL_KEYS: &[(&str, &str)] = &[
    ("name", "null"),
    ("final_equity", "1.0"),
    ("total_return", "0.0"),
    ("n_trades", "0"),
    ("win_rate", "0.0"),
    ("sharpe", "0.0"),
    ("max_drawdown", "0.0"),
];

/// The oldest shape a `report.json` ever had, optionally with one key removed and optionally
/// with LATER keys appended — a COMPLETE document either way, which is what the required
/// originals now oblige every fixture to be.
pub(super) fn oldest_report_with(without: Option<&str>, extra: &[(&str, &str)]) -> String {
    let body: Vec<String> = ORIGINAL_KEYS
        .iter()
        .filter(|(k, _)| Some(*k) != without)
        .chain(extra.iter())
        .map(|(k, v)| format!("  \"{k}\": {v}"))
        .collect();
    format!("{{\n{}\n}}\n", body.join(",\n"))
}

/// [`oldest_report_with`] with no later keys.
pub(super) fn oldest_report(without: Option<&str>) -> String {
    oldest_report_with(without, &[])
}

#[cfg(test)]
mod catalog;
#[cfg(test)]
mod compose;
#[cfg(test)]
mod honesty;
#[cfg(test)]
mod wire;
