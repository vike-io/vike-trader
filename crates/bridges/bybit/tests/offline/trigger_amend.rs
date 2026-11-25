//! **The bybit TRIGGER amend** (offline: no network): ONE `/v5/order/amend` that carries `triggerPrice`.
//!
//! * a stop-limit's and a take-profit's trigger (`VenueRest::modify_trigger`, the seam's door for a kind whose
//!   `new_price` is its limit) is the amend's `triggerPrice` on the tick, beside its `qty` and limit `price` when
//!   asked, on a derivative book and on a spot pair (the page gives both);
//! * a STOP's trigger rides the seam's `new_price` (`vike_model::modified_price_is_trigger`), and its native
//!   modify sends it as the amend's `triggerPrice` (`bybit_trigger_amend_smoke`, demo, 2026-11-08), never `price`;
//! * the `OrderModified` echoes the trigger, and folding it moves the order's trigger and nothing else;
//! * every other amend keeps its bytes: no `triggerPrice` key, the echo's trigger `None`.

use serde_json::{Value, json};
use vike_bridge_core::rest::VenueRest;
use vike_bybit::execution::rest::PATH_ORDER_AMEND;
use vike_bybit::history::Category;
use vike_model::events::Event;
use vike_model::{OrderRecord, OrderRequest, OrderStatus, OrderType};

use super::stop_limit::client;

fn order(kind: OrderType, price: Option<f64>, trigger: f64) -> OrderRequest {
    OrderRequest {
        order_client_id: "ta1".into(),
        symbol: "BTCUSDT".into(),
        side: -1,
        qty: 0.002,
        order_type: kind,
        price,
        trigger_price: Some(trigger),
        ..Default::default()
    }
}

/// `request` resting at the venue (accepted), so an amend folds on it.
fn resting(request: OrderRequest) -> OrderRecord {
    let mut managed = OrderRecord::new(request);
    managed.status = OrderStatus::Accepted;
    managed
}

/// The one signed call's path and params.
fn the_call(
    c: &vike_bybit::execution::rest::VenueRestBybit<super::stop_limit::Wire>,
) -> (String, Vec<(String, Value)>) {
    let calls = c.transport.0.borrow();
    let [call] = calls.as_slice() else { panic!("one signed call: {calls:?}") };
    call.clone()
}

fn amend_params(category: &str, extra: &[(&str, Value)]) -> Vec<(String, Value)> {
    let mut params = vec![
        ("category".to_string(), json!(category)),
        ("symbol".to_string(), json!("BTCUSDT")),
        ("orderLinkId".to_string(), json!("ta1")),
    ];
    params.extend(extra.iter().map(|(k, v)| (k.to_string(), v.clone())));
    params
}

#[test]
fn a_stop_limit_and_a_take_profit_move_their_trigger_in_one_amend() {
    for (category, book) in [(Category::Linear, "linear"), (Category::Spot, "spot")] {
        for (kind, limit) in [
            (OrderType::StopLimit, Some(57990.1)),
            (OrderType::TakeProfit, Some(62010.9)),
            (OrderType::TakeProfit, None),
        ] {
            let c = client("BTCUSDT", category);
            let placed = order(kind.clone(), limit, 58000.3);
            // the trigger alone
            let events = c.modify_trigger(&placed, None, None, 58100.37);
            let [Event::OrderModified(m)] = events.as_slice() else { panic!("{events:?}") };
            assert_eq!((m.new_qty, m.new_price, m.new_trigger_price), (None, None, Some(58100.37)));
            let (path, params) = the_call(&c);
            assert_eq!(path, PATH_ORDER_AMEND);
            assert_eq!(
                params,
                amend_params(book, &[("triggerPrice", json!("58100.3"))]),
                "{book} {kind}"
            );

            // the trigger, the size and the limit, in one call
            let c = client("BTCUSDT", category);
            c.modify_trigger(&placed, Some(0.0031), Some(57980.26), 58100.37);
            let (_, params) = the_call(&c);
            assert_eq!(
                params,
                amend_params(
                    book,
                    &[
                        ("qty", json!("0.003")),
                        ("price", json!("57980.2")),
                        ("triggerPrice", json!("58100.3")),
                    ]
                ),
                "{book} {kind}"
            );
        }
    }
}

