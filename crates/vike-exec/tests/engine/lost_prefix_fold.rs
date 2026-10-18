//! W19-D1: the venue's ANSWER to an order whose `OrderSubmitted`/`OrderAccepted` were lost (the resolution of
//! an UNKNOWN submit across the gateway hop) lands on an order still `Initialized`, instead of being dropped
//! until the stuck-order watchdog's last resort REJECTS an order that rests at the venue.
//!
//! The transition table is the r5 fixture's and does not move (`ManagedOrder::apply_over_lost_prefix` is a
//! separate door the fold opens only after `apply` refused an `Initialized` order). What this file holds is
//! the FOLD's side: the verdict, the counters, the account, replay determinism (the refold of one recorded
//! stream, and an ordinary stream's bytes, are unchanged).

use vike_exec::testing::RecordingClient;
use vike_exec::{EventBus, ExecutionEngine, Fold, Outbox};
use vike_model::OrderStatus;
use vike_model::events::{
    Event, FillEvent, OrderAccepted, OrderCanceled, OrderFilled, OrderPartiallyFilled,
    OrderSubmitted,
};

use crate::limit_order::req;
use crate::support::EngineBuilder;

fn engine() -> ExecutionEngine<RecordingClient> {
    EngineBuilder { venue: "binance".into(), ..Default::default() }.build()
}

fn accepted(coid: &str) -> Event {
    Event::OrderAccepted(OrderAccepted {
        client_order_id: coid.into(),
        venue_order_id: Some("v-1".into()),
        ts: 5,
    })
}

fn fill(coid: &str, trade_id: &'static str, qty: f64) -> FillEvent {
    FillEvent {
        trade_id: trade_id.into(),
        client_order_id: coid.to_string(),
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        last_qty: qty,
        last_px: 100.0,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "taker".to_string().into(),
        ts: 0,
        mark_price: Some(100.0),
        position_side: vike_model::events::PositionSide::Both,
    }
}

fn filled(coid: &str, trade_id: &'static str) -> Event {
    Event::OrderFilled(OrderFilled {
        client_order_id: coid.into(),
        fill: fill(coid, trade_id, 1.0),
        ts: 6,
    })
}

#[test]
fn a_bare_accepted_for_an_initialized_order_is_applied_not_dropped() {
    let mut eng = engine();
    let mut bus = EventBus::new();
    let mut outbox = Outbox::default();
    eng.submit_order(&req("c1"), 1, &mut outbox);
    assert_eq!(eng.registry["c1"].status, OrderStatus::Initialized);

    assert_eq!(bus.publish(accepted("c1"), &mut eng), Fold::Applied);

    assert_eq!(
        eng.registry["c1"].status,
        OrderStatus::Accepted,
        "the order rests, as the venue says"
    );
    assert_eq!(eng.registry["c1"].venue_order_id.as_deref(), Some("v-1"));
    assert_eq!(eng.dropped_terminal_on_live, 0);
    assert_eq!(eng.stranded_terminal_drops, 0);
    // …and the venue's later events apply through the ordinary table.
    assert_eq!(bus.publish(filled("c1", "t1"), &mut eng), Fold::Applied);
    assert_eq!(eng.registry["c1"].status, OrderStatus::Filled);
}

#[test]
fn a_fill_for_an_initialized_order_is_still_dropped_and_the_later_answer_then_lands_it_with_the_fill()
 {
    // The `out_of_order` scenario of `fixtures/r5/hub.json`, restated through the new door: only a bare
    // `OrderAccepted` opens it. A fill that finds the order `Initialized` is dropped as it always was (the
    // table is the fixture's); the order keeps it, and the answer that follows lands the order WITH the fill
    // (W19g: it used to rest `Accepted` with `filled_qty 0` while the venue held it part-filled).
    let mut eng = engine();
    let mut bus = EventBus::new();
    let mut outbox = Outbox::default();
    eng.submit_order(&req("c1"), 1, &mut outbox);
    let part = Event::OrderPartiallyFilled(OrderPartiallyFilled {
        client_order_id: "c1".into(),
        fill: fill("c1", "t1", 0.4),
        ts: 6,
    });
    assert_eq!(bus.publish(part, &mut eng), Fold::Dropped);
    assert_eq!(eng.registry["c1"].status, OrderStatus::Initialized);
    assert_eq!(eng.registry["c1"].filled_qty, 0.0);

    assert_eq!(bus.publish(accepted("c1"), &mut eng), Fold::Applied);
    assert_eq!(eng.registry["c1"].status, OrderStatus::PartiallyFilled);
    assert_eq!(eng.registry["c1"].filled_qty, 0.4);
    // a second answer (a replay) is the ordinary refusal: Accepted does not accept Accepted
    assert_eq!(bus.publish(accepted("c1"), &mut eng), Fold::Dropped);
    assert_eq!(eng.stranded_terminal_drops, 0, "a live order is not a strand");
}

