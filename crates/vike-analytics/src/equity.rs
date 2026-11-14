//! Equity-curve builders for the tearsheet.
//!
//! Two sources, in preference order:
//!  1. The vike-core equity sampler's persisted `kind=equity` HistoryStore series
//!     (`crates/vike/src/tearsheet_cli/store.rs`'s `equity_curve_from_store`, over
//!     [`equity_curve_from_samples`]) — a true mark-to-market curve (`EquitySample.equity` =
//!     realized + unrealized).
//!  2. A realized-only fallback derived from the reconstructed trades
//!     ([`equity_curve_from_trades`]) when no store series is available.
//!
//! `crate::tearsheet_from_fills` samples the second once per UTC day (`daily_equity_curve`)
//! before the sheet annualises it.
//!
//! Both return `(equity, ts)` aligned pairs, ts-ascending, feeding
//! `vike_analytics::result::BacktestResult { equity_curve, equity_timestamp }` and thus the reused
//! `metrics::` catalog (sharpe/drawdown/… operate on the equity slice).
//!
//! ⚠ Only the first source needs a `HistoryStore`, so only its store READ stayed out when this module
//! moved here on 2026-09-28 (it sat in `vike-report` then and lives with the `tearsheet` CLI now:
//! `crates/vike/src/tearsheet_cli/store.rs`'s
//! `equity_curve_from_store`, a thin wrapper over [`equity_curve_from_samples`]). The two
//! builders here are pure folds over `vike_model` types, which is what a consumer that renders a
//! curve it already holds builds one with — and this crate may name nothing that reads a store.

use vike_model::EquitySample;
use vike_model::Trade;

/// Realized-only equity curve from reconstructed trades: `seed`, then a running cumulative of
/// `(pnl - fees)` sampled at each trade's `exit_timestamp`.
///
/// This is a REALIZED curve — it moves only when a trade closes and has NO mark-to-market between
/// closes (a bare fill stream carries no marks). Prefer a stored `kind=equity` series
/// ([`equity_curve_from_samples`]) when one exists; use this when it does not. The leading point is
/// the seed itself (paired with the first trade's `exit_timestamp`, or `0` when there are no trades) so
/// `metrics::total_return` measures growth from the seed.
pub fn equity_curve_from_trades(seed: f64, trades: &[Trade]) -> (Vec<f64>, Vec<i64>) {
    let first_timestamp = trades.first().map(|t| t.exit_timestamp).unwrap_or(0);
    let mut equity = Vec::with_capacity(trades.len() + 1);
    let mut ts = Vec::with_capacity(trades.len() + 1);
    equity.push(seed);
    ts.push(first_timestamp);
    let mut running = seed;
    for t in trades {
        running += t.pnl - t.fees;
        equity.push(running);
        ts.push(t.exit_timestamp);
    }
    (equity, ts)
}

/// The curve sampled once per UTC calendar day: its first point (the opening equity, stamped at
/// that day's first millisecond), then the LAST point of every day from that day to the last
/// point's, each stamped at its day's last millisecond. A day with no point carries the day
/// before, so the curve has one sample per calendar day and a yearly count of days annualises it.
///
/// Why it exists: [`equity_curve_from_trades`] has one point per CLOSED TRADE, and the live
/// tearsheet annualised that at 252 as if every trade were a day (`crate::tearsheet_from_fills`
/// samples it through here now). A curve of fewer than two points is returned as it is.
pub(crate) fn daily_equity_curve(equity: &[f64], ts: &[i64]) -> (Vec<f64>, Vec<i64>) {
    use vike_model::time::{MS_PER_DAY, utc_day};
    if equity.len() < 2 || ts.len() != equity.len() {
        return (equity.to_vec(), ts.to_vec());
    }
    let mut day = utc_day(ts[0]);
    let (mut out_eq, mut out_timestamp) = (vec![equity[0]], vec![day * MS_PER_DAY]);
    let mut close = equity[0];
    for (&v, &t) in equity.iter().zip(ts).skip(1) {
        // shortcut: a point stamped before the current day (a journal folded out of time order)
        // closes the current day rather than reopening a past one.
        let d = utc_day(t).max(day);
        while day < d {
            out_eq.push(close);
            out_timestamp.push((day + 1) * MS_PER_DAY - 1);
            day += 1;
        }
        close = v;
    }
    out_eq.push(close);
    out_timestamp.push((day + 1) * MS_PER_DAY - 1);
    (out_eq, out_timestamp)
}

/// Build an `(equity, ts)` curve from a slice of persisted `EquitySample`s (the vike-core equity
/// sampler's `kind=equity` series). Reads `.equity` / `.ts` in the slice's order; `scan_equity`
/// already returns them ts-ascending.
pub fn equity_curve_from_samples(samples: &[EquitySample]) -> (Vec<f64>, Vec<i64>) {
    let equity = samples.iter().map(|s| s.equity).collect();
    let ts = samples.iter().map(|s| s.timestamp).collect();
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

    /// One sample per UTC day: the opening, each day's LAST point, a day with no point carried.
    #[test]
    fn the_daily_curve_closes_every_calendar_day_and_carries_the_empty_ones() {
        const DAY: i64 = 86_400_000;
        // Day 0: two trades; day 1: none; day 2: one; and one from day 0 read late (out of order).
        let (eq, ts) = daily_equity_curve(
            &[1_000.0, 1_010.0, 1_005.0, 1_020.0, 1_018.0],
            &[5, 5, DAY - 1, 2 * DAY + 7, 3],
        );
        assert_eq!(eq, vec![1_000.0, 1_005.0, 1_005.0, 1_018.0]);
        assert_eq!(ts, vec![0, DAY - 1, 2 * DAY - 1, 3 * DAY - 1]);
        // A lone seed (no trade) stays the lone seed, so a zero-trade sheet does not move.
        assert_eq!(daily_equity_curve(&[500.0], &[0]), (vec![500.0], vec![0]));
    }

    #[test]
    fn samples_curve_maps_equity_and_timestamp() {
        let samples = vec![
            EquitySample {
                timestamp: 1,
                venue: "binance".into(),
                equity: 100.0,
                realized: 0.0,
                unrealized: 0.0,
                missing_prices: 0,
            },
            EquitySample {
                timestamp: 2,
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
