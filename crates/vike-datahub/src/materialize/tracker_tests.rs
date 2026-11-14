//! Order-FSM fold tests: `JournalMaterializerTracker` driven directly, one lifecycle event at a time.

use super::*;
use crate::materialize::testkit::{accepted_event, fill_event, modified_event, submitted_event};
use vike_model::events::{
    OrderCanceled, OrderDenied, OrderExpired, OrderFilled, OrderLiquidated, OrderPartiallyFilled,
    OrderRejected, OrderTriggered,
};

/// Drive one order from a Submit intent through the adapter's own `OrderSubmitted` to ACCEPTED
/// — the real WAL prefix for every live order ("Submitted → REST → Accepted|Rejected"). Every
/// test below that wants a MODIFIABLE order has to walk it, because the FSM's guards are now
/// the materializer's guards.
fn seed_accepted(
    tracker: &mut JournalMaterializerTracker,
    req: OrderRequest,
    touched: &mut HashSet<String>,
) {
    let order_client_id = req.order_client_id.clone();
    tracker.seed_from_intent(&OrderIntent::Submit(Box::new(req)), 10, touched);
    tracker.fold_event(&submitted_event(&order_client_id, 11), touched);
    tracker.fold_event(&accepted_event(&order_client_id, None, 12), touched);
}

fn stop_request(order_client_id: &str) -> OrderRequest {
    OrderRequest {
        order_client_id: order_client_id.to_string(),
        venue: "binance".to_string(),
        symbol: "BTCUSDT".to_string(),
        side: 1,
        qty: 1.0,
        order_type: vike_model::OrderType::Stop,
        price: None,
        trigger_price: Some(90.0),
        ..Default::default()
    }
}

