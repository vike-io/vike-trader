use super::{
    ExecCommand, MAX_ROUTES, RouteTable, coid_of, refuse_every_command, to_fxcm_instrument,
};
use crate::event_mapper::map_fxcm_event;
use vike_exec::CancelIntent;
use vike_exec::Ingest;
use vike_model::OrderRequest;
use vike_model::events::Event;

/// A mount whose ForexConnect session never opened REFUSES what it is handed, naming the
/// failure — it does not drop it.
///
/// ⚠ **This is the assertion the whole change turns on, so read what it would look like
/// broken.** Restore `run`'s old `Err(_) => return` and this function is never called at all;
/// the commands go to a closed channel and the caller sees, at best,
/// `vike_bridge_core::exec_actor`'s generic dead-channel reject — which names no venue, no
/// cause, and nothing an operator can act on — and at worst nothing whatsoever, which is the 45
/// seconds of silence this work exists to remove. Delete the `reason` from the refusal and the
/// message assertions below go red while the event-shape ones stay green, which is deliberate:
/// the shape was never the defect.
#[test]
fn a_dead_session_refuses_every_command_with_the_reason() {
    const REASON: &str = "fxcm session unavailable: FXCM login failed — the venue said: \
                              User or connection doesn't exist.";
    let (events, mut ingest) = vike_exec::event_channel(64);
    let (tx, rx) = std::sync::mpsc::channel::<ExecCommand>();
    tx.send(ExecCommand::Submit(Box::new(OrderRequest {
        client_order_id: "c-dead".into(),
        venue: "fxcm".into(),
        symbol: "EURUSD".into(),
        side: 1,
        qty: 1000.0,
        order_type: "limit".into(),
        ts: 7,
        ..Default::default()
    })))
    .expect("the loop holds the receiver");
    tx.send(ExecCommand::Cancel {
        client_order_id: "c-dead".into(),
        intent: CancelIntent::Unspecified,
    })
    .expect("the loop holds the receiver");
    tx.send(ExecCommand::Shutdown).expect("the loop holds the receiver");

    refuse_every_command(&events, rx, REASON);

    let mut seen: Vec<Event> = Vec::new();
    while let Ok(Ingest::Event(ev)) = ingest.try_recv() {
        seen.push(ev);
    }
    assert_eq!(
        seen.len(),
        3,
        "expected Submitted + Rejected for the submit and one cancel reject: {seen:?}"
    );
    match &seen[0] {
        Event::OrderSubmitted(e) => {
            assert_eq!(e.client_order_id, "c-dead");
            assert_eq!(e.ts, 7, "the request's own timestamp, never a fabricated one");
        }
        other => panic!("the emitter split owes OrderSubmitted first, got {other:?}"),
    }
    match &seen[1] {
        Event::OrderRejected(e) => {
            assert_eq!(e.client_order_id, "c-dead");
            assert_eq!(
                e.reason.as_str(),
                REASON,
                "the reject must carry the LOGIN failure verbatim — a reason that does not \
                     name the cause is the defect this change exists to close"
            );
        }
        other => panic!("expected exactly one terminal OrderRejected, got {other:?}"),
    }
    match &seen[2] {
        Event::OrderCancelRejected(e) => {
            assert_eq!(e.client_order_id, "c-dead");
            assert_eq!(e.reason.as_str(), REASON, "a cancel must not vanish either");
        }
        other => panic!("expected a NON-terminal OrderCancelRejected, got {other:?}"),
    }
}

/// ...and it STOPS on `Shutdown`, so `ExecActor::detach` still joins this thread.
///
/// Without this the refusal loop would be a hang rather than a degradation: every shipped
/// binary's teardown joins the venue thread, and a loop that ignored `Shutdown` would block it
/// until the channel closed — which, with the actor holding the sender, is never.
#[test]
fn the_refusal_loop_returns_on_shutdown() {
    let (events, mut ingest) = vike_exec::event_channel(8);
    let (tx, rx) = std::sync::mpsc::channel::<ExecCommand>();
    tx.send(ExecCommand::Shutdown).expect("the loop holds the receiver");
    tx.send(ExecCommand::Submit(Box::new(OrderRequest {
        client_order_id: "c-after-shutdown".into(),
        ..Default::default()
    })))
    .expect("the loop holds the receiver");
    // The sender is deliberately KEPT alive past the call: if `Shutdown` did not break, `recv`
    // would block forever here rather than ending the test.
    refuse_every_command(&events, rx, "reason");
    assert!(
        ingest.try_recv().is_err(),
        "nothing queued after Shutdown may be answered — the loop must have returned"
    );
    drop(tx);
}

