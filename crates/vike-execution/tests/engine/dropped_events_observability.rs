//! The core FSM fold (`on_event`) must not drop lifecycle events SILENTLY. Dropping an
//! illegal/unknown transition is correct (idempotent WS replays, not-our-order), but a
//! genuinely-lost terminal (a terminal event on a still-live order) and unknown-order-client-id events are
//! counted, so a stranded order can be detected. Counting does not change WHAT is dropped.

use vike_execution::testing::{EngineBuilder, RecordingClient, canceled, resting_limit};
use vike_execution::{Engine, EventBus, EventBusOutbox};
use vike_model::events::{Event, FillEvent, OrderFilled, OrderRejected};

fn engine() -> Engine<RecordingClient> {
    EngineBuilder { venue: "binance".into(), ..Default::default() }.build()
}

#[test]
fn terminal_event_on_a_live_order_is_counted_not_silently_dropped() {
    let mut engine_under_test = engine();
    let mut bus = EventBus::new();
    let mut outbox = EventBusOutbox::default();
    // Register the order (status = Initialized, still live).
    engine_under_test.submit_order(&resting_limit("c1"), 1, &mut outbox);
    // An OrderCanceled is a terminal event, illegal from Initialized → apply() rejects it. The order
    // is still live, so this is a genuinely-lost terminal, not a benign replay.
    bus.publish(canceled("c1", "", 0), &mut engine_under_test);

    assert_eq!(engine_under_test.dropped_terminal_on_live, 1, "lost terminal must be counted");
    assert!(engine_under_test.registry.contains_key("c1"), "order still tracked (drop unchanged)");
}

#[test]
fn lifecycle_event_for_unknown_order_is_counted() {
    let mut engine_under_test = engine();
    let mut bus = EventBus::new();
    // No such order was ever submitted.
    bus.publish(canceled("ghost", "", 0), &mut engine_under_test);

    assert_eq!(
        engine_under_test.dropped_unknown_order_client_id, 1,
        "unknown-order-client-id drop must be counted"
    );
}

fn rejected(order_client_id: &str) -> Event {
    Event::OrderRejected(OrderRejected {
        cause: None,
        order_client_id: order_client_id.into(),
        reason: "watchdog: no venue ack within confirm-grace".to_string().into(),
        timestamp: 0,
    })
}

fn filled(order_client_id: &str, trade_id: &'static str) -> Event {
    Event::OrderFilled(OrderFilled {
        order_client_id: order_client_id.into(),
        fill: FillEvent {
            trade_id: trade_id.into(),
            order_client_id: order_client_id.to_string(),
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            side: 1,
            last_qty: 1.0,
            last_price: 100.0,
            commission: 0.0,
            commission_asset: String::new().into(),
            liquidity_side: "taker".to_string().into(),
            timestamp: 0,
            mark_price: Some(100.0),
            position_side: vike_model::events::PositionSide::Both,
        },
        timestamp: 0,
    })
}

/// The phantom-reject clobber: the watchdog's stage-2 backstop synthesizes an `OrderRejected` for a
/// wedged-but-actually-LIVE order, then the venue's real `OrderFilled` lands, an illegal
/// `Rejected → Filled` the FSM drops. Its `status_after` (Rejected) IS terminal, so the
/// lost-terminal counter above misses it; it must increment `stranded_terminal_drops` and warn,
/// WITHOUT changing the FSM (the order stays Rejected).
#[test]
fn late_fill_on_a_phantom_rejected_order_is_counted_as_a_strand() {
    let mut engine_under_test = engine();
    let mut bus = EventBus::new();
    let mut outbox = EventBusOutbox::default();
    // Register the order, then phantom-reject it (the stage-2 backstop terminalizing a live order).
    engine_under_test.submit_order(&resting_limit("c1"), 1, &mut outbox);
    bus.publish(rejected("c1"), &mut engine_under_test);
    assert!(
        engine_under_test.registry["c1"].status.is_terminal(),
        "order phantom-rejected (now terminal)"
    );

    // The venue's REAL fill (fresh trade_id, so not deduped) arrives after the reject.
    bus.publish(filled("c1", "t-late"), &mut engine_under_test);

    assert_eq!(
        engine_under_test.stranded_terminal_drops, 1,
        "a late fill dropped onto an already-killed order must be counted as a strand"
    );
    assert_eq!(
        engine_under_test.dropped_terminal_on_live, 0,
        "not a lost-terminal-on-live: the order was already terminal, a different signal"
    );
    assert_eq!(
        engine_under_test.registry["c1"].status,
        vike_model::OrderStatus::Rejected,
        "FSM legality is unchanged — the strand is only made observable, the fill is still dropped"
    );
}

