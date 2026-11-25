//! The venue's funding SCHEDULE through `run_backtest` (`engine.attach_funding`), over a temp
//! `HistoryStoreDataFusion` (the `vike-store` dev dependency). Each test pins one defect the funding audit of
//! 2026-11-08 measured against real venue payments, and each failed on the engine before the fix:
//!
//! 1. bars coarser than the schedule kept only the LAST funding of each bar;
//! 2. the position charged was the one AFTER the bar's fills (an entry on the funding bar paid);
//! 3. the charge was valued at the bar's CLOSE, not the venue's stored mark (else the open);
//! 4. a coin-margined contract's charge rode that close too (`rate · s · u / mark`, in coin);
//! 5. the tick lane charged nothing (the key was refused there), and a hole in the series was silent;
//! 6. deribit's CONTINUOUS accrual was charged as a settlement on whatever was held at the hour.
//!
//! Every price bar is built by hand so a charge at the open, the close and the mark differ.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use vike_backtest::run::data_plan::{DataPlanSeriesRole, auxiliary_series};
use vike_backtest::run::{profile::ProfileBacktest, run_backtest};
use vike_data::{HistoryStore, HistoryStoreError, HistoryStoreMemory, TimestampRange};
use vike_marketdata::Bar;
use vike_marketdata::test_support::{flat_bar_zero_volume, quote};
use vike_store::HistoryStoreDataFusion;

const HOUR: i64 = 3_600_000;
const MINUTE: i64 = 60_000;

fn store() -> (tempfile::TempDir, Arc<HistoryStoreDataFusion>) {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(HistoryStoreDataFusion::open(dir.path()).unwrap());
    (dir, store)
}

/// A price bar opening at `open` and closing at `close`.
fn bar(ts: i64, open: f64, close: f64) -> Bar {
    Bar {
        open,
        high: open.max(close),
        low: open.min(close),
        close,
        ..flat_bar_zero_volume(ts, open)
    }
}

/// A row of the stored funding series: the rate at its instant and the venue's mark on `close`
/// (`0.0` = the venue published none), the shape `vike_backfill::funding_rate::point_to_bar` writes.
fn funding(ts: i64, rate: f64, mark: f64) -> Bar {
    Bar { funding: Some(rate), close: mark, ..flat_bar_zero_volume(ts, 0.0) }
}

/// A `buy_hold` profile with `attach_funding = true`: it submits on the first bar and fills at the
/// second bar's open.
fn profile(
    venue: &str,
    symbol: &str,
    kind: &str,
    interval: &str,
    cash: f64,
    size: f64,
) -> ProfileBacktest {
    profile_with(venue, symbol, kind, interval, cash, size, "attach_funding = true")
}

/// [`profile`] with `funding` as the `[engine]` funding line (`""` leaves the key absent).
fn profile_with(
    venue: &str,
    symbol: &str,
    kind: &str,
    interval: &str,
    cash: f64,
    size: f64,
    funding: &str,
) -> ProfileBacktest {
    ProfileBacktest::from_toml_str(&format!(
        r#"
[data]
venue = "{venue}"
symbols = ["{symbol}"]
kind = "{kind}"
interval = "{interval}"
from = "0"
to = "1000000000"

[engine]
cash = {cash}
{funding}

[strategy]
file = "trend/buy_hold.rs"
[strategy.params]
size = {size}
symbol = "{symbol}"
"#
    ))
    .unwrap()
    .with_stand_in(vike_strategy_lab::stand_in)
}

/// Defect 1: Hyperliquid funds every hour; on 4h bars all four fundings of a bar are charged, not the last.
/// Held from bar 1's fill (4h) to the last bar (44h): the fundings due at bars 2..=11 are 5h..=44h,
/// forty of them at `1 · 100 · 0.0001` each.
#[test]
fn bars_coarser_than_the_schedule_charge_every_funding() {
    let (_d, store) = store();
    let price: Vec<Bar> = (0..12).map(|i| flat_bar_zero_volume(i * 4 * HOUR, 100.0)).collect();
    store.append_bars("hyperliquid", "BTCUSDC.P", "4h", &price, None).unwrap();
    let hourly: Vec<Bar> = (0..48).map(|h| funding(h * HOUR, 0.0001, 0.0)).collect();
    store.append_bars("hyperliquid", "BTCUSDC.P", "funding", &hourly, None).unwrap();

    let r = run_backtest(&profile("hyperliquid", "BTCUSDC.P", "bar", "4h", 100_000.0, 1.0), store)
        .unwrap();
    assert!(
        (r.funding_paid - (-40.0 * 0.01)).abs() < 1e-9,
        "40 hourly fundings x 0.01 = -0.40; keeping one per 4h bar charged -0.11: got {}",
        r.funding_paid
    );
}