/// The routing table is a BOUND, not a leak: it holds what it is given, and past the cap the
/// OLDEST placement is the one that goes. Driven at a deliberately tiny scale against
/// `MAX_ROUTES` itself so the property is the cap's, not a magic number's.
#[test]
fn the_route_table_evicts_oldest_first_at_its_cap() {
    let mut t = RouteTable::default();
    for i in 0..MAX_ROUTES {
        t.insert(format!("v-{i}"), format!("c-{i}"));
    }
    assert_eq!(t.routes().len(), MAX_ROUTES, "everything up to the cap is retained");
    assert!(t.routes().contains_key("v-0"), "…including the very first");

    // One past the cap: the oldest row goes, the newest is held, the total does not grow.
    t.insert("v-new".to_string(), "c-new".to_string());
    assert_eq!(t.routes().len(), MAX_ROUTES, "the table must not grow past its cap");
    assert!(!t.routes().contains_key("v-0"), "the OLDEST placement is the one evicted");
    assert_eq!(t.routes().get("v-new").map(String::as_str), Some("c-new"));
}

/// An explicit removal drops the row from BOTH halves, so a later eviction cannot "free" an
/// already-freed id and take a live row with it. Re-inserting one id likewise enqueues once.
#[test]
fn route_removal_and_reinsertion_are_stable() {
    let mut t = RouteTable::default();
    t.insert("v-1".into(), "c-1".into());
    t.remove("v-1");
    assert!(t.routes().is_empty(), "a canceled/rejected order's route is dropped");

    t.insert("v-2".into(), "c-2".into());
    t.insert("v-2".into(), "c-2-again".into());
    assert_eq!(t.routes().len(), 1, "re-inserting one id must not enqueue it twice");
    assert_eq!(t.routes().get("v-2").map(String::as_str), Some("c-2-again"));

    // The dangling-id hazard, driven at the cap: a removal followed by enough inserts to
    // trigger eviction must not evict one row too many. Without the queue-side removal above,
    // the freed id would be popped first, count as a freed slot, and drop a LIVE row.
    let mut t = RouteTable::default();
    t.insert("v-first".into(), "c-first".into());
    t.remove("v-first");
    for i in 0..MAX_ROUTES {
        t.insert(format!("v-{i}"), format!("c-{i}"));
    }
    assert_eq!(t.routes().len(), MAX_ROUTES, "the cap holds exactly, not one short");
    assert!(t.routes().contains_key("v-0"), "no live row was evicted early");
}

/// The pruning key: `drain_events` removes the CANCEL entry for whichever coid the decoded
/// events name, so a fill on order A can never evict order B's cancel ids.
#[test]
fn the_pruned_coid_is_the_one_the_events_name() {
    let fill = map_fxcm_event(
        &serde_json::json!({"kind":"fill","trade_id":"T1","instrument":"EUR/USD","side":"B",
                                "amount":10000,"rate":1.09,"commission":0.0,"ts":0}),
        "c-A",
    );
    assert_eq!(coid_of(&fill), Some("c-A"));
    let cancel = map_fxcm_event(&serde_json::json!({"kind":"canceled","ts":0}), "c-B");
    assert_eq!(coid_of(&cancel), Some("c-B"));
    assert_eq!(coid_of(&[]), None, "a heartbeat names no order, so nothing is pruned");
}

#[test]
fn instrument_mapping() {
    assert_eq!(to_fxcm_instrument("EURUSD"), "EUR/USD");
    assert_eq!(to_fxcm_instrument("eurusd"), "EUR/USD");
    assert_eq!(to_fxcm_instrument("eur/usd"), "EUR/USD"); // already slashed → upper-case
    assert_eq!(to_fxcm_instrument("XAUUSD"), "XAU/USD");
}
