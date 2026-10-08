//! Equity-curve builders for the tearsheet.
//!
//! Two sources, in preference order:
//!  1. The vike-core equity sampler's persisted `kind=equity` HistStore series
//!     (`vike_report::equity_curve_from_store` / [`equity_curve_from_samples`]) — a true
//!     mark-to-market curve (`EquitySample.equity` = realized + unrealized).
//!  2. A realized-only fallback derived from the reconstructed trades
//!     ([`equity_curve_from_trades`]) when no store series is available.
//!
//! Both return `(equity, ts)` aligned pairs, ts-ascending, feeding
//! `vike_analytics::result::BacktestResult { equity_curve, equity_ts }` and thus the reused
//! `metrics::` catalog (sharpe/drawdown/… operate on the equity slice).
//!
//! ⚠ Only the first source needs a `HistStore`, so only its store READ stayed in `vike-report`
//! when this module moved here on 2026-09-28 (`crates/vike-report/src/store.rs`'s
//! `equity_curve_from_store`, a thin wrapper over [`equity_curve_from_samples`]). The two
//! builders here are pure folds over `vike_model` types, which is what a consumer that renders a
//! curve it already holds builds one with — and this crate may name nothing that reads a store.

use vike_model::EquitySample;
use vike_model::Trade;

/// Realized-only equity curve from reconstructed trades: `seed`, then a running cumulative of
/// `(pnl - fees)` sampled at each trade's `exit_ts`.
///
/// This is a REALIZED curve — it moves only when a trade closes and has NO mark-to-market between
/// closes (a bare fill stream carries no marks). Prefer a stored `kind=equity` series
/// ([`equity_curve_from_samples`]) when one exists; use this when it does not. The leading point is
/// the seed itself (paired with the first trade's `exit_ts`, or `0` when there are no trades) so
/// `metrics::total_return` measures growth from the seed.
pub fn equity_curve_from_trades(seed: f64, trades: &[Trade]) -> (Vec<f64>, Vec<i64>) {
    let first_ts = trades.first().map(|t| t.exit_ts).unwrap_or(0);
    let mut equity = Vec::with_capacity(trades.len() + 1);
    let mut ts = Vec::with_capacity(trades.len() + 1);
    equity.push(seed);
    ts.push(first_ts);
    let mut running = seed;
    for t in trades {
        running += t.pnl - t.fees;
        equity.push(running);
        ts.push(t.exit_ts);
    }
    (equity, ts)
}

/// Build an `(equity, ts)` curve from a slice of persisted `EquitySample`s (the vike-core equity
/// sampler's `kind=equity` series). Reads `.equity` / `.ts` in the slice's order; `scan_equity`
/// already returns them ts-ascending.
pub fn equity_curve_from_samples(samples: &[EquitySample]) -> (Vec<f64>, Vec<i64>) {
    let equity = samples.iter().map(|s| s.equity).collect();
    let ts = samples.iter().map(|s| s.ts).collect();
    (equity, ts)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trade(pnl: f64, fees: f64, exit_ts: i64) -> Trade {
        crate::test_support::trade(pnl, fees, true, exit_ts)
    }

    #[test]
    fn trades_curve_is_seed_plus_running_net() {
        let trades = vec![trade(10.0, 0.5, 2_000), trade(-4.0, 0.5, 3_000)];
        let (eq, ts) = equity_curve_from_trades(1_000.0, &trades);
        assert_eq!(eq, vec![1_000.0, 1_009.5, 1_005.0]);
        assert_eq!(ts, vec![2_000, 2_000, 3_000]);
    }

    #[test]
    fn trades_curve_empty_is_just_the_seed() {
        let (eq, ts) = equity_curve_from_trades(500.0, &[]);
        assert_eq!(eq, vec![500.0]);
        assert_eq!(ts, vec![0]);
    }

    #[test]
    fn samples_curve_maps_equity_and_ts() {
        let samples = vec![
            EquitySample {
                ts: 1,
                venue: "binance".into(),
                equity: 100.0,
                realized: 0.0,
                unrealized: 0.0,
                missing_prices: 0,
            },
            EquitySample {
                ts: 2,
                venue: "binance".into(),
                equity: 110.0,
                realized: 10.0,
                unrealized: 0.0,
                missing_prices: 0,
            },
        ];
        let (eq, ts) = equity_curve_from_samples(&samples);
        assert_eq!(eq, vec![100.0, 110.0]);
        assert_eq!(ts, vec![1, 2]);
    }
}