/// A benign duplicate terminal replay (a re-sent `OrderRejected` on an already-`Rejected` order) is
/// NOT a strand (not a liveness/fill event), so it stays uncounted and silent: no false positives
/// on ordinary WS reconnect replays.
#[test]
fn duplicate_terminal_replay_is_not_counted_as_a_strand() {
    let mut engine_under_test = engine();
    let mut bus = EventBus::new();
    let mut outbox = EventBusOutbox::default();
    engine_under_test.submit_order(&resting_limit("c1"), 1, &mut outbox);
    bus.publish(rejected("c1"), &mut engine_under_test);
    bus.publish(rejected("c1"), &mut engine_under_test); // reconnect replay of the same terminal

    assert_eq!(
        engine_under_test.stranded_terminal_drops, 0,
        "a duplicate terminal replay is not a strand"
    );
    assert_eq!(
        engine_under_test.dropped_terminal_on_live, 0,
        "and not a lost-terminal-on-live either"
    );
}

/// WHY an order was refused is remembered beside the registry (`Engine::rejection_reason`,
/// published as `OrderView::reason`): the `OrderRejected` reason, per order client id. An order that was not
/// refused, and a refusal with an empty reason, have none; the FSM itself is unchanged.
#[test]
fn a_rejection_reason_is_remembered_per_order() {
    let mut engine_under_test = engine();
    let mut bus = EventBus::new();
    let mut outbox = EventBusOutbox::default();
    engine_under_test.submit_order(&resting_limit("c1"), 1, &mut outbox);
    engine_under_test.submit_order(&resting_limit("c2"), 1, &mut outbox);
    engine_under_test.submit_order(&resting_limit("c3"), 1, &mut outbox);
    bus.publish(rejected("c1"), &mut engine_under_test);
    bus.publish(
        Event::OrderRejected(OrderRejected {
            cause: None,
            order_client_id: "c3".into(),
            reason: String::new().into(),
            timestamp: 0,
        }),
        &mut engine_under_test,
    );

    assert_eq!(
        engine_under_test.rejection_reason("c1"),
        Some("watchdog: no venue ack within confirm-grace"),
        "the reason the rejection carried"
    );
    assert_eq!(
        engine_under_test.rejection_reason("c2"),
        None,
        "an order that was not refused has none"
    );
    assert_eq!(engine_under_test.rejection_reason("c3"), None, "an empty reason is no reason");
    assert_eq!(engine_under_test.registry["c1"].status, vike_model::OrderStatus::Rejected);
}

/// The side table is bounded: the newest refusals keep their reason, the oldest are evicted, and
/// a long-running node cannot grow it without limit.
#[test]
fn the_rejection_reason_table_keeps_only_the_newest() {
    let mut engine_under_test = engine();
    let mut bus = EventBus::new();
    let total = 1_100;
    for i in 0..total {
        let order_client_id = format!("o{i}");
        engine_under_test.registry.insert(
            order_client_id.clone(),
            vike_model::OrderRecord::new(resting_limit(&order_client_id)),
        );
        bus.publish(
            Event::OrderRejected(OrderRejected {
                cause: None,
                order_client_id,
                reason: format!("why {i}").into(),
                timestamp: 0,
            }),
            &mut engine_under_test,
        );
    }
    assert_eq!(engine_under_test.rejection_reason("o0"), None, "the oldest refusal was evicted");
    let last = format!("o{}", total - 1);
    assert_eq!(
        engine_under_test.rejection_reason(&last),
        Some(format!("why {}", total - 1).as_str())
    );
}

/// A refused MODIFY leaves the order live with its terms, and WHY it was refused is shown beside it
/// (`OrderView::reason`, the REASON column of `order ls`) until a modify lands. Side table only: no
/// snapshot or `state_hash` input moves.
#[test]
fn a_modify_refusal_is_remembered_until_a_modify_lands() {
    use vike_model::events::{OrderAccepted, OrderModified, OrderModifyRejected, OrderSubmitted};

    let mut engine_under_test = engine();
    let mut bus = EventBus::new();
    let mut outbox = EventBusOutbox::default();
    engine_under_test.submit_order(&resting_limit("c1"), 1, &mut outbox);
    bus.publish(
        Event::OrderSubmitted(OrderSubmitted { order_client_id: "c1".into(), timestamp: 1 }),
        &mut engine_under_test,
    );
    bus.publish(
        Event::OrderAccepted(OrderAccepted {
            order_client_id: "c1".into(),
            venue_order_id: Some("v1".into()),
            timestamp: 2,
        }),
        &mut engine_under_test,
    );
    bus.publish(
        Event::OrderModifyRejected(OrderModifyRejected {
            order_client_id: "c1".into(),
            reason: "risk: insufficient-margin".into(),
            timestamp: 3,
        }),
        &mut engine_under_test,
    );
    assert_eq!(engine_under_test.rejection_reason("c1"), Some("risk: insufficient-margin"));
    assert_eq!(
        engine_under_test.registry["c1"].status,
        vike_model::OrderStatus::Accepted,
        "non-terminal"
    );

    bus.publish(
        Event::OrderModified(OrderModified {
            order_client_id: "c1".into(),
            venue_order_id: None,
            new_qty: Some(2.0),
            new_price: None,
            new_trigger_price: None,
            timestamp: 4,
        }),
        &mut engine_under_test,
    );
    assert_eq!(
        engine_under_test.rejection_reason("c1"),
        None,
        "a landed modify clears the stale refusal"
    );
}
