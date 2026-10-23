//! The decoder and `vike_options::apply_markprice_to_chains` in LOCKSTEP: a captured
//! `markprice.options` frame goes through this crate's `options_feed::parse_markprice_options` and
//! folds onto a seeded grid. Moved here from `crates/vike-options/src/chain/fold/markprice_tests.rs`
//! so the layer-15 maths crate names no venue; the fold's venue-free tests stay there.

use std::collections::BTreeMap;
use vike_marketdata::{
    Expiry, OptionChain, OptionKind, OptionQuote, StrikeRow, UnderlyingChains, UnderlyingKind,
};
use vike_options::{OptionIndex, apply_markprice_to_chains};

const NOW: i64 = 1_780_387_200_000; // 2026-06-02 08:00 UTC (chain.rs's NOW)

/// The option index every name these tests stream is looked up in: the coin-settled BTC/ETH
/// options (strikes on and off the grid, a fetched and an unfetched expiry), the USDC-settled SOL
/// put, and the BTC perpetual (a future: indexed, but no option address).
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

/// A one-strike BTC chain for 2026-06-27 (call+put), mimicking a REST-seeded grid the WS stream
/// then updates. Spot 100k; each quote carries a stale mark/iv the stream overwrites.
fn seeded_btc() -> BTreeMap<String, UnderlyingChains> {
    let mk = |name: &str, kind: OptionKind| OptionQuote {
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

/// The decoder and the fold in LOCKSTEP: the captured `markprice.options` row (mainnet,
/// 2026-07-26, `crates/bridges/deribit/src/options_feed_tests.rs`'s `FRAME`, its instrument renamed
/// onto this grid's strike) goes through `parse_markprice_options` and lands with the decimal IV
/// verbatim and the coin mark scaled by the chain spot — the values the old row type produced.
#[test]
fn a_decoded_markprice_frame_folds_to_the_same_grid_values() {
    let frame = serde_json::json!({
        "jsonrpc": "2.0", "method": "subscription",
        "params": {"channel": "markprice.options.btc_usd", "data": [
            {"timestamp": 1_785_079_023_870_i64, "iv": 0.3706,
             "instrument_name": "BTC-27JUN26-100000-C", "mark_price": 0.005}
        ]}
    });
    let rows = vike_deribit::options_feed::parse_markprice_options(&frame, NOW).unwrap();
    let mut by = seeded_btc();
    assert_eq!(apply_markprice_to_chains(&mut by, &index(), &rows, NOW, 0.0), 1);
    let call = by["BTC"].chains["2026-06-27"].rows[0].call.as_ref().unwrap();
    assert_eq!(call.mark, Some(0.005 * 100_000.0), "coin→USD by the chain spot");
    assert_eq!(call.iv, Some(0.3706), "the wire's decimal iv, verbatim end to end");
}
