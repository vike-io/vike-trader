//! [`fill_trade_id`] — a fill on a symbol other than the mounted one is qualified with its symbol,
//! because `t` is a per-SYMBOL sequence and the engine's fill ledger keys on the bare string; a fill
//! on the mounted symbol keeps the bare `t` it always had. Driven through the real mappers (spot
//! executionReport, perp ORDER_TRADE_UPDATE and TRADE_LITE), the three places a fill id is minted.

use super::*;
use serde_json::json;
use vike_model::events::Event;

fn id_of(events: &[Event]) -> String {
    events
        .iter()
        .find_map(|e| match e {
            Event::Fill(f) => Some(f.trade_id.to_string()),
            _ => None,
        })
        .expect("a bare Event::Fill")
}

fn spot_fill(symbol: &str, t: u64) -> serde_json::Value {
    json!({
        "e": "executionReport", "s": symbol, "x": "TRADE", "X": "FILLED", "S": "BUY",
        "c": "c-1", "i": 9, "T": 5, "l": "1", "L": "10", "n": "0", "N": "USDT", "t": t, "m": false
    })
}

fn perp_update(symbol: &str, t: u64) -> serde_json::Value {
    json!({
        "e": "ORDER_TRADE_UPDATE", "T": 5,
        "o": {"s": symbol, "c": "c-1", "x": "TRADE", "X": "FILLED", "S": "BUY", "l": "1",
              "L": "10", "n": "0", "N": "USDT", "t": t, "m": false, "ps": "BOTH"}
    })
}

#[test]
fn the_helper_keeps_the_mounted_symbols_bare_id_and_qualifies_every_other() {
    let id = |t, sym, mounted| fill_trade_id(t, sym, mounted).unwrap().to_string();
    assert_eq!(id("77", "BTCUSDT", "BTCUSDT"), "77");
    assert_eq!(id("77", "btcusdt", "BTCUSDT"), "77", "case does not make a second symbol");
    assert_eq!(id("77", "BTCUSDT.P", "BTCUSDT.P"), "77");
    assert_eq!(id("77", "BTCUSDT.P", "BTCUSDT"), "77", "the lane suffix is not a second symbol");
    assert_eq!(id("77", "ETHUSDT", "BTCUSDT"), "ETHUSDT:77");
    assert_eq!(id("77", "ETHUSDT.P", "BTCUSDT.P"), "ETHUSDT.P:77");
    assert!(fill_trade_id("", "ETHUSDT", "BTCUSDT").is_err(), "an empty `t` is still refused");
}

/// The defect: BTC and ETH fills that share the venue trade id `500` must reach the engine under
/// two different keys, or the second is dropped as a re-delivery.
#[test]
fn two_symbols_fills_with_one_venue_trade_id_get_two_dedup_keys_on_the_spot_lane() {
    let btc = id_of(&map_execution_report_for(&spot_fill("BTCUSDT", 500)));
    let eth = id_of(&map_execution_report_for(&spot_fill("ETHUSDT", 500)));
    assert_eq!(btc, "500", "the mounted symbol's id is byte-identical to before");
    assert_eq!(eth, "ETHUSDT:500");
    assert_ne!(btc, eth);
}

fn map_execution_report_for(frame: &serde_json::Value) -> Vec<Event> {
    crate::family::event_mapper::map_execution_report(frame, "binance", "BTCUSDT")
}

#[test]
fn two_symbols_fills_with_one_venue_trade_id_get_two_dedup_keys_on_the_perp_lane() {
    let map = |frame: &serde_json::Value| {
        crate::family::perp_mapper::map_perp(frame, "binance", "BTCUSDT.P")
    };
    assert_eq!(id_of(&map(&perp_update("BTCUSDT", 500))), "500");
    assert_eq!(id_of(&map(&perp_update("ETHUSDT", 500))), "ETHUSDT.P:500");
}

/// TRADE_LITE and its ORDER_TRADE_UPDATE twin must still collapse into ONE booking: both go through
/// the same helper, so a foreign symbol's pair agrees on the qualified key.
#[test]
fn a_foreign_symbols_trade_lite_hint_and_its_twin_share_one_key() {
    let lite = json!({
        "e": "TRADE_LITE", "T": 5, "s": "ETHUSDT", "m": false, "c": "c-1", "S": "BUY",
        "L": "10", "l": "1", "t": 500, "i": 9
    });
    let early = crate::family::perp_mapper::map_perp_opts(&lite, "binance", "BTCUSDT.P", true);
    let slow =
        crate::family::perp_mapper::map_perp(&perp_update("ETHUSDT", 500), "binance", "BTCUSDT.P");
    assert_eq!(id_of(&early), id_of(&slow));
    assert_eq!(id_of(&early), "ETHUSDT.P:500");
}