/// Defect 2: The venue charges the position held AT the funding instant. `buy_hold` fills at bar 1's open
/// (1h), so the funding due at 1h finds it flat and charges nothing; the one at 5h charges it.
#[test]
fn an_entry_filled_on_the_funding_bar_does_not_pay_it() {
    let (_d, store) = store();
    let price: Vec<Bar> = (0..10).map(|i| flat_bar_zero_volume(i * HOUR, 100.0)).collect();
    store.append_bars("binance", "BTCUSDT", "1h", &price, None).unwrap();
    let rows = vec![funding(HOUR, 0.01, 0.0), funding(5 * HOUR, 0.01, 0.0)];
    store.append_bars("binance", "BTCUSDT", "funding", &rows, None).unwrap();

    let r =
        run_backtest(&profile("binance", "BTCUSDT", "bar", "1h", 100_000.0, 1.0), store).unwrap();
    assert!(
        (r.funding_paid - (-1.0)).abs() < 1e-9,
        "only the 5h funding finds the position: -1.0, got {} (-2.0 charged the entry bar)",
        r.funding_paid
    );
}

/// Defect 3: Valued at the venue's stored mark (binance publishes the settlement `markPrice`), else at the
/// bar's OPEN, the price at the instant; never at the close an hour later. Bars open 100, close 110.
#[test]
fn a_funding_is_valued_at_the_stored_mark_else_the_open_never_the_close() {
    let (_d, store) = store();
    let price: Vec<Bar> = (0..10).map(|i| bar(i * HOUR, 100.0, 110.0)).collect();
    store.append_bars("binance", "BTCUSDT", "1h", &price, None).unwrap();
    let rows = vec![funding(5 * HOUR, 0.01, 105.0), funding(7 * HOUR, 0.01, 0.0)];
    store.append_bars("binance", "BTCUSDT", "funding", &rows, None).unwrap();

    let r =
        run_backtest(&profile("binance", "BTCUSDT", "bar", "1h", 100_000.0, 1.0), store).unwrap();
    assert!(
        (r.funding_paid - (-(1.05 + 1.00))).abs() < 1e-9,
        "5h at its mark 105 (1.05) + 7h at the open 100 (1.00) = -2.05, got {} (the close: -2.20)",
        r.funding_paid
    );
}

/// Defect 4: A coin-margined contract pays `rate · s · u / mark` in its settle coin (decision 0258): bybit
/// `BTCUSD.P`, one contract one dollar, 1,000 contracts long, the funding at the open 50,000.
#[test]
fn an_inverse_funding_is_coin_at_the_open() {
    let (_d, store) = store();
    let price: Vec<Bar> = (0..10).map(|i| bar(i * HOUR, 50_000.0, 60_000.0)).collect();
    store.append_bars("bybit", "BTCUSD.P", "1h", &price, None).unwrap();
    store
        .append_bars("bybit", "BTCUSD.P", "funding", &[funding(5 * HOUR, 0.0001, 0.0)], None)
        .unwrap();

    let r = run_backtest(&profile("bybit", "BTCUSD.P", "bar", "1h", 1.0, 1_000.0), store).unwrap();
    let want = -(0.0001 * 1_000.0 / 50_000.0);
    assert!(
        (r.funding_paid - want).abs() < 1e-15,
        "-2e-6 BTC at the open, got {} (at the close: -1.667e-6)",
        r.funding_paid
    );
}

/// Defect 5a: The tick lane charges the schedule: on the first tick at or after the instant, on the position
/// held then, at that tick's price (mid 100). `buy_hold` is long 1.0 from the second quote.
#[test]
fn the_tick_lane_charges_funding() {
    let (_d, store) = store();
    let quotes: Vec<_> = (0..11).map(|i| quote(i * 1_000, 99.0, 101.0)).collect();
    store.append_quotes("binance", "BTCUSDT", &quotes, None).unwrap();
    store.append_bars("binance", "BTCUSDT", "funding", &[funding(5_000, 0.01, 0.0)], None).unwrap();

    let p = profile("binance", "BTCUSDT", "tick", "1h", 100_000.0, 1.0);
    let r = run_backtest(&p, store).expect("the tick lane takes attach_funding");
    assert!((r.funding_paid - (-1.0)).abs() < 1e-9, "1 x 100 x 0.01: got {}", r.funding_paid);
}

