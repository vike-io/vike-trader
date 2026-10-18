//! A modify reaches the venue on the instrument GRID, through the same `RiskGate` normalization
//! as a submit. Before, `ExecutionEngine::modify_order` gated the projected order but forwarded the
//! RAW `new_price`/`new_qty`, so an off-grid 66714.545 went to the wire (and into the local order)
//! on a 0.01 tick (live drill 2026-10-16: binance, okx and deribit all accepted it).

use vike_exec::{EventBus, Outbox};
use vike_model::OrderRequest;
use vike_model::RiskLimits;
use vike_model::events::{Event, OrderAccepted, OrderSubmitted};

use crate::support::EngineBuilder;

fn resting(limits: RiskLimits) -> vike_exec::ExecutionEngine<vike_exec::testing::RecordingClient> {
    let mut e = EngineBuilder { limits, ..Default::default() }.build();
    let req = OrderRequest {
        client_order_id: "c1".into(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 1.0,
        order_type: vike_model::OrderType::Limit,
        price: Some(66_000.0),
        ts: 1,
        ..Default::default()
    };
    e.submit_order(&req, 0, &mut Outbox::default());
    let mut bus = EventBus::new();
    bus.publish(
        Event::OrderSubmitted(OrderSubmitted { client_order_id: "c1".into(), ts: 0 }),
        &mut e,
    );
    bus.publish(
        Event::OrderAccepted(OrderAccepted {
            client_order_id: "c1".into(),
            venue_order_id: Some("v1".into()),
            ts: 1,
        }),
        &mut e,
    );
    e
}

fn limits() -> RiskLimits {
    let mut l = RiskLimits::new();
    l.tick_size = Some(0.01);
    l.lot_size = Some(0.001);
    l
}

#[test]
fn an_off_grid_modify_price_and_qty_are_snapped_before_the_client_sees_them() {
    let mut e = resting(limits());
    e.modify_order("c1", Some(1.2344), Some(66_714.546), 2, &mut Outbox::default());

    let (_, qty, price) = e.client.modifies.pop().expect("the modify reached the client");
    assert!((price.unwrap() - 66_714.55).abs() < 1e-9, "price on the 0.01 tick, got {price:?}");
    assert!((qty.unwrap() - 1.234).abs() < 1e-9, "qty on the 0.001 lot, got {qty:?}");
}

#[test]
fn an_omitted_field_stays_omitted() {
    let mut e = resting(limits());
    e.modify_order("c1", None, Some(66_714.546), 2, &mut Outbox::default());
    let (_, qty, _) = e.client.modifies.pop().expect("forwarded");
    assert_eq!(qty, None, "a price-only modify must not grow a qty");

    e.modify_order("c1", Some(2.0), None, 3, &mut Outbox::default());
    let (_, _, price) = e.client.modifies.pop().expect("forwarded");
    assert_eq!(price, None, "a qty-only modify must not grow a price");
}
