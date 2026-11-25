//! An amend OKX accepted and then FAILED keeps the order's OLD terms (offline).
//!
//! `amend-order`'s `sCode` 0 is only "request accepted", and `VenueRestOkx::modify_order` folds it as an
//! `OrderModified` of the new terms. OKX documents that the amend can still fail after it: the `orders`
//! push then carries `amendResult` `-1` (the orders channel's field table). The mapper ignored the field,
//! so the engine held the new price and size while the venue kept the old ones. No failed amend has been
//! captured: the frame is the captured `live` row of `tests/fixtures/orders_channel/limit_fine_tick_live.json`
//! with the documented fields set (its `code` and `msg` are placeholders, not measured words).

use serde_json::Value;
use vike_execution::testing::{RecordingClient, fold};
use vike_execution::{Account, BalanceMode, Engine, RiskGate};
use vike_model::events::{Event, OrderModified};
use vike_model::{OrderRecord, OrderRequest, OrderStatus, OrderType, RiskLimits};
use vike_okx::execution::event_mapper::{
    AMEND_FAILED_REASON, map_okx_perp, map_okx_perp_by_instrument,
};

const LIVE: &str = include_str!("../fixtures/orders_channel/limit_fine_tick_live.json");
const ORDER_CLIENT_ID: &str = "b98mk025511b";
const SYMBOL: &str = "BTC-USDT-SWAP";
/// BTC-USDT-SWAP's `ctVal`: the row's `sz` of 0.01 contracts is 0.0001 BTC.
const CT_VAL: f64 = 0.01;
/// The terms the captured row rests on.
const OLD_PRICE: f64 = 80925.93;
const OLD_QTY: f64 = 0.0001;
/// The terms of the amend whose REST success already folded.
const NEW_PRICE: f64 = 80900.0;
const NEW_QTY: f64 = 0.0002;
const VENUE_WORDS: &str = "Order modification failed.";

/// The captured `live` row with `fields` set (`None` removes the key).
fn push(fields: &[(&str, Option<&str>)]) -> Value {
    let mut frame: Value = serde_json::from_str(LIVE).expect("the capture is JSON");
    let row = frame["data"][0].as_object_mut().expect("one row");
    for (key, value) in fields {
        match value {
            Some(v) => row.insert((*key).to_string(), Value::String((*v).to_string())),
            None => row.remove(*key),
        };
    }
    frame
}

fn failed_amend() -> Value {
    push(&[("amendResult", Some("-1")), ("code", Some("1")), ("msg", Some(VENUE_WORDS))])
}

