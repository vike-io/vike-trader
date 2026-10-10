//! Order-FSM fold tests: `OrderTracker` driven directly, one lifecycle event at a time.

use super::*;
use crate::materialize::testkit::{accepted_ev, fill_ev, modified_ev, submitted_ev};
use vike_model::events::{
    OrderCanceled, OrderDenied, OrderExpired, OrderFilled, OrderLiquidated, OrderRejected,
    OrderTriggered,
};

/// Drive one order from a Submit intent through the adapter's own `OrderSubmitted` to ACCEPTED
/// — the real WAL prefix for every live order ("Submitted → REST → Accepted|Rejected"). Every
/// test below that wants a MODIFIABLE order has to walk it, because the FSM's guards are now
/// the materializer's guards.
fn seed_accepted(tracker: &mut OrderTracker, req: OrderRequest, touched: &mut HashSet<String>) {
    let coid = req.client_order_id.clone();
    tracker.seed_from_intent(&OrderIntent::Submit(Box::new(req)), 10, touched);
    tracker.fold_event(&submitted_ev(&coid, 11), touched);
    tracker.fold_event(&accepted_ev(&coid, None, 12), touched);
}

fn stop_req(coid: &str) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.to_string(),
        venue: "binance".to_string(),
        symbol: "BTCUSDT".to_string(),
        side: 1,
        qty: 1.0,
        order_type: "stop".to_string(),
        price: None,
        trigger_price: Some(90.0),
        ..Default::default()
    }
}

fn limit_req(coid: &str) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.to_string(),
        venue: "binance".to_string(),
        symbol: "BTCUSDT".to_string(),
        side: 1,
        qty: 1.0,
        order_type: "limit".to_string(),
        price: Some(100.0),
        trigger_price: None,
        ..Default::default()
    }
}

/// Regression: an `OrderModified` on a STOP order must write its new price to the durable
/// `trigger_price` column (what the order rests on), not `price`. This used to be a hand-copied
/// `modified_price_is_trigger` branch in this module; it is now simply what the FSM did.
#[test]
fn stop_order_modify_routes_new_price_to_trigger_column() {
    let mut tracker = OrderTracker::default();
    let mut touched = HashSet::new();

    // STOP order resting on trigger 90.0, no limit price.
    seed_accepted(&mut tracker, stop_req("s1"), &mut touched);
    tracker.fold_event(&modified_ev("s1", None, Some(95.0), 20), &mut touched);
    let stop_row = exec_order_row(&tracker.rows["s1"]);
    assert_eq!(stop_row.trigger_price, Some(95.0), "stop modify updates the trigger column");
    assert_eq!(stop_row.price, None, "stop modify must NOT write the limit-price column");

    // Control: a LIMIT order's modify routes to the limit price, leaving trigger untouched.
    seed_accepted(&mut tracker, limit_req("l1"), &mut touched);
    tracker.fold_event(&modified_ev("l1", None, Some(105.0), 20), &mut touched);
    let limit_row = exec_order_row(&tracker.rows["l1"]);
    assert_eq!(limit_row.price, Some(105.0), "limit modify updates the limit-price column");
    assert_eq!(limit_row.trigger_price, None, "limit modify must NOT write the trigger column");
}

/// THE DIVERGENCE THIS FOLD EXISTS TO CLOSE.
///
/// The WAL is WRITE-AHEAD, so it carries events the live FSM went on to REFUSE. The reachable
/// case: a venue amend-ack lands AFTER the fill that terminalized the order (the amend is a
/// REST round trip — e.g. `BinancePerpRest::modify_order`'s `PUT /fapi/v1/order`, whose `Ok`
/// arm emits `OrderModified` — while the fill is a one-hop user-WS push; both are pushed onto
/// the same ingest lane and journaled in arrival order).
///
/// `ManagedOrder::apply` refuses it — `OrderStatus::MODIFIABLE` is {ACCEPTED, TRIGGERED,
/// PARTIALLY_FILLED} and the order is FILLED — so `ExecutionEngine::on_event` returns and the
/// live order kept qty 1.0 @ 100.0. The old unguarded copy applied it and appended an
/// `exec_order` row saying qty 5.0 @ 123.0. Now the materializer gives the FSM's answer.
#[test]
fn modify_after_terminal_is_refused_exactly_as_the_live_fsm_refuses_it() {
    let mut tracker = OrderTracker::default();
    let mut touched = HashSet::new();

    seed_accepted(&mut tracker, limit_req("c1"), &mut touched);
    tracker.fold_event(
        &Event::OrderFilled(OrderFilled {
            client_order_id: "c1".to_string(),
            fill: fill_ev("c1", "BTCUSDT", 1.0, 100.0, 13),
            ts: 13,
        }),
        &mut touched,
    );
    let filled = exec_order_row(&tracker.rows["c1"]);
    assert_eq!(filled.status, "FILLED");

    // The late amend-ack: qty 1.0 -> 5.0, price 100.0 -> 123.0. The live FSM dropped it.
    touched.clear();
    tracker.fold_event(&modified_ev("c1", Some(5.0), Some(123.0), 20), &mut touched);

    let after = exec_order_row(&tracker.rows["c1"]);
    assert_eq!(after.qty, 1.0, "a terminal order's qty is NOT rewritten (old copy wrote 5.0)");
    assert_eq!(
        after.price,
        Some(100.0),
        "a terminal order's price is NOT rewritten (old copy wrote 123.0)"
    );
    assert_eq!(after.status, "FILLED", "status untouched");
    assert_eq!(after.ts, filled.ts, "a refused event must not advance the durable row clock");
    assert!(
        touched.is_empty(),
        "a refused event marks nothing touched — no exec_order row is appended for it"
    );
    assert_eq!(tracker.dropped_invalid, 1, "the refusal is counted, not silently absorbed");
}

