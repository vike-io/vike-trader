use super::*;
use std::collections::HashMap;
use vike_data::{DataError, HistStore, TsRange};
use vike_model::Bar;

/// A minimal DataFusion-free `HistStore` double: only `load_bars` carries behavior (returns the
/// seeded bars for a `(venue, symbol)` key, filtered to the requested ts range). Every other
/// method is an inert stub — this double exists for the excursion back-fill path only. (The
/// shipped `vike_data::MemHistStore` stores bars too, but keys them on `(venue, symbol,
/// interval)`; this double keys on `(venue, symbol)` and ignores the interval.)
#[derive(Default)]
struct SeededBarStore {
    bars: HashMap<(String, String), Vec<Bar>>,
    /// ⚠ How many times `load_bars` was CALLED — the thing this double could not see.
    ///
    /// It answered correctly and said nothing about ROUND TRIPS, which is exactly why one
    /// read per TRADE survived unnoticed: every assertion in this module passed under it.
    /// `RemoteHistStore` dials per verb call, so the call count IS the cost once a reader
    /// is routed (`docs/decisions/0084-only-the-datahub-touches-the-store.md`).
    calls: std::cell::Cell<usize>,
}

impl SeededBarStore {
    fn with_bars(venue: &str, symbol: &str, bars: Vec<Bar>) -> Self {
        let mut m = HashMap::new();
        m.insert((venue.to_string(), symbol.to_string()), bars);
        Self { bars: m, calls: std::cell::Cell::new(0) }
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
        self.calls.set(self.calls.get() + 1);
        Ok(self
            .bars
            .get(&(venue.to_string(), symbol.to_string()))
            .map(|v| v.iter().filter(|b| in_range(b.ts, range)).cloned().collect())
            .unwrap_or_default())
    }
    vike_data::hist_store_stubs!(inert: writes, scan_quotes, scan_trades, scan_book_updates,
        scan_symbol_properties, scan_equity, scan_exec_fills, scan_exec_orders);
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

/// ⚠ **ONE read per SYMBOL, whatever the trade count** — the assertion the routed path needs
/// and the one nothing here could make before.
///
/// `RemoteHistStore` dials per verb call, so one read per trade is one TCP connect per trade —
/// plus a handshake each against a keyed server. Six trades over two symbols must cost two
/// reads, not six, and only a counting fixture can tell those apart.
#[test]
fn the_bar_reads_are_one_per_symbol_not_one_per_trade() {
    let mut inner = SeededBarStore::with_bars(
        "binance",
        "AAA",
        vec![bar(0, 120.0, 90.0), bar(100, 130.0, 95.0), bar(300, 125.0, 92.0)],
    );
    inner.bars.insert(
        ("binance".to_string(), "BBB".to_string()),
        vec![bar(0, 220.0, 190.0), bar(100, 230.0, 195.0), bar(300, 225.0, 192.0)],
    );
    let store = inner;

    let mut trades: Vec<Trade> = ["AAA", "BBB", "AAA", "BBB", "AAA", "BBB"]
        .iter()
        .enumerate()
        .map(|(i, s)| trade(s, 100.0, 110.0, 10.0, i as i64, 300 - i as i64))
        .collect();

    backfill_excursions(&mut trades, &store, "binance", "1m");

    assert_eq!(
        store.calls.get(),
        2,
        "six trades over two symbols must cost two reads — one per trade is {} round trips on \
             a routed store",
        trades.len()
    );
}

/// ...and the numbers do not move. A wider slice is safe because
/// `vike_analytics::excursions::mae_mfe` filters its own input on the SAME inclusive bound the
/// per-trade `TsRange` used to apply, so batching is a transport change and never a semantic
/// one. This asserts that equivalence directly rather than trusting the argument.
#[test]
fn a_batched_read_produces_the_same_numbers_as_a_per_trade_one() {
    // Bars OUTSIDE each trade's own window as well as inside — with a single window the two
    // shapes would agree trivially and the test would witness nothing.
    let seeded = SeededBarStore::with_bars(
        "binance",
        "AAA",
        vec![bar(0, 120.0, 90.0), bar(150, 140.0, 80.0), bar(300, 125.0, 92.0)],
    );
    let mut batched =
        vec![trade("AAA", 100.0, 110.0, 10.0, 0, 150), trade("AAA", 100.0, 110.0, 10.0, 150, 300)];
    backfill_excursions(&mut batched, &seeded, "binance", "1m");

    // The pre-batching shape, spelled out: one narrow read per trade, computed the old way.
    let mut per_trade =
        vec![trade("AAA", 100.0, 110.0, 10.0, 0, 150), trade("AAA", 100.0, 110.0, 10.0, 150, 300)];
    for t in per_trade.iter_mut() {
        let bars = seeded
            .load_bars("binance", &t.symbol, "1m", TsRange::of(t.entry_ts, t.exit_ts))
            .unwrap();
        let (mae, mfe) = vike_analytics::excursions::mae_mfe(t, &bars, None);
        t.mae = mae;
        t.mfe = mfe;
    }

    for (b, p) in batched.iter().zip(&per_trade) {
        assert_eq!(b.mae.to_bits(), p.mae.to_bits(), "mae must be bit-identical");
        assert_eq!(b.mfe.to_bits(), p.mfe.to_bits(), "mfe must be bit-identical");
    }
}
