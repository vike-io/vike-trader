//! A fee field the venue sent and this decoder cannot read is COUNTED, not silently zero.
//!
//! `vike_bridge_core::json::get_f64` turns anything unreadable into `0.0`, and a binance fill's
//! `n` rode it: a venue that changed the field's shape (a word, a bool, an object) booked every fill
//! fee-free with no line anywhere (FEES-AUDIT 2026-10-26 §4.8 e). `get_fee_f64` keeps the booking
//! (`0.0`: a fill is never dropped for its fee) and adds the count and the line.
//!
//! The counter is process-wide and these tests share a process under plain `cargo test`, so each one
//! asserts "at least one more than before", never an exact delta.

use serde_json::{Value, json};
use vike_binance::event_mapper::map_binance_private;
use vike_binance::perp_mapper::map_binance_perp;
use vike_bridge_core::json::malformed_fee_count;
use vike_model::events::{Event, FillEvent};

fn bare_fill(events: &[Event]) -> FillEvent {
    events
        .iter()
        .find_map(|e| match e {
            Event::Fill(f) => Some(f.clone()),
            _ => None,
        })
        .expect("a bare Event::Fill")
}

/// A spot executionReport TRADE whose `n` is `n`.
fn spot_report(n: Value) -> Value {
    json!({
        "e": "executionReport", "s": "BTCUSDT", "c": "fee-test", "S": "SELL", "x": "TRADE",
        "X": "FILLED", "t": 7, "l": "1.0", "L": "100.0", "n": n, "N": "USDT", "m": false,
        "T": 1_784_710_420_755_i64, "i": 9
    })
}

/// A perp ORDER_TRADE_UPDATE TRADE whose `n` is `n`.
fn perp_update(n: Value) -> Value {
    json!({
        "e": "ORDER_TRADE_UPDATE", "T": 1_784_710_420_755_i64,
        "o": {
            "s": "BTCUSDT", "c": "fee-test", "S": "SELL", "x": "TRADE", "X": "FILLED", "t": 8,
            "l": "1.0", "L": "100.0", "n": n, "N": "USDT", "m": false, "i": 9, "ps": "BOTH"
        }
    })
}

/// The control: a readable `n` is booked as stated and moves no count.
#[test]
fn a_readable_fee_is_booked_and_counts_nothing() {
    let before = malformed_fee_count();
    let spot = bare_fill(&map_binance_private(&spot_report(json!("0.25")), "binance", "BTCUSDT"));
    assert_eq!(spot.commission, 0.25);
    let perp = bare_fill(&map_binance_perp(&perp_update(json!("0.5")), "binance", "BTCUSDT"));
    assert_eq!(perp.commission, 0.5);
    assert_eq!(malformed_fee_count(), before, "nothing malformed was read");
}

/// A spot `n` that is a word: the fill is still folded (fee `0.0`) and the unreadable field is counted.
#[test]
fn a_spot_fee_that_is_not_a_number_is_counted() {
    let before = malformed_fee_count();
    let fill = bare_fill(&map_binance_private(&spot_report(json!("oops")), "binance", "BTCUSDT"));
    assert_eq!(fill.commission, 0.0, "the fill still books, fee-free");
    assert!(malformed_fee_count() > before, "the unreadable `n` must be counted");
}

/// The perp twin, on the fill arm.
#[test]
fn a_perp_fee_that_is_not_a_number_is_counted() {
    let before = malformed_fee_count();
    let fill = bare_fill(&map_binance_perp(&perp_update(json!({"x": 1})), "binance", "BTCUSDT"));
    assert_eq!(fill.commission, 0.0, "the fill still books, fee-free");
    assert!(malformed_fee_count() > before, "the unreadable `n` must be counted");
}