/// Defect 5b: A hole in the stored series is charged nothing, so the report says so.
#[test]
fn a_hole_in_the_funding_series_is_a_report_line() {
    let (_d, store) = store();
    let price: Vec<Bar> = (0..48).map(|i| flat_bar_zero_volume(i * HOUR, 100.0)).collect();
    store.append_bars("binance", "BTCUSDT", "1h", &price, None).unwrap();
    // 8h settlements with the 24h and 32h ones missing: one 24h hole.
    let rows: Vec<Bar> = [0, 8, 16, 40].iter().map(|h| funding(h * HOUR, 0.0001, 0.0)).collect();
    store.append_bars("binance", "BTCUSDT", "funding", &rows, None).unwrap();

    let r =
        run_backtest(&profile("binance", "BTCUSDT", "bar", "1h", 100_000.0, 1.0), store).unwrap();
    assert!(
        r.data_warnings.iter().any(|w| w.contains("BTCUSDT") && w.contains("gap")),
        "the 24h hole must be named: {:?}",
        r.data_warnings
    );
}

/// Defect 6: Deribit accrues continuously and publishes the hour ending at each point: a 15m-bar run long
/// from bar 1's fill (15m) to the last bar (165m) owes exactly the 150 minutes it held, valued at the
/// published index (2,000), not three whole hours at the close.
#[test]
fn deribits_continuous_accrual_is_charged_by_the_minute_held() {
    let (_d, store) = store();
    let price: Vec<Bar> = (0..12).map(|i| flat_bar_zero_volume(i * 15 * MINUTE, 1_900.0)).collect();
    store.append_bars("deribit", "ETH_USDC-PERPETUAL", "15m", &price, None).unwrap();
    let hours: Vec<Bar> = (1..=3).map(|h| funding(h * HOUR, 0.0006, 2_000.0)).collect();
    store.append_bars("deribit", "ETH_USDC-PERPETUAL", "funding", &hours, None).unwrap();

    let p = profile("deribit", "ETH_USDC-PERPETUAL", "bar", "15m", 100_000.0, 1.0);
    let r = run_backtest(&p, store).unwrap();
    let want = -(150.0 * (0.0006 / 60.0) * 2_000.0);
    assert!(
        (r.funding_paid - want).abs() < 1e-9,
        "150 held minutes x 0.00001 x 2000 = -3.0, got {} (three settlements at the close: -3.42)",
        r.funding_paid
    );
}

// --- the default: OFF for every product (the owner's ruling of 2026-11-21) ----------------------

/// Stores one 1h price series and one funding point at 5h under `symbol`, so a run that attaches
/// funding pays `1 · 100 · 0.01` and one that does not pays nothing.
fn seeded(symbol: &str) -> (tempfile::TempDir, Arc<HistoryStoreDataFusion>) {
    let (dir, store) = store();
    let price: Vec<Bar> = (0..10).map(|i| flat_bar_zero_volume(i * HOUR, 100.0)).collect();
    store.append_bars("binance", symbol, "1h", &price, None).unwrap();
    store.append_bars("binance", symbol, "funding", &[funding(5 * HOUR, 0.01, 0.0)], None).unwrap();
    (dir, store)
}

/// A perpetual (`.P` on binance) with stored history pays it only when the profile says `true`: the
/// absent key and `false` charge nothing and say nothing.
#[test]
fn a_perpetual_pays_funding_only_when_the_profile_says_true() {
    let run = |line: &str| {
        let (_d, store) = seeded("BTCUSDT.P");
        let p = profile_with("binance", "BTCUSDT.P", "bar", "1h", 100_000.0, 1.0, line);
        run_backtest(&p, store).unwrap()
    };
    let on = run("attach_funding = true");
    assert!((on.funding_paid - (-1.0)).abs() < 1e-9, "`true` charges it: {}", on.funding_paid);
    for line in ["", "attach_funding = false"] {
        let r = run(line);
        assert_eq!(r.funding_paid, 0.0, "{line:?} is off");
        assert!(!r.data_warnings.iter().any(|w| w.contains("funding")), "{:?}", r.data_warnings);
    }
}

