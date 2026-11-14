//! An order's death by a fill (`OrderEventKind::Filled`) reaches its strategy ONCE, whichever way the
//! venue's two frames arrive.
//!
//! The core's applied-fill delivery (`crates/vike-core/src/runtime/strategy_drive/order_events.rs`'s
//! `dispatch_applied_fills`, run after every message) tells the death with the money when the registry
//! is terminal by then: the bare fill and its `OrderFilled` wrap folded in ONE message. A venue that
//! sends the bare fill in an EARLIER message (binance's early trade frame, then its order frame) left
//! nothing to carry the death; the wrap that ends the order now says it, on the order-event lane.

use vike_execution::testing::{EngineBuilder, RecordingClient};
use vike_execution::{Engine, EventBusOutbox, EventHandler};
use vike_model::events::{Event, OrderAccepted, OrderFilled, OrderSubmitted};
use vike_model::{OrderEventKind, OrderRequest};

use crate::fill::fill;

const VENUE: &str = "binance";
const SYM: &str = "BTCUSDT";

/// An engine a strategy is mounted on, with `o1` (a buy of 1) resting.
fn resting() -> Engine<RecordingClient> {
    let mut e =
        EngineBuilder { venue: VENUE.into(), symbol: SYM.into(), ..Default::default() }.build();
    e.collect_applied_fills = true;
    let mut ob = EventBusOutbox::default();
    let req = OrderRequest {
        order_client_id: "o1".into(),
        venue: VENUE.into(),
        symbol: SYM.into(),
        side: 1,
        qty: 1.0,
        order_type: vike_model::OrderType::Limit,
        price: Some(100.0),
        ..Default::default()
    };
    e.submit_order(&req, 1, &mut ob);
    e.on_event(
        &Event::OrderSubmitted(OrderSubmitted { order_client_id: "o1".into(), timestamp: 1 }),
        &mut ob,
    );
    let accepted =
        OrderAccepted { order_client_id: "o1".into(), venue_order_id: None, timestamp: 1 };
    e.on_event(&Event::OrderAccepted(accepted), &mut ob);
    e.order_events.clear();
    e
}

fn deaths(e: &Engine<RecordingClient>) -> usize {
    e.order_events.iter().filter(|oe| oe.event.kind == OrderEventKind::Filled).count()
}

fn wrap() -> Event {
    let f = fill(VENUE, "o1", "t-o1", SYM, 1, 1.0, 100.0);
    Event::OrderFilled(OrderFilled { order_client_id: "o1".into(), fill: f, timestamp: 2 })
}

/// Two messages: the bare fill's money is delivered (the core drains `applied_fills` after it), then the
/// wrap ends the order and tells its death on the order-event lane, once.
#[test]
fn a_wrap_after_an_earlier_bare_fill_tells_the_orders_death_once() {
    let mut e = resting();
    let mut ob = EventBusOutbox::default();
    e.on_event(&Event::Fill(fill(VENUE, "o1", "t-o1", SYM, 1, 1.0, 100.0)), &mut ob);
    assert_eq!(e.applied_fills.len(), 1);
    e.applied_fills.clear(); // the core delivered the money after the first message
    assert_eq!(deaths(&e), 0, "the order is not finished by its money alone");
    e.on_event(&wrap(), &mut ob);
    assert_eq!(e.registry["o1"].status, vike_model::OrderStatus::Filled);
    assert_eq!(deaths(&e), 1, "the wrap tells the death: {:?}", e.order_events);
    e.on_event(&wrap(), &mut ob); // a reconnect replay of the wrap
    assert_eq!(deaths(&e), 1, "once");
}

/// One message: the money is still waiting for delivery when the wrap folds, so the applied-fill
/// delivery tells the death with it and the wrap adds none (no death told twice).
#[test]
fn a_fill_and_its_wrap_in_one_message_leave_the_death_to_the_money() {
    let mut e = resting();
    let mut ob = EventBusOutbox::default();
    e.on_event(&Event::Fill(fill(VENUE, "o1", "t-o1", SYM, 1, 1.0, 100.0)), &mut ob);
    e.on_event(&wrap(), &mut ob);
    assert_eq!(e.applied_fills.len(), 1, "the money waits for delivery");
    assert_eq!(deaths(&e), 0, "{:?}", e.order_events);
}
