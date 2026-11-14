use super::*;
use std::collections::HashMap;
use vike_data::{HistoryStore, HistoryStoreError, TimestampRange};
use vike_marketdata::Bar;

/// A minimal DataFusion-free `HistoryStore` double: only `load_bars` carries behavior (returns the
/// seeded bars for a `(venue, symbol)` key, filtered to the requested ts range). Every other
/// method is an inert stub — this double exists for the excursion back-fill path only. (The
/// shipped `vike_data::HistoryStoreMemory` stores bars too, but keys them on `(venue, symbol,
/// interval)`; this double keys on `(venue, symbol)` and ignores the interval.)
#[derive(Default)]
struct SeededBarStore {
    bars: HashMap<(String, String), Vec<Bar>>,
    /// ⚠ How many times `load_bars` was CALLED. Correct answers say nothing about ROUND TRIPS,
    /// and every `load_bars` on a routed `HistoryStoreDatahub` is one, so the call count IS the cost
    /// (`docs/decisions/0084-only-the-datahub-touches-the-store.md`).
    calls: std::cell::Cell<usize>,
}

impl SeededBarStore {
    fn with_bars(venue: &str, symbol: &str, bars: Vec<Bar>) -> Self {
        let mut m = HashMap::new();
        m.insert((venue.to_string(), symbol.to_string()), bars);
        Self { bars: m, calls: std::cell::Cell::new(0) }
    }
}

fn in_range(ts: i64, r: TimestampRange) -> bool {
    r.start.map(|s| ts >= s).unwrap_or(true) && r.end.map(|e| ts <= e).unwrap_or(true)
}

impl HistoryStore for SeededBarStore {
    fn load_bars(
        &self,
        venue: &str,
        symbol: &str,
        _interval: &str,
        range: TimestampRange,
    ) -> Result<Vec<Bar>, HistoryStoreError> {
        self.calls.set(self.calls.get() + 1);
        Ok(self
            .bars
            .get(&(venue.to_string(), symbol.to_string()))
            .map(|v| v.iter().filter(|b| in_range(b.timestamp, range)).cloned().collect())
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
        entry_timestamp: entry_ts,
        exit_timestamp: exit_ts,
        symbol: symbol.to_string(),
        mae: 0.0,
        mfe: 0.0,
        is_long: true,
    }
}

use vike_marketdata::test_support::mid_bar;

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
        mid_bar(0, 101.0, 99.0),
        mid_bar(100, 102.0, 98.0),
        mid_bar(200, 115.0, 95.0),
        mid_bar(300, 101.0, 99.0),
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
    // All seeded bars sit AFTER the trade's exit_timestamp, so the window is empty → 0.0 preserved.
    let bars = vec![mid_bar(10_000, 120.0, 80.0), mid_bar(10_100, 130.0, 70.0)];
    let store = SeededBarStore::with_bars("binance", "BTCUSDT", bars);
    let mut trades = vec![trade("BTCUSDT", 100.0, 110.0, 10.0, 0, 300)];
    backfill_excursions(&mut trades, &store, "binance", "1m");
    assert_eq!(trades[0].mae, 0.0);
    assert_eq!(trades[0].mfe, 0.0);
}

#[test]
fn wrong_symbol_leaves_zero() {
    // Bars seeded for ETHUSDT; the trade is BTCUSDT → no bars for that key → 0.0 preserved.
    let bars = vec![mid_bar(200, 115.0, 95.0)];
    let store = SeededBarStore::with_bars("binance", "ETHUSDT", bars);
    let mut trades = vec![trade("BTCUSDT", 100.0, 110.0, 10.0, 0, 300)];
    backfill_excursions(&mut trades, &store, "binance", "1m");
    assert_eq!(trades[0].mae, 0.0);
    assert_eq!(trades[0].mfe, 0.0);
}

/// A store that holds nothing it can give: every `load_bars` fails.
struct UnreadableBars;

impl HistoryStore for UnreadableBars {
    fn load_bars(
        &self,
        _: &str,
        _: &str,
        _: &str,
        _: TimestampRange,
    ) -> Result<Vec<Bar>, HistoryStoreError> {
        Err(HistoryStoreError::Io("the datahub went away".to_string()))
    }
    vike_data::hist_store_stubs!(inert: writes, scan_quotes, scan_trades, scan_book_updates,
        scan_symbol_properties, scan_equity, scan_exec_fills, scan_exec_orders);
}

