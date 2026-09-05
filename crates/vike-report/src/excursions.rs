//! `backfill_excursions` — fill each reconstructed [`Trade`]'s `mae`/`mfe` from a `HistStore`.
//!
//! A bare fill stream carries no intrabar path, so [`crate::reconstruct_trades`] leaves
//! `mae`/`mfe` at `0.0` (correct, not a shortcut — see `trades.rs`). When a bar series for the
//! traded symbol IS available in a [`HistStore`], this back-fills those two fields the same way
//! the backtest does: load the OHLC bars spanning the trade's `[entry_ts, exit_ts]`
//! window and hand them to the shared `vike_analytics::excursions::mae_mfe` (the ONE excursion
//! primitive — no math is reimplemented here).
//!
//! DataFusion-free: this takes the venue-agnostic [`HistStore`] TRAIT object, never a concrete
//! backend, so the library needs no feature. The `tearsheet` bin opens a concrete
//! `DataFusionHist` behind its `hist` feature and passes it in as `&dyn HistStore`.
//!
//! Best-effort: an empty bar window or a load error leaves that trade's `mae`/`mfe` at `0.0`
//! rather than failing the whole tearsheet — the store is an enrichment, not a hard dependency.

use vike_data::{HistStore, TsRange};
use vike_model::Trade;

