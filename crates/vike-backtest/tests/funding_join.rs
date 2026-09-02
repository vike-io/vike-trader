//! `engine.attach_funding` — the market funding-rate JOIN + its accrual-correctness proof.
//!
//! The market funding-rate backfill (#761) stores a `(venue, symbol, "funding")` bar series where
//! each `Bar.funding = Some(rate)` sits at a funding-event ts. `engine.attach_funding` window-joins
//! those rates onto the replayed PRICE bars so BOTH consumers work:
//!   * the `SimBroker` bar-loop accrual (`engine.rs`), which charges funding on every bar whose
//!     `Bar.funding.is_some()`; and
//!   * a funding-reading strategy's signal (`FundingCarryController`'s funding book).
//!
//! The correctness crux is the ANTI-FORWARD-FILL guard: the join attaches a rate ONLY to the price
//! bar whose window contains the funding event, never to every bar until the next event — otherwise
//! the accrual would charge every bar instead of once per interval, a catastrophic over-charge.
//!
//! Store-backed, so the whole file is behind `datafusion-store` (the concrete `DataFusionHist`).
#![cfg(feature = "datafusion-store")]

use std::sync::Arc;

use vike_backtest::harness::{run_backtest, window_join_funding, BacktestProfile};
use vike_data::{DataFusionHist, HistStore};
use vike_model::Bar;

const VENUE: &str = "binance";
const SYMBOL: &str = "BTCUSDT";
const HOUR: i64 = 3_600_000; // 1h in epoch-ms

/// A price bar at `ts` with a flat OHLC of `price` (constant price keeps the funding arithmetic
/// exact: `charge = size · price · rate · mult`). `funding` starts `None`, as an OHLCV bar does.
fn bar(ts: i64, price: f64) -> Bar {
    Bar {
        ts,
        open: price,
        high: price,
        low: price,
        close: price,
        volume: 0.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}

/// A row of the stored market `funding` series: an otherwise-empty bar carrying `funding =
/// Some(rate)` at a funding-event `ts` (the shape #761 writes).
fn funding_bar(ts: i64, rate: f64) -> Bar {
    Bar { funding: Some(rate), ..bar(ts, 0.0) }
}

/// A bar-mode buy-hold profile over `SYMBOL`; `extra_engine` injects extra `[engine]` lines.
/// `snap_to_properties` stays OFF, so bars keep their bare symbol and the explicit
/// `strategy.params.symbol` routes buy_hold onto the same key `SimBroker` indexes the position by.
fn profile_toml(extra_engine: &str) -> String {
    format!(
        r#"
[data]
venue = "{VENUE}"
symbols = ["{SYMBOL}"]
kind = "bar"
interval = "1h"
from = "0"
to = "1000000000"

[engine]
cash = 100000.0
{extra_engine}

[strategy]
name = "buy_hold"
[strategy.params]
size = 1.0
symbol = "{SYMBOL}"
"#
    )
}

/// THE ANTI-FORWARD-FILL GUARD: the join places a funding rate ONLY on the bar whose window
/// contains the event; every other bar stays `None`. 24 hourly bars, events at bars 0/8/16.
#[test]
fn join_places_funding_only_on_event_bars_never_forward_filled() {
    let mut price: Vec<Bar> = (0..24).map(|i| bar(i * HOUR, 100.0)).collect();
    // Distinct rates so a misplacement is visible, not just present/absent.
    let events = vec![(0, 0.01), (8 * HOUR, 0.02), (16 * HOUR, 0.03)];

    let stats = window_join_funding(&mut price, &events);
    assert_eq!(stats.placed, 3, "exactly three bars received a rate");
    assert_eq!(stats.collisions, 0, "each 8h event sits in its own hourly-bar window");
    assert_eq!(stats.dropped_before, 0, "no event predates the first bar (bar 0 ts = 0)");

    for (i, b) in price.iter().enumerate() {
        match i {
            0 => assert_eq!(b.funding, Some(0.01), "bar 0 carries its event"),
            8 => assert_eq!(b.funding, Some(0.02), "bar 8 carries its event"),
            16 => assert_eq!(b.funding, Some(0.03), "bar 16 carries its event"),
            _ => assert_eq!(b.funding, None, "bar {i} must NOT be forward-filled"),
        }
    }
}

/// THE CORRECTNESS PROOF: accrual fires ONCE PER EVENT, not once per bar. A held perp long over
/// 24 hourly bars with three funding events; the total funding charged is exactly the three
/// events' charges, not 24 bars' worth.
///
/// Events are at bars 8/12/16 — all AFTER the buy_hold market fills (submitted on bar 0, fills on
/// bar 1's NEXT-bar-open), so the 1.0-unit position is held through every event.
///   charge/event = size(1.0) · close(100.0) · rate(0.01) · mult(1.0) = 1.0
///   funding_paid = −(3 · 1.0) = −3.0   (a LONG PAYS positive funding; `funding_paid -= charge`)
#[test]
fn accrual_fires_once_per_event_not_per_bar() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(DataFusionHist::open(dir.path()).unwrap());

    let price: Vec<Bar> = (0..24).map(|i| bar(i * HOUR, 100.0)).collect();
    store.append_bars(VENUE, SYMBOL, "1h", &price, None).unwrap();

    let funding = vec![
        funding_bar(8 * HOUR, 0.01),
        funding_bar(12 * HOUR, 0.01),
        funding_bar(16 * HOUR, 0.01),
    ];
    store.append_bars(VENUE, SYMBOL, "funding", &funding, None).unwrap();

    let profile = BacktestProfile::from_toml_str(&profile_toml("attach_funding = true")).unwrap();
    let result = run_backtest(&profile, store).unwrap();

    assert!(
        (result.funding_paid - (-3.0)).abs() < 1e-9,
        "expected -3.0 (3 events x 1.0), got {} — a per-BAR charge would be ~-16 (16 held bars)",
        result.funding_paid
    );
}

