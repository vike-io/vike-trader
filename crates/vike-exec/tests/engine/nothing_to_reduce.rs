//! The venue's "nothing to reduce" (`RejectCause::NothingToReduce` on an `OrderRejected`) is PARKED by the
//! engine as a claim, for the core's cold sweep to hold. The engine records it in the same cold arm of the
//! lifecycle fold that records a refusal's reason (rejections only, never per message) and folds NOTHING for it.
//!
//! What each test pins:
//!
//! * a typed refusal of a REDUCE-ONLY order records one claim (symbol, side, coid), and taking it empties the box;
//! * the refusal still ends the order exactly as before (`Rejected`) and the book does not move;
//! * an UNTYPED refusal records nothing (every venue but one refuses in words only);
//! * a cause on an OPENING order records nothing: only a close can say "nothing to reduce";
//! * a cause for an order the registry never held records nothing;
//! * a repeat for one (symbol, side) is one claim, and two sides are two.

use vike_exec::testing::RecordingClient;
use vike_exec::{EventBus, ExecutionEngine, Fold, NothingToReduce};
use vike_model::events::{Event, OrderRejected, OrderSubmitted, RejectCause};
use vike_model::{ManagedOrder, OrderRequest, OrderStatus, OrderType};

use crate::support::EngineBuilder;

const VENUE: &str = "dukascopy";
const SYMBOL: &str = "USDJPY";

fn engine() -> ExecutionEngine<RecordingClient> {
    EngineBuilder { venue: VENUE.into(), symbol: SYMBOL.into(), ..Default::default() }.build()
}

/// A market order of the engine's venue, registered and `Submitted` the way the core leaves it.
fn registered(
    eng: &mut ExecutionEngine<RecordingClient>,
    coid: &str,
    side: i32,
    reduce_only: bool,
) {
    let request = OrderRequest {
        client_order_id: coid.into(),
        venue: VENUE.into(),
        symbol: SYMBOL.into(),
        side,
        qty: 1000.0,
        order_type: OrderType::Market,
        reduce_only,
        ts: 1,
        ..Default::default()
    };
    eng.registry.insert(coid.into(), ManagedOrder::new(request));
    let mut bus = EventBus::new();
    let sent = Event::OrderSubmitted(OrderSubmitted { client_order_id: coid.into(), ts: 2 });
    assert_eq!(bus.publish(sent, eng), Fold::Applied);
}

fn refused(
    eng: &mut ExecutionEngine<RecordingClient>,
    coid: &str,
    cause: Option<RejectCause>,
) -> Fold {
    let rejection = Event::OrderRejected(OrderRejected {
        client_order_id: coid.into(),
        reason: "reduce-only: no open long order to reduce ... this order was NOT sent".into(),
        ts: 3,
        cause,
    });
    EventBus::new().publish(rejection, eng)
}

#[test]
fn a_typed_refusal_of_a_reduce_only_close_parks_one_claim_and_ends_the_order_as_ever() {
    let mut eng = engine();
    registered(&mut eng, "flatten", -1, true);
    assert_eq!(refused(&mut eng, "flatten", Some(RejectCause::NothingToReduce)), Fold::Applied);
    assert_eq!(
        eng.registry["flatten"].status,
        OrderStatus::Rejected,
        "the order ends as it always did"
    );
    assert_eq!(eng.position_size(vike_model::events::PositionSide::Both), 0.0, "nothing folded");
    assert_eq!(
        eng.take_nothing_to_reduce(),
        vec![NothingToReduce { symbol: SYMBOL.into(), side: -1, coid: "flatten".into() }]
    );
    assert!(eng.take_nothing_to_reduce().is_empty(), "taking a claim empties the box");
}

#[test]
fn an_untyped_refusal_parks_nothing() {
    let mut eng = engine();
    registered(&mut eng, "flatten", -1, true);
    assert_eq!(refused(&mut eng, "flatten", None), Fold::Applied);
    assert!(eng.take_nothing_to_reduce().is_empty(), "words alone are never read as a claim");
}

#[test]
fn a_cause_on_an_opening_order_parks_nothing() {
    let mut eng = engine();
    registered(&mut eng, "opening", 1, false);
    assert_eq!(refused(&mut eng, "opening", Some(RejectCause::NothingToReduce)), Fold::Applied);
    assert!(
        eng.take_nothing_to_reduce().is_empty(),
        "only a close can say there is nothing to reduce"
    );
}

#[test]
fn a_cause_for_an_order_the_registry_never_held_parks_nothing() {
    let mut eng = engine();
    assert_eq!(refused(&mut eng, "ghost", Some(RejectCause::NothingToReduce)), Fold::Dropped);
    assert!(eng.take_nothing_to_reduce().is_empty());
}

#[test]
fn a_repeat_is_one_claim_and_two_sides_are_two() {
    let mut eng = engine();
    for coid in ["sell-1", "sell-2"] {
        registered(&mut eng, coid, -1, true);
        refused(&mut eng, coid, Some(RejectCause::NothingToReduce));
    }
    registered(&mut eng, "buy-1", 1, true);
    refused(&mut eng, "buy-1", Some(RejectCause::NothingToReduce));
    let claims = eng.take_nothing_to_reduce();
    assert_eq!(claims.len(), 2, "{claims:?}");
    let sell = claims.iter().find(|c| c.side == -1).expect("the sell claim");
    assert_eq!(
        sell.coid, "sell-2",
        "the later refusal replaces the earlier for one (symbol, side)"
    );
    assert!(claims.iter().any(|c| c.side == 1 && c.coid == "buy-1"));
}