/// Back-fill `mae`/`mfe` on each trade from `store`'s `(venue, symbol, interval)` bar series.
///
/// For each trade: load bars over its `[entry_ts, exit_ts]` window (inclusive) and, if the window
/// is non-empty, write `vike_analytics::excursions::mae_mfe(trade, &bars, None)` into
/// `trade.mae`/`trade.mfe`. Empty bars OR a load error → the trade is left untouched (its `mae`/
/// `mfe` stay `0.0`). `venue`/`interval` are fixed across the whole trade set; `symbol` comes from
/// each `Trade`.
pub fn backfill_excursions(
    trades: &mut [Trade],
    store: &dyn HistStore,
    venue: &str,
    interval: &str,
) {
    for t in trades.iter_mut() {
        let range = TsRange::of(t.entry_ts, t.exit_ts);
        // Immutable borrow of `*t` for the compute, then write back — no borrow conflict since the
        // returned tuple is owned.
        if let Ok(bars) = store.load_bars(venue, &t.symbol, interval, range)
            && !bars.is_empty()
        {
            let (mae, mfe) = vike_analytics::excursions::mae_mfe(t, &bars, None);
            t.mae = mae;
            t.mfe = mfe;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use vike_data::{DataError, ExecFillRow, ExecOrderRow, HistStore, TsRange};
    use vike_model::{Bar, BookUpdate, EquitySample, QuoteTick, SymbolProperties, TradeTick};

    /// A minimal DataFusion-free `HistStore` double: only `load_bars` carries behavior (returns the
    /// seeded bars for a `(venue, symbol)` key, filtered to the requested ts range). Every other
    /// method is an inert stub — this double exists for the excursion back-fill path only. (The
    /// shipped `vike_data::MemHistStore` can't be used here: its `load_bars` is inert, so it can
    /// never produce a non-zero excursion.)
    #[derive(Default)]
    struct SeededBarStore {
        bars: HashMap<(String, String), Vec<Bar>>,
    }

    impl SeededBarStore {
        fn with_bars(venue: &str, symbol: &str, bars: Vec<Bar>) -> Self {
            let mut m = HashMap::new();
            m.insert((venue.to_string(), symbol.to_string()), bars);
            Self { bars: m }
        }
    }

    fn in_range(ts: i64, r: TsRange) -> bool {
        r.start.map(|s| ts >= s).unwrap_or(true) && r.end.map(|e| ts <= e).unwrap_or(true)
    }

    impl HistStore for SeededBarStore {
        fn load_bars(
            &self,
            venue: &str,
            symbol: &str,
            _interval: &str,
            range: TsRange,
        ) -> Result<Vec<Bar>, DataError> {
            Ok(self
                .bars
                .get(&(venue.to_string(), symbol.to_string()))
                .map(|v| v.iter().filter(|b| in_range(b.ts, range)).cloned().collect())
                .unwrap_or_default())
        }
        fn scan_quotes(&self, _: &str, _: &str, _: TsRange) -> Result<Vec<QuoteTick>, DataError> {
            Ok(Vec::new())
        }
        fn scan_trades(&self, _: &str, _: &str, _: TsRange) -> Result<Vec<TradeTick>, DataError> {
            Ok(Vec::new())
        }
        fn append_bars(
            &self,
            _: &str,
            _: &str,
            _: &str,
            _: &[Bar],
            _: Option<&str>,
        ) -> Result<usize, DataError> {
            Ok(0)
        }
        fn append_quotes(
            &self,
            _: &str,
            _: &str,
            _: &[QuoteTick],
            _: Option<&str>,
        ) -> Result<usize, DataError> {
            Ok(0)
        }
        fn append_trades(
            &self,
            _: &str,
            _: &str,
            _: &[TradeTick],
            _: Option<&str>,
        ) -> Result<usize, DataError> {
            Ok(0)
        }
        fn append_book_updates(
            &self,
            _: &str,
            _: &str,
            _: &[BookUpdate],
            _: Option<&str>,
        ) -> Result<usize, DataError> {
            Ok(0)
        }
        fn scan_book_updates(
            &self,
            _: &str,
            _: &str,
            _: TsRange,
        ) -> Result<Vec<BookUpdate>, DataError> {
            Ok(Vec::new())
        }
        fn append_symbol_properties(
            &self,
            _: &str,
            _: &str,
            _: &[(i64, SymbolProperties)],
            _: Option<&str>,
        ) -> Result<usize, DataError> {
            Ok(0)
        }
        fn scan_symbol_properties(
            &self,
            _: &str,
            _: &str,
            _: TsRange,
        ) -> Result<Vec<(i64, SymbolProperties)>, DataError> {
            Ok(Vec::new())
        }
        fn append_equity(
            &self,
            _: &str,
            _: &str,
            _: &[EquitySample],
            _: Option<&str>,
        ) -> Result<usize, DataError> {
            Ok(0)
        }
        fn scan_equity(
            &self,
            _: &str,
            _: &str,
            _: TsRange,
        ) -> Result<Vec<EquitySample>, DataError> {
            Ok(Vec::new())
        }
        fn append_exec_fills(
            &self,
            _: &str,
            _: &str,
            _: &[ExecFillRow],
            _: Option<&str>,
        ) -> Result<usize, DataError> {
            Ok(0)
        }
        fn scan_exec_fills(&self, _: &str, _: &str) -> Result<Vec<ExecFillRow>, DataError> {
            Ok(Vec::new())
        }
        fn append_exec_orders(
            &self,
            _: &str,
            _: &str,
            _: &[ExecOrderRow],
            _: Option<&str>,
        ) -> Result<usize, DataError> {
            Ok(0)
        }
        fn scan_exec_orders(&self, _: &str, _: &str) -> Result<Vec<ExecOrderRow>, DataError> {
            Ok(Vec::new())
        }
        fn resample_quotes_to_bars(
            &self,
            _: &str,
            _: &str,
            _: &str,
            _: TsRange,
            _: Option<&str>,
        ) -> Result<usize, DataError> {
            Ok(0)
        }
        fn resample_trades_to_bars(
            &self,
            _: &str,
            _: &str,
            _: &str,
            _: TsRange,
            _: Option<&str>,
        ) -> Result<usize, DataError> {
            Ok(0)
        }
    }

    fn trade(symbol: &str, entry: f64, exit: f64, pnl: f64, entry_ts: i64, exit_ts: i64) -> Trade {
        Trade {
            entry_price: entry,
            exit_price: exit,
            size: 1.0,
            pnl,
            fees: 0.0,
            entry_ts,
            exit_ts,
            symbol: symbol.to_string(),
            mae: 0.0,
            mfe: 0.0,
            is_long: true,
        }
    }

    fn bar(ts: i64, high: f64, low: f64) -> Bar {
        Bar {
            ts,
            open: (high + low) / 2.0,
            high,
            low,
            close: (high + low) / 2.0,
            volume: 0.0,
            funding: None,
            bid: None,
            ask: None,
            symbol: None,
        }
    }

    #[test]
    fn no_bars_leaves_excursions_zero() {
        // Empty store → load_bars returns [] → mae/mfe stay 0.0.
        let store = SeededBarStore::default();
        let mut trades = vec![trade("BTCUSDT", 100.0, 110.0, 10.0, 0, 300)];
        backfill_excursions(&mut trades, &store, "binance", "1m");
        assert_eq!(trades[0].mae, 0.0);
        assert_eq!(trades[0].mfe, 0.0);
    }

    #[test]
    fn non_empty_bars_produce_non_zero_excursions() {
        // A long trade (entry 100, exit 110) whose window dips to 95 (adverse) and peaks at 115
        // (favorable): MAE = (100-95)/100 = 0.05, MFE = (115-100)/100 = 0.15.
        let bars = vec![
            bar(0, 101.0, 99.0),
            bar(100, 102.0, 98.0),
            bar(200, 115.0, 95.0),
            bar(300, 101.0, 99.0),
        ];
        let store = SeededBarStore::with_bars("binance", "BTCUSDT", bars);
        let mut trades = vec![trade("BTCUSDT", 100.0, 110.0, 10.0, 0, 300)];
        backfill_excursions(&mut trades, &store, "binance", "1m");
        assert!(trades[0].mae > 0.0, "mae should be non-zero, got {}", trades[0].mae);
        assert!(trades[0].mfe > 0.0, "mfe should be non-zero, got {}", trades[0].mfe);
        assert!((trades[0].mae - 0.05).abs() < 1e-9);
        assert!((trades[0].mfe - 0.15).abs() < 1e-9);
    }

    #[test]
    fn bars_outside_window_are_ignored_and_leave_zero() {
        // All seeded bars sit AFTER the trade's exit_ts, so the window is empty → 0.0 preserved.
        let bars = vec![bar(10_000, 120.0, 80.0), bar(10_100, 130.0, 70.0)];
        let store = SeededBarStore::with_bars("binance", "BTCUSDT", bars);
        let mut trades = vec![trade("BTCUSDT", 100.0, 110.0, 10.0, 0, 300)];
        backfill_excursions(&mut trades, &store, "binance", "1m");
        assert_eq!(trades[0].mae, 0.0);
        assert_eq!(trades[0].mfe, 0.0);
    }

    #[test]
    fn wrong_symbol_leaves_zero() {
        // Bars seeded for ETHUSDT; the trade is BTCUSDT → no bars for that key → 0.0 preserved.
        let bars = vec![bar(200, 115.0, 95.0)];
        let store = SeededBarStore::with_bars("binance", "ETHUSDT", bars);
        let mut trades = vec![trade("BTCUSDT", 100.0, 110.0, 10.0, 0, 300)];
        backfill_excursions(&mut trades, &store, "binance", "1m");
        assert_eq!(trades[0].mae, 0.0);
        assert_eq!(trades[0].mfe, 0.0);
    }
}