/// Off / absent / on-but-empty are all a no-op: no funding is ever charged.
#[test]
fn attach_funding_off_or_empty_is_a_noop() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(DataFusionHist::open(dir.path()).unwrap());

    let price: Vec<Bar> = (0..24).map(|i| bar(i * HOUR, 100.0)).collect();
    store.append_bars(VENUE, SYMBOL, "1h", &price, None).unwrap();
    // A funding series EXISTS, to prove the flag — not its mere presence — is what reads it.
    let funding = vec![funding_bar(8 * HOUR, 0.01), funding_bar(16 * HOUR, 0.01)];
    store.append_bars(VENUE, SYMBOL, "funding", &funding, None).unwrap();

    // flag OFF: the funding series is never read.
    let p = BacktestProfile::from_toml_str(&profile_toml("attach_funding = false")).unwrap();
    assert_eq!(run_backtest(&p, store.clone()).unwrap().funding_paid, 0.0, "off must not charge");

    // flag ABSENT: same.
    let p = BacktestProfile::from_toml_str(&profile_toml("")).unwrap();
    assert_eq!(run_backtest(&p, store).unwrap().funding_paid, 0.0, "absent must not charge");
}

/// On, but no stored funding data at all: a warning + unchanged bars, NEVER an error or a charge.
#[test]
fn attach_funding_on_with_no_funding_series_is_a_warn_noop() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(DataFusionHist::open(dir.path()).unwrap());

    let price: Vec<Bar> = (0..24).map(|i| bar(i * HOUR, 100.0)).collect();
    store.append_bars(VENUE, SYMBOL, "1h", &price, None).unwrap();
    // NO "funding" series is written — an absent series must load as empty, not error.

    let p = BacktestProfile::from_toml_str(&profile_toml("attach_funding = true")).unwrap();
    let result = run_backtest(&p, store).expect("absent funding data is a no-op, not an error");
    assert_eq!(result.funding_paid, 0.0, "no funding data must charge nothing");
}

/// The price-interval-COARSER-than-funding-cadence violation: two funding events fall in one price
/// bar's window. The join keeps only the LAST and counts the collision (the caller warns).
#[test]
fn two_events_in_one_bar_window_keeps_last_and_counts_the_collision() {
    // Two DAILY price bars; three 8h funding events inside day 0's window [0, DAY).
    let day = 24 * HOUR;
    let mut price = vec![bar(0, 100.0), bar(day, 100.0)];
    let events = vec![(0, 0.01), (8 * HOUR, 0.02), (16 * HOUR, 0.03)];

    let stats = window_join_funding(&mut price, &events);
    assert_eq!(stats.placed, 1, "only day-0 got a rate; day-1 window had no event");
    assert_eq!(stats.collisions, 2, "two of the three events collided into day-0's window");
    // Last event in the window wins.
    assert_eq!(price[0].funding, Some(0.03), "the LAST colliding rate is kept");
    assert_eq!(price[1].funding, None, "day-1 stays None");
}

/// A funding event earlier than the first price bar has no window to attach to — it is dropped and
/// counted, never mis-attached to bar 0.
#[test]
fn an_event_before_the_first_bar_is_dropped() {
    let mut price = vec![bar(10 * HOUR, 100.0), bar(11 * HOUR, 100.0)];
    let events = vec![(5 * HOUR, 0.01), (10 * HOUR, 0.02)];

    let stats = window_join_funding(&mut price, &events);
    assert_eq!(stats.dropped_before, 1, "the ts=5h event predates the first bar (ts=10h)");
    assert_eq!(stats.placed, 1);
    assert_eq!(price[0].funding, Some(0.02), "only the in-window event lands on bar 0");
    assert_eq!(price[1].funding, None);
}