/// The same guard, on the other axis: a fill wrap arriving on an already-CANCELED order (the
/// cancel-vs-fill race the engine warns about as `stranded_terminal_drops`). The FSM refuses it
/// — `OrderFilled` is legal only from {ACCEPTED, TRIGGERED, PARTIALLY_FILLED} — so the durable
/// row must NOT flip to FILLED nor accumulate the qty. The bare `Event::Fill` for the same
/// execution still materializes into `exec_fill` on its own lane; only the ORDER snapshot is
/// held to what the engine actually folded.
#[test]
fn fill_wrap_after_terminal_neither_flips_status_nor_accumulates_qty() {
    let mut tracker = OrderTracker::default();
    let mut touched = HashSet::new();

    seed_accepted(&mut tracker, limit_req("c2"), &mut touched);
    tracker.fold_event(
        &Event::OrderCanceled(OrderCanceled {
            client_order_id: "c2".to_string(),
            reason: String::new().into(),
            ts: 20,
        }),
        &mut touched,
    );
    touched.clear();
    tracker.fold_event(
        &Event::OrderFilled(OrderFilled {
            client_order_id: "c2".to_string(),
            fill: fill_ev("c2", "BTCUSDT", 1.0, 100.0, 21),
            ts: 21,
        }),
        &mut touched,
    );

    let row = exec_order_row(&tracker.rows["c2"]);
    assert_eq!(row.status, "CANCELED", "the canceled order does NOT become FILLED");
    assert_eq!(row.filled_qty, 0.0, "no qty accumulated onto a terminal order");
    assert!(touched.is_empty(), "nothing touched, so no fabricated snapshot row");
    assert_eq!(tracker.dropped_invalid, 1);
}

/// A coid first seen via a bare lifecycle event — the post-restart tail whose submit/accept sits
/// below the checkpoint — is ADOPTED into the state that event's allowed-from set requires, so
/// the tail still terminalizes instead of stalling at INITIALIZED. The adoption is one-shot:
/// the NEXT event is guarded like any other.
#[test]
fn bare_first_sighting_is_adopted_then_guarded_like_any_other_order() {
    let mut tracker = OrderTracker::default();
    let mut touched = HashSet::new();

    // No submit, no MintedSubmit: the first record for this coid is a fill wrap.
    tracker.fold_event(
        &Event::OrderFilled(OrderFilled {
            client_order_id: "orphan".to_string(),
            fill: fill_ev("orphan", "ETHUSDT", 2.0, 50.0, 30),
            ts: 30,
        }),
        &mut touched,
    );
    let row = exec_order_row(&tracker.rows["orphan"]);
    assert_eq!(row.status, "FILLED", "adopted at ACCEPTED, so the fill wrap applies");
    assert_eq!(row.symbol, "ETHUSDT", "partition key learned from the fill");
    assert_eq!(row.filled_qty, 2.0);
    assert_eq!(tracker.dropped_invalid, 0, "the adopting event is never a refusal");

    // …and the adoption does not disable the guards: a modify on the now-terminal order is
    // refused exactly as it is for a fully-seeded order.
    touched.clear();
    tracker.fold_event(&modified_ev("orphan", Some(9.0), None, 31), &mut touched);
    assert_eq!(exec_order_row(&tracker.rows["orphan"]).qty, 0.0, "terms not rewritten");
    assert_eq!(tracker.dropped_invalid, 1);
}

