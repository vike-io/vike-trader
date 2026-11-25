//! `Event::order_client_id`, its `_mut` twin and `Event::closed_order`, over ONE EVENT OF EVERY
//! VARIANT: each order-naming event names `o-<its wire tag>`, so a method that answered another
//! variant's id, or none where one is named, goes red here.

use super::*;

/// The fields a fill states, without its order (serde's defaults fill the rest).
const FILL: &str = r#""trade_id":"t1","venue":"binance","symbol":"BTCUSDT","side":1,"last_qty":1.0,"last_price":100.0"#;

/// The position fields a position or funding event states.
const POSITION: &str = r#""venue":"binance","symbol":"BTCUSDT","position_side":"BOTH""#;

/// One event of every variant, read off the wire, with the order client id it names (`None`: it
/// names none).
fn one_of_each() -> Vec<(Event, Option<String>)> {
    let named = |tag: &str, rest: &str| {
        let id = format!("o-{tag}");
        let json = format!(r#"{{"type":"{tag}","order_client_id":"{id}"{rest}}}"#);
        (serde_json::from_str::<Event>(&json).expect(tag), Some(id))
    };
    let unnamed = |tag: &str, rest: &str| {
        let json = format!(r#"{{"type":"{tag}",{rest}}}"#);
        (serde_json::from_str::<Event>(&json).expect(tag), None)
    };
    let wrapped_fill = format!(r#","fill":{{"order_client_id":"o-wrapped",{FILL}}}"#);
    vec![
        named("FillEvent", &format!(",{FILL}")),
        named("OrderSubmitted", ""),
        named("OrderAccepted", ""),
        named("OrderRejected", ""),
        named("OrderDenied", ""),
        named("OrderTriggered", ""),
        named("OrderPartiallyFilled", &wrapped_fill),
        named("OrderFilled", &wrapped_fill),
        named("OrderCanceled", ""),
        named("OrderExpired", ""),
        named("OrderLiquidated", ""),
        named("OrderModified", ""),
        unnamed("PositionOpened", &format!(r#"{POSITION},"qty":1.0,"average_price":100.0"#)),
        unnamed("PositionChanged", &format!(r#"{POSITION},"qty":1.0,"average_price":100.0"#)),
        unnamed("PositionClosed", POSITION),
        unnamed("AccountState", r#""venue":"binance""#),
        unnamed("FundingEvent", &format!(r#"{POSITION},"funding_rate":0.0001,"amount":-0.5"#)),
        unnamed("PositionLiquidated", &format!(r#"{POSITION},"qty":1.0,"liquidation_price":90.0"#)),
        named("OrderCancelRejected", ""),
        named("OrderModifyRejected", ""),
        named(
            "FeeAdjustmentEvent",
            r#","trade_id":"t1","venue":"binance","symbol":"BTCUSDT","last_price":100.0,"extra_commission":0.1"#,
        ),
    ]
}

/// The sample is the whole roster, one each.
#[test]
fn the_sample_holds_every_variant_once() {
    let tags: Vec<&str> = one_of_each().iter().map(|(e, _)| e.wire_tag()).collect();
    let table: Vec<&str> = Event::TABLE.iter().map(|r| r.wire_tag).collect();
    assert_eq!(tags, table);
}

/// Every order-naming variant answers ITS id, every other variant `None`, and the mutable twin
/// rewrites exactly the id the reader answers.
#[test]
fn every_variant_names_its_own_order_or_none() {
    for (mut event, named) in one_of_each() {
        let tag = event.wire_tag();
        assert_eq!(event.order_client_id(), named.as_deref(), "{tag}");
        match event.order_client_id_mut() {
            Some(id) => *id = "renamed".to_string(),
            None => assert_eq!(named, None, "{tag}: the mutable twin missed the id"),
        }
        let expected = named.as_ref().map(|_| "renamed");
        assert_eq!(event.order_client_id(), expected, "{tag}: renamed in place");
    }
}

/// The events that close an order: the five `Event::ended_order` answers (a denial among them) and a
/// liquidation; the partial fill, the accept and the advisories close nothing.
#[test]
fn an_order_is_closed_by_its_ending_events_and_a_liquidation() {
    let closing = [
        "OrderRejected",
        "OrderDenied",
        "OrderFilled",
        "OrderCanceled",
        "OrderExpired",
        "OrderLiquidated",
    ];
    for (event, named) in one_of_each() {
        let tag = event.wire_tag();
        let expected = if closing.contains(&tag) { named.as_deref() } else { None };
        assert_eq!(event.closed_order(), expected, "{tag}");
        if tag != "OrderLiquidated" {
            assert_eq!(event.closed_order(), event.ended_order(), "{tag}");
        }
    }
}