#[test]
fn a_cancel_for_an_initialized_order_is_still_a_counted_drop() {
    // The pinned behaviour of `dropped_events_observability`: the door is for a bare `OrderAccepted`, not
    // "any event".
    let mut eng = engine();
    let mut bus = EventBus::new();
    let mut outbox = Outbox::default();
    eng.submit_order(&req("c1"), 1, &mut outbox);
    let canceled = Event::OrderCanceled(OrderCanceled {
        client_order_id: "c1".into(),
        reason: String::new().into(),
        ts: 0,
    });
    assert_eq!(bus.publish(canceled, &mut eng), Fold::Dropped);
    assert_eq!(eng.dropped_terminal_on_live, 1);
    assert_eq!(eng.registry["c1"].status, OrderStatus::Initialized);
}

/// Replay determinism. The journal records the RAW event stream, so a refold goes through this same fold:
/// the same stream refolds to the same bytes (the hash the journal's snapshots carry), and a stream that
/// never needed the door is untouched by it (the door is behind an `Err` on an `Initialized` order).
#[test]
fn a_recorded_stream_refolds_to_the_same_state_and_an_ordinary_stream_is_unchanged() {
    fn run(stream: &[Event]) -> (u64, OrderStatus) {
        let mut eng = engine();
        let mut bus = EventBus::new();
        let mut outbox = Outbox::default();
        eng.submit_order(&req("c1"), 1, &mut outbox);
        for e in stream {
            bus.publish(e.clone(), &mut eng);
        }
        let status = eng.registry["c1"].status;
        (vike_exec::state_hash(&[eng.snapshot_state()]), status)
    }

    // The stream of an UNKNOWN resolve: the submit's events are gone, the answer and then a fill remain.
    let resolved = [accepted("c1"), filled("c1", "t1")];
    let first = run(&resolved);
    assert_eq!(first.1, OrderStatus::Filled);
    assert_eq!(run(&resolved), first, "a refold of the same records is the same state");

    // The same order's ORDINARY stream reaches the same end through the table alone, hop by hop, and the
    // two differ in nothing the hash covers but the venue id the implied hops do not carry... which the
    // ordinary stream carries on its Accepted, as the resolved one does here.
    let ordinary = [
        Event::OrderSubmitted(OrderSubmitted { client_order_id: "c1".into(), ts: 5 }),
        accepted("c1"),
        filled("c1", "t1"),
    ];
    let normal = run(&ordinary);
    assert_eq!(normal.1, OrderStatus::Filled);
    assert_eq!(run(&ordinary), normal);
    assert_eq!(first, normal, "the resolved order ends where the ordinary one does");
}

// ---- W19g: what a venue event that found the order `Initialized` must not lose, and what a bare accept
// with no venue id must not claim ----

fn answer_without_id(coid: &str) -> Event {
    Event::OrderAccepted(OrderAccepted {
        client_order_id: coid.into(),
        venue_order_id: None,
        ts: 5,
    })
}

fn partially_filled(coid: &str, trade_id: &'static str, qty: f64) -> Event {
    Event::OrderPartiallyFilled(OrderPartiallyFilled {
        client_order_id: coid.into(),
        fill: fill(coid, trade_id, qty),
        ts: 6,
    })
}