/// A STOP's native modify sends its `new_price` (its trigger) as the amend's `triggerPrice`, the very bytes
/// `VenueRestBybit::amend` sent for a stop on the demo host (`bybit_trigger_amend_smoke`, 2026-11-08), with its
/// `qty` beside it when asked and never a `price` (a stop with a limit would be a stop-limit).
#[test]
fn a_stops_native_modify_moves_its_trigger_price() {
    let c = client("BTCUSDT", Category::Linear);
    let stop = order(OrderType::Stop, None, 58000.3);
    // the trait door (the inherent `modify_order` takes an order client id)
    let events = VenueRest::modify_order(&c, &stop, None, Some(58100.37));
    let [Event::OrderModified(m)] = events.as_slice() else { panic!("{events:?}") };
    assert_eq!((m.new_qty, m.new_price, m.new_trigger_price), (None, None, Some(58100.37)));
    let (path, params) = the_call(&c);
    assert_eq!(path, PATH_ORDER_AMEND);
    assert_eq!(params, amend_params("linear", &[("triggerPrice", json!("58100.3"))]));

    let measured = client("BTCUSDT", Category::Linear);
    measured.amend("ta1", None, None, Some(58100.37));
    assert_eq!(the_call(&measured), the_call(&c), "the smoke's wire, byte for byte");

    let mut managed = resting(stop.clone());
    managed.apply(&Event::OrderModified(m.clone())).expect("a live order takes the amend");
    assert_eq!(managed.request.trigger_price, Some(58100.37), "the echo moves the stop's trigger");
    assert_eq!(managed.request.price, None, "and gives it no limit");

    let c = client("BTCUSDT", Category::Linear);
    VenueRest::modify_order(&c, &stop, Some(0.0031), Some(58100.37));
    let (_, params) = the_call(&c);
    assert_eq!(
        params,
        amend_params("linear", &[("qty", json!("0.003")), ("triggerPrice", json!("58100.3"))])
    );
}

/// Every other kind's plain amend keeps its bytes (its `new_price` is its `price`): no `triggerPrice`, no
/// trigger in the echo; on a derivative book and on a spot pair.
#[test]
fn an_amend_with_no_trigger_keeps_its_bytes() {
    for (category, book) in [(Category::Linear, "linear"), (Category::Spot, "spot")] {
        for placed in [
            order(OrderType::Limit, Some(57990.1), 58000.3),
            order(OrderType::StopLimit, Some(57990.1), 58000.3),
            order(OrderType::TakeProfit, Some(57990.1), 58000.3),
        ] {
            let c = client("BTCUSDT", category);
            let events = VenueRest::modify_order(&c, &placed, Some(0.0031), Some(57980.26));
            let [Event::OrderModified(m)] = events.as_slice() else { panic!("{events:?}") };
            assert_eq!((m.new_price, m.new_trigger_price), (Some(57980.26), None));
            let (_, params) = the_call(&c);
            assert_eq!(
                params,
                amend_params(book, &[("qty", json!("0.003")), ("price", json!("57980.2"))]),
                "{book} {}",
                placed.order_type
            );
        }
    }
}

/// The echo of a trigger amend, folded on the resting order, moves the trigger and keeps the limit.
#[test]
fn a_folded_trigger_amend_moves_the_trigger_and_keeps_the_limit() {
    let c = client("BTCUSDT", Category::Linear);
    let request = order(OrderType::StopLimit, Some(57990.1), 58000.3);
    let events = c.modify_trigger(&request, None, None, 58100.4);
    let [Event::OrderModified(m)] = events.as_slice() else { panic!("{events:?}") };
    let mut managed = resting(request);
    managed.apply(&Event::OrderModified(m.clone())).expect("a live order takes the amend");
    assert_eq!(managed.request.trigger_price, Some(58100.4));
    assert_eq!(managed.request.price, Some(57990.1));
}
