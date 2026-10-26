//! The decoder and `vike_options::apply_ticker_to_chains` in LOCKSTEP: captured `ticker` frames go
//! through this crate's `options_feed::parse_ticker` and fold onto a seeded grid. Moved here from
//! `crates/vike-options/src/chain/fold/ticker_tests.rs` so the layer-15 maths crate names no venue;
//! the fold's venue-free tests stay there.

use std::collections::BTreeMap;
use vike_marketdata::{
    Expiry, OptionChain, OptionKind, OptionQuote, StrikeRow, UnderlyingChains, UnderlyingKind,
};
use vike_options::{OptionIndex, apply_ticker_to_chains};

const NOW: i64 = 1_780_387_200_000; // 2026-06-02 08:00 UTC (chain.rs's NOW)

/// The option index every name these tests stream is looked up in (the markprice tests' set): the
/// ticker channel carries no strike, so the index is the ONLY source of one.
fn index() -> OptionIndex {
    use crate::option_fixtures::{test_delta_one, test_option};
    use OptionKind::{Call, Put};
    OptionIndex::from_instruments(&[
        test_option("BTC-27JUN26-100000-C", "BTC", "2026-06-27", 100_000.0, Call, "BTC"),
        test_option("BTC-27JUN26-100000-P", "BTC", "2026-06-27", 100_000.0, Put, "BTC"),
        test_option("BTC-27JUN26-999000-C", "BTC", "2026-06-27", 999_000.0, Call, "BTC"),
        test_option("BTC-25SEP26-100000-C", "BTC", "2026-09-25", 100_000.0, Call, "BTC"),
        test_option("ETH-27JUN26-3000-C", "ETH", "2026-06-27", 3_000.0, Call, "ETH"),
        test_option("SOL_USDC-25SEP26-45-P", "SOL", "2026-09-25", 45.0, Put, "USDC"),
        test_delta_one("BTC-PERPETUAL", "BTC"),
    ])
}

/// A one-strike BTC chain for 2026-06-27 (call+put), mimicking a REST-seeded grid the ticker then
/// updates. Spot 100k; each quote carries a stale bid/ask/mark/iv the stream overwrites.
fn seeded_btc() -> BTreeMap<String, UnderlyingChains> {
    let mk = |name: &str, kind: OptionKind| OptionQuote {
        bid: Some(9.9),
        ask: Some(9.9),
        mark: Some(1.0),
        iv: Some(0.10),
        instrument_name: Some(name.to_string()),
        ..OptionQuote::new(100_000.0, kind)
    };
    let chain = OptionChain {
        underlying: "BTC".into(),
        underlying_kind: UnderlyingKind::Crypto,
        underlying_price: Some(100_000.0),
        expiry: Expiry { date: "2026-06-27".into(), dte: 25, label: "27 Jun".into() },
        asof_ms: NOW,
        source: "deribit".into(),
        rows: vec![StrikeRow {
            strike: 100_000.0,
            call: Some(mk("BTC-27JUN26-100000-C", OptionKind::Call)),
            put: Some(mk("BTC-27JUN26-100000-P", OptionKind::Put)),
        }],
    };
    let mut chains = BTreeMap::new();
    chains.insert("2026-06-27".to_string(), chain);
    let bundle = UnderlyingChains {
        default_expiry: "2026-06-27".into(),
        expiries: vec![Expiry { date: "2026-06-27".into(), dte: 25, label: "27 Jun".into() }],
        chains,
    };
    let mut by = BTreeMap::new();
    by.insert("BTC".to_string(), bundle);
    by
}

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
    let mut by = seeded_btc();
    assert_eq!(apply_ticker_to_chains(&mut by, &index(), &[row], NOW, 0.0), 1);
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

    let mut plain_grid = seeded_btc();
    let mut rich_grid = seeded_btc();
    assert_eq!(apply_ticker_to_chains(&mut plain_grid, &index(), &[plain_row], NOW, 0.0), 1);
    assert_eq!(apply_ticker_to_chains(&mut rich_grid, &index(), &[rich_row], NOW, 0.0), 1);
    assert_eq!(rich_grid["BTC"].chains, plain_grid["BTC"].chains, "every grid value identical");
    let call = rich_grid["BTC"].chains["2026-06-27"].rows[0].call.as_ref().unwrap();
    assert_ne!(call.vega, Some(9.9), "the grid's vega is derived, not the venue's");
}
