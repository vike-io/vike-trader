//! Store-backed gate for `mtm_curve_from_store`: fold a synthetic journal-fill stream against a
//! seeded bar store and check the reconstructed mark-to-market equity/exposure — including a
//! sample with no as-of price (the missing-price gap is FLAGGED, not dropped).
//!
//! Seeds the shared `vike_data::MemHistStore` double through `append_bars`, keyed on
//! `(venue, symbol, interval)` exactly as the code under test reads it.
//!
//! ⚠ This file carried `#![cfg(feature = "journal")]` until 2026-09-28, when the function under
//! test was the one `journal`-gated item in vike-report's `mtm.rs`. The fold it feeds moved to
//! `vike-analytics` with `RuntimeStats` and `mtm_equity_curve`, the store builder stayed here in
//! `src/store.rs`, and the feature was deleted — so nothing here is conditional. The folds are
//! still only interesting beside the store, so the file travels as one.

use vike_analytics::{RuntimeStats, mtm_equity_curve};
use vike_data::{HistStore, MemHistStore};
use vike_model::Bar;
use vike_model::events::{FillEvent, TradeId};
use vike_report::mtm_curve_from_store;

/// A `MemHistStore` holding `bars` for `(venue, symbol)` at the `1m` interval every test reads.
fn seeded(venue: &str, symbol: &str, bars: Vec<Bar>) -> MemHistStore {
    let store = MemHistStore::new();
    store
        .append_bars(venue, symbol, "1m", &bars, None)
        .expect("MemHistStore accepts the seed bars");
    store
}

fn fill(side: i32, qty: f64, px: f64, ts: i64) -> FillEvent {
    FillEvent {
        // minted here, not read off a wire — same `t<ts>` bytes as before
        trade_id: TradeId::prefixed("t", ts),
        client_order_id: String::new(),
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        side,
        last_qty: qty,
        last_px: px,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "taker".into(),
        ts,
        mark_price: Some(px),
        position_side: "BOTH".into(),
    }
}

fn bar(ts: i64, close: f64) -> Bar {
    Bar {
        ts,
        open: close,
        high: close,
        low: close,
        close,
        volume: 0.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}

#[test]
fn mtm_curve_from_store_reconstructs_marked_equity() {
    // A long round-trip; the bar closes rise 100 -> 110 -> 120 across the hold.
    let store = seeded(
        "binance",
        "BTCUSDT",
        vec![bar(1000, 100.0), bar(2000, 110.0), bar(3000, 120.0), bar(4000, 120.0)],
    );
    let fills = vec![fill(1, 1.0, 100.0, 1000), fill(-1, 1.0, 120.0, 3000)];
    let points = mtm_curve_from_store(&store, &fills, 1_000.0, "binance", "1m").expect("mtm");

    // Sample grid = union of the fill timestamps {1000,3000} and the bars inside the fill window
    // [1000,3000] = {1000,2000,3000}; the 4000 bar is outside the window, so it is not a mark.
    let (eq, ts) = mtm_equity_curve(&points);
    assert_eq!(ts, vec![1000, 2000, 3000]);
    // 1000: flat @ mark 100. 2000: long marked at 110 -> +10. 3000: closed +20, flat.
    assert_eq!(eq, vec![1_000.0, 1_010.0, 1_020.0]);
    assert!(points.iter().all(|p| p.missing_prices == 0), "every sample is priced");

    let rs = RuntimeStats::from_points(&points);
    assert_eq!(rs.traded_notional, 220.0, "|1|*100 + |1|*120");
    assert_eq!(rs.peak_margin_used, 110.0, "peak gross notional is the 110 mark at ts 2000");
}

#[test]
fn store_flags_missing_price_before_first_bar() {
    // The buy lands at ts 500, BEFORE the first bar (1000) — so the ts-500 sample has no as-of
    // price for the open position: it must be FLAGGED missing, not dropped.
    let store = seeded("binance", "BTCUSDT", vec![bar(1000, 100.0), bar(2000, 110.0)]);
    let fills = vec![fill(1, 1.0, 100.0, 500), fill(-1, 1.0, 110.0, 2000)];
    let points = mtm_curve_from_store(&store, &fills, 1_000.0, "binance", "1m").expect("mtm");

    // grid = {500,2000} ∪ bars in [500,2000] {1000,2000} = {500,1000,2000}.
    assert_eq!(points.len(), 3, "three samples, including the pre-bar one");
    let at500 = points.iter().find(|p| p.ts == 500).expect("a sample at ts 500");
    assert_eq!(at500.missing_prices, 1, "no bar at/before 500 -> flagged, not dropped");
    assert_eq!(at500.unrealized, 0.0, "silent-zero for the unpriceable open position");
    assert_eq!(at500.gross_notional, 0.0, "unpriceable -> excluded from notional (LEAN skip)");
    assert_eq!(at500.equity, 1_000.0, "equity still resolves through the gap");
    let at1000 = points.iter().find(|p| p.ts == 1000).expect("a sample at ts 1000");
    assert_eq!(at1000.missing_prices, 0, "the 1000 bar prices the position");
}
