//! Golden field-for-field mapping of Aster user-data frames → Event. Frames are Binance-shaped
//! (Aster clones the schema); values below mirror the Aster futures-v3 / spot-v3 doc examples.
use serde_json::json;
use vike_aster::event_mapper::map_aster_private;
use vike_aster::perp_mapper::map_aster_perp;
use vike_binance::family::event_mapper::map_execution_report;

#[test]
fn spot_execution_report_new_maps_to_order_accepted() {
    let frame = json!({
        "e": "executionReport", "s": "BTCUSDT", "c": "c-1", "S": "BUY",
        "x": "NEW", "X": "NEW", "i": 42, "T": 1_700_000_000_000_i64
    });
    let evs = map_execution_report(&frame, "aster", "BTCUSDT");
    assert_eq!(evs.len(), 1);
    // matches Event::OrderAccepted { client_order_id: "c-1", venue_order_id: Some("42"), .. }
    let v = serde_json::to_value(&evs[0]).unwrap();
    assert_eq!(v["type"], "OrderAccepted");
    assert_eq!(v["client_order_id"], "c-1");
}

#[test]
fn spot_execution_report_trade_dual_publishes_fill_and_wrap() {
    let frame = json!({
        "e": "executionReport", "s": "BTCUSDT", "c": "c-1", "S": "BUY", "x": "TRADE", "X": "FILLED",
        "t": 7, "l": "0.01", "L": "50000", "n": "0.5", "N": "USDT", "m": false, "T": 1_700_000_000_000_i64
    });
    let evs = map_execution_report(&frame, "aster", "BTCUSDT");
    assert_eq!(evs.len(), 2, "bare Fill + OrderFilled wrap");
    assert_eq!(serde_json::to_value(&evs[1]).unwrap()["type"], "OrderFilled");
}

#[test]
fn perp_order_trade_update_trade_emits_fill() {
    let frame = json!({
        "e": "ORDER_TRADE_UPDATE", "T": 1_700_000_000_000_i64,
        "o": { "s": "BTCUSDT", "c": "c-1", "S": "BUY", "x": "TRADE", "X": "PARTIALLY_FILLED",
               "t": 9, "l": "0.01", "L": "50000", "n": "0.5", "ps": "BOTH" }
    });
    let evs = map_aster_perp(&frame, "aster", "BTCUSDT");
    assert!(evs.iter().any(|e| serde_json::to_value(e).unwrap()["type"] == "OrderPartiallyFilled"));
}

#[test]
fn spot_account_position_maps_to_account_state() {
    let frame = json!({
        "e": "outboundAccountPosition", "E": 1_700_000_000_000_i64,
        "B": [ { "a": "USDT", "f": "1000.0", "l": "0.0" } ]
    });
    let evs = map_aster_private(&frame, "aster", "BTCUSDT");
    assert_eq!(serde_json::to_value(&evs[0]).unwrap()["type"], "AccountState");
}

/// Aster's spot lane runs the family's fee rule (`vike_binance::family::spot_fee`): a fee the venue
/// STATES in the symbol's base coin is booked at the fill price in the quote, a quote fee is untouched.
/// What aster's spot actually charges is unmeasured (no spot fill was ever journaled); the rule keys on
/// the asset the frame names, so it converts only a fee aster itself calls a base-coin fee.
#[test]
fn spot_fee_in_the_base_coin_is_booked_in_the_quote_and_a_quote_fee_is_not_touched() {
    let fill_of = |asset: &str, fee: &str| {
        let frame = json!({
            "e": "executionReport", "s": "BTCUSDT", "c": "c-1", "S": "BUY", "x": "TRADE",
            "X": "FILLED", "t": 7, "l": "0.001", "L": "60000", "n": fee, "N": asset, "m": false,
            "T": 1_700_000_000_000_i64
        });
        map_aster_private(&frame, "aster", "BTCUSDT")
            .into_iter()
            .find_map(|e| match e {
                vike_model::events::Event::Fill(f) => Some(f),
                _ => None,
            })
            .expect("a bare Fill")
    };
    let base = fill_of("BTC", "0.000001");
    assert!((base.commission - 0.06).abs() < 1e-12, "0.000001 BTC x 60000 = 0.06 USDT");
    assert_eq!(base.commission_asset, "USDT");
    let quote = fill_of("USDT", "0.06");
    assert_eq!((quote.commission, quote.commission_asset.as_str()), (0.06, "USDT"));
}
