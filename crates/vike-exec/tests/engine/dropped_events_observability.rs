//! The core FSM fold (`on_event`) must not drop lifecycle events SILENTLY. Dropping an
//! illegal/unknown transition is correct (idempotent WS replays, not-our-order), but a
//! genuinely-lost terminal (a terminal event on a still-live order) and unknown-coid events are
//! counted, so a stranded order can be detected. Counting does not change WHAT is dropped.

use vike_exec::testing::RecordingClient;
use vike_exec::{EventBus, ExecutionEngine, Outbox};
use vike_model::events::{Event, FillEvent, OrderCanceled, OrderFilled, OrderRejected};

use crate::limit_order::req;
use crate::support::EngineBuilder;

fn engine() -> ExecutionEngine<RecordingClient> {
    EngineBuilder { venue: "binance".into(), ..Default::default() }.build()
}

fn canceled(coid: &str) -> Event {
    Event::OrderCanceled(OrderCanceled {
        client_order_id: coid.into(),
        reason: String::new().into(),
        ts: 0,
    })
}

#[test]
fn terminal_event_on_a_live_order_is_counted_not_silently_dropped() {
    let mut eng = engine();
    let mut bus = EventBus::new();
    let mut outbox = Outbox::default();
    // Register the order (status = Initialized, still live).
    eng.submit_order(&req("c1"), 1, &mut outbox);
    // An OrderCanceled is a terminal event, illegal from Initialized → apply() rejects it. The order
    // is still live, so this is a genuinely-lost terminal, not a benign replay.
    bus.publish(canceled("c1"), &mut eng);

    assert_eq!(eng.dropped_terminal_on_live, 1, "lost terminal must be counted");
    assert!(eng.registry.contains_key("c1"), "order still tracked (drop unchanged)");
}

#[test]
fn lifecycle_event_for_unknown_order_is_counted() {
    let mut eng = engine();
    let mut bus = EventBus::new();
    // No such order was ever submitted.
    bus.publish(canceled("ghost"), &mut eng);

    assert_eq!(eng.dropped_unknown_coid, 1, "unknown-coid drop must be counted");
}

fn rejected(coid: &str) -> Event {
    Event::OrderRejected(OrderRejected {
        client_order_id: coid.into(),
        reason: "watchdog: no venue ack within confirm-grace".to_string().into(),
        ts: 0,
    })
}

fn filled(coid: &str, trade_id: &'static str) -> Event {
    Event::OrderFilled(OrderFilled {
        client_order_id: coid.into(),
        fill: FillEvent {
            trade_id: trade_id.into(),
            client_order_id: coid.to_string(),
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            side: 1,
            last_qty: 1.0,
            last_px: 100.0,
            commission: 0.0,
            commission_asset: String::new().into(),
            liquidity_side: "taker".to_string().into(),
            ts: 0,
            mark_price: Some(100.0),
            position_side: vike_model::events::PositionSide::Both,
        },
        ts: 0,
    })
}

/// The phantom-reject clobber: the watchdog's stage-2 backstop synthesizes an `OrderRejected` for a
/// wedged-but-actually-LIVE order, then the venue's real `OrderFilled` lands, an illegal
/// `Rejected → Filled` the FSM drops. Its `status_after` (Rejected) IS terminal, so the
/// lost-terminal counter above misses it; it must increment `stranded_terminal_drops` and warn,
/// WITHOUT changing the FSM (the order stays Rejected).
#[test]
fn late_fill_on_a_phantom_rejected_order_is_counted_as_a_strand() {
    let mut eng = engine();
    let mut bus = EventBus::new();
    let mut outbox = Outbox::default();
    // Register the order, then phantom-reject it (the stage-2 backstop terminalizing a live order).
    eng.submit_order(&req("c1"), 1, &mut outbox);
    bus.publish(rejected("c1"), &mut eng);
    assert!(eng.registry["c1"].status.is_terminal(), "order phantom-rejected (now terminal)");

    // The venue's REAL fill (fresh trade_id, so not deduped) arrives after the reject.
    bus.publish(filled("c1", "t-late"), &mut eng);

    assert_eq!(
        eng.stranded_terminal_drops, 1,
        "a late fill dropped onto an already-killed order must be counted as a strand"
    );
    assert_eq!(
        eng.dropped_terminal_on_live, 0,
        "not a lost-terminal-on-live: the order was already terminal, a different signal"
    );
    assert_eq!(
        eng.registry["c1"].status,
        vike_model::OrderStatus::Rejected,
        "FSM legality is unchanged — the strand is only made observable, the fill is still dropped"
    );
}

/// A benign duplicate terminal replay (a re-sent `OrderRejected` on an already-`Rejected` order) is
/// NOT a strand (not a liveness/fill event), so it stays uncounted and silent: no false positives
/// on ordinary WS reconnect replays.
#[test]
fn duplicate_terminal_replay_is_not_counted_as_a_strand() {
    let mut eng = engine();
    let mut bus = EventBus::new();
    let mut outbox = Outbox::default();
    eng.submit_order(&req("c1"), 1, &mut outbox);
    bus.publish(rejected("c1"), &mut eng);
    bus.publish(rejected("c1"), &mut eng); // reconnect replay of the same terminal

    assert_eq!(eng.stranded_terminal_drops, 0, "a duplicate terminal replay is not a strand");
    assert_eq!(eng.dropped_terminal_on_live, 0, "and not a lost-terminal-on-live either");
}

/// WHY an order was refused is remembered beside the registry (`ExecutionEngine::rejection_reason`,
/// published as `OrderView::reason`): the `OrderRejected` reason, per coid. An order that was not
/// refused, and a refusal with an empty reason, have none; the FSM itself is unchanged.
#[test]
fn a_rejection_reason_is_remembered_per_order() {
    let mut eng = engine();
    let mut bus = EventBus::new();
    let mut outbox = Outbox::default();
    eng.submit_order(&req("c1"), 1, &mut outbox);
    eng.submit_order(&req("c2"), 1, &mut outbox);
    eng.submit_order(&req("c3"), 1, &mut outbox);
    bus.publish(rejected("c1"), &mut eng);
    bus.publish(
        Event::OrderRejected(OrderRejected {
            client_order_id: "c3".into(),
            reason: String::new().into(),
            ts: 0,
        }),
        &mut eng,
    );

    assert_eq!(
        eng.rejection_reason("c1"),
        Some("watchdog: no venue ack within confirm-grace"),
        "the reason the rejection carried"
    );
    assert_eq!(eng.rejection_reason("c2"), None, "an order that was not refused has none");
    assert_eq!(eng.rejection_reason("c3"), None, "an empty reason is no reason");
    assert_eq!(eng.registry["c1"].status, vike_model::OrderStatus::Rejected);
}

/// The side table is bounded: the newest refusals keep their reason, the oldest are evicted, and
/// a long-running node cannot grow it without limit.
#[test]
fn the_rejection_reason_table_keeps_only_the_newest() {
    let mut eng = engine();
    let mut bus = EventBus::new();
    let total = 1_100;
    for i in 0..total {
        let coid = format!("o{i}");
        eng.registry.insert(coid.clone(), vike_model::ManagedOrder::new(req(&coid)));
        bus.publish(
            Event::OrderRejected(OrderRejected {
                client_order_id: coid,
                reason: format!("why {i}").into(),
                ts: 0,
            }),
            &mut eng,
        );
    }
    assert_eq!(eng.rejection_reason("o0"), None, "the oldest refusal was evicted");
    let last = format!("o{}", total - 1);
    assert_eq!(eng.rejection_reason(&last), Some(format!("why {}", total - 1).as_str()));
}
