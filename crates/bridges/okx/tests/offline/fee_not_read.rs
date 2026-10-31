//! A fee field the venue sent and this decoder cannot read is COUNTED, not silently zero.
//!
//! `fillFee` rode `vike_bridge_core::json::get_f64`, which turns anything unreadable into `0.0`: a
//! venue that changed the field's shape booked every fill fee-free with no line anywhere
//! (FEES-AUDIT 2026-10-26 §4.8 e). `get_fee_f64` keeps the booking (`0.0`) and adds the count and
//! the line. An EMPTY `fillFee` is not malformed: OKX sends `""` on a row that is not a fill.
//!
//! The counter is process-wide and these tests share a process under plain `cargo test`, so each one
//! asserts "at least one more than before", never an exact delta.

use serde_json::{Value, json};
use vike_bridge_core::json::malformed_fee_count;
use vike_model::events::Event;
use vike_okx::event_mapper::map_okx_order;

/// An `orders` row carrying a real fill whose `fillFee` is `fee`.
fn row(fee: Value) -> Value {
    json!({
        "instId": "BTC-USDT", "clOrdId": "c1", "state": "filled", "side": "buy",
        "fillSz": "1.0", "fillPx": "50000", "fillFee": fee, "fillFeeCcy": "USDT",
        "execType": "T", "accFillSz": "1.0", "sz": "1.0", "fillTime": "5", "ordId": "o9",
        "tradeId": "t1"
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

/// The control: a readable `fillFee` (OKX negates it: a charge is negative on the wire) is booked
/// and moves no count, and an EMPTY one is the ordinary "no fee" and moves none either.
#[test]
fn a_readable_or_empty_fee_counts_nothing() {
    let before = malformed_fee_count();
    assert_eq!(commission_of(&map_okx_order(&row(json!("-0.1")), "okx", "BTC-USDT")), 0.1);
    assert_eq!(commission_of(&map_okx_order(&row(json!("")), "okx", "BTC-USDT")), 0.0);
    assert_eq!(malformed_fee_count(), before, "nothing malformed was read");
}

/// FEES-AUDIT 2026-10-26 §4.12 found no committed okx fill whose FEE anything read. This is the
/// ordinary market BUY CAPTURED on the demo account on 2026-10-24 (`tests/fixtures/algo/`, the frame the
/// triggered-stop tests use as their control): OKX states a charge as a NEGATIVE `fillFee`
/// (`-0.0040126` USDT on `execType` `T`, a taker), and the ledger must hold exactly the opposite sign, a
/// CHARGE, in the quote, at the table row's magnitude: within [½ maker, 1.5 taker] of the notional
/// (`fee_schedule_for("okx")`: 2 and 5 bps) and, the fill being a taker's, within 1 % of the taker rate.
#[test]
fn a_captured_fill_books_a_charge_of_the_table_rows_magnitude() {
    let frame: Value =
        serde_json::from_str(include_str!("../fixtures/algo/ws_orders_ordinary_filled.json"))
            .expect("the capture is JSON");
    assert_eq!(frame["data"][0]["fillFee"], "-0.0040126", "the capture states a negative charge");
    let events = vike_okx::event_mapper::map_okx_perp(&frame, "okx", "BTC-USDT-SWAP", 0.01);
    let fill = events
        .iter()
        .find_map(|e| match e {
            Event::Fill(f) => Some(f),
            _ => None,
        })
        .expect("the capture is a fill");
    assert_eq!(fill.commission, 0.0040126, "negated: a charge is positive in the ledger");
    assert_eq!(fill.commission_asset.as_str(), "USDT");
    let (maker, taker) = vike_venues::fee_schedule_for("okx").maker_taker_rates();
    let notional = fill.last_qty * fill.last_px;
    assert!(
        (0.5 * maker * notional..=1.5 * taker * notional).contains(&fill.commission),
        "fee {} is outside the table row's band for {notional}",
        fill.commission
    );
    assert!(
        (fill.commission / notional / taker - 1.0).abs() < 0.01,
        "a taker fill is charged the taker rate: {} of {notional}",
        fill.commission
    );
}

/// A `fillFee` that is a word: the fill is still folded (fee `0.0`) and the field is counted.
#[test]
fn a_fee_that_is_not_a_number_is_counted() {
    let before = malformed_fee_count();
    let fill = commission_of(&map_okx_order(&row(json!("oops")), "okx", "BTC-USDT"));
    assert_eq!(fill, 0.0, "the fill still books, fee-free");
    assert!(malformed_fee_count() > before, "the unreadable `fillFee` must be counted");
}
