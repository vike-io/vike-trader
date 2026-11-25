//! **The bybit TAKE-PROFIT wire on a derivative book** (offline: no network).
//!
//! * a `take_profit` is ONE conditional on `/v5/order/create`, firing the OTHER way from a stop of its side: a
//!   BUY on a FALL (`triggerDirection 2`), a SELL on a RISE (`1`);
//! * with a `price` it is a limit-if-touched: a conditional `Limit` carrying the limit's `timeInForce` and
//!   `price`, keyed exactly as `stop_limit.rs` pins the stop-limit's;
//! * without one it is a market-if-touched: a conditional `Market` with no `timeInForce` and no `price`;
//! * a requested trigger source is the venue's word for it, and the real submit door sends the builder's bytes.
//!
//! The venue's echo of what this builds is `tests/bybit_smokes/bybit_take_profit_smoke.rs`'s (passed on the demo
//! 2026-11-08).

use serde_json::{Value, json};
use vike_bridge_core::rest::VenueRest;
use vike_bybit::execution::rest::PATH_ORDER_CREATE;
use vike_bybit::history::Category;
use vike_model::events::Event;
use vike_model::{OrderRequest, OrderType, TriggerBy};

use super::stop_limit::{client, owned};

fn take_profit(side: i32, trigger: f64, limit: Option<f64>) -> OrderRequest {
    OrderRequest {
        order_client_id: "tp1".into(),
        symbol: "BTCUSDT".into(),
        side,
        qty: 0.002,
        order_type: OrderType::TakeProfit,
        price: limit,
        trigger_price: Some(trigger),
        ..Default::default()
    }
}

/// The derivative params of a take-profit, in the order `build_order_params` writes them.
fn expected(
    side: &str,
    limit: Option<&str>,
    trigger: &str,
    direction: i64,
) -> Vec<(String, Value)> {
    let mut params = vec![
        ("category", json!("linear")),
        ("symbol", json!("BTCUSDT")),
        ("side", json!(side)),
        ("orderType", json!(if limit.is_some() { "Limit" } else { "Market" })),
        ("qty", json!("0.002")),
        ("orderLinkId", json!("tp1")),
        ("positionIdx", json!(0)),
        ("reduceOnly", json!(false)),
    ];
    if let Some(limit) = limit {
        params.push(("timeInForce", json!("GTC")));
        params.push(("price", json!(limit)));
    }
    params.push(("triggerPrice", json!(trigger)));
    params.push(("triggerDirection", json!(direction)));
    params.push(("triggerBy", json!("LastPrice")));
    owned(params)
}

#[test]
fn a_limit_if_touched_is_a_conditional_limit_firing_against_a_stops_direction() {
    let c = client("BTCUSDT", Category::Linear);
    let buy = owned(c.build_order_params(&take_profit(1, 58000.3, Some(57990.1))));
    assert_eq!(
        buy,
        expected("Buy", Some("57990.1"), "58000.3", 2),
        "a BUY take-profit fires on a fall"
    );
    let sell = owned(c.build_order_params(&take_profit(-1, 62000.7, Some(62010.9))));
    assert_eq!(
        sell,
        expected("Sell", Some("62010.9"), "62000.7", 1),
        "a SELL take-profit on a rise"
    );
}

#[test]
fn a_market_if_touched_is_a_conditional_market_with_no_price() {
    let c = client("BTCUSDT", Category::Linear);
    let buy = owned(c.build_order_params(&take_profit(1, 58000.3, None)));
    assert_eq!(buy, expected("Buy", None, "58000.3", 2));
    let sell = owned(c.build_order_params(&take_profit(-1, 62000.7, None)));
    assert_eq!(sell, expected("Sell", None, "62000.7", 1));
}

/// The stop and the take-profit of one side fire in opposite directions: the one bit the take-profit adds.
#[test]
fn a_stop_and_a_take_profit_of_one_side_fire_opposite_ways() {
    let c = client("BTCUSDT", Category::Linear);
    let direction = |kind: OrderType, side: i32| {
        let mut request = take_profit(side, 60000.0, None);
        request.order_type = kind;
        let params = owned(c.build_order_params(&request));
        params.into_iter().find(|(k, _)| k == "triggerDirection").map(|(_, v)| v)
    };
    for side in [1, -1] {
        let stop = direction(OrderType::Stop, side).expect("a stop states a direction");
        let take = direction(OrderType::TakeProfit, side).expect("a take-profit states one");
        assert_ne!(stop, take, "side {side}");
    }
    assert_eq!(direction(OrderType::Stop, 1), Some(json!(1)), "a BUY stop still fires on a rise");
    assert_eq!(direction(OrderType::Stop, -1), Some(json!(2)), "a SELL stop still fires on a fall");
}

#[test]
fn a_take_profit_honours_a_requested_trigger_source() {
    let c = client("BTCUSDT", Category::Linear);
    for (by, wire) in [
        (TriggerBy::Mark, "MarkPrice"),
        (TriggerBy::Index, "IndexPrice"),
        (TriggerBy::Last, "LastPrice"),
    ] {
        let mut request = take_profit(-1, 62000.7, None);
        request.trigger_by = Some(by);
        let params = owned(c.build_order_params(&request));
        let sent = params.iter().find(|(k, _)| k == "triggerBy").map(|(_, v)| v.clone());
        assert_eq!(sent, Some(json!(wire)), "{by:?}");
    }
}

/// The real submit door sends exactly the builder's params to the create endpoint and reads the accept.
#[test]
fn the_submit_door_sends_the_take_profit_and_reads_the_accept() {
    let c = client("BTCUSDT", Category::Linear);
    let events = c.submit_order(&take_profit(1, 58000.3, Some(57990.1)));
    let Some(Event::OrderAccepted(accepted)) = events.last() else { panic!("{events:?}") };
    assert_eq!(accepted.venue_order_id.as_deref(), Some("oid-1"));
    let calls = c.transport.0.borrow();
    let [(path, params)] = calls.as_slice() else { panic!("one signed call: {calls:?}") };
    assert_eq!(path, PATH_ORDER_CREATE);
    assert_eq!(*params, expected("Buy", Some("57990.1"), "58000.3", 2));
}
