//! Ports `tests/unit/data/test_options_deribit.py` (captured payloads inline).

use super::*;

/// 2026-06-02 08:00 UTC — the oracle tests' `_ms(2026, 6, 2)`.
const NOW: i64 = 1_780_387_200_000;

fn summary() -> Vec<Value> {
    // two expiries (27 Jun + 25 Sep 2026); within 27 Jun: two strikes, call+put on the first
    serde_json::json!([
        {"instrument_name": "BTC-27JUN26-100000-C", "bid_price": 0.05, "ask_price": 0.06,
         "mark_price": 0.055, "mark_iv": 62.5, "open_interest": 120.0, "volume": 8.0,
         "underlying_price": 104000.0},
        {"instrument_name": "BTC-27JUN26-100000-P", "bid_price": 0.04, "ask_price": 0.05,
         "mark_price": 0.045, "mark_iv": 61.0, "open_interest": 90.0, "volume": 3.0,
         "underlying_price": 104000.0},
        {"instrument_name": "BTC-27JUN26-110000-C", "bid_price": 0.02, "ask_price": 0.03,
         "mark_price": 0.025, "mark_iv": 64.0, "open_interest": 50.0, "volume": 1.0,
         "underlying_price": 104000.0},
        {"instrument_name": "BTC-25SEP26-120000-C", "bid_price": 0.01, "ask_price": 0.02,
         "mark_price": 0.015, "mark_iv": 70.0, "open_interest": 10.0, "volume": 1.0,
         "underlying_price": 104000.0},
        {"instrument_name": "BTC-PERPETUAL", "mark_price": 104000.0}
    ])
    .as_array()
    .unwrap()
    .clone()
}

fn usdc_summary() -> Vec<Value> {
    // The shared USDC book mixes coins; SOL's chain must pick out only SOL_USDC rows.
    serde_json::json!([
        {"instrument_name": "SOL_USDC-26JUN26-90-P", "bid_price": 17.5, "ask_price": 18.0,
         "mark_price": 17.75, "mark_iv": 60.0, "open_interest": 40.0, "volume": 5.0,
         "underlying_price": 74.5},
        {"instrument_name": "SOL_USDC-26JUN26-90-C", "bid_price": 1.0, "ask_price": 1.2,
         "mark_price": 1.1, "mark_iv": 61.0, "open_interest": 30.0, "volume": 2.0,
         "underlying_price": 74.5},
        {"instrument_name": "BTC_USDC-31JUL26-115000-P", "bid_price": 5000.0, "ask_price": 5100.0,
         "mark_price": 5050.0, "mark_iv": 55.0, "underlying_price": 104000.0},
        {"instrument_name": "XRP_USDC-26JUN26-3-C", "bid_price": 0.1, "ask_price": 0.12,
         "mark_price": 0.11, "mark_iv": 70.0, "underlying_price": 2.4}
    ])
    .as_array()
    .unwrap()
    .clone()
}

#[test]
fn parses_instrument_names() {
    assert_eq!(
        parse_instrument_name("BTC-27JUN26-100000-C"),
        Some(("BTC".into(), "2026-06-27".into(), 100000.0, OptionKind::Call))
    );
    assert_eq!(parse_instrument_name("BTC-PERPETUAL"), None);
    assert_eq!(parse_instrument_name("BTC-27JUN26-100000-Z"), None);
    assert_eq!(parse_instrument_name("BTC-27XXX26-100000-C"), None); // non-month token
    // USDC-margined altcoin form: base carries a _USDC suffix, dropped -> "SOL"
    assert_eq!(
        parse_instrument_name("SOL_USDC-26JUN26-90-P"),
        Some(("SOL".into(), "2026-06-26".into(), 90.0, OptionKind::Put))
    );
    assert_eq!(
        parse_instrument_name("SOL_USDC-5JUN26-80-C"),
        Some(("SOL".into(), "2026-06-05".into(), 80.0, OptionKind::Call))
    );
}

#[test]
fn lists_expiries_ascending_distinct() {
    let exps = list_expiries_from_summary(&summary(), NOW, None);
    let dates: Vec<&str> = exps.iter().map(|e| e.date.as_str()).collect();
    assert_eq!(dates, ["2026-06-27", "2026-09-25"]);
    assert_eq!(exps[0].dte, 25);
}

#[test]
fn lists_expiries_filters_to_requested_coin_in_shared_book() {
    let exps = list_expiries_from_summary(&usdc_summary(), NOW, Some("SOL"));
    let dates: Vec<&str> = exps.iter().map(|e| e.date.as_str()).collect();
    assert_eq!(dates, ["2026-06-26"]); // only SOL, not the BTC 31 Jul row
}

#[test]
fn builds_chain_groups_scales_and_enriches() {
    let chain = build_chain_from_summary("BTC", &summary(), "2026-06-27", NOW, false, 0.0);
    assert_eq!(chain.source, "deribit");
    assert_eq!(chain.underlying_kind, UnderlyingKind::Crypto);
    assert_eq!(chain.underlying_price, Some(104000.0));
    // the 25 Sep / 120000 row is filtered out
    let strikes: Vec<f64> = chain.rows.iter().map(|r| r.strike).collect();
    assert_eq!(strikes, [100000.0, 110000.0]);
    let row = &chain.rows[0];
    let (call, put) = (row.call.as_ref().unwrap(), row.put.as_ref().unwrap());
    assert_eq!(call.iv, Some(0.625)); // mark_iv % -> decimal
    assert_eq!(put.iv, Some(0.61));
    // Deribit premiums are coin units, scaled to USD by underlying_price (exact arithmetic)
    assert_eq!(call.bid, Some(0.05 * 104000.0));
    assert_eq!(call.mark, Some(0.055 * 104000.0));
    assert!(call.delta.is_some(), "greeks enriched from IV");
    assert!(chain.rows[1].put.is_none(), "only a call at 110000");
}