/// Every funding read of the store, counted, over a [`HistoryStoreMemory`].
struct Counting {
    inner: HistoryStoreMemory,
    funding_reads: AtomicUsize,
}

impl HistoryStore for Counting {
    fn load_bars(
        &self,
        venue: &str,
        symbol: &str,
        interval: &str,
        range: TimestampRange,
    ) -> Result<Vec<Bar>, HistoryStoreError> {
        if interval == "funding" {
            self.funding_reads.fetch_add(1, Ordering::SeqCst);
        }
        self.inner.load_bars(venue, symbol, interval, range)
    }
    fn load_bars_sourced_capped(
        &self,
        venue: &str,
        source: &str,
        symbol: &str,
        interval: &str,
        range: TimestampRange,
        budget: Option<usize>,
    ) -> Result<Vec<Bar>, HistoryStoreError> {
        if interval == "funding" {
            self.funding_reads.fetch_add(1, Ordering::SeqCst);
        }
        self.inner.load_bars_sourced_capped(venue, source, symbol, interval, range, budget)
    }
    vike_data::hist_store_stubs!(inert: writes, scan_quotes, scan_trades, scan_book_updates,
        scan_symbol_properties, scan_equity, scan_exec_fills, scan_exec_orders);
}

/// Funding OFF fetches nothing: with the key absent or `false` the run neither plans the `funding`
/// series (the rows an armed `data.require_coverage` judges) nor reads it from the store; `true`
/// does both.
#[test]
fn funding_off_plans_and_reads_no_funding_series() {
    let cases = [("", false), ("attach_funding = false", false), ("attach_funding = true", true)];
    for (line, on) in cases {
        let p = profile_with("binance", "BTCUSDT.P", "bar", "1h", 1e5, 1.0, line);
        let planned = auxiliary_series(&p).iter().any(|(_, r)| *r == DataPlanSeriesRole::Funding);
        assert_eq!(planned, on, "{line:?}: the plan's funding row");

        let store = Arc::new(Counting {
            inner: HistoryStoreMemory::new(),
            funding_reads: AtomicUsize::new(0),
        });
        let price: Vec<Bar> = (0..10).map(|i| flat_bar_zero_volume(i * HOUR, 100.0)).collect();
        store.inner.append_bars("binance", "BTCUSDT.P", "1h", &price, None).unwrap();
        let rows = [funding(5 * HOUR, 0.01, 0.0)];
        store.inner.append_bars("binance", "BTCUSDT.P", "funding", &rows, None).unwrap();
        let shared: Arc<dyn HistoryStore + Send + Sync> = store.clone();
        let r = run_backtest(&p, shared).unwrap();
        let reads = store.funding_reads.load(Ordering::SeqCst);
        assert_eq!(reads > 0, on, "{line:?}: {reads} funding read(s)");
        assert_eq!(r.funding_paid != 0.0, on, "{line:?}: paid {}", r.funding_paid);
    }
}

/// A perpetual that ASKED for funding (`true`) and has NO stored history runs uncharged, and its report
/// says so and how to fix it.
#[test]
fn a_perpetual_without_history_is_named_on_the_report() {
    let (_d, store) = store();
    let price: Vec<Bar> = (0..10).map(|i| flat_bar_zero_volume(i * HOUR, 100.0)).collect();
    store.append_bars("binance", "ETHUSDT.P", "1h", &price, None).unwrap();
    let p = profile("binance", "ETHUSDT.P", "bar", "1h", 100_000.0, 1.0);
    let r = run_backtest(&p, store).unwrap();
    assert_eq!(r.funding_paid, 0.0);
    assert!(
        r.data_warnings.iter().any(|w| w.contains("ETHUSDT.P")
            && w.contains("NO FUNDING")
            && w.contains("binance:ETHUSDT.P:funding")),
        "{:?}",
        r.data_warnings
    );
}

/// A spot name reads no funding by default even when a series sits under it, and says nothing.
#[test]
fn a_spot_name_reads_no_funding_by_default() {
    let (_d, store) = seeded("BTCUSDT");
    let r = run_backtest(&profile_with("binance", "BTCUSDT", "bar", "1h", 1e5, 1.0, ""), store)
        .unwrap();
    assert_eq!(r.funding_paid, 0.0, "absent is off");
    assert!(!r.data_warnings.iter().any(|w| w.contains("funding")), "{:?}", r.data_warnings);
}