fn canceled(coid: &str) -> Event {
    Event::OrderCanceled(OrderCanceled {
        client_order_id: coid.into(),
        reason: "venue".to_string().into(),
        ts: 6,
    })
}

#[test]
fn a_partial_fill_that_arrived_before_the_answer_is_not_lost() {
    // The venue filled part of an order whose `OrderSubmitted` was lost; the fill reached the core BEFORE
    // the resolution of the UNKNOWN submit. The fill is refused from `Initialized` (the r5 table), then the
    // answer lands: the order must not rest `Accepted` with `filled_qty 0` while the venue holds it filled.
    let mut eng = engine();
    let mut bus = EventBus::new();
    let mut outbox = Outbox::default();
    eng.submit_order(&req("c1"), 1, &mut outbox);
    assert_eq!(bus.publish(partially_filled("c1", "t1", 0.4), &mut eng), Fold::Dropped);
    assert_eq!(eng.registry["c1"].status, OrderStatus::Initialized);

    assert_eq!(bus.publish(accepted("c1"), &mut eng), Fold::Applied);

    let order = &eng.registry["c1"];
    assert_eq!(
        order.status,
        OrderStatus::PartiallyFilled,
        "the fill the venue sent is in the order"
    );
    assert_eq!(order.filled_qty, 0.4);
    assert_eq!(order.avg_fill_px, 100.0);
    assert_eq!(order.venue_order_id.as_deref(), Some("v-1"));
    // the venue redelivers the same fill (a reconnect replay): deduplicated, not counted twice
    assert_eq!(bus.publish(partially_filled("c1", "t1", 0.4), &mut eng), Fold::Dropped);
    assert_eq!(eng.registry["c1"].filled_qty, 0.4);
    // and the order goes on to complete through the unchanged table
    assert_eq!(bus.publish(filled("c1", "t2"), &mut eng), Fold::Applied);
    assert_eq!(eng.registry["c1"].status, OrderStatus::Filled);
}

#[test]
fn a_full_fill_that_arrived_before_the_answer_ends_the_order_filled() {
    let mut eng = engine();
    let mut bus = EventBus::new();
    let mut outbox = Outbox::default();
    eng.submit_order(&req("c1"), 1, &mut outbox);
    assert_eq!(bus.publish(filled("c1", "t1"), &mut eng), Fold::Dropped);

    assert_eq!(bus.publish(accepted("c1"), &mut eng), Fold::Applied);

    assert_eq!(eng.registry["c1"].status, OrderStatus::Filled, "no ghost resting order");
    assert_eq!(eng.registry["c1"].filled_qty, 1.0);
}

#[test]
fn a_venue_cancel_that_arrived_before_the_answer_ends_the_order_canceled() {
    let mut eng = engine();
    eng.collect_applied_fills = true;
    let mut bus = EventBus::new();
    let mut outbox = Outbox::default();
    eng.submit_order(&req("c1"), 1, &mut outbox);
    assert_eq!(bus.publish(canceled("c1"), &mut eng), Fold::Dropped);
    assert_eq!(eng.dropped_terminal_on_live, 1, "the refusal is still counted, as it always was");

    assert_eq!(bus.publish(accepted("c1"), &mut eng), Fold::Applied);

    assert_eq!(eng.registry["c1"].status, OrderStatus::Canceled);
    // the strategy hears both outcomes, in order
    let kinds: Vec<_> = eng.order_events.iter().map(|o| o.event.kind.clone()).collect();
    assert_eq!(
        kinds,
        vec![
            vike_model::OrderEventKind::Accepted,
            vike_model::OrderEventKind::Canceled { reason: "venue".into() },
        ]
    );
}

