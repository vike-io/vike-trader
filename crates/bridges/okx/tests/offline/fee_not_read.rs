//! A fee field the venue sent and this decoder cannot read is COUNTED, not silently zero.
//!
//! `fillFee` rode `vike_bridge_core::json::get_f64`, which turns anything unreadable into `0.0`: a
//! venue that changed the field's shape booked every fill fee-free with no line anywhere
//! (FEES-AUDIT 2026-10-26 §4.8 e). `get_fee_f64` keeps the booking (`0.0`) and adds the count and
//! the line. An EMPTY `fillFee` is not malformed: OKX sends `""` on a row that is not a fill.
//!
//! The counter is process-wide and these tests share a process under plain `cargo test`, so each one
//! asserts "at least one more than before", never an exact delta, and the "nothing" control asks again
//! when another test's malformed fee lands inside its window.

use serde_json::{Value, json};
use vike_bridge_core::execution_drive::{bare_fills, first_commission};
use vike_bridge_core::json::malformed_fee_count;
use vike_okx::execution::event_mapper::map_okx_order;

/// An `orders` row carrying a real fill whose `fillFee` is `fee`.
fn row(fee: Value) -> Value {
    json!({
        "instId": "BTC-USDT", "clOrdId": "c1", "state": "filled", "side": "buy",
        "fillSz": "1.0", "fillPx": "50000", "fillFee": fee, "fillFeeCcy": "USDT",
        "execType": "T", "accFillSz": "1.0", "sz": "1.0", "fillTime": "5", "ordId": "o9",
        "tradeId": "t1"
    })
}

/// The control: a readable `fillFee` (OKX negates it: a charge is negative on the wire) is booked
/// and moves no count, and an EMPTY one is the ordinary "no fee" and moves none either.
#[test]
fn a_readable_or_empty_fee_counts_nothing() {
    // A race with a sibling's malformed fee costs a retry, never a false red; a counted readable fee
    // moves the count on every try.
    let unmoved = (0..100).any(|_| {
        let before = malformed_fee_count();
        assert_eq!(first_commission(&map_okx_order(&row(json!("-0.1")), "okx", "BTC-USDT")), 0.1);
        assert_eq!(first_commission(&map_okx_order(&row(json!("")), "okx", "BTC-USDT")), 0.0);
        malformed_fee_count() == before
    });
    assert!(unmoved, "nothing malformed was read");
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
    let events =
        vike_okx::execution::event_mapper::map_okx_perp(&frame, "okx", "BTC-USDT-SWAP", 0.01);
    let fill = *bare_fills(&events).first().expect("the capture is a fill");
    assert_eq!(fill.commission, 0.0040126, "negated: a charge is positive in the ledger");
    assert_eq!(fill.commission_asset.as_str(), "USDT");
    let (maker, taker) = vike_venues::fee_schedule_for("okx").maker_taker_rates();
    let notional = fill.last_qty * fill.last_price;
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

/// The REST legs read `fee` off a `trade/fills` / `fills-history` row (the reconcile read and the
/// resync replay): a word there is counted as the stream's `fillFee` is (F-7). Red before the fix:
/// both read it with `get_f64`/`json_number` and counted nothing.
#[test]
fn a_rest_fill_fee_that_is_not_a_number_is_counted_on_both_legs() {
    let rows = |fee: Value| {
        json!([{
            "instId": "BTC-USDT-SWAP", "tradeId": "t1", "ordId": "o9", "clOrdId": "c1", "side": "sell",
            "fillPx": "50000", "fillSz": "1", "fee": fee, "feeCcy": "USDT", "execType": "T",
            "subType": "2", "ts": "5"
        }])
    };
    let orders = json!([{"ordId": "o9", "clOrdId": "c1", "state": "filled"}]);
    let reconcile = |fee: Value| {
        vike_okx::reconcile::parse_fills(&rows(fee).to_string(), 1.0).unwrap()[0].commission
    };
    let replay = |fee: Value| {
        let events = vike_okx::execution::resync::map_okx_history(
            &orders,
            &rows(fee),
            "okx",
            "BTC-USDT-SWAP",
            1.0,
        );
        first_commission(&events)
    };
    type Read<'a> = &'a dyn Fn(Value) -> f64;
    let legs: [(&str, Read<'_>); 2] = [("reconcile", &reconcile), ("replay", &replay)];
    for (leg, read) in legs {
        let before = malformed_fee_count();
        assert_eq!(read(json!("oops")), 0.0, "{leg}: the fill still books, fee-free");
        assert!(malformed_fee_count() > before, "{leg}: the unreadable `fee` must be counted");
        assert_eq!(read(json!("-0.1")), 0.1, "{leg}: a readable charge is negated");
    }
}

/// A `fillFee` that is a word: the fill is still folded (fee `0.0`) and the field is counted.
#[test]
fn a_fee_that_is_not_a_number_is_counted() {
    let before = malformed_fee_count();
    let fill = first_commission(&map_okx_order(&row(json!("oops")), "okx", "BTC-USDT"));
    assert_eq!(fill, 0.0, "the fill still books, fee-free");
    assert!(malformed_fee_count() > before, "the unreadable `fillFee` must be counted");
}
