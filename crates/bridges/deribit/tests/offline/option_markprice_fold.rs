//! The decoder and `vike_options::apply_markprice_to_chains` in LOCKSTEP: a captured
//! `markprice.options` frame goes through this crate's `options_feed::parse_markprice_options` and
//! folds onto a seeded grid. Moved here from `crates/vike-options/src/chain/fold/markprice_tests.rs`
//! so the layer-15 maths crate names no venue; the fold's venue-free tests stay there.

use vike_options::apply_markprice_to_chains;
use vike_options::test_support::{NOW, fold_index, seeded_btc};

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
    let mut by = seeded_btc(None);
    assert_eq!(apply_markprice_to_chains(&mut by, &fold_index(), &rows, NOW, 0.0), 1);
    let call = by["BTC"].chains["2026-06-27"].rows[0].call.as_ref().unwrap();
    assert_eq!(call.mark, Some(0.005 * 100_000.0), "coin→USD by the chain spot");
    assert_eq!(call.iv, Some(0.3706), "the wire's decimal iv, verbatim end to end");
}
