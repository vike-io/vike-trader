use super::{
    ExecCommand, MAX_ROUTES, MAX_UNKNOWN, RouteTable, UnknownTable, coid_of, refuse_every_command,
    to_fxcm_instrument,
};
use crate::event_mapper::map_fxcm_event;
use vike_exec::CancelIntent;
use vike_exec::Ingest;
use vike_model::OrderRequest;
use vike_model::events::Event;

/// A start whose ForexConnect session never opened REFUSES what it is handed, naming the
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
        order_type: vike_model::OrderType::Limit,
        ts: 7,
        ..Default::default()
    })))
    .expect("the loop holds the receiver");
    tx.send(ExecCommand::Cancel {
        client_order_id: "c-dead".into(),
        intent: CancelIntent::Unspecified,
    })
    .expect("the loop holds the receiver");
    tx.send(ExecCommand::Modify {
        order: Box::new(OrderRequest {
            client_order_id: "c-dead".into(),
            ts: 9,
            ..Default::default()
        }),
        new_qty: None,
        new_price: Some(1.09),
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
        4,
        "expected Submitted + Rejected for the submit, one cancel reject and one modify reject: {seen:?}"
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
    match &seen[3] {
        Event::OrderModifyRejected(e) => {
            assert_eq!(e.client_order_id, "c-dead");
            assert_eq!(e.reason.as_str(), REASON, "an amend must not vanish either");
        }
        other => panic!("expected a NON-terminal OrderModifyRejected, got {other:?}"),
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

fn lost_placement() -> super::UnknownPlacement {
    super::UnknownPlacement {
        lookup: crate::sys::PendingLookup {
            request_id: "R-1".into(),
            account_id: "ACC".into(),
            offer_id: "1".into(),
            side: crate::sys::Side::Buy,
            amount: 10_000,
            limit: true,
            rate: 1.0925,
        },
        ts: 42,
    }
}

fn unknown_with(coid: &str) -> UnknownTable {
    let mut t = UnknownTable::default();
    t.insert(coid.to_string(), lost_placement());
    t
}

/// A `confirm` that finds the order turns a lost reply into the SAME accept a normal placement
/// emits AND the rows a normal accept creates, so the order can be cancelled and its fills routed.
#[test]
fn a_confirm_that_finds_the_lost_order_accepts_it_and_teaches_the_routes() {
    let mut unknown = unknown_with("c-1");
    let (mut placed, mut routes) = (std::collections::HashMap::new(), RouteTable::default());
    let seen = std::cell::RefCell::new(None);
    let evs = super::confirm_unknown(&mut unknown, &mut placed, &mut routes, "c-1", |l| {
        *seen.borrow_mut() = Some(l.clone());
        Ok(Some("V-9".to_string()))
    });
    assert_eq!(seen.borrow().as_ref().map(|l| l.request_id.as_str()), Some("R-1"));
    match evs.as_slice() {
        [Event::OrderAccepted(a)] => {
            assert_eq!(a.client_order_id, "c-1");
            assert_eq!(a.venue_order_id.as_deref(), Some("V-9"));
            assert_eq!(a.ts, 42, "the request's own timestamp");
        }
        other => panic!("expected exactly one OrderAccepted, got {other:?}"),
    }
    assert!(
        unknown.map.is_empty() && unknown.inserted.is_empty(),
        "a resolved placement is no longer unknown"
    );
    let p = placed.get("c-1").expect("cancel ids taught");
    assert_eq!(
        (p.order_id.as_str(), p.offer_id.as_str(), p.account_id.as_str()),
        ("V-9", "1", "ACC")
    );
    assert_eq!(routes.routes().get("V-9").map(String::as_str), Some("c-1"));
}

/// ...and every other answer is SILENCE, never a terminal: the order may be booked late, and the row
/// stays for the watchdog's next confirm. A coid this process never left unknown is not looked up.
#[test]
fn a_confirm_that_cannot_find_the_order_says_nothing_and_keeps_the_row() {
    for answer in [Ok(None), Err("timeout waiting for the refresh-table response".to_string())] {
        let mut unknown = unknown_with("c-1");
        let (mut placed, mut routes) = (std::collections::HashMap::new(), RouteTable::default());
        let evs = super::confirm_unknown(&mut unknown, &mut placed, &mut routes, "c-1", |_| answer);
        assert!(evs.is_empty(), "no verdict on an absence: {evs:?}");
        assert!(unknown.map.contains_key("c-1") && placed.is_empty() && routes.routes().is_empty());
    }
    let mut unknown = UnknownTable::default();
    let (mut placed, mut routes) = (std::collections::HashMap::new(), RouteTable::default());
    let evs = super::confirm_unknown(&mut unknown, &mut placed, &mut routes, "c-x", |_| {
        panic!("a coid never left unknown must not reach the venue")
    });
    assert!(evs.is_empty());
}

/// The `confirm` worker never waits on a dead exec thread, and returns once the thread acks.
#[test]
fn the_confirm_worker_asks_the_exec_thread_and_survives_its_absence() {
    let (tx, rx) = std::sync::mpsc::channel::<super::ConfirmRequest>();
    drop(rx);
    let t0 = std::time::Instant::now();
    super::ask_exec_thread(&tx, "c-1");
    assert!(t0.elapsed() < std::time::Duration::from_secs(5), "a closed lane returns at once");

    let (tx, rx) = std::sync::mpsc::channel::<super::ConfirmRequest>();
    let t = std::thread::spawn(move || {
        let req = rx.recv().expect("one request");
        assert_eq!(req.coid, "c-7");
        req.reply.send(()).expect("the worker is still waiting for its ack");
    });
    super::ask_exec_thread(&tx, "c-7");
    t.join().expect("the exec side");
}

fn request(coid: &str) -> (super::ConfirmRequest, std::sync::mpsc::Receiver<()>) {
    let (reply, ack) = std::sync::mpsc::channel();
    (super::ConfirmRequest { coid: coid.to_string(), reply }, ack)
}

/// THE late-confirm hole: the exec thread finds the order after the worker gave up (its ack receiver
/// is gone). The events must still go out from the exec thread, or the order stays Submitted with
/// its routes already taught.
#[test]
fn a_confirm_whose_worker_gave_up_still_emits_its_events() {
    let (tx, rx) = std::sync::mpsc::channel();
    let (req, ack) = request("c-1");
    drop(ack);
    tx.send(req).expect("queued");
    let mut unknown = unknown_with("c-1");
    let (mut placed, mut routes) = (std::collections::HashMap::new(), RouteTable::default());
    let mut out = Vec::new();
    super::serve_confirms(
        &rx,
        std::time::Duration::from_secs(10),
        &mut unknown,
        &mut placed,
        &mut routes,
        |_| Ok(Some("V-9".to_string())),
        |ev| out.push(ev),
    );
    assert!(
        matches!(out.as_slice(), [Event::OrderAccepted(a)] if a.client_order_id == "c-1"),
        "the accept was lost with the worker: {out:?}"
    );
    assert!(placed.contains_key("c-1"), "the cancel row is taught either way");
}

/// A burst of confirms is bounded in total time: past the budget the rest stay QUEUED (not dropped,
/// not acked early) for the next pass of the loop.
#[test]
fn a_confirm_burst_stops_at_the_budget_and_keeps_the_rest_queued() {
    let (tx, rx) = std::sync::mpsc::channel();
    let acks: Vec<_> = (0..3)
        .map(|i| {
            let (req, ack) = request(&format!("c-{i}"));
            tx.send(req).expect("queued");
            ack
        })
        .collect();
    let (mut unknown, mut placed) = (UnknownTable::default(), std::collections::HashMap::new());
    let mut routes = RouteTable::default();
    super::serve_confirms(
        &rx,
        std::time::Duration::ZERO,
        &mut unknown,
        &mut placed,
        &mut routes,
        |_| panic!("no coid here is unknown"),
        |_| {},
    );
    assert!(acks[0].try_recv().is_ok(), "the first was served");
    assert!(
        acks[1].try_recv().is_err() && acks[2].try_recv().is_err(),
        "the rest wait for the next pass"
    );
    assert_eq!(rx.try_iter().count(), 2, "two requests are still queued");
}

/// A lookup that lands on an order id this session already holds for ANOTHER coid (a RequestID
/// reused across sessions) must not accept it for this coid, however it is held. The same coid
/// finding its own id again is not a collision.
#[test]
fn a_confirm_never_adopts_an_order_id_held_under_another_coid() {
    use crate::sys::PlacedOrder;
    let held = |order_id: &str| PlacedOrder {
        order_id: order_id.to_string(),
        offer_id: "1".into(),
        account_id: "ACC".into(),
        rate: 1.0,
    };
    // Held in the fill-routing table.
    let mut unknown = unknown_with("c-1");
    let (mut placed, mut routes) = (std::collections::HashMap::new(), RouteTable::default());
    routes.insert("V-9".into(), "c-2".into());
    let evs = super::confirm_unknown(&mut unknown, &mut placed, &mut routes, "c-1", |_| {
        Ok(Some("V-9".to_string()))
    });
    assert!(evs.is_empty() && placed.is_empty(), "adopted a routed id: {evs:?}");
    assert!(unknown.map.contains_key("c-1"), "the row stays for the watchdog's next confirm");
    assert_eq!(routes.routes().get("V-9").map(String::as_str), Some("c-2"), "route untouched");

    // Held in the cancel map only.
    let mut unknown = unknown_with("c-1");
    let (mut placed, mut routes) = (std::collections::HashMap::new(), RouteTable::default());
    placed.insert("c-2".to_string(), held("V-9"));
    let evs = super::confirm_unknown(&mut unknown, &mut placed, &mut routes, "c-1", |_| {
        Ok(Some("V-9".to_string()))
    });
    assert!(evs.is_empty() && !placed.contains_key("c-1"), "adopted a placed id: {evs:?}");

    // Its own id again: fine.
    let mut unknown = unknown_with("c-1");
    let (mut placed, mut routes) = (std::collections::HashMap::new(), RouteTable::default());
    routes.insert("V-9".into(), "c-1".into());
    let evs = super::confirm_unknown(&mut unknown, &mut placed, &mut routes, "c-1", |_| {
        Ok(Some("V-9".to_string()))
    });
    assert_eq!(evs.len(), 1);
}

/// The unknown table is bounded FIFO: past the cap the OLDEST goes (not an arbitrary row), and a
/// removal leaves the queue and the map holding the same set.
#[test]
fn the_unknown_table_evicts_oldest_first_and_stays_consistent() {
    let mut t = UnknownTable::default();
    for i in 0..MAX_UNKNOWN + 2 {
        t.insert(format!("c-{i}"), lost_placement());
    }
    assert_eq!((t.map.len(), t.inserted.len()), (MAX_UNKNOWN, MAX_UNKNOWN));
    assert!(!t.map.contains_key("c-0") && !t.map.contains_key("c-1"), "the oldest two are gone");
    assert!(t.map.contains_key("c-2") && t.map.contains_key(&format!("c-{}", MAX_UNKNOWN + 1)));
    assert!(t.remove("c-5").is_some() && t.remove("c-5").is_none());
    assert_eq!((t.map.len(), t.inserted.len()), (MAX_UNKNOWN - 1, MAX_UNKNOWN - 1));
}

/// The wrapper must DELEGATE `confirm`: the trait default is a silent no-op, so a missing delegation
/// leaves the wired re-query unreachable while `COID_LOOKUP` says `true`. Through the real `ExecActor`.
#[test]
fn confirm_reaches_the_actors_re_query() {
    use vike_bridge_core::exec_actor::{ConfirmFn, ExecActor};
    use vike_exec::ExecutionClient;
    use vike_model::events::OrderAccepted;
    let (tx, mut rx) = vike_exec::event_channel(8);
    let re_query: ConfirmFn = std::sync::Arc::new(|coid: &str| {
        vec![Event::OrderAccepted(OrderAccepted {
            client_order_id: coid.to_string(),
            venue_order_id: None,
            ts: 1,
        })]
    });
    let mut client = super::FxcmExecutionClient(
        ExecActor::spawn("fxcm-confirm-test", tx, |_rx| {}).with_confirm(re_query),
    );
    client.confirm("coid-9");
    client.detach(); // joins the one-shot confirm worker
    assert!(
        matches!(rx.try_recv(), Ok(Ingest::Event(Event::OrderAccepted(a)))
            if a.client_order_id == "coid-9"),
        "the wrapper's `confirm` never reached the actor"
    );
}
/// A lost amend reply is read back, and a STALE first read earns exactly one more look after the
/// pause: the broker may apply the edit late. A first read at the requested rate needs no pause.
#[test]
fn a_lost_amend_is_read_twice_when_the_first_read_is_stale() {
    let tick = 1e-5;
    let (calls, paused) = (std::cell::Cell::new(0usize), std::cell::Cell::new(false));
    let counter = &calls;
    let read = |reads: [Option<f64>; 2]| {
        move || {
            let i = counter.get();
            counter.set(i + 1);
            Ok(Some((reads[i.min(1)].expect("a rate"), tick)))
        }
    };
    // Stale, then applied: settled by the SECOND read.
    let res =
        super::settle_lost_amend(1.1025, "timeout", read([Some(1.1030), Some(1.1025)]), || {
            paused.set(true)
        });
    assert_eq!(res, Ok(()));
    assert!(paused.get() && calls.get() == 2);

    // Stale both times: NOT confirmed, never "settled".
    calls.set(0);
    let res =
        super::settle_lost_amend(1.1025, "timeout", read([Some(1.1030), Some(1.1030)]), || ());
    let text = res.expect_err("still the old rate");
    assert!(text.contains("NOT confirmed") && calls.get() == 2, "{text}");

    // Applied at once: one read, no pause.
    calls.set(0);
    let res = super::settle_lost_amend(1.1025, "timeout", read([Some(1.1025), None]), || {
        panic!("no pause when the first read already shows the requested rate")
    });
    assert_eq!(res, Ok(()));
    assert_eq!(calls.get(), 1);
}