#[test]
fn a_deferred_fill_survives_a_snapshot_restore_but_not_the_state_hash() {
    // Replay determinism: the buffer is part of the order, so a refold that starts from a `Snap` taken
    // between the fill and the answer lands the same state as the live run; and the hash fence (the bytes
    // an older build recorded) does not move for it.
    let mut eng = engine();
    let mut bus = EventBus::new();
    let mut outbox = Outbox::default();
    eng.submit_order(&req("c1"), 1, &mut outbox);
    let before = vike_exec::state_hash(&[eng.snapshot_state()]);
    assert_eq!(bus.publish(partially_filled("c1", "t1", 0.4), &mut eng), Fold::Dropped);
    let snap = eng.snapshot_state();
    assert_eq!(
        vike_exec::state_hash(std::slice::from_ref(&snap)),
        before,
        "the buffer is not hashed"
    );

    let mut restored = ExecutionEngine::from_snapshot(&snap, RecordingClient::default());
    assert_eq!(bus.publish(accepted("c1"), &mut restored), Fold::Applied);
    assert_eq!(restored.registry["c1"].status, OrderStatus::PartiallyFilled);
    assert_eq!(restored.registry["c1"].filled_qty, 0.4);
    // …and the live engine, fed the same answer, lands the same hash
    assert_eq!(bus.publish(accepted("c1"), &mut eng), Fold::Applied);
    assert_eq!(
        vike_exec::state_hash(&[restored.snapshot_state()]),
        vike_exec::state_hash(&[eng.snapshot_state()])
    );
}

#[test]
fn an_optimistic_accept_with_no_venue_id_does_not_move_an_initialized_order() {
    // `resolve_ambiguous_submit`'s `Err` arm (the re-query itself failed) answers an optimistic accept with
    // NO venue id, "never a false terminal". For an order that never left `Initialized` that is no evidence
    // the venue holds it: it must stay where the stuck-order watchdog still sweeps it (as before W19f).
    let mut eng = engine();
    let mut bus = EventBus::new();
    let mut outbox = Outbox::default();
    eng.submit_order(&req("c1"), 1, &mut outbox);

    assert_eq!(bus.publish(answer_without_id("c1"), &mut eng), Fold::Dropped);

    assert_eq!(eng.registry["c1"].status, OrderStatus::Initialized);
    assert_eq!(eng.registry["c1"].venue_order_id, None);
    assert_eq!(eng.stranded_terminal_drops, 0);
}

#[test]
fn a_fill_redelivered_under_its_trade_id_before_the_answer_is_counted_once() {
    // W13c finding 4: the bridge's reconnect-resync (or a reconcile-synthesized fill) redelivers a trade the
    // user stream also delivered: the SAME trade id, another timestamp. Both reach the core before the
    // answer, both were held, and the replay applied both: `filled_qty` doubled. The ordinary fold keeps the
    // first arrival of a trade id; so does the replay.
    let mut eng = engine();
    let mut bus = EventBus::new();
    let mut outbox = Outbox::default();
    eng.submit_order(&req("c1"), 1, &mut outbox);
    assert_eq!(bus.publish(partially_filled("c1", "t1", 0.4), &mut eng), Fold::Dropped);
    let mut again = partially_filled("c1", "t1", 0.4);
    if let Event::OrderPartiallyFilled(w) = &mut again {
        w.ts = 99;
        w.fill.ts = 99;
    }
    assert_eq!(bus.publish(again, &mut eng), Fold::Dropped);
    // a refold that starts from a `Snap` taken between the two fills and the answer
    let snap = eng.snapshot_state();

    assert_eq!(bus.publish(accepted("c1"), &mut eng), Fold::Applied);

    let order = &eng.registry["c1"];
    assert_eq!(order.filled_qty, 0.4, "one fill, not two");
    assert_eq!(order.status, OrderStatus::PartiallyFilled);
    let mut restored = ExecutionEngine::from_snapshot(&snap, RecordingClient::default());
    assert_eq!(bus.publish(accepted("c1"), &mut restored), Fold::Applied);
    assert_eq!(restored.registry["c1"].filled_qty, 0.4);
    assert_eq!(
        vike_exec::state_hash(&[restored.snapshot_state()]),
        vike_exec::state_hash(&[eng.snapshot_state()]),
        "the refold lands the live run's state"
    );
    // and the venue's later redelivery is still dropped by the engine's own set
    assert_eq!(bus.publish(partially_filled("c1", "t1", 0.4), &mut eng), Fold::Dropped);
    assert_eq!(eng.registry["c1"].filled_qty, 0.4);
}
