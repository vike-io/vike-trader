//! Test-only builders the crate's unit tests share: one `Trade` fixture, one returns-to-equity fold.

use vike_model::Trade;

/// A closed one-unit `BTCUSDT` trade: entered at 100.0 at `exit_ts - 1`, exited at `100.0 + pnl`
/// at `exit_ts`, no excursions recorded.
pub fn trade(pnl: f64, fees: f64, is_long: bool, exit_ts: i64) -> Trade {
    Trade {
        entry_price: 100.0,
        exit_price: 100.0 + pnl,
        size: 1.0,
        pnl,
        fees,
        entry_ts: exit_ts - 1,
        exit_ts,
        symbol: "BTCUSDT".to_string(),
        mae: 0.0,
        mfe: 0.0,
        is_long,
    }
}

/// An equity curve starting at `start` whose per-step simple returns equal `rets` exactly.
pub fn equity_from_returns(start: f64, rets: &[f64]) -> Vec<f64> {
    let mut curve = vec![start];
    for &r in rets {
        curve.push(curve.last().unwrap() * (1.0 + r));
    }
    curve
}
