//! A fee field the venue sent and this decoder cannot read is COUNTED, not silently zero.
//!
//! `execFee` rode `vike_bridge_core::json::get_f64`, which turns anything unreadable into `0.0`: a
//! venue that changed the field's shape booked every fill fee-free with no line anywhere
//! (FEES-AUDIT 2026-10-26 §4.8 e). `get_fee_f64` keeps the booking (`0.0`) and adds the count and
//! the line, on the live row, the replayed row and the reconcile row alike.
//!
//! The counter is process-wide and these tests share a process under plain `cargo test`, so each one
//! asserts "at least one more than before", never an exact delta.

use serde_json::{Value, json};
use vike_bridge_core::json::malformed_fee_count;
use vike_bybit::event_mapper::map_execution;
use vike_bybit::history::map_bybit_history;
use vike_bybit::recon_client::parse_fills;
use vike_model::events::Event;

/// A fully filled `execution` row whose `execFee` is `fee`.
fn row(fee: Value) -> Value {
    json!({
        "execType": "Trade", "symbol": "BTCUSDT", "orderLinkId": "c1", "side": "Buy",
        "execPrice": "50000", "execQty": "1.0", "execFee": fee, "feeCurrency": "USDT",
        "isMaker": false, "execTime": "5", "cumExecQty": "1.0", "orderQty": "1.0", "execId": "e1",
        "orderId": "o9"
    })
}

fn commission_of(events: &[Event]) -> f64 {
    events
        .iter()
        .find_map(|e| match e {
            Event::Fill(f) => Some(f.commission),
            _ => None,
        })
        .expect("a bare Event::Fill")
}

/// The control: a readable `execFee` (and a rebate) is booked as stated and moves no count.
#[test]
fn a_readable_fee_is_booked_and_counts_nothing() {
    let before = malformed_fee_count();
    assert_eq!(commission_of(&map_execution(&row(json!("0.1")), "bybit", "BTCUSDT")), 0.1);
    assert_eq!(commission_of(&map_execution(&row(json!("-0.02")), "bybit", "BTCUSDT")), -0.02);
    assert_eq!(malformed_fee_count(), before, "nothing malformed was read");
}

/// The live row: still folded, fee `0.0`, the unreadable field counted.
#[test]
fn a_live_fee_that_is_not_a_number_is_counted() {
    let before = malformed_fee_count();
    let fill = commission_of(&map_execution(&row(json!("oops")), "bybit", "BTCUSDT"));
    assert_eq!(fill, 0.0, "the fill still books, fee-free");
    assert!(malformed_fee_count() > before);
}

/// The resync replay of the same execution.
#[test]
fn a_replayed_fee_that_is_not_a_number_is_counted() {
    let before = malformed_fee_count();
    let events = map_bybit_history(&json!([]), &json!([row(json!(true))]), "bybit", "BTCUSDT");
    assert_eq!(commission_of(&events), 0.0);
    assert!(malformed_fee_count() > before);
}

/// The reconcile fill report of the same execution.
#[test]
fn a_reconciled_fee_that_is_not_a_number_is_counted() {
    let before = malformed_fee_count();
    let body = json!({ "list": [row(json!({"x": 1}))] });
    let fills = parse_fills(&body.to_string()).expect("the body parses");
    assert_eq!(fills.len(), 1);
    assert_eq!(fills[0].commission, 0.0);
    assert!(malformed_fee_count() > before);
}