/// The child of [`a_failed_bar_read_prints_one_line_per_symbol`], which reads its stderr.
#[test]
#[ignore = "run by a_failed_bar_read_prints_one_line_per_symbol, which reads its stderr"]
fn backfills_where_the_bars_cannot_be_read() {
    let mut trades = vec![
        trade("AAA", 100.0, 110.0, 10.0, 0, 100),
        trade("AAA", 100.0, 110.0, 10.0, 100, 200),
        trade("BBB", 100.0, 110.0, 10.0, 0, 100),
    ];
    backfill_excursions(&mut trades, &UnreadableBars, "binance", "1m");
    assert!(trades.iter().all(|t| t.mae == 0.0 && t.mfe == 0.0), "still best-effort");
    // An empty answer is the store saying it holds nothing, and it stays quiet.
    backfill_excursions(&mut trades, &SeededBarStore::default(), "binance", "1m");
}

/// A trade's `mae`/`mfe` of 0.0 reads as a trade that never moved against or for itself, so a read
/// that FAILED says so. A child process, because the test harness captures this process's stderr.
#[test]
fn a_failed_bar_read_prints_one_line_per_symbol() {
    let out = std::process::Command::new(std::env::current_exe().expect("this test binary"))
        .args(["backfills_where_the_bars_cannot_be_read", "--ignored", "--nocapture"])
        .output()
        .expect("run the child");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "the child failed: {stderr}");
    let lines: Vec<&str> = stderr.lines().filter(|l| l.starts_with("tearsheet:")).collect();
    assert_eq!(lines.len(), 2, "one line per symbol, not per trade: {stderr}");
    assert!(lines[0].contains("AAA") && lines[0].contains("went away"), "{}", lines[0]);
    assert!(lines[1].contains("BBB") && lines[1].contains("went away"), "{}", lines[1]);
}

/// ⚠ **ONE read per SYMBOL, whatever the trade count** — the assertion the routed path needs.
///
/// Every `load_bars` on a `HistoryStoreDatahub` is a network round trip. Six trades over two
/// symbols must cost two reads, not six, and only a counting fixture can tell those apart.
#[test]
fn the_bar_reads_are_one_per_symbol_not_one_per_trade() {
    let mut store = SeededBarStore::with_bars(
        "binance",
        "AAA",
        vec![mid_bar(0, 120.0, 90.0), mid_bar(100, 130.0, 95.0), mid_bar(300, 125.0, 92.0)],
    );
    store.bars.insert(
        ("binance".to_string(), "BBB".to_string()),
        vec![mid_bar(0, 220.0, 190.0), mid_bar(100, 230.0, 195.0), mid_bar(300, 225.0, 192.0)],
    );

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
/// per-trade `TimestampRange` used to apply, so batching is a transport change and never a semantic
/// one. This asserts that equivalence directly rather than trusting the argument.
#[test]
fn a_batched_read_produces_the_same_numbers_as_a_per_trade_one() {
    // Bars OUTSIDE each trade's own window as well as inside — with a single window the two
    // shapes would agree trivially and the test would witness nothing.
    let seeded = SeededBarStore::with_bars(
        "binance",
        "AAA",
        vec![mid_bar(0, 120.0, 90.0), mid_bar(150, 140.0, 80.0), mid_bar(300, 125.0, 92.0)],
    );
    let mut batched =
        vec![trade("AAA", 100.0, 110.0, 10.0, 0, 150), trade("AAA", 100.0, 110.0, 10.0, 150, 300)];
    backfill_excursions(&mut batched, &seeded, "binance", "1m");

    // The pre-batching shape, spelled out: one narrow read per trade, computed the old way.
    let mut per_trade =
        vec![trade("AAA", 100.0, 110.0, 10.0, 0, 150), trade("AAA", 100.0, 110.0, 10.0, 150, 300)];
    for t in per_trade.iter_mut() {
        let bars = seeded
            .load_bars(
                "binance",
                &t.symbol,
                "1m",
                TimestampRange::of(t.entry_timestamp, t.exit_timestamp),
            )
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