#[test]
fn builds_sol_chain_from_usdc_book_unscaled_premiums() {
    // usd_quoted=true: USDC premiums are already USD -> NOT scaled by the ~74.5 underlying
    let chain = build_chain_from_summary("SOL", &usdc_summary(), "2026-06-26", NOW, true, 0.0);
    assert_eq!(chain.underlying, "SOL");
    assert_eq!(chain.underlying_price, Some(74.5));
    let strikes: Vec<f64> = chain.rows.iter().map(|r| r.strike).collect();
    assert_eq!(strikes, [90.0]); // BTC/XRP rows excluded
    assert_eq!(chain.rows[0].put.as_ref().unwrap().bid, Some(17.5)); // not 17.5 * 74.5
    assert_eq!(chain.rows[0].call.as_ref().unwrap().mark, Some(1.1));
}

#[test]
fn chain_carries_exact_instrument_name() {
    // the arm path needs the exact venue id; the _USDC suffix must survive verbatim
    let chain = build_chain_from_summary("BTC", &summary(), "2026-06-27", NOW, false, 0.0);
    let row = &chain.rows[0];
    assert_eq!(row.call.as_ref().unwrap().instrument_name.as_deref(), Some("BTC-27JUN26-100000-C"));
    assert_eq!(row.put.as_ref().unwrap().instrument_name.as_deref(), Some("BTC-27JUN26-100000-P"));
    assert_eq!(
        chain.rows[1].call.as_ref().unwrap().instrument_name.as_deref(),
        Some("BTC-27JUN26-110000-C")
    );
    let sol = build_chain_from_summary("SOL", &usdc_summary(), "2026-06-26", NOW, true, 0.0);
    assert_eq!(
        sol.rows[0].put.as_ref().unwrap().instrument_name.as_deref(),
        Some("SOL_USDC-26JUN26-90-P")
    );
}

// ---- kind=chain snapshot recording (opt-in ChainRecorder hook) ----------------------------
// New additive tests (no Python twin): the fixture chain flattens to `ChainRow`s and lands in
// the store through `record_chain`, exercised over the DataFusion-free `MemHistStore` double
// (vike-data `test-support` dev-feature) exactly like client.rs's properties_recorder_tests.

use std::sync::Arc;
use vike_data::{ChainRecorder, HistStore, MemHistStore, TsRange};

#[test]
fn chain_snapshot_rows_flatten_fixture_chain() {
    let chain = build_chain_from_summary("BTC", &summary(), "2026-06-27", NOW, false, 0.0);
    let rows = chain_snapshot_rows(&chain);
    // 100000 call+put + 110000 call = 3 rows; the 25 Sep row was already expiry-filtered
    assert_eq!(rows.len(), 3);
    assert!(rows.iter().all(|r| r.ts == NOW && r.underlying == "BTC"));
    assert!(rows.iter().all(|r| r.expiry_ms == vike_options::expiry_ms("2026-06-27")));
    let call = &rows[0];
    assert_eq!(call.instrument, "BTC-27JUN26-100000-C", "verbatim venue id");
    assert!(call.is_call);
    assert_eq!(call.strike, 100_000.0);
    assert_eq!(call.bid, Some(0.05 * 104_000.0), "USD-scaled premium copied through");
    assert_eq!(call.iv, Some(0.625), "decimal IV copied through");
    assert!(call.delta.is_some(), "enriched greeks recorded");
    let put = &rows[1];
    assert_eq!(put.instrument, "BTC-27JUN26-100000-P");
    assert!(!put.is_call);
    // the 110000 strike has no put — only 3 rows total, and the lone call carries its strike
    assert_eq!(rows[2].strike, 110_000.0);
}

#[test]
fn record_chain_persists_fixture_and_same_minute_is_one_snapshot() {
    let store = Arc::new(MemHistStore::new());
    let rec = ChainRecorder::new(store.clone(), true);
    let chain = build_chain_from_summary("BTC", &summary(), "2026-06-27", NOW, false, 0.0);
    record_chain(&rec, &chain);
    record_chain(&rec, &chain); // same asof → same minute bucket → store no-op
    let got = store.scan_chain("deribit", "BTC", TsRange::all()).unwrap();
    assert_eq!(got.len(), 3, "recorded once, keyed venue=deribit/symbol=BTC");
    assert_eq!(got, chain_snapshot_rows(&chain));
}

#[test]
fn record_chain_disabled_recorder_writes_nothing() {
    let store = Arc::new(MemHistStore::new());
    let rec = ChainRecorder::new(store.clone(), false);
    let chain = build_chain_from_summary("BTC", &summary(), "2026-06-27", NOW, false, 0.0);
    record_chain(&rec, &chain);
    assert!(store.scan_chain("deribit", "BTC", TsRange::all()).unwrap().is_empty());
}
