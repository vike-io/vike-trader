//! End-to-end DEMO + regression for the MULTI-VENUE funding carry: a `FundingCarryController`
//! (registry `funding_carry`) mounted with per-symbol venue routing (`[strategy.params.venues]`)
//! runs over TWO venue series, each carrying its market funding rate via `engine.attach_funding`
//! (#768), and OPENS a carry off the cross-venue funding differential.
//!
//! Three behaviors are pinned:
//!   * TWO-LEG (empty `symbol`) — the true delta-neutral carry: LONG the low-funding venue, SHORT
//!     the high-funding one, one executor per leg, from ONE mount.
//!   * SINGLE-LEG (`symbol` set) — open only that symbol's leg (directional funding capture).
//!   * INERT (no `venues` map) — one fixed venue ⇒ the book never reaches 2 venues ⇒ nothing opens.
//!
//! The capability rests on two pieces landed together: the harness `venue_map` (asks the controller
//! `evaluate` per `(venue, symbol)` under each series' OWN venue) and the controller observing every
//! asked venue's funding BEFORE the symbol gate (so its cross-venue book fills). Store-backed ⇒
//! behind `datafusion-store`.
#![cfg(feature = "datafusion-store")]

use std::collections::BTreeSet;
use std::sync::Arc;

use vike_backtest::harness::{run_backtest, BacktestProfile};
use vike_data::{DataFusionHist, HistStore};
use vike_model::{Bar, Trade};

const HOUR: i64 = 3_600_000;

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
fn funding_bar(ts: i64, rate: f64) -> Bar {
    Bar { funding: Some(rate), ..bar(ts, 0.0) }
}

/// Seed a `DataFusionHist` with two venue price series + their 8h funding series (binance funds HIGH
/// +0.02 → longs pay; okx LOW -0.01) — a fat +0.03 gross carry. Returns the `TempDir` guard too: the
/// caller MUST keep it alive for the store's whole life (dropping it deletes the parquet dir).
fn seeded_store() -> (tempfile::TempDir, Arc<DataFusionHist>) {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(DataFusionHist::open(dir.path()).unwrap());
    let price: Vec<Bar> = (0..24).map(|i| bar(i * HOUR, 100.0)).collect();
    store.append_bars("binance", "BTCBIN", "1h", &price, None).unwrap();
    store.append_bars("okx", "BTCOKX", "1h", &price, None).unwrap();
    let hi = vec![funding_bar(0, 0.02), funding_bar(8 * HOUR, 0.02), funding_bar(16 * HOUR, 0.02)];
    let lo =
        vec![funding_bar(0, -0.01), funding_bar(8 * HOUR, -0.01), funding_bar(16 * HOUR, -0.01)];
    store.append_bars("binance", "BTCBIN", "funding", &hi, None).unwrap();
    store.append_bars("okx", "BTCOKX", "funding", &lo, None).unwrap();
    (dir, store)
}

/// `strategy_params` is spliced verbatim under `[strategy.params]` — each test supplies its own
/// `symbol`/`venues`/barrier knobs.
fn profile(strategy_params: &str) -> String {
    format!(
        r#"
[data]
kind = "bar"
interval = "1h"
from = "0"
to = "1000000000"
[[data.series]]
venue = "binance"
symbol = "BTCBIN"
[[data.series]]
venue = "okx"
symbol = "BTCOKX"

[engine]
cash = 100000.0
attach_funding = true

[strategy]
name = "funding_carry"
[strategy.params]
qty = 1.0
entry_threshold = 0.0
{strategy_params}
"#
    )
}

/// The distinct symbols that appear in the result's closed trades.
fn traded_symbols(trades: &[Trade]) -> BTreeSet<String> {
    trades.iter().map(|t| t.symbol.clone()).collect()
}

/// TWO-LEG delta-neutral: empty `symbol` ⇒ open a leg on BOTH carry venues. A 4h time barrier closes
/// each leg into a recorded trade, so we can assert BOTH venue symbols were traded.
#[test]
fn two_leg_delta_neutral_carry_opens_both_venues() {
    let params = "\
time_limit_ms = 14400000
cooldown_ms = 360000000
[strategy.params.venues]
BTCBIN = \"binance\"
BTCOKX = \"okx\"
";
    let p = BacktestProfile::from_toml_str(&profile(params)).unwrap();
    let (_dir, store) = seeded_store();
    let r = run_backtest(&p, store).unwrap();
    let symbols = traded_symbols(&r.trades);
    assert_eq!(
        symbols,
        BTreeSet::from(["BTCBIN".to_string(), "BTCOKX".to_string()]),
        "the delta-neutral carry must open a leg on BOTH venues (traded symbols={symbols:?}, n_trades={})",
        r.n_trades
    );
}

/// SINGLE-LEG: `symbol = "BTCBIN"` ⇒ open ONLY the BTCBIN leg, even though the cross-venue book still
/// informs the decision (okx is observed, never traded).
#[test]
fn single_leg_carry_opens_only_the_configured_symbol() {
    let params = "\
symbol = \"BTCBIN\"
time_limit_ms = 14400000
cooldown_ms = 360000000
[strategy.params.venues]
BTCBIN = \"binance\"
BTCOKX = \"okx\"
";
    let p = BacktestProfile::from_toml_str(&profile(params)).unwrap();
    let (_dir, store) = seeded_store();
    let r = run_backtest(&p, store).unwrap();
    let symbols = traded_symbols(&r.trades);
    assert_eq!(
        symbols,
        BTreeSet::from(["BTCBIN".to_string()]),
        "single-leg must trade ONLY the configured symbol (traded symbols={symbols:?})"
    );
}

/// INERT: no `venues` map ⇒ both series route to ONE default venue ⇒ the book never reaches two
/// venues ⇒ `rank_best_carry` (needs ≥2) is always None ⇒ nothing opens. Pins that the venue map is
/// what unlocks the carry.
#[test]
fn without_venue_map_nothing_opens() {
    let params = "\
time_limit_ms = 14400000
";
    let p = BacktestProfile::from_toml_str(&profile(params)).unwrap();
    let (_dir, store) = seeded_store();
    let r = run_backtest(&p, store).unwrap();
    assert_eq!(r.n_trades, 0, "no venue map ⇒ one venue ⇒ no carry ⇒ no trades");
    assert_eq!(r.funding_paid, 0.0, "nothing opened ⇒ no funding accrued");
}