/// An engine holding the captured order, then the amend's REST success folded (what
/// `VenueRestOkx::modify_order` returns on `sCode` 0).
fn engine_after_the_amend_ack() -> Engine<RecordingClient> {
    let mut engine = Engine::new(
        Account::new(1.0, "okx", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "okx",
        SYMBOL,
    );
    let mut order = OrderRecord::new(OrderRequest {
        order_client_id: ORDER_CLIENT_ID.to_string(),
        venue: "okx".to_string(),
        symbol: SYMBOL.to_string(),
        side: 1,
        qty: OLD_QTY,
        price: Some(OLD_PRICE),
        order_type: OrderType::Limit,
        ..Default::default()
    });
    order.status = OrderStatus::Accepted;
    engine.registry.insert(ORDER_CLIENT_ID.to_string(), order);
    fold(
        &mut engine,
        &[Event::OrderModified(OrderModified {
            order_client_id: ORDER_CLIENT_ID.to_string(),
            venue_order_id: Some("4048466779407560705".into()),
            new_qty: Some(NEW_QTY),
            new_price: Some(NEW_PRICE),
            new_trigger_price: None,
            timestamp: 1_793_156_112_200,
        })],
    );
    engine
}

fn terms(engine: &Engine<RecordingClient>) -> (OrderStatus, f64, Option<f64>) {
    let order = &engine.registry[ORDER_CLIENT_ID];
    (order.status, order.request.qty, order.request.price)
}

#[test]
fn a_failed_amend_puts_the_old_terms_back_and_says_why_in_the_venues_words() {
    let events = map_okx_perp(&failed_amend(), "okx", SYMBOL, CT_VAL);
    let [Event::OrderModified(restore), Event::OrderModifyRejected(refusal)] = events.as_slice()
    else {
        panic!("a restore of the venue's terms, then the refusal: {events:?}");
    };
    assert_eq!(restore.order_client_id, ORDER_CLIENT_ID);
    assert_eq!(restore.new_price, Some(OLD_PRICE));
    let qty = restore.new_qty.expect("the row states its size");
    assert!((qty - OLD_QTY).abs() < 1e-12, "0.01 contracts x ctVal: {qty}");
    assert_eq!(restore.venue_order_id, None);
    assert_eq!(
        (refusal.order_client_id.as_str(), refusal.reason.as_str()),
        (ORDER_CLIENT_ID, VENUE_WORDS)
    );

    let mut engine = engine_after_the_amend_ack();
    assert_eq!(terms(&engine), (OrderStatus::Accepted, NEW_QTY, Some(NEW_PRICE)));
    fold(&mut engine, &events);
    let (status, qty, price) = terms(&engine);
    assert_eq!(
        (status, price),
        (OrderStatus::Accepted, Some(OLD_PRICE)),
        "still live, at the old price"
    );
    assert!((qty - OLD_QTY).abs() < 1e-12, "the old size: {qty}");
    assert_eq!(engine.rejection_reason(ORDER_CLIENT_ID), Some(VENUE_WORDS));
}

#[test]
fn a_failed_amend_with_no_message_is_refused_in_our_words() {
    let events = map_okx_perp(&push(&[("amendResult", Some("-1"))]), "okx", SYMBOL, CT_VAL);
    let Some(Event::OrderModifyRejected(refusal)) = events.last() else {
        panic!("the refusal goes last: {events:?}");
    };
    assert_eq!(refusal.reason.as_str(), AMEND_FAILED_REASON);
}

/// A size whose `ctVal` cannot be read is left out of the restore; the price and the refusal still go.
#[test]
fn a_failed_amend_of_an_unsized_instrument_restores_the_price_alone() {
    let events = map_okx_perp_by_instrument(&failed_amend(), "okx", SYMBOL, |_| None);
    let [Event::OrderModified(restore), Event::OrderModifyRejected(_)] = events.as_slice() else {
        panic!("a restore, then the refusal: {events:?}");
    };
    assert_eq!((restore.new_qty, restore.new_price), (None, Some(OLD_PRICE)));
}

/// The fast path: an absent, empty or `0` `amendResult` (a successful amend's push states the NEW terms)
/// maps exactly as the captured row does, and the terms the REST success folded stand.
#[test]
fn an_amend_result_that_is_absent_empty_or_zero_maps_as_it_always_did() {
    let captured = map_okx_perp(&push(&[]), "okx", SYMBOL, CT_VAL);
    assert!(matches!(captured.as_slice(), [Event::OrderAccepted(_)]), "{captured:?}");
    for amend_result in [None, Some(""), Some("0")] {
        let frame =
            push(&[("amendResult", amend_result), ("px", Some("80900")), ("sz", Some("0.02"))]);
        let events = map_okx_perp(&frame, "okx", SYMBOL, CT_VAL);
        assert_eq!(events, captured, "amendResult {amend_result:?}");
        let mut engine = engine_after_the_amend_ack();
        fold(&mut engine, &events);
        assert_eq!(terms(&engine), (OrderStatus::Accepted, NEW_QTY, Some(NEW_PRICE)));
    }
}

/// Every captured `orders` push carries `amendResult` `""`: each still decodes as it did without the field.
#[test]
fn every_captured_orders_push_decodes_as_it_did_without_the_field() {
    let captures = [
        include_str!("../fixtures/orders_channel/limit_fine_tick_live.json"),
        include_str!("../fixtures/orders_channel/market_buy_live.json"),
        include_str!("../fixtures/orders_channel/market_buy_filled.json"),
        include_str!("../fixtures/orders_channel/market_reduce_only_sell_live.json"),
        include_str!("../fixtures/algo/ws_orders_child_live.json"),
        include_str!("../fixtures/algo/ws_orders_child_filled.json"),
        include_str!("../fixtures/algo/ws_orders_child_canceled.json"),
        include_str!("../fixtures/algo/ws_orders_ordinary_filled.json"),
    ];
    for capture in captures {
        let frame: Value = serde_json::from_str(capture).expect("the capture is JSON");
        let mut without = frame.clone();
        for row in without["data"].as_array_mut().expect("rows") {
            assert_eq!(row["amendResult"], "", "the capture states the field empty");
            row.as_object_mut().expect("a row").remove("amendResult");
        }
        let events = map_okx_perp(&frame, "okx", SYMBOL, CT_VAL);
        assert_eq!(events, map_okx_perp(&without, "okx", SYMBOL, CT_VAL));
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, Event::OrderModified(_) | Event::OrderModifyRejected(_))),
            "{events:?}"
        );
    }
}
