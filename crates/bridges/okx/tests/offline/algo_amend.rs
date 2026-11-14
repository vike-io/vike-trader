//! A resting STOP, STOP-LIMIT or TAKE-PROFIT on okx is amended in place on `amend-algos` (the official
//! "Amend algo order" table, <https://www.okx.com/docs-v5/en/#order-book-trading-algo-trading-post-amend-algo-order>:
//! "Amend unfilled algo orders (Support Stop order and Trigger order only ...)"), never on `amend-order`,
//! which does not reach the algo book. Pinned with the recording transport of `stop_limit.rs` and the
//! table's own documented response:
//!
//! - which words move on each kind: a stop's `new_price` is its trigger (`newSlTriggerPx`, the order
//!   price kept at `-1`); a stop-limit's `new_price` is its limit and `modify_trigger` its trigger; a
//!   take-profit's words are `newTp*`;
//! - BOTH words of the side go whenever either price moves (the table reads a 0 as "delete" and is silent
//!   on an absent one), and a size alone goes alone;
//! - the refusals before the wire (a market-if-touched has no limit; a kind that is not a trigger order)
//!   and the venue's own refusal, both the non-terminal `OrderModifyRejected`;
//! - PRESERVED: a limit order's amend is `amend-order`, as it always was.

use serde_json::{Value, json};
use vike_bridge_core::rest::VenueRest;
use vike_model::events::{Event, OrderModified};
use vike_model::{OrderRequest, OrderType};
use vike_okx::execution::rest::VenueRestOkx;

use crate::stop_limit::{Wire, refused, rest};

/// The table's documented response to an amend (`algoId`, `algoClOrdId`, `reqId`, `sCode`).
fn documented_ack() -> Value {
    json!({"code": "0", "msg": "", "data": [{
        "algoClOrdId": "algo_01", "algoId": "2510789768709120", "reqId": "po103ux",
        "sCode": "0", "sMsg": ""}]})
}

/// A resting SELL order of `kind` on the fixture's BTC-USDT-SWAP (tick 0.1, ctVal 0.01): one contract.
fn resting(kind: OrderType, limit: Option<f64>, trigger: Option<f64>) -> OrderRequest {
    OrderRequest {
        order_client_id: "sl1".into(),
        venue: "okx".into(),
        symbol: "BTC-USDT-SWAP".into(),
        side: -1,
        qty: 0.01,
        order_type: kind,
        price: limit,
        trigger_price: trigger,
        reduce_only: true,
        ..OrderRequest::default()
    }
}

fn stop() -> OrderRequest {
    resting(OrderType::Stop, None, Some(58_000.0))
}
fn stop_limit() -> OrderRequest {
    resting(OrderType::StopLimit, Some(57_900.0), Some(58_000.0))
}
fn limit_if_touched() -> OrderRequest {
    resting(OrderType::TakeProfit, Some(62_100.0), Some(62_000.0))
}
fn market_if_touched() -> OrderRequest {
    resting(OrderType::TakeProfit, None, Some(62_000.0))
}

/// The seam's two doors: `modify_order` without a trigger, `modify_trigger` with one.
fn amend(
    c: &VenueRestOkx<Wire>,
    order: &OrderRequest,
    qty: Option<f64>,
    price: Option<f64>,
    trigger: Option<f64>,
) -> Vec<Event> {
    match trigger {
        None => VenueRest::modify_order(c, order, qty, price),
        Some(t) => VenueRest::modify_trigger(c, order, qty, price, t),
    }
}

/// The one `amend-algos` call an amend made, and the one `OrderModified` it answered.
fn amended(
    order: &OrderRequest,
    qty: Option<f64>,
    price: Option<f64>,
    trigger: Option<f64>,
) -> (OrderModified, Value) {
    let c = rest(Wire::new(|tail, _| match tail {
        "amend-algos" => Ok(documented_ack()),
        other => panic!("only amend-algos may be called, got {other}"),
    }));
    let events = amend(&c, order, qty, price, trigger);
    assert_eq!(c.transport.calls(), ["POST amend-algos"], "one amend-algos and nothing else");
    match &events[..] {
        [Event::OrderModified(m)] => (m.clone(), c.transport.params(0)),
        other => panic!("one OrderModified, got {other:?}"),
    }
}

#[test]
fn a_stops_trigger_moves_on_amend_algos_with_its_market_order_price_kept() {
    let (m, body) = amended(&stop(), None, Some(57_500.06), None);
    assert_eq!(
        body,
        json!({"instId": "BTC-USDT-SWAP", "algoClOrdId": "sl1",
               "newSlTriggerPx": "57500.0", "newSlOrdPx": "-1"}),
        "a stop's new_price IS its trigger, on the tick grid; its order price stays market"
    );
    assert_eq!(m.order_client_id, "sl1");
    assert_eq!(m.venue_order_id.as_deref(), Some("2510789768709120"), "the algoId");
    assert_eq!((m.new_price, m.new_trigger_price), (Some(57_500.06), None), "as the seam asked");
}

#[test]
fn a_stop_limits_limit_moves_with_its_trigger_restated() {
    let (m, body) = amended(&stop_limit(), None, Some(57_800.0), None);
    assert_eq!(body["newSlOrdPx"], "57800.0", "the new limit");
    assert_eq!(body["newSlTriggerPx"], "58000.0", "the trigger it has, restated, never absent");
    assert_eq!(m.new_trigger_price, None);
}

