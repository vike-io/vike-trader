//! `TradingResults` tests: the fixtures and the old-document builders several topics share.

use super::*;
use vike_model::Trade;

pub(super) fn trade(pnl: f64) -> Trade {
    crate::test_support::trade(pnl, 0.0, true, 1)
}

pub(super) fn sample_result() -> TradingRecord {
    TradingRecord {
        trades: vec![trade(10.0), trade(-5.0)],
        equity_curve: vec![1000.0, 1010.0, 990.0, 1005.0],
        final_equity: 1005.0,
        n_trades: 2,
        per_symbol_pnl: vec![("BTCUSDT".to_string(), 5.0)],
        per_symbol_curves: Vec::new(),
        equity_timestamp: Vec::new(),
        funding_paid: 0.0,
        fees_paid: 0.0,
        settle_price: None,
        data_warnings: Vec::new(),
        closed_trades: None,
        forecasts: Vec::new(),
        sampling: None,
        mounts: None,
        strategy_failures: Default::default(),
        source: crate::record::TradingRecordSource::Backtest(
            crate::record::TradingRecordSimulator { taker_fills: 2, ..Default::default() },
        ),
    }
}

/// `r`'s simulator counters, for a test to set: `r` is a backtest's record.
pub(super) fn counters(r: &mut TradingRecord) -> &mut crate::record::TradingRecordSimulator {
    match &mut r.source {
        crate::record::TradingRecordSource::Backtest(simulator) => simulator,
        other => panic!("not a backtest record: {other:?}"),
    }
}

/// A report with nothing degenerate in it — the base the sentinel tests perturb one field of.
pub(super) fn a_finite_report() -> TradingResults {
    TradingResults {
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
        invalid: None,
        extended: None,
        honesty: None,
        realism: None,
        data_warnings: Vec::new(),
        in_dollars: None,
        closed_trades: None,
        strategy_file: String::new(),
        strategy_sha256: String::new(),
        strategy_sdk: None,
        returns_interval_ms: None,
        mounts: None,
        strategy_failures: Default::default(),
        source: None,
        period: None,
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
mod source;
#[cfg(test)]
mod wire;
