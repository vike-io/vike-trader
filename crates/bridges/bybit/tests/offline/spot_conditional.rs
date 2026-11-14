//! **The bybit SPOT conditional order: what is built, what stays refused** (offline: no network).
//!
//! The spot book takes a conditional as `orderFilter=StopOrder` plus a `triggerPrice`; `triggerDirection` and
//! `triggerBy` are derivative words ("valid for `linear` & `inverse`",
//! https://bybit-exchange.github.io/docs/v5/order/create-order).
//!
//! * a spot `stop_limit` and a spot `take_profit` with a `price` are a conditional `Limit` (the limit's
//!   `timeInForce` and `price`), a spot `take_profit` with none a conditional `Market` carrying the spot market
//!   order's `marketUnit baseCoin`; each ends `triggerPrice`, `orderFilter StopOrder`, and states no direction,
//!   no source, no position index and no reduce-only word;
//! * a spot `stop` stays refused, in its own words, and a spot conditional asked for a mark or index trigger is
//!   refused by name: on the single and the batch door, nothing signed.
//!
//! The venue's echo of what this builds is `tests/bybit_smokes/bybit_spot_conditional_smoke.rs`'s (passed on the
//! demo 2026-11-08).

use serde_json::{Value, json};
use vike_bridge_core::rest::VenueRest;
use vike_bybit::execution::rest::PATH_ORDER_CREATE;
use vike_bybit::history::Category;
use vike_model::events::Event;
use vike_model::{OrderRequest, OrderType, TriggerBy};

use super::stop_limit::{client, owned, rejected};

fn conditional(kind: OrderType, side: i32, trigger: f64, limit: Option<f64>) -> OrderRequest {
    OrderRequest {
        order_client_id: "sc1".into(),
        symbol: "BTCUSDT.S".into(),
        side,
        qty: 0.002,
        order_type: kind,
        price: limit,
        trigger_price: Some(trigger),
        ..Default::default()
    }
}

/// The spot params of a conditional, in the order `build_order_params` writes them.
fn expected(side: &str, limit: Option<&str>, trigger: &str) -> Vec<(String, Value)> {
    let mut params = vec![
        ("category", json!("spot")),
        ("symbol", json!("BTCUSDT")),
        ("side", json!(side)),
        ("orderType", json!(if limit.is_some() { "Limit" } else { "Market" })),
        ("qty", json!("0.002")),
        ("orderLinkId", json!("sc1")),
    ];
    match limit {
        Some(limit) => {
            params.push(("timeInForce", json!("GTC")));
            params.push(("price", json!(limit)));
        }
        None => params.push(("marketUnit", json!("baseCoin"))),
    }
    params.push(("triggerPrice", json!(trigger)));
    params.push(("orderFilter", json!("StopOrder")));
    owned(params)
}

#[test]
fn a_spot_stop_limit_and_both_take_profits_are_one_stop_order_conditional() {
    let c = client("BTCUSDT", Category::Spot);
    let cases = [
        (
            OrderType::StopLimit,
            -1,
            58000.3,
            Some(57990.1),
            expected("Sell", Some("57990.1"), "58000.3"),
        ),
        (
            OrderType::StopLimit,
            1,
            62000.7,
            Some(62010.9),
            expected("Buy", Some("62010.9"), "62000.7"),
        ),
        (
            OrderType::TakeProfit,
            1,
            58000.3,
            Some(57990.1),
            expected("Buy", Some("57990.1"), "58000.3"),
        ),
        (OrderType::TakeProfit, -1, 62000.7, None, expected("Sell", None, "62000.7")),
        (OrderType::TakeProfit, 1, 58000.3, None, expected("Buy", None, "58000.3")),
    ];
    for (kind, side, trigger, limit, want) in cases {
        let mut request = conditional(kind.clone(), side, trigger, limit);
        // a reduce-only spot order needs no word (a sale cannot exceed the coin held), as for a limit
        request.reduce_only = true;
        let sent = owned(c.build_order_params(&request));
        assert_eq!(sent, want, "{kind} side {side}");
        // a requested LAST trigger is the venue's own source: nothing extra is sent
        request.trigger_by = Some(TriggerBy::Last);
        assert_eq!(owned(c.build_order_params(&request)), want, "{kind} side {side}, Last");
    }
}

/// The real submit door sends exactly the builder's params and reads the accept.
#[test]
fn the_submit_door_sends_the_spot_conditional_and_reads_the_accept() {
    let c = client("BTCUSDT", Category::Spot);
    let events = c.submit_order(&conditional(OrderType::StopLimit, -1, 58000.3, Some(57990.1)));
    let Some(Event::OrderAccepted(accepted)) = events.last() else { panic!("{events:?}") };
    assert_eq!(accepted.venue_order_id.as_deref(), Some("oid-1"));
    let calls = c.transport.0.borrow();
    let [(path, params)] = calls.as_slice() else { panic!("one signed call: {calls:?}") };
    assert_eq!(path, PATH_ORDER_CREATE);
    assert_eq!(*params, expected("Sell", Some("57990.1"), "58000.3"));
}

#[test]
fn a_spot_stop_keeps_its_refusal_in_its_own_words() {
    let spot = client("DOGEUSDT", Category::Spot);
    let mut stop = conditional(OrderType::Stop, -1, 0.05, None);
    stop.symbol = "DOGEUSDT.S".into();
    assert_eq!(
        rejected(&spot.submit_order(&stop)),
        "bybit: a stop order on the spot pair DOGEUSDT is not built (the spot book's conditional order is \
         another wire than the derivatives'); nothing was sent. Rest a limit order, or trade the perpetual \
         for a protective stop"
    );
    let events = spot.submit_batch(&[stop]);
    assert!(events.iter().any(|e| matches!(e, Event::OrderRejected(_))), "{events:?}");
    assert!(spot.transport.0.borrow().is_empty(), "nothing is signed");
}

/// The spot wire names no trigger source, so a mark or index trigger would silently become the venue's last
/// price: refused by name instead.
#[test]
fn a_spot_conditional_on_a_mark_or_index_trigger_is_refused_by_name() {
    let spot = client("BTCUSDT", Category::Spot);
    for by in [TriggerBy::Mark, TriggerBy::Index] {
        for kind in [OrderType::StopLimit, OrderType::TakeProfit] {
            let mut request = conditional(kind.clone(), 1, 58000.3, Some(57990.1));
            request.trigger_by = Some(by);
            let reason = rejected(&spot.submit_order(&request));
            assert_eq!(
                reason,
                format!(
                    "bybit: a {kind} order on the spot pair BTCUSDT triggers on the last price only (the spot \
                     conditional order names no trigger source); a {by:?} price trigger is not built, nothing \
                     was sent"
                )
            );
            let events = spot.submit_batch(&[request]);
            assert!(events.iter().any(|e| matches!(e, Event::OrderRejected(_))), "{events:?}");
        }
    }
    assert!(spot.transport.0.borrow().is_empty(), "nothing is signed");

    // a derivative book's mark trigger is honoured, as it always was
    let linear = client("BTCUSDT", Category::Linear);
    let mut request = conditional(OrderType::StopLimit, 1, 58000.3, Some(57990.1));
    request.trigger_by = Some(TriggerBy::Mark);
    assert!(matches!(linear.submit_order(&request).last(), Some(Event::OrderAccepted(_))));
}
