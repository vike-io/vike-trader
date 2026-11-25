//! **A conditional limit's lifetime: sent as the row maps it, or refused by name** (offline: no network).
//!
//! A stop-limit and a limit-if-touched carry their limit's `timeInForce` from bybit's `venue_time_in_force` row. A lifetime
//! the row says `Unsupported` (`Gtd`, `Day`, `AtOpen`, `AtClose`) is refused whole on the single and the batch
//! door, nothing signed, instead of leaving with no `timeInForce` and resting GTC
//! (`crates/bridges/bybit/src/execution/rest/mod.rs`'s `conditional_time_in_force_refusal`); `Gtc`, `Ioc` and `Fok` are sent as before.
//!
//! A stop and a market-if-touched (a conditional `Market`, no lifetime on its wire) with an AUCTION lifetime
//! (`AtOpen`, `AtClose`) are refused the same way: bybit has no auction. A stop's `Gtd` and `Day` keep the path
//! they always had: sent, with no `timeInForce`.

use serde_json::json;
use vike_bridge_core::rest::VenueRest;
use vike_bybit::history::Category;
use vike_model::events::Event;
use vike_model::{OrderRequest, OrderType, TimeInForce};

use super::stop_limit::{client, rejected};

fn conditional_limit(kind: OrderType, tif: TimeInForce) -> OrderRequest {
    OrderRequest {
        order_client_id: "ct1".into(),
        symbol: "BTCUSDT".into(),
        side: -1,
        qty: 0.002,
        order_type: kind,
        price: Some(57990.1),
        trigger_price: Some(58000.3),
        time_in_force: tif,
        ..Default::default()
    }
}

#[test]
fn a_lifetime_the_conditional_limit_cannot_carry_is_refused_by_name() {
    for kind in [OrderType::StopLimit, OrderType::TakeProfit] {
        for tif in [TimeInForce::Gtd, TimeInForce::Day, TimeInForce::AtOpen, TimeInForce::AtClose] {
            let c = client("BTCUSDT", Category::Linear);
            let request = conditional_limit(kind.clone(), tif);
            assert_eq!(
                rejected(&c.submit_order(&request)),
                format!(
                    "bybit: a {kind} order with the lifetime {tif:?} is not built (its conditional limit carries \
                     no such timeInForce, and it would rest GTC); nothing was sent"
                ),
                "{kind} {tif:?}"
            );
            let events = c.submit_batch(&[request]);
            assert!(
                events.iter().any(|e| matches!(e, Event::OrderRejected(_))),
                "{kind} {tif:?}: {events:?}"
            );
            assert!(c.transport.0.borrow().is_empty(), "{kind} {tif:?}: nothing is signed");
        }
    }
}

#[test]
fn a_lifetime_the_row_maps_is_sent_as_before() {
    for kind in [OrderType::StopLimit, OrderType::TakeProfit] {
        for (tif, wire) in
            [(TimeInForce::Gtc, "GTC"), (TimeInForce::Ioc, "IOC"), (TimeInForce::Fok, "FOK")]
        {
            let c = client("BTCUSDT", Category::Linear);
            let events = c.submit_order(&conditional_limit(kind.clone(), tif));
            assert!(
                matches!(events.last(), Some(Event::OrderAccepted(_))),
                "{kind} {tif:?}: {events:?}"
            );
            let calls = c.transport.0.borrow();
            let [(_, params)] = calls.as_slice() else { panic!("one signed call: {calls:?}") };
            let sent = params.iter().find(|(k, _)| k == "timeInForce").map(|(_, v)| v.clone());
            assert_eq!(sent, Some(json!(wire)), "{kind} {tif:?}");
        }
    }
}

/// A stop or a market-if-touched: the same request with no `price`.
fn conditional_market(kind: OrderType, tif: TimeInForce) -> OrderRequest {
    OrderRequest { price: None, ..conditional_limit(kind, tif) }
}

#[test]
fn an_auction_lifetime_on_a_stop_or_a_market_if_touched_is_refused_by_name() {
    for kind in [OrderType::Stop, OrderType::TakeProfit] {
        for tif in [TimeInForce::AtOpen, TimeInForce::AtClose] {
            let c = client("BTCUSDT", Category::Linear);
            let request = conditional_market(kind.clone(), tif);
            assert_eq!(
                rejected(&c.submit_order(&request)),
                format!(
                    "bybit: a {kind} order with the lifetime {tif:?} is not built (bybit has no auction, and it \
                     would fire as a plain conditional); nothing was sent"
                ),
                "{kind} {tif:?}"
            );
            let events = c.submit_batch(&[request]);
            assert!(
                events.iter().any(|e| matches!(e, Event::OrderRejected(_))),
                "{kind} {tif:?}: {events:?}"
            );
            assert!(c.transport.0.borrow().is_empty(), "{kind} {tif:?}: nothing is signed");
        }
    }
}

/// A stop's `Gtd` and `Day` keep the path they had before the auction lifetimes existed: sent, no `timeInForce`.
#[test]
fn a_stops_gtd_and_day_are_sent_as_before() {
    for tif in [TimeInForce::Gtd, TimeInForce::Day] {
        let c = client("BTCUSDT", Category::Linear);
        let events = c.submit_order(&conditional_market(OrderType::Stop, tif));
        assert!(matches!(events.last(), Some(Event::OrderAccepted(_))), "{tif:?}: {events:?}");
        let calls = c.transport.0.borrow();
        let [(_, params)] = calls.as_slice() else { panic!("one signed call: {calls:?}") };
        assert!(params.iter().all(|(k, _)| k != "timeInForce"), "{tif:?}: {params:?}");
    }
}
