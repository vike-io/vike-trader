//! The venue's "nothing to reduce" (`RejectCause::NothingToReduce` on an `OrderRejected`) is PARKED by the
//! engine as a claim, for the core's cold sweep to hold. The engine records it in the same cold arm of the
//! lifecycle fold that records a refusal's reason (rejections only, never per message) and folds NOTHING for it.
//!
//! What each test pins:
//!
//! * a typed refusal of a REDUCE-ONLY order records one claim (symbol, side, order client id), and taking it empties the box;
//! * the refusal still ends the order exactly as before (`Rejected`) and the book does not move;
//! * an UNTYPED refusal records nothing (every venue but one refuses in words only);
//! * a cause on an OPENING order records nothing: only a close can say "nothing to reduce";
//! * a cause for an order the registry never held records nothing;
//! * a repeat for one (symbol, side) is one claim, and two sides are two.

use vike_execution::testing::RecordingClient;
use vike_execution::{Engine, EventBus, EventBusFold, NothingToReduce};
use vike_model::events::{Event, OrderRejected, OrderSubmitted, RejectCause};
use vike_model::{OrderRecord, OrderRequest, OrderStatus, OrderType};

use vike_execution::testing::EngineBuilder;

const VENUE: &str = "dukascopy";
const SYMBOL: &str = "USDJPY";

fn engine() -> Engine<RecordingClient> {
    EngineBuilder { venue: VENUE.into(), symbol: SYMBOL.into(), ..Default::default() }.build()
}

/// A market order of the engine's venue, registered and `Submitted` the way the core leaves it.
fn registered(
    engine: &mut Engine<RecordingClient>,
    order_client_id: &str,
    side: i32,
    reduce_only: bool,
) {
    let request = OrderRequest {
        order_client_id: order_client_id.into(),
        venue: VENUE.into(),
        symbol: SYMBOL.into(),
        side,
        qty: 1000.0,
        order_type: OrderType::Market,
        reduce_only,
        timestamp: 1,
        ..Default::default()
    };
    engine.registry.insert(order_client_id.into(), OrderRecord::new(request));
    let mut bus = EventBus::new();
    let sent = Event::OrderSubmitted(OrderSubmitted {
        order_client_id: order_client_id.into(),
        timestamp: 2,
    });
    assert_eq!(bus.publish(sent, engine), EventBusFold::Applied);
}

fn refused(
    engine: &mut Engine<RecordingClient>,
    order_client_id: &str,
    cause: Option<RejectCause>,
) -> EventBusFold {
    let rejection = Event::OrderRejected(OrderRejected {
        order_client_id: order_client_id.into(),
        reason: "reduce-only: no open long order to reduce ... this order was NOT sent".into(),
        timestamp: 3,
        cause,
    });
    EventBus::new().publish(rejection, engine)
}

#[test]
fn a_typed_refusal_of_a_reduce_only_close_parks_one_claim_and_ends_the_order_as_ever() {
    let mut engine_under_test = engine();
    registered(&mut engine_under_test, "flatten", -1, true);
    assert_eq!(
        refused(&mut engine_under_test, "flatten", Some(RejectCause::NothingToReduce)),
        EventBusFold::Applied
    );
    assert_eq!(
        engine_under_test.registry["flatten"].status,
        OrderStatus::Rejected,
        "the order ends as it always did"
    );
    assert_eq!(
        engine_under_test.position_size(vike_model::events::PositionSide::Both),
        0.0,
        "nothing folded"
    );
    assert_eq!(
        engine_under_test.take_nothing_to_reduce(),
        vec![NothingToReduce {
            symbol: SYMBOL.into(),
            side: -1,
            order_client_id: "flatten".into()
        }]
    );
    assert!(
        engine_under_test.take_nothing_to_reduce().is_empty(),
        "taking a claim empties the box"
    );
}

#[test]
fn an_untyped_refusal_parks_nothing() {
    let mut engine_under_test = engine();
    registered(&mut engine_under_test, "flatten", -1, true);
    assert_eq!(refused(&mut engine_under_test, "flatten", None), EventBusFold::Applied);
    assert!(
        engine_under_test.take_nothing_to_reduce().is_empty(),
        "words alone are never read as a claim"
    );
}

#[test]
fn a_cause_on_an_opening_order_parks_nothing() {
    let mut engine_under_test = engine();
    registered(&mut engine_under_test, "opening", 1, false);
    assert_eq!(
        refused(&mut engine_under_test, "opening", Some(RejectCause::NothingToReduce)),
        EventBusFold::Applied
    );
    assert!(
        engine_under_test.take_nothing_to_reduce().is_empty(),
        "only a close can say there is nothing to reduce"
    );
}

#[test]
fn a_cause_for_an_order_the_registry_never_held_parks_nothing() {
    let mut engine_under_test = engine();
    assert_eq!(
        refused(&mut engine_under_test, "ghost", Some(RejectCause::NothingToReduce)),
        EventBusFold::Dropped
    );
    assert!(engine_under_test.take_nothing_to_reduce().is_empty());
}

#[test]
fn a_repeat_is_one_claim_and_two_sides_are_two() {
    let mut engine_under_test = engine();
    for order_client_id in ["sell-1", "sell-2"] {
        registered(&mut engine_under_test, order_client_id, -1, true);
        refused(&mut engine_under_test, order_client_id, Some(RejectCause::NothingToReduce));
    }
    registered(&mut engine_under_test, "buy-1", 1, true);
    refused(&mut engine_under_test, "buy-1", Some(RejectCause::NothingToReduce));
    let claims = engine_under_test.take_nothing_to_reduce();
    assert_eq!(claims.len(), 2, "{claims:?}");
    let sell = claims.iter().find(|c| c.side == -1).expect("the sell claim");
    assert_eq!(
        sell.order_client_id, "sell-2",
        "the later refusal replaces the earlier for one (symbol, side)"
    );
    assert!(claims.iter().any(|c| c.side == 1 && c.order_client_id == "buy-1"));
}
