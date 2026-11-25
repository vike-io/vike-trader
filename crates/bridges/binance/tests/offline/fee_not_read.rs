//! A fee field the venue sent and this decoder cannot read is COUNTED, not silently zero.
//!
//! `vike_bridge_core::json::get_f64` turns anything unreadable into `0.0`, and a binance fill's
//! `n` rode it: a venue that changed the field's shape (a word, a bool, an object) booked every fill
//! fee-free with no line anywhere (FEES-AUDIT 2026-10-26 §4.8 e). `get_fee_f64` keeps the booking
//! (`0.0`: a fill is never dropped for its fee) and adds the count and the line.
//!
//! The counter is process-wide and these tests share a process under plain `cargo test`, so each one
//! asserts "at least one more than before", never an exact delta, and all three hold [`COUNTER`]: the
//! control's "no change" read raced its two siblings' increments (a lane run read 2 for 0).

use std::sync::{Mutex, MutexGuard, PoisonError};

use crate::fills::bare_fill;
use serde_json::{Value, json};
use vike_binance::execution::event_mapper::perp::map_binance_perp;
use vike_binance::execution::event_mapper::spot::map_binance_private;
use vike_bridge_core::json::malformed_fee_count;

/// Serializes this file's tests over the process-wide counter.
static COUNTER: Mutex<()> = Mutex::new(());

fn hold() -> MutexGuard<'static, ()> {
    COUNTER.lock().unwrap_or_else(PoisonError::into_inner)
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
    let _held = hold();
    let before = malformed_fee_count();
    let spot =
        bare_fill(&map_binance_private(&spot_report(json!("0.25")), "binance", "BTCUSDT", None));
    assert_eq!(spot.commission, 0.25);
    let perp = bare_fill(&map_binance_perp(&perp_update(json!("0.5")), "binance", "BTCUSDT", None));
    assert_eq!(perp.commission, 0.5);
    assert_eq!(malformed_fee_count(), before, "nothing malformed was read");
}

/// A spot `n` that is a word: the fill is still folded (fee `0.0`) and the unreadable field is counted.
#[test]
fn a_spot_fee_that_is_not_a_number_is_counted() {
    let _held = hold();
    let before = malformed_fee_count();
    let fill =
        bare_fill(&map_binance_private(&spot_report(json!("oops")), "binance", "BTCUSDT", None));
    assert_eq!(fill.commission, 0.0, "the fill still books, fee-free");
    assert!(malformed_fee_count() > before, "the unreadable `n` must be counted");
}

/// The perp twin, on the fill arm.
#[test]
fn a_perp_fee_that_is_not_a_number_is_counted() {
    let _held = hold();
    let before = malformed_fee_count();
    let fill =
        bare_fill(&map_binance_perp(&perp_update(json!({"x": 1})), "binance", "BTCUSDT", None));
    assert_eq!(fill.commission, 0.0, "the fill still books, fee-free");
    assert!(malformed_fee_count() > before, "the unreadable `n` must be counted");
}

/// One `allOrders` row and one `myTrades`/`userTrades` row whose `commission` is `commission`.
fn history_rows(commission: Value) -> (Value, Value) {
    let order =
        json!([{"orderId": 9, "clientOrderId": "fee-test", "status": "FILLED", "updateTime": 5}]);
    let trade = json!([{
        "id": 7, "orderId": 9, "symbol": "BTCUSDT", "price": "100.0", "qty": "1.0",
        "commission": commission, "commissionAsset": "USDT", "side": "SELL", "maker": false,
        "isBuyer": false, "isMaker": false, "time": 5
    }]);
    (order, trade)
}

/// The legs that read the same field off a REST row (the resync replay and the reconcile fill
/// read, spot and perp): an unreadable `commission` is counted there too (F-7). Red before the fix:
/// `get_f64` read it as `0.0` and counted nothing.
#[test]
fn a_rest_row_fee_that_is_not_a_number_is_counted_on_every_leg() {
    use vike_binance::family::execution::resync::{map_history, map_perp_history};
    use vike_binance::family::recon::{parse_perp_user_trades, parse_spot_my_trades};
    let _held = hold();
    let (orders, trades) = history_rows(json!("oops"));
    let body = trades.to_string();
    type Read<'a> = Box<dyn Fn() -> f64 + 'a>;
    let legs: [(&str, Read<'_>); 4] = [
        (
            "spot replay",
            Box::new(|| {
                bare_fill(&map_history(&orders, &trades, "binance", "BTCUSDT", None)).commission
            }),
        ),
        (
            "perp replay",
            Box::new(|| {
                bare_fill(&map_perp_history(&orders, &trades, "binance", "BTCUSDT.P", None))
                    .commission
            }),
        ),
        (
            "spot reconcile",
            Box::new(|| parse_spot_my_trades(&body, "binance").unwrap()[0].commission),
        ),
        (
            "perp reconcile",
            Box::new(|| parse_perp_user_trades(&body, "binance").unwrap()[0].commission),
        ),
    ];
    for (leg, read) in legs {
        let before = malformed_fee_count();
        assert_eq!(read(), 0.0, "{leg}: the fill still books, fee-free");
        assert!(
            malformed_fee_count() > before,
            "{leg}: the unreadable `commission` must be counted"
        );
    }
    let (orders, trades) = history_rows(json!("0.25"));
    let before = malformed_fee_count();
    assert_eq!(
        bare_fill(&map_history(&orders, &trades, "binance", "BTCUSDT", None)).commission,
        0.25
    );
    assert_eq!(parse_spot_my_trades(&trades.to_string(), "binance").unwrap()[0].commission, 0.25);
    assert_eq!(malformed_fee_count(), before, "a readable `commission` counts nothing");
}