fn limit_request(order_client_id: &str) -> OrderRequest {
    OrderRequest {
        order_client_id: order_client_id.to_string(),
        venue: "binance".to_string(),
        symbol: "BTCUSDT".to_string(),
        side: 1,
        qty: 1.0,
        order_type: vike_model::OrderType::Limit,
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
    let mut tracker = JournalMaterializerTracker::default();
    let mut touched = HashSet::new();

    // STOP order resting on trigger 90.0, no limit price.
    seed_accepted(&mut tracker, stop_request("s1"), &mut touched);
    tracker.fold_event(&modified_event("s1", None, Some(95.0), 20), &mut touched);
    let stop_row = execution_order_row(&tracker.rows["s1"]);
    assert_eq!(stop_row.trigger_price, Some(95.0), "stop modify updates the trigger column");
    assert_eq!(stop_row.price, None, "stop modify must NOT write the limit-price column");

    // Control: a LIMIT order's modify routes to the limit price, leaving trigger untouched.
    seed_accepted(&mut tracker, limit_request("l1"), &mut touched);
    tracker.fold_event(&modified_event("l1", None, Some(105.0), 20), &mut touched);
    let limit_row = execution_order_row(&tracker.rows["l1"]);
    assert_eq!(limit_row.price, Some(105.0), "limit modify updates the limit-price column");
    assert_eq!(limit_row.trigger_price, None, "limit modify must NOT write the trigger column");
}

/// THE DIVERGENCE THIS FOLD EXISTS TO CLOSE.
///
/// The WAL is WRITE-AHEAD, so it carries events the live FSM went on to REFUSE. The reachable
/// case: a venue amend-ack lands AFTER the fill that terminalized the order (the amend is a
/// REST round trip — e.g. `VenueRestBinancePerp::modify_order`'s `PUT /fapi/v1/order`, whose `Ok`
/// arm emits `OrderModified` — while the fill is a one-hop user-WS push; both are pushed onto
/// the same ingest lane and journaled in arrival order).
///
/// `OrderRecord::apply` refuses it — `OrderStatus::MODIFIABLE` is {ACCEPTED, TRIGGERED,
/// PARTIALLY_FILLED} and the order is FILLED — so `Engine::on_event` returns and the
/// live order kept qty 1.0 @ 100.0. The old unguarded copy applied it and appended an
/// `execution_order` row saying qty 5.0 @ 123.0. Now the materializer gives the FSM's answer.
#[test]
fn modify_after_terminal_is_refused_exactly_as_the_live_state_machine_refuses_it() {
    let mut tracker = JournalMaterializerTracker::default();
    let mut touched = HashSet::new();

    seed_accepted(&mut tracker, limit_request("c1"), &mut touched);
    tracker.fold_event(
        &Event::OrderFilled(OrderFilled {
            order_client_id: "c1".to_string(),
            fill: fill_event("c1", "BTCUSDT", 1.0, 100.0, 13),
            timestamp: 13,
        }),
        &mut touched,
    );
    let filled = execution_order_row(&tracker.rows["c1"]);
    assert_eq!(filled.status, "FILLED");

    // The late amend-ack: qty 1.0 -> 5.0, price 100.0 -> 123.0. The live FSM dropped it.
    touched.clear();
    tracker.fold_event(&modified_event("c1", Some(5.0), Some(123.0), 20), &mut touched);

    let after = execution_order_row(&tracker.rows["c1"]);
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
fn fill_wrap_after_terminal_neither_flips_status_nor_accumulates_quantity() {
    let mut tracker = JournalMaterializerTracker::default();
    let mut touched = HashSet::new();

    seed_accepted(&mut tracker, limit_request("c2"), &mut touched);
    tracker.fold_event(
        &Event::OrderCanceled(OrderCanceled {
            order_client_id: "c2".to_string(),
            reason: String::new().into(),
            timestamp: 20,
        }),
        &mut touched,
    );
    touched.clear();
    tracker.fold_event(
        &Event::OrderFilled(OrderFilled {
            order_client_id: "c2".to_string(),
            fill: fill_event("c2", "BTCUSDT", 1.0, 100.0, 21),
            timestamp: 21,
        }),
        &mut touched,
    );

    let row = execution_order_row(&tracker.rows["c2"]);
    assert_eq!(row.status, "CANCELED", "the canceled order does NOT become FILLED");
    assert_eq!(row.filled_qty, 0.0, "no qty accumulated onto a terminal order");
    assert!(touched.is_empty(), "nothing touched, so no fabricated snapshot row");
    assert_eq!(tracker.dropped_invalid, 1);
}

/// An INVERSE order filled in parts is recorded at its HARMONIC average, the one the live engine
/// folds (`ContractValue::average_price`): a deribit `BTC-PERPETUAL` buy of 2,000 USD filled 1,000 at
/// 50,000 and 1,000 at 60,000 paid 54,545.45 a coin, not the 55,000 this row used to carry. A linear
/// symbol keeps the arithmetic VWAP, bit for bit.
#[test]
fn an_inverse_orders_recorded_average_is_harmonic_and_a_linear_one_is_unchanged() {
    let fill = |order_client_id: &str, venue: &str, symbol: &str, qty: f64, px: f64, ts: i64| {
        FillEvent { venue: venue.into(), ..fill_event(order_client_id, symbol, qty, px, ts) }
    };
    let partial = |f: FillEvent| {
        Event::OrderPartiallyFilled(OrderPartiallyFilled {
            order_client_id: f.order_client_id.clone(),
            timestamp: f.timestamp,
            fill: f,
        })
    };
    let mut tracker = JournalMaterializerTracker::default();
    let mut touched = HashSet::new();
    let deribit = OrderRequest {
        venue: "deribit".to_string(),
        symbol: "BTC-PERPETUAL".to_string(),
        qty: 2_000.0,
        ..limit_request("inv")
    };
    seed_accepted(&mut tracker, deribit, &mut touched);
    let inverse = |qty, px, ts| partial(fill("inv", "deribit", "BTC-PERPETUAL", qty, px, ts));
    tracker.fold_event(&inverse(1_000.0, 50_000.0, 13), &mut touched);
    tracker.fold_event(&inverse(1_000.0, 60_000.0, 14), &mut touched);
    let row = execution_order_row(&tracker.rows["inv"]);
    assert_eq!(row.filled_qty, 2_000.0);
    assert!((row.avg_fill_px - 54_545.454_545).abs() < 1e-6, "harmonic: {}", row.avg_fill_px);

    seed_accepted(&mut tracker, OrderRequest { qty: 10.0, ..limit_request("lin") }, &mut touched);
    let lin = |qty, px, ts| partial(fill("lin", "binance", "BTCUSDT", qty, px, ts));
    let fills = [(2.0, 100.1), (6.0, 110.3), (2.0, 90.7)];
    for (ts, (q, px)) in (20..).zip(fills) {
        tracker.fold_event(&lin(q, px, ts), &mut touched);
    }
    let (mut held, mut avg) = (0.0, 0.0);
    for (q, px) in fills {
        avg = (avg * held + px * q) / (held + q);
        held += q;
    }
    let row = execution_order_row(&tracker.rows["lin"]);
    assert_eq!(row.avg_fill_px.to_bits(), avg.to_bits(), "the running VWAP it always was");
}

/// An order client id first seen via a bare lifecycle event — the post-restart tail whose submit/accept sits
/// below the checkpoint — is ADOPTED into the state that event's allowed-from set requires, so
/// the tail still terminalizes instead of stalling at INITIALIZED. The adoption is one-shot:
/// the NEXT event is guarded like any other.
#[test]
fn bare_first_sighting_is_adopted_then_guarded_like_any_other_order() {
    let mut tracker = JournalMaterializerTracker::default();
    let mut touched = HashSet::new();

    // No submit, no MintedSubmit: the first record for this order client id is a fill wrap.
    tracker.fold_event(
        &Event::OrderFilled(OrderFilled {
            order_client_id: "orphan".to_string(),
            fill: fill_event("orphan", "ETHUSDT", 2.0, 50.0, 30),
            timestamp: 30,
        }),
        &mut touched,
    );
    let row = execution_order_row(&tracker.rows["orphan"]);
    assert_eq!(row.status, "FILLED", "adopted at ACCEPTED, so the fill wrap applies");
    assert_eq!(row.symbol, "ETHUSDT", "partition key learned from the fill");
    assert_eq!(row.filled_qty, 2.0);
    assert_eq!(tracker.dropped_invalid, 0, "the adopting event is never a refusal");

    // …and the adoption does not disable the guards: a modify on the now-terminal order is
    // refused exactly as it is for a fully-seeded order.
    touched.clear();
    tracker.fold_event(&modified_event("orphan", Some(9.0), None, 31), &mut touched);
    assert_eq!(execution_order_row(&tracker.rows["orphan"]).qty, 0.0, "terms not rewritten");
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
    // (order client id, seed depth, event, expected durable status).
    let cases: &[FoldCase] = &[
        (
            "rejected",
            Seed::Submitted,
            |c, ts| {
                Event::OrderRejected(OrderRejected {
                    cause: None,
                    order_client_id: c.to_string(),
                    reason: "insufficient margin".into(),
                    timestamp: ts,
                })
            },
            "REJECTED",
        ),
        (
            "expired",
            Seed::Accepted,
            |c, ts| {
                Event::OrderExpired(OrderExpired { order_client_id: c.to_string(), timestamp: ts })
            },
            "EXPIRED",
        ),
        (
            "triggered",
            Seed::AcceptedStop,
            |c, ts| {
                Event::OrderTriggered(OrderTriggered {
                    order_client_id: c.to_string(),
                    timestamp: ts,
                })
            },
            "TRIGGERED",
        ),
        (
            "liquidated",
            Seed::Accepted,
            |c, ts| {
                Event::OrderLiquidated(OrderLiquidated {
                    order_client_id: c.to_string(),
                    liquidation_price: 88.0,
                    timestamp: ts,
                })
            },
            "LIQUIDATED",
        ),
        (
            "denied",
            Seed::Initialized,
            |c, ts| {
                Event::OrderDenied(OrderDenied {
                    order_client_id: c.to_string(),
                    reason: "risk gate".into(),
                    timestamp: ts,
                })
            },
            "DENIED",
        ),
    ];

    for (order_client_id, seed, make, expected) in cases {
        let mut tracker = JournalMaterializerTracker::default();
        let mut touched = HashSet::new();

        let req = if matches!(seed, Seed::AcceptedStop) {
            stop_request(order_client_id)
        } else {
            limit_request(order_client_id)
        };
        tracker.seed_from_intent(&OrderIntent::Submit(Box::new(req)), 10, &mut touched);
        if !matches!(seed, Seed::Initialized) {
            tracker.fold_event(&submitted_event(order_client_id, 11), &mut touched);
        }
        if matches!(seed, Seed::Accepted | Seed::AcceptedStop) {
            tracker.fold_event(&accepted_event(order_client_id, None, 12), &mut touched);
        }
        let baseline = execution_order_row(&tracker.rows[*order_client_id]).status;
        assert_eq!(
            baseline,
            seed.status(),
            "{order_client_id}: the baseline itself must be the state this event is admitted FROM — \
                 otherwise `apply` refuses and the assertion below proves nothing"
        );

        touched.clear();
        tracker.fold_event(&make(order_client_id, 30), &mut touched);

        assert_eq!(
            execution_order_row(&tracker.rows[*order_client_id]).status,
            *expected,
            "{order_client_id}: the durable row must carry the FSM's status after the fold — a deleted \
                 `fold_event` arm leaves it at {baseline} and says nothing"
        );
        assert!(
            touched.contains(*order_client_id),
            "{order_client_id}: a folded event must mark the row dirty, or it never reaches the store"
        );
    }
}

/// One row of the fold table: the order's order client id, how far its baseline is driven, the event to
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