#[test]
fn a_stop_limits_trigger_moves_with_its_limit_restated() {
    let (m, body) = amended(&stop_limit(), None, None, Some(58_200.0));
    assert_eq!(body["newSlTriggerPx"], "58200.0", "the new trigger");
    assert_eq!(body["newSlOrdPx"], "57900.0", "the limit it has, restated, never absent");
    assert_eq!((m.new_price, m.new_trigger_price), (None, Some(58_200.0)));

    // both at once, and a size: `newSz` in contracts
    let (m, body) = amended(&stop_limit(), Some(0.02), Some(58_100.0), Some(58_200.0));
    assert_eq!(body["newSz"], "2.00", "0.02 BTC = two contracts");
    assert_eq!(body["newSlTriggerPx"], "58200.0");
    assert_eq!(body["newSlOrdPx"], "58100.0");
    assert_eq!(
        (m.new_qty, m.new_price, m.new_trigger_price),
        (Some(0.02), Some(58_100.0), Some(58_200.0))
    );
}

#[test]
fn a_take_profit_amends_on_its_take_profit_side() {
    let (_, lit) = amended(&limit_if_touched(), None, None, Some(61_900.0));
    assert_eq!(
        lit,
        json!({"instId": "BTC-USDT-SWAP", "algoClOrdId": "sl1",
               "newTpTriggerPx": "61900.0", "newTpOrdPx": "62100.0"}),
        "the trigger moves, the limit restated; no stop-loss word"
    );
    let (_, lit_limit) = amended(&limit_if_touched(), None, Some(62_200.0), None);
    assert_eq!(lit_limit["newTpTriggerPx"], "62000.0");
    assert_eq!(lit_limit["newTpOrdPx"], "62200.0");
    let (_, mit) = amended(&market_if_touched(), None, None, Some(61_900.0));
    assert_eq!(mit["newTpTriggerPx"], "61900.0");
    assert_eq!(mit["newTpOrdPx"], "-1", "a market-if-touched stays market");
}

#[test]
fn a_size_alone_sends_new_sz_alone() {
    let (m, body) = amended(&stop_limit(), Some(0.03), None, None);
    assert_eq!(body, json!({"instId": "BTC-USDT-SWAP", "algoClOrdId": "sl1", "newSz": "3.00"}));
    assert_eq!(m.new_qty, Some(0.03));
}

/// Refused in words before the wire, the non-terminal advisory: nothing is signed.
#[test]
fn an_amend_that_cannot_be_written_is_refused_before_the_wire() {
    let c = rest(Wire::none());
    let refusal = |order: &OrderRequest, price: Option<f64>, trigger: Option<f64>| match &amend(
        &c, order, None, price, trigger,
    )[..]
    {
        [Event::OrderModifyRejected(r)] => r.reason.to_string(),
        other => panic!("one OrderModifyRejected, got {other:?}"),
    };
    let mit_limit = refusal(&market_if_touched(), Some(62_100.0), None);
    assert!(mit_limit.contains("market-if-touched"), "{mit_limit}");
    let zero = refusal(&stop_limit(), None, Some(0.0));
    assert!(zero.contains("trigger price"), "{zero}");
    let limit = resting(OrderType::Limit, Some(57_000.0), None);
    let not_trigger = refusal(&limit, None, Some(58_000.0));
    assert!(not_trigger.contains("a limit order is not one"), "{not_trigger}");
    assert!(c.transport.calls().is_empty(), "nothing was sent");
}

/// The venue's refusal is the non-terminal advisory in its words; a status-unknown code says the amend
/// may have been applied.
#[test]
fn the_venues_refusal_of_an_amend_is_the_non_terminal_advisory() {
    for (code, word) in [("51603", "venue words"), ("50004", "outcome unknown")] {
        let c = rest(Wire::new(move |tail, _| match tail {
            "amend-algos" => Ok(refused(code)),
            other => panic!("unexpected {other}"),
        }));
        match &amend(&c, &stop_limit(), None, None, Some(58_200.0))[..] {
            [Event::OrderModifyRejected(r)] => {
                assert!(r.reason.contains(word), "{code}: {}", r.reason);
            }
            other => panic!("{code}: one OrderModifyRejected, got {other:?}"),
        }
    }
}

/// PRESERVED: a resting LIMIT order's amend is `amend-order` by `clOrdId`, byte for byte as before.
#[test]
fn a_limit_orders_amend_is_amend_order_as_it_always_was() {
    let c = rest(Wire::new(|tail, _| match tail {
        "amend-order" => {
            Ok(json!({"code": "0", "msg": "", "data": [{"ordId": "o-1", "sCode": "0"}]}))
        }
        other => panic!("only amend-order may be called, got {other}"),
    }));
    let limit = resting(OrderType::Limit, Some(57_000.0), None);
    let events = amend(&c, &limit, None, Some(57_100.0), None);
    assert_eq!(c.transport.calls(), ["POST amend-order"]);
    assert_eq!(
        c.transport.params(0),
        json!({"instId": "BTC-USDT-SWAP", "clOrdId": "sl1", "newPx": "57100.0"})
    );
    assert!(matches!(events[..], [Event::OrderModified(_)]), "{events:?}");
}
