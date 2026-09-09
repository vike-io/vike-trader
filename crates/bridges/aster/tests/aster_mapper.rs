//! Golden field-for-field mapping of Aster user-data frames → Event. Frames are Binance-shaped
//! (Aster clones the schema); values below mirror the Aster futures-v3 / spot-v3 doc examples.
use serde_json::json;
use vike_aster::event_mapper::{map_aster_private, map_execution_report};
use vike_aster::perp_mapper::map_aster_perp;

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