/// `fold_event`'s match ends in `_ => return`, so DELETING one of its arms compiles clean and
/// silently stops folding that event — the durable row simply keeps whatever status it had.
/// A mutation sweep found five arms no test pinned; this pins the two that matter, and the
/// other three ride along because a table costs nothing to widen.
///
/// Why these two are worth a test and the rest are not:
///
/// - `OrderRejected` is half of every submit's outcome ("Submitted → REST → Accepted|Rejected",
///   the WAL prefix `seed_accepted` walks). Unfolded, the durable row rests at SUBMITTED
///   forever for an order the venue refused outright.
/// - `OrderExpired` leaves the row at ACCEPTED permanently, and that one is not merely stale:
///   `recon/diff.rs` asks the journal whether an order is live, so an expired order that still
///   reads ACCEPTED manufactures a `JournalDivergence` — the one divergence kind held for
///   operator confirm before the policy is even consulted. A silent fold gap becomes a
///   quarantine an operator has to clear by hand.
///
/// `OrderTriggered`/`OrderLiquidated`/`OrderDenied` are asserted here only because they share
/// the loop. Do not read this test as a claim that their inputs are reachable in this seam.
#[test]
fn every_lifecycle_event_folds_into_the_durable_status() {
    // Each arm gets its OWN baseline, because the FSM's entry states differ per event and a
    // single shared prefix would silently test nothing: `transition_for` admits
    // `OrderRejected` only from {Initialized, Submitted} and `OrderDenied` only from
    // {Initialized}, so seeding everything to ACCEPTED would have `apply` REFUSE those two,
    // leave the row at ACCEPTED, and pass just as happily with the fold arm deleted.
    //
    // (coid, seed depth, event, expected durable status).
    let cases: &[FoldCase] = &[
        (
            "rejected",
            Seed::Submitted,
            |c, ts| {
                Event::OrderRejected(OrderRejected {
                    client_order_id: c.to_string(),
                    reason: "insufficient margin".into(),
                    ts,
                })
            },
            "REJECTED",
        ),
        (
            "expired",
            Seed::Accepted,
            |c, ts| Event::OrderExpired(OrderExpired { client_order_id: c.to_string(), ts }),
            "EXPIRED",
        ),
        (
            "triggered",
            Seed::AcceptedStop,
            |c, ts| Event::OrderTriggered(OrderTriggered { client_order_id: c.to_string(), ts }),
            "TRIGGERED",
        ),
        (
            "liquidated",
            Seed::Accepted,
            |c, ts| {
                Event::OrderLiquidated(OrderLiquidated {
                    client_order_id: c.to_string(),
                    liq_price: 88.0,
                    ts,
                })
            },
            "LIQUIDATED",
        ),
        (
            "denied",
            Seed::Initialized,
            |c, ts| {
                Event::OrderDenied(OrderDenied {
                    client_order_id: c.to_string(),
                    reason: "risk gate".into(),
                    ts,
                })
            },
            "DENIED",
        ),
    ];

    for (coid, seed, make, expected) in cases {
        let mut tracker = OrderTracker::default();
        let mut touched = HashSet::new();

        let req = if matches!(seed, Seed::AcceptedStop) { stop_req(coid) } else { limit_req(coid) };
        tracker.seed_from_intent(&OrderIntent::Submit(Box::new(req)), 10, &mut touched);
        if !matches!(seed, Seed::Initialized) {
            tracker.fold_event(&submitted_ev(coid, 11), &mut touched);
        }
        if matches!(seed, Seed::Accepted | Seed::AcceptedStop) {
            tracker.fold_event(&accepted_ev(coid, None, 12), &mut touched);
        }
        let baseline = exec_order_row(&tracker.rows[*coid]).status;
        assert_eq!(
            baseline,
            seed.status(),
            "{coid}: the baseline itself must be the state this event is admitted FROM — \
                 otherwise `apply` refuses and the assertion below proves nothing"
        );

        touched.clear();
        tracker.fold_event(&make(coid, 30), &mut touched);

        assert_eq!(
            exec_order_row(&tracker.rows[*coid]).status,
            *expected,
            "{coid}: the durable row must carry the FSM's status after the fold — a deleted \
                 `fold_event` arm leaves it at {baseline} and says nothing"
        );
        assert!(
            touched.contains(*coid),
            "{coid}: a folded event must mark the row dirty, or it never reaches the store"
        );
    }
}

/// One row of the fold table: the order's coid, how far its baseline is driven, the event to
/// fold, and the durable status that must come out.
type FoldCase = (&'static str, Seed, fn(&str, i64) -> Event, &'static str);

/// How far down the WAL prefix a case's baseline order is driven before the event under test.
#[derive(Clone, Copy)]
enum Seed {
    Initialized,
    Submitted,
    Accepted,
    /// ACCEPTED, but seeded from a STOP request — the only shape `OrderTriggered` applies to.
    AcceptedStop,
}

impl Seed {
    fn status(self) -> &'static str {
        match self {
            Seed::Initialized => "INITIALIZED",
            Seed::Submitted => "SUBMITTED",
            Seed::Accepted | Seed::AcceptedStop => "ACCEPTED",
        }
    }
}
