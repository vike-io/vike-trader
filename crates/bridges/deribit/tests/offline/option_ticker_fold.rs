//! The decoder and `vike_options::apply_ticker_to_chains` in LOCKSTEP: captured `ticker` frames go
//! through this crate's `options_feed::parse_ticker` and fold onto a seeded grid. Moved here from
//! `crates/vike-options/src/chain/fold/ticker_tests.rs` so the layer-15 maths crate names no venue;
//! the fold's venue-free tests stay there.

use vike_options::apply_ticker_to_chains;
use vike_options::test_support::{NOW, fold_index, seeded_btc};

/// The decoder and the fold in LOCKSTEP: the captured `ticker` frame (mainnet, 2026-07-26,
/// `crates/bridges/deribit/src/ticker_tests.rs`'s `TICKER_FRAME`, its instrument renamed onto this
/// grid's strike) goes through `parse_ticker` and lands with the wire's PERCENT `mark_iv` divided
/// ONCE (66.33 → 0.6633, the value the old fold computed) and the coin prices scaled by the chain
/// spot.
#[test]
fn a_decoded_ticker_frame_folds_to_the_same_grid_values() {
    let frame = serde_json::json!({
        "jsonrpc": "2.0", "method": "subscription",
        "params": {"channel": "ticker.BTC-27JUN26-100000-C.100ms", "data": {
            "instrument_name": "BTC-27JUN26-100000-C", "best_bid_price": 0.1015,
            "best_ask_price": 0.106, "mark_price": 0.1036, "mark_iv": 66.33,
            "open_interest": 0.0, "stats": {"volume": 0.0}, "underlying_price": 64701.57
        }}
    });
    let row = vike_deribit::options_feed::parse_ticker(&frame, NOW).unwrap();
    let mut by = seeded_btc(Some(9.9));
    assert_eq!(apply_ticker_to_chains(&mut by, &fold_index(), &[row], NOW, 0.0), 1);
    let call = by["BTC"].chains["2026-06-27"].rows[0].call.as_ref().unwrap();
    assert_eq!(call.bid, Some(0.1015 * 100_000.0));
    assert_eq!(call.ask, Some(0.106 * 100_000.0));
    assert_eq!(call.mark, Some(0.1036 * 100_000.0));
    assert_eq!(call.iv, Some(0.6633), "percent divided exactly once, at the deribit boundary");
}

/// DECODE AND STORE ONLY: `parse_ticker` now fills sizes, `last`, `bid_iv`/`ask_iv`, the venue's
/// greeks and `index_price`, and the fold must not read any of them — a frame carrying them folds
/// to EXACTLY the grid the same frame without them folds to (the grid's greeks stay the ones
/// `enrich_quote` derives from the IV, never the venue's).
#[test]
fn the_extra_decoded_fields_change_no_grid_value() {
    let base = serde_json::json!({
        "instrument_name": "BTC-27JUN26-100000-C", "best_bid_price": 0.1015,
        "best_ask_price": 0.106, "mark_price": 0.1036, "mark_iv": 66.33,
        "open_interest": 0.0, "stats": {"volume": 0.0}, "underlying_price": 64701.57
    });
    let mut rich = base.clone();
    let extra = serde_json::json!({
        "best_bid_amount": 12.5, "best_ask_amount": 3.0, "last_price": 0.104,
        "bid_iv": 61.2, "ask_iv": 71.8, "index_price": 64688.12,
        "greeks": {"delta": 0.42, "gamma": 0.001, "vega": 9.9, "theta": -77.0, "rho": 5.5}
    });
    for (k, v) in extra.as_object().unwrap() {
        rich.as_object_mut().unwrap().insert(k.clone(), v.clone());
    }
    let frame = |data: &serde_json::Value| {
        serde_json::json!({
            "jsonrpc": "2.0", "method": "subscription",
            "params": {"channel": "ticker.BTC-27JUN26-100000-C.100ms", "data": data}
        })
    };
    let plain_row = vike_deribit::options_feed::parse_ticker(&frame(&base), NOW).unwrap();
    let rich_row = vike_deribit::options_feed::parse_ticker(&frame(&rich), NOW).unwrap();
    assert_eq!(rich_row.vega, Some(9.9), "the rich frame's extras ARE decoded");
    assert_eq!(rich_row.bid_size, Some(12.5));

    let mut plain_grid = seeded_btc(Some(9.9));
    let mut rich_grid = seeded_btc(Some(9.9));
    assert_eq!(apply_ticker_to_chains(&mut plain_grid, &fold_index(), &[plain_row], NOW, 0.0), 1);
    assert_eq!(apply_ticker_to_chains(&mut rich_grid, &fold_index(), &[rich_row], NOW, 0.0), 1);
    assert_eq!(rich_grid["BTC"].chains, plain_grid["BTC"].chains, "every grid value identical");
    let call = rich_grid["BTC"].chains["2026-06-27"].rows[0].call.as_ref().unwrap();
    assert_ne!(call.vega, Some(9.9), "the grid's vega is derived, not the venue's");
}
