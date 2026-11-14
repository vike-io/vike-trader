use super::{
    ConfirmRequest, ExecutionClientActorCommand, MAX_ROUTES, MAX_UNKNOWN, RouteTable, Spell,
    UnknownTable, open_session, order_client_id_of, refuse_every_command, to_fxcm_instrument,
};
use crate::execution::event_mapper::map_fxcm_event;
use std::time::{Duration, Instant};
use vike_bridge_core::execution_drive::drain_ready;
use vike_model::OrderCancelIntent;
use vike_model::OrderRequest;
use vike_model::events::Event;

/// A deadline no test reaches: the loop under test must leave by another road.
fn far() -> Instant {
    Instant::now() + Duration::from_secs(3600)
}

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
    let (events, mut ingest) = vike_execution::event_channel(64);
    let (tx, rx) = std::sync::mpsc::channel::<ExecutionClientActorCommand>();
    tx.send(ExecutionClientActorCommand::Submit(
        Box::new(OrderRequest {
            order_client_id: "c-dead".into(),
            venue: "fxcm".into(),
            symbol: "EURUSD".into(),
            side: 1,
            qty: 1000.0,
            order_type: vike_model::OrderType::Limit,
            timestamp: 7,
            ..Default::default()
        }),
        std::time::Instant::now(),
    ))
    .expect("the loop holds the receiver");
    tx.send(ExecutionClientActorCommand::Cancel {
        order_client_id: "c-dead".into(),
        intent: OrderCancelIntent::Unspecified,
    })
    .expect("the loop holds the receiver");
    // An order an earlier process placed (the core restored it): the venue may still hold it.
    tx.send(ExecutionClientActorCommand::Cancel {
        order_client_id: "c-restored".into(),
        intent: OrderCancelIntent::Unspecified,
    })
    .expect("the loop holds the receiver");
    tx.send(ExecutionClientActorCommand::Modify {
        order: Box::new(OrderRequest {
            order_client_id: "c-dead".into(),
            timestamp: 9,
            ..Default::default()
        }),
        new_qty: None,
        new_price: Some(1.09),
        new_trigger_price: None,
    })
    .expect("the loop holds the receiver");
    tx.send(ExecutionClientActorCommand::Shutdown).expect("the loop holds the receiver");

    assert_eq!(refuse_every_command(&events, &rx, REASON, far(), || {}), Spell::Stop);

    let seen = drain_ready(|| ingest.try_recv().ok());
    assert_eq!(
        seen.len(),
        5,
        "expected Submitted + Rejected for the submit, one answer per cancel and one for the amend (the one form \
         of an arming refusal, B-15; every amend refused in words, owner 2026-11-11): {seen:?}"
    );
    match &seen[0] {
        Event::OrderSubmitted(e) => {
            assert_eq!(e.order_client_id, "c-dead");
            assert_eq!(e.timestamp, 7, "the request's own timestamp, never a fabricated one");
        }
        other => panic!("the emitter split owes OrderSubmitted first, got {other:?}"),
    }
    match &seen[1] {
        Event::OrderRejected(e) => {
            assert_eq!(e.order_client_id, "c-dead");
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
        Event::OrderCanceled(e) => assert_eq!(e.order_client_id, "c-dead"),
        other => {
            panic!("the cancel of an order this thread refused ends it cancelled, got {other:?}")
        }
    }
    match &seen[3] {
        Event::OrderCancelRejected(e) => {
            assert_eq!(e.order_client_id, "c-restored");
            assert!(
                e.reason.starts_with(REASON) && e.reason.contains("NOT sent"),
                "an earlier process's order may still rest: refused NON-terminally, in the login's words: {}",
                e.reason
            );
        }
        other => panic!("expected a NON-terminal OrderCancelRejected, got {other:?}"),
    }
    match &seen[4] {
        Event::OrderModifyRejected(e) => {
            assert_eq!(e.order_client_id, "c-dead");
            assert_eq!(e.reason.as_str(), REASON, "an amend must not vanish either");
        }
        other => panic!("expected a NON-terminal OrderModifyRejected, got {other:?}"),
    }
}

/// ...and it STOPS on `Shutdown`, so `ExecutionClientActor::detach` still joins this thread.
///
/// Without this the refusal loop would be a hang rather than a degradation: every shipped
/// binary's teardown joins the venue thread, and a loop that ignored `Shutdown` would block it
/// until the channel closed — which, with the actor holding the sender, is never.
#[test]
fn the_refusal_loop_returns_on_shutdown() {
    let (events, mut ingest) = vike_execution::event_channel(8);
    let (tx, rx) = std::sync::mpsc::channel::<ExecutionClientActorCommand>();
    tx.send(ExecutionClientActorCommand::Shutdown).expect("the loop holds the receiver");
    tx.send(ExecutionClientActorCommand::Submit(
        Box::new(OrderRequest { order_client_id: "c-after-shutdown".into(), ..Default::default() }),
        std::time::Instant::now(),
    ))
    .expect("the loop holds the receiver");
    // The sender is deliberately KEPT alive past the call: if `Shutdown` did not break, `recv`
    // would block forever here rather than ending the test.
    assert_eq!(refuse_every_command(&events, &rx, "reason", far(), || {}), Spell::Stop);
    assert!(
        ingest.try_recv().is_err(),
        "nothing queued after Shutdown may be answered — the loop must have returned"
    );
    drop(tx);
}

/// ...and it ENDS at its deadline, so the login is tried again: a spell with no way out is the
/// "dead until the process restarts" this module no longer has.
#[test]
fn the_refusal_loop_ends_at_its_deadline() {
    let (events, _ingest) = vike_execution::event_channel(8);
    let (_tx, rx) = std::sync::mpsc::channel::<ExecutionClientActorCommand>();
    let started = Instant::now();
    let mut ticks = 0;
    let spell =
        refuse_every_command(&events, &rx, "reason", started + Duration::from_millis(250), || {
            ticks += 1
        });
    assert_eq!(spell, Spell::Elapsed);
    assert!(started.elapsed() < Duration::from_secs(10), "{:?}", started.elapsed());
    assert!(ticks >= 2, "the confirm lane is served every poll, not once per spell: {ticks}");
}

fn submit(order_client_id: &str) -> ExecutionClientActorCommand {
    ExecutionClientActorCommand::Submit(
        Box::new(OrderRequest {
            order_client_id: order_client_id.into(),
            venue: "fxcm".into(),
            symbol: "EURUSD".into(),
            side: 1,
            qty: 1000.0,
            order_type: vike_model::OrderType::Limit,
            timestamp: 7,
            ..Default::default()
        }),
        std::time::Instant::now(),
    )
}

fn rejection_of<'a>(seen: &'a [Event], order_client_id: &str) -> &'a str {
    seen.iter()
        .find_map(|event| match event {
            Event::OrderRejected(e) if e.order_client_id == order_client_id => {
                Some(e.reason.as_str())
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("no OrderRejected for {order_client_id} in {seen:?}"))
}

/// **A session that failed to open is retried, and the one that finally opens is used.** Attempt 1
/// and 2 fail (the second with another reason), attempt 3 opens. Across them: a command sent while
/// down is refused with the LATEST reason and the SESSION_UNAVAILABLE marker (the live smoke relies
/// on it); one that queued during the login that succeeded is refused too, not placed (the watchdog
/// may already have timed it out); and the thread keeps serving afterwards.
#[test]
fn a_session_that_failed_to_open_is_retried_until_it_opens() {
    let (events, mut ingest) = vike_execution::event_channel(64);
    let (tx, rx) = std::sync::mpsc::channel::<ExecutionClientActorCommand>();
    let (_confirm_tx, confirms) = std::sync::mpsc::channel::<ConfirmRequest>();
    let mut attempts = 0;
    let waits = std::cell::RefCell::new(Vec::new());

    let opened = open_session(
        || {
            attempts += 1;
            match attempts {
                1 => Err("first words".to_string()),
                2 => {
                    tx.send(submit("c-down")).expect("the loop holds the receiver");
                    Err("second words".to_string())
                }
                _ => {
                    tx.send(submit("c-queued-during-login")).expect("the loop holds the receiver");
                    Ok(())
                }
            }
        },
        &events,
        &rx,
        &confirms,
        |failed| {
            waits.borrow_mut().push(failed);
            Duration::from_millis(30)
        },
    );

    assert_eq!(opened, Some(()));
    assert_eq!(attempts, 3);
    assert_eq!(
        *waits.borrow(),
        [1, 2],
        "the pause grows with the failures in a row, and only after a failure"
    );
    let seen = drain_ready(|| ingest.try_recv().ok());
    assert_eq!(seen.len(), 4, "Submitted + Rejected for each of the two commands: {seen:?}");
    let down = rejection_of(&seen, "c-down");
    assert!(down.starts_with(crate::SESSION_UNAVAILABLE), "marker first: {down}");
    assert!(down.contains("second words"), "the LATEST reason, not the first: {down}");
    assert!(!down.contains("first words"), "{down}");
    let late = rejection_of(&seen, "c-queued-during-login");
    assert!(late.starts_with(crate::SESSION_UNAVAILABLE), "marker first: {late}");
    assert!(late.contains("send it again"), "{late}");
    assert!(rx.try_recv().is_err(), "everything queued before the open was answered");
}

/// The FIRST login is not a retry: what queued behind it is served by the normal loop, exactly as
/// before the retry existed (nothing has refused or timed out anything yet).
#[test]
fn what_queued_behind_the_first_login_is_left_for_the_normal_loop() {
    let (events, mut ingest) = vike_execution::event_channel(8);
    let (tx, rx) = std::sync::mpsc::channel::<ExecutionClientActorCommand>();
    let (_confirm_tx, confirms) = std::sync::mpsc::channel::<ConfirmRequest>();
    let opened = open_session(
        || {
            tx.send(submit("c-first")).expect("the loop holds the receiver");
            Ok(())
        },
        &events,
        &rx,
        &confirms,
        |_| Duration::from_millis(30),
    );
    assert_eq!(opened, Some(()));
    assert!(
        drain_ready(|| ingest.try_recv().ok()).is_empty(),
        "nothing is refused when the first login works"
    );
    assert!(
        matches!(rx.try_recv(), Ok(ExecutionClientActorCommand::Submit(..))),
        "...and the command is still queued"
    );
}

/// Run `open_session` over an opener that ALWAYS fails with `failure`, pausing 20 ms between attempts, and
/// shut the thread down 300 ms in. Returns how many logins were attempted and what the one queued
/// command was refused with.
fn attempts_before_shutdown(failure: &'static str) -> (u32, String) {
    attempts_before_shutdown_pausing(failure, |_| Duration::from_millis(20))
}

/// [`attempts_before_shutdown`] with the pause between attempts as a parameter.
fn attempts_before_shutdown_pausing(
    failure: &'static str,
    pause: impl Fn(u32) -> Duration,
) -> (u32, String) {
    let (events, mut ingest) = vike_execution::event_channel(64);
    let (tx, rx) = std::sync::mpsc::channel::<ExecutionClientActorCommand>();
    let (_confirm_tx, confirms) = std::sync::mpsc::channel::<ConfirmRequest>();
    tx.send(submit("c-refused")).expect("the loop holds the receiver");
    let stopper = tx.clone();
    let stop = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        stopper.send(ExecutionClientActorCommand::Shutdown).expect("the loop is still waiting");
    });
    let mut attempts = 0;
    let opened: Option<()> = open_session(
        || {
            attempts += 1;
            Err(failure.to_string())
        },
        &events,
        &rx,
        &confirms,
        pause,
    );
    stop.join().expect("the stopper thread");
    assert_eq!(opened, None);
    let seen = drain_ready(|| ingest.try_recv().ok());
    (attempts, rejection_of(&seen, "c-refused").to_string())
}

/// **A wrong password is tried ONCE.** The same loop over a host that is down keeps trying (the
/// control: without it a loop that never retried would pass), and so does `User or connection doesn't
/// exist.`, which FXCM answered to a VALID account whose previous session was still being closed
/// (2026-10-24 09:11 UTC): it is a transient conflict and is retried on the schedule. The credential text
/// ends the logins after the first, and the mount still refuses every command with the marker and the
/// venue's words.
#[test]
fn a_wrong_password_is_not_retried_and_a_transient_failure_is() {
    for heals in [
        "FXCM login failed — the venue said: Unable to obtain station descriptor. \
         HTTP request failed object='/Hosts.jsp?<query redacted>' errorCode=56",
        "FXCM login failed — the venue said: User or connection doesn't exist.",
    ] {
        let (tries, refusal) = attempts_before_shutdown(heals);
        assert!(tries >= 3, "a failure that heals keeps the schedule: {tries} attempts: {heals}");
        assert!(refusal.starts_with(crate::SESSION_UNAVAILABLE), "marker first: {refusal}");
        assert!(refusal.contains("the venue said"), "{refusal}");
    }

    let refused =
        "FXCM login failed — the venue said: Login failed. Incorrect user name or password";
    let (tries, refusal) = attempts_before_shutdown(refused);
    assert_eq!(tries, 1, "one failed login is all a rotated password gets");
    assert!(refusal.starts_with(crate::SESSION_UNAVAILABLE), "marker first: {refusal}");
    assert!(refusal.contains(refused), "the venue's words survive: {refusal}");
}

/// **A login that failed once the FX market closed is NOT retried before the open** (owner ruling 2026-11-13): the
/// production pause (`pause_to_a_login`) is `vike_bridge_core::fx_login::held_pause` over the schedule, so on a fixed
/// clock at Friday 22:30 UTC the one wait runs to Sunday 21:00 and the thread logs in once in 300 ms, refusing the queued
/// command with the reason; on a Wednesday the same 20 ms schedule keeps logging in (the control).
#[test]
fn a_login_that_fails_after_the_fx_close_is_retried_only_at_the_open() {
    let at = |stamp: &str| vike_model::time::parse_utc_timestamp_ms(stamp).expect("an instant");
    let failure = "FXCM login failed — the venue said: Unable to obtain station descriptor.";
    let friday = at("2026-11-13T22:30:00Z");
    let (tries, refusal) = attempts_before_shutdown_pausing(failure, |_| {
        vike_bridge_core::fx_login::held_pause_at("fxcm", Duration::from_millis(20), friday)
    });
    assert_eq!(tries, 1, "no login before the open");
    assert!(refusal.starts_with(crate::SESSION_UNAVAILABLE), "marker first: {refusal}");
    let wednesday = at("2026-11-11T12:00:00Z");
    let (tries, _) = attempts_before_shutdown_pausing(failure, |_| {
        vike_bridge_core::fx_login::held_pause_at("fxcm", Duration::from_millis(20), wednesday)
    });
    assert!(tries >= 3, "an open market keeps the schedule: {tries} attempts");
}

/// `Shutdown` ends the wait between attempts at once and no login follows it: `ExecutionClientActor::detach`
/// joins this thread, and the shipped pause is minutes.
#[test]
fn a_shutdown_during_the_pause_leaves_without_another_login() {
    let (events, _ingest) = vike_execution::event_channel(8);
    let (tx, rx) = std::sync::mpsc::channel::<ExecutionClientActorCommand>();
    let (_confirm_tx, confirms) = std::sync::mpsc::channel::<ConfirmRequest>();
    tx.send(ExecutionClientActorCommand::Shutdown).expect("the loop holds the receiver");
    let mut attempts = 0;
    let started = Instant::now();
    let opened: Option<()> = open_session(
        || {
            attempts += 1;
            Err("down".to_string())
        },
        &events,
        &rx,
        &confirms,
        |_| Duration::from_secs(3600),
    );
    assert_eq!(opened, None);
    assert_eq!(attempts, 1);
    assert!(started.elapsed() < Duration::from_secs(10), "{:?}", started.elapsed());
}

/// While the session is down a `confirm` is answered at once with nothing (the lane used to be
/// dropped for that, which a later session cannot have): the watchdog's worker is not left to wait
/// out `CONFIRM_REPLY_WAIT` on a thread that only refuses.
#[test]
fn a_confirm_is_acked_while_the_session_is_down() {
    let (events, _ingest) = vike_execution::event_channel(8);
    let (_tx, rx) = std::sync::mpsc::channel::<ExecutionClientActorCommand>();
    let (confirm_tx, confirms) = std::sync::mpsc::channel::<ConfirmRequest>();
    let (down, acked_down) = request("c-down");
    confirm_tx.send(down).expect("the exec thread holds the lane");
    let mut attempts = 0;
    let opened = open_session(
        || {
            attempts += 1;
            if attempts == 1 { Err("down".to_string()) } else { Ok(()) }
        },
        &events,
        &rx,
        &confirms,
        |_| Duration::from_millis(30),
    );
    assert_eq!(opened, Some(()));
    assert!(acked_down.try_recv().is_ok(), "the worker was not left waiting out its 45 s");
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

/// The pruning key: `drain_events` removes the CANCEL entry for whichever order client id the decoded
/// events name, so a fill on order A can never evict order B's cancel ids.
#[test]
fn the_pruned_order_client_id_is_the_one_the_events_name() {
    let fill = map_fxcm_event(
        &serde_json::json!({"kind":"fill","trade_id":"T1","instrument":"EUR/USD","side":"B",
                                "amount":10000,"rate":1.09,"commission":0.0,"timestamp":0}),
        "c-A",
    );
    assert_eq!(order_client_id_of(&fill), Some("c-A"));
    let cancel = map_fxcm_event(&serde_json::json!({"kind":"canceled","timestamp":0}), "c-B");
    assert_eq!(order_client_id_of(&cancel), Some("c-B"));
    assert_eq!(order_client_id_of(&[]), None, "a heartbeat names no order, so nothing is pruned");
}

// ---- the wiring of a close, driven by SCRIPTED shim envelopes (what `drain_events` does with each one) ----------------
//
// The pure pieces (`execution/close.rs`) have their own tests; these hold the glue the exec thread owns: that every further leg's
// closing order id is ROUTED to the order, that the order's tracker is REGISTERED and read by the drain, and the pruning
// the drain does around it. Before they existed that glue was proven by the ignored live smoke alone.

use crate::execution::close::CloseTracker;
use std::collections::HashMap;

/// `fold_envelope` for an order that is no exit group: the tests of closes and of a single resting order, which hold no
/// group (the group's own wiring is tested below, with `exits`).
fn fold_without_exits(
    v: &serde_json::Value,
    routes: &mut RouteTable,
    placed: &mut HashMap<String, super::PlacedOrder>,
    closes: &mut HashMap<String, CloseTracker>,
) -> Vec<Event> {
    super::fold_envelope(v, routes, placed, closes, &mut HashMap::new())
}

/// One shim fill envelope, in the grammar `fcshim.cpp`'s `pushFill` writes.
fn fill_envelope(order_id: &str, trade_id: &str, side: &str, amount: i64) -> serde_json::Value {
    serde_json::json!({"kind":"fill","order_id":order_id,"trade_id":trade_id,"instrument":"EUR/USD",
        "side":side,"amount":amount,"rate":1.1,"commission":0.0,"remain_trade_id":"","timestamp":0})
}

fn event_names(evs: &[Event]) -> Vec<&'static str> {
    evs.iter()
        .map(|e| match e {
            Event::Fill(_) => "Fill",
            Event::OrderFilled(_) => "OrderFilled",
            Event::OrderPartiallyFilled(_) => "OrderPartiallyFilled",
            Event::OrderCanceled(_) => "OrderCanceled",
            Event::OrderRejected(_) => "OrderRejected",
            _ => "other",
        })
        .collect()
}

/// What the exec thread holds for ONE close order of two legs (`o1` routed like any placement, `o2` by `register_close`).
fn two_leg_close(
    stopped: Option<String>,
    sent: i64,
) -> (RouteTable, HashMap<String, super::PlacedOrder>, HashMap<String, CloseTracker>) {
    let mut routes = RouteTable::default();
    let placed = HashMap::new();
    let mut closes = HashMap::new();
    routes.insert("o1".to_string(), "c-close".to_string());
    super::register_close(
        &mut routes,
        &mut closes,
        "c-close",
        vec!["o2".to_string()],
        CloseTracker::new(sent, stopped),
    );
    (routes, placed, closes)
}

/// **Every leg's fill reaches the order, and the order reads as ONE.** Drop `register_close`'s routing of `more_ids` and the
/// second leg's envelope is for an order "this process did not place": nothing is published and the core never hears the
/// last fill. Drop the registration of the tracker and the first leg's `OrderFilled` ends the order, so the second is an
/// illegal edge. Either breaks this test.
#[test]
fn both_legs_of_a_close_reach_the_order_and_read_as_one() {
    let (mut routes, mut placed, mut closes) = two_leg_close(None, 2000);

    let first = fold_without_exits(
        &fill_envelope("o1", "101-close-o1", "S", 1000),
        &mut routes,
        &mut placed,
        &mut closes,
    );
    assert_eq!(
        event_names(&first),
        ["Fill", "OrderPartiallyFilled"],
        "the first leg is a partial fill"
    );
    assert!(closes.contains_key("c-close"), "the close is still in progress");

    let second = fold_without_exits(
        &fill_envelope("o2", "102-close-o2", "S", 1000),
        &mut routes,
        &mut placed,
        &mut closes,
    );
    assert_eq!(
        event_names(&second),
        ["Fill", "OrderFilled"],
        "the second leg's order id is routed to the same order and its fill is the last"
    );
    assert!(closes.is_empty(), "a completed close is forgotten");
    assert_eq!(order_client_id_of(&second), Some("c-close"));
}

/// Legs that were not sent end the order cancelled AFTER the fills of the legs that were, and the drain's own pruning
/// still runs around it (the last order's route and the order's cancel ids go).
#[test]
fn a_close_that_stopped_short_ends_cancelled_after_its_last_fill_and_prunes() {
    let (mut routes, mut placed, mut closes) =
        two_leg_close(Some("the time bound of one order ran out".into()), 1000);
    placed.insert(
        "c-close".to_string(),
        super::PlacedOrder {
            order_id: "o1".into(),
            offer_id: "1".into(),
            account_id: "A".into(),
            rate: 0.0,
        },
    );
    let evs = fold_without_exits(
        &fill_envelope("o1", "101-close-o1", "S", 1000),
        &mut routes,
        &mut placed,
        &mut closes,
    );
    assert_eq!(event_names(&evs), ["Fill", "OrderPartiallyFilled", "OrderCanceled"]);
    assert!(closes.is_empty());
    assert!(!routes.routes().contains_key("o1"), "the cancel closed that order's route");
    assert!(
        routes.routes().contains_key("o2"),
        "the leg that was never sent keeps its row until evicted"
    );
}

/// The control: an order that is NOT a registered close is decoded exactly as before this change (one `OrderFilled`, no
/// relabel), so a plain fill is never mistaken for a leg.
#[test]
fn a_fill_of_an_ordinary_order_is_not_relabelled() {
    let mut routes = RouteTable::default();
    routes.insert("o9".to_string(), "c-plain".to_string());
    let mut placed = HashMap::new();
    let mut closes = HashMap::new();
    let evs = fold_without_exits(
        &fill_envelope("o9", "T9", "B", 1000),
        &mut routes,
        &mut placed,
        &mut closes,
    );
    assert_eq!(event_names(&evs), ["Fill", "OrderFilled"]);
}

/// An envelope for an order nobody routed publishes nothing (the declared restart hole), whatever the closes hold.
#[test]
fn an_envelope_for_an_unrouted_order_publishes_nothing() {
    let (mut routes, mut placed, mut closes) = two_leg_close(None, 2000);
    let evs = fold_without_exits(
        &fill_envelope("o-stranger", "T0", "S", 1000),
        &mut routes,
        &mut placed,
        &mut closes,
    );
    assert!(evs.is_empty());
    assert_eq!(closes.len(), 1, "and the close in progress is untouched");
}

fn exit_order(order_id: &str) -> super::PlacedOrder {
    super::PlacedOrder {
        order_id: order_id.into(),
        offer_id: "1".into(),
        account_id: "7".into(),
        rate: 1.1175,
    }
}

fn canceled_envelope(order_id: &str) -> serde_json::Value {
    serde_json::json!({"kind":"canceled","order_id":order_id,"timestamp":0})
}

/// **A trade's stop-loss and take-profit through the exec thread's own folding** (the venue's measured batch of a FIRED
/// stop, 2026-10-26: the take-profit's Orders Delete with status `C` first, then the ClosedTrades fill whose order id is
/// the stop's): the take-profit is cancelled exactly once and leaves BOTH bookkeeping maps (it has no resting order and
/// nothing further can arrive for it), the stop is FILLED (never relabelled: it is no close in progress), leaves the cancel
/// map (a fill ends cancelability) and KEEPS its route (a fill never closes routing: a reconnect replays it).
#[test]
fn a_fired_stop_and_the_venues_cancel_of_its_take_profit_each_prune_what_they_end() {
    let mut routes = RouteTable::default();
    routes.insert("416154904".to_string(), "stop".to_string());
    routes.insert("416154901".to_string(), "take-profit".to_string());
    let mut placed = HashMap::new();
    placed.insert("stop".to_string(), exit_order("416154904"));
    placed.insert("take-profit".to_string(), exit_order("416154901"));
    let mut closes = HashMap::new();

    let evs =
        fold_without_exits(&canceled_envelope("416154901"), &mut routes, &mut placed, &mut closes);
    assert_eq!(event_names(&evs), ["OrderCanceled"]);
    assert_eq!(order_client_id_of(&evs), Some("take-profit"));
    assert!(!placed.contains_key("take-profit") && !routes.routes().contains_key("416154901"));
    assert!(placed.contains_key("stop") && routes.routes().contains_key("416154904"));

    let evs = fold_without_exits(
        &fill_envelope("416154904", "186983241-close-416154904", "S", 1000),
        &mut routes,
        &mut placed,
        &mut closes,
    );
    assert_eq!(
        event_names(&evs),
        ["Fill", "OrderFilled"],
        "the stop is filled, not a partial fill"
    );
    assert_eq!(order_client_id_of(&evs), Some("stop"));
    assert!(!placed.contains_key("stop"), "a filled order has nothing left to cancel");
    assert!(routes.routes().contains_key("416154904"), "a fill keeps its route");
    assert!(closes.is_empty());
}

/// An exit the node cancelled ITSELF: the `Cancel` arm removes the routing row when the venue confirms, so the venue's own
/// `canceled` envelope for it (its Orders Delete, status `C`) is unroutable and publishes nothing: one cancel event, not two.
#[test]
fn the_venues_echo_of_a_cancel_the_node_made_publishes_nothing_and_touches_nothing() {
    let mut routes = RouteTable::default();
    routes.insert("416154901".to_string(), "take-profit".to_string());
    routes.insert("416154904".to_string(), "stop".to_string());
    let mut placed = HashMap::new();
    placed.insert("stop".to_string(), exit_order("416154904"));
    let mut closes = HashMap::new();
    routes.remove("416154901"); // what the Cancel arm does once `delete_order` answered
    let evs =
        fold_without_exits(&canceled_envelope("416154901"), &mut routes, &mut placed, &mut closes);
    assert!(evs.is_empty());
    assert!(placed.contains_key("stop") && routes.routes().contains_key("416154904"));
}

/// One Orders row as `forex_connect_orders` writes it: a resting BUY limit entry of 1000 EUR/USD.
fn order_row(order_id: &str) -> String {
    format!(
        r#"[{{"order_id":"{order_id}","instrument":"EUR/USD","buysell":"B","amount":1000,"type":"LE","status":"W"}}]"#
    )
}

/// The order client id the reconcile listing names for `order_id`'s row.
fn listed_order_client_id(order_id: &str) -> Option<String> {
    let rows = crate::reconcile::parse_orders(&order_row(order_id), "EURUSD").expect("rows");
    rows.into_iter().next().and_then(|o| o.order_client_id)
}

/// **The listing names our order client id for an order this process placed** (the demo of 2026-11-09: a far limit `fxt2fx0801`,
/// venue order `416262101`, listed as `order_client_id=None`, so every pass held an `UnknownOrder` beside an `OrphanLocalOrder`).
/// The placement's answer routes the venue id (`RouteTable::insert`), and that is what binds it for the listing; the
/// venue's own cancel of it ends OUR order and the binding with it.
#[test]
fn a_placed_orders_listing_row_names_its_order_client_id_until_the_venue_ends_it() {
    let mut routes = RouteTable::default();
    routes.insert("416262101".to_string(), "fxt2fx0801".to_string());
    assert_eq!(listed_order_client_id("416262101").as_deref(), Some("fxt2fx0801"));

    let (mut placed, mut closes) = (HashMap::new(), HashMap::new());
    placed.insert("fxt2fx0801".to_string(), exit_order("416262101"));
    let evs =
        fold_without_exits(&canceled_envelope("416262101"), &mut routes, &mut placed, &mut closes);
    assert_eq!(event_names(&evs), ["OrderCanceled"]);
    assert_eq!(order_client_id_of(&evs), Some("fxt2fx0801"), "the venue's cancel ends OUR order");
    assert_eq!(listed_order_client_id("416262101"), None, "a dead route names nothing");
}

/// An order this process does not route (a person's, or one an earlier process placed) stays foreign: its row names no
/// order client id and its venue events publish nothing.
#[test]
fn an_order_this_process_does_not_route_stays_foreign() {
    assert_eq!(listed_order_client_id("416262199"), None);
    let mut routes = RouteTable::default();
    let (mut placed, mut closes) = (HashMap::new(), HashMap::new());
    let evs =
        fold_without_exits(&canceled_envelope("416262199"), &mut routes, &mut placed, &mut closes);
    assert!(evs.is_empty(), "{evs:?}");
}

/// **After a gatewayhub restart** the new process holds no route and no binding, so an order the old one placed lists as
/// `order_client_id=None` and its venue events publish nothing (the test above). The re-teaching carries the venue order id the core
/// holds, and that rebuilds both: the listing names our order client id at once (`bind_restored`, what `adopt_restored` runs on the
/// caller's thread), and once the session thread has routed the teaching (`adopt_taught`) a fill of the order routes to
/// it and the venue's removal of another ends ITS order and drops the route, as for an order this process placed. An
/// order taught with no venue id (or an empty one) teaches nothing.
#[test]
fn a_pre_restart_order_lists_under_our_order_client_id_and_routes_its_events_after_the_reteaching()
{
    let (resting, filled) = ("416262301", "416262302");
    let restored = [
        vike_execution::RestoredOrder {
            venue_order_id: Some(resting.into()),
            ..vike_execution::RestoredOrder::new("fxt2fx0901", "EURUSD")
        },
        vike_execution::RestoredOrder {
            venue_order_id: Some(filled.into()),
            ..vike_execution::RestoredOrder::new("fxt2fx0902", "EURUSD")
        },
        vike_execution::RestoredOrder::new("fxt2fx0903", "EURUSD"),
        vike_execution::RestoredOrder {
            venue_order_id: Some(String::new()),
            ..vike_execution::RestoredOrder::new("fxt2fx0904", "EURUSD")
        },
    ];
    // The listing, over a table of its own (the restart left it empty).
    let mut ours = crate::execution::registry::OrderClientIdLookup::default();
    let listed = |ours: &crate::execution::registry::OrderClientIdLookup| {
        crate::reconcile::parse_orders_with(&order_row(resting), "EURUSD", ours)
            .expect("rows")
            .remove(0)
            .order_client_id
    };
    assert_eq!(listed(&ours), None, "the restart's hole");
    let taught = super::bind_restored(&restored, &mut ours);
    assert_eq!(taught, restored[..2], "only an order with a venue id teaches");
    assert_eq!(
        listed(&ours).as_deref(),
        Some("fxt2fx0901"),
        "the listing names our order client id"
    );

    // The session thread's routes, empty after the restart until the teaching reaches them.
    let mut routes = RouteTable::default();
    let (mut placed, mut closes) = (HashMap::new(), HashMap::new());
    let fill = fill_envelope(filled, "T-0902", "B", 1000);
    assert!(fold_without_exits(&fill, &mut routes, &mut placed, &mut closes).is_empty());
    let (tx, rx) = std::sync::mpsc::channel();
    tx.send(taught).expect("the session thread's lane");
    // Neither order is a trade-bound exit (the venue lists none), and here the entry reads fail: nothing more is rebuilt.
    super::adopt_taught(
        &rx,
        &mut routes,
        &mut placed,
        &mut HashMap::new(),
        &mut HashMap::new(),
        || Ok((Vec::new(), Vec::new())),
        || Err("not read in this test".to_string()),
    );
    assert!(placed.is_empty());
    // Routed, but still a previous process's: an exit among them stays one a new exit of its rate takes over.
    assert!(routes.routes().contains_key(resting) && !routes.placed_here(resting));
    let evs = fold_without_exits(&fill, &mut routes, &mut placed, &mut closes);
    assert_eq!(event_names(&evs), ["Fill", "OrderFilled"], "{evs:?}");
    assert_eq!(order_client_id_of(&evs), Some("fxt2fx0902"), "the fill reaches OUR order");
    let evs =
        fold_without_exits(&canceled_envelope(resting), &mut routes, &mut placed, &mut closes);
    assert_eq!(event_names(&evs), ["OrderCanceled"]);
    assert_eq!(order_client_id_of(&evs), Some("fxt2fx0901"), "the venue's removal ends OUR order");
    assert!(!routes.routes().contains_key(resting), "and drops its route");
    // This process's own placement (or a takeover's answer) of a re-taught id makes it this process's.
    routes.insert(filled.to_string(), "fxt2fx0907".to_string());
    assert!(routes.placed_here(filled));
}

/// The actor's half of the re-teaching ([`super::adopt_restored_hook`]): `adopt_restored` hands the session thread the
/// orders with a venue id (FIFO with nothing else: its own lane), and a teaching with no venue id sends nothing at all.
#[test]
fn adopt_restored_hands_the_session_thread_the_venue_ids() {
    use vike_bridge_core::exec_actor::ExecutionClientActor;
    use vike_execution::ExecutionClient;
    let (events, _rx) = vike_execution::event_channel(8);
    let (tx, taught) = std::sync::mpsc::channel();
    let mut client = ExecutionClientActor::spawn("fxcm-adopt-test", events, |_rx| {})
        .with_adopt_restored(super::adopt_restored_hook(tx));
    let with_id = vike_execution::RestoredOrder {
        venue_order_id: Some("416262305".into()),
        ..vike_execution::RestoredOrder::new("fxt2fx0906", "EURUSD")
    };
    client.adopt_restored(&[vike_execution::RestoredOrder::new("fxt2fx0905", "EURUSD")]);
    client.adopt_restored(std::slice::from_ref(&with_id));
    client.detach();
    assert_eq!(taught.try_iter().collect::<Vec<_>>(), [vec![with_id]]);
}

/// The node's OWN cancel unbinds the order when the venue confirms it (the `Cancel` arm's `routes.remove`), as it drops the
/// route: the listing then names nothing for it, and the venue's echo publishes nothing (the test above).
#[test]
fn the_nodes_own_cancel_unbinds_the_order() {
    let mut routes = RouteTable::default();
    routes.insert("416262102".to_string(), "fxt2fx0802".to_string());
    assert_eq!(listed_order_client_id("416262102").as_deref(), Some("fxt2fx0802"));
    routes.remove("416262102");
    assert_eq!(listed_order_client_id("416262102"), None);
}

/// **The advisory a cancel of an exit the VENUE already removed gets** (the core's own OCO cancel of the take-profit
/// of a stop that fired: the venue deleted it in the fire's own batch, so the exec thread holds no row) says that the
/// venue may have removed it, instead of reading as a fault. The head stays what callers and logs have always matched.
#[test]
fn a_cancel_that_finds_no_row_says_the_venue_may_have_removed_the_order() {
    let why = super::no_resting_order("take-profit");
    assert!(why.starts_with("no resting order for take-profit"), "{why}");
    for needle in
        ["never placed here", "already ended", "removed by the venue", "other exit of a trade"]
    {
        assert!(why.contains(needle), "`{needle}` is missing from: {why}");
    }
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
        lookup: crate::transport::sys::PendingLookup {
            request_id: "R-1".into(),
            account_id: "ACC".into(),
            offer_id: "1".into(),
            side: crate::transport::sys::OrderSideWire::Buy,
            amount: 10_000,
            limit: true,
            rate: 1.0925,
        },
        timestamp: 42,
    }
}

fn unknown_with(order_client_id: &str) -> UnknownTable {
    let mut t = UnknownTable::default();
    t.insert(order_client_id.to_string(), lost_placement());
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
            assert_eq!(a.order_client_id, "c-1");
            assert_eq!(a.venue_order_id.as_deref(), Some("V-9"));
            assert_eq!(a.timestamp, 42, "the request's own timestamp");
        }
        other => panic!("expected exactly one OrderAccepted, got {other:?}"),
    }
    assert!(unknown.as_map().is_empty(), "a resolved placement is no longer unknown");
    let p = placed.get("c-1").expect("cancel ids taught");
    assert_eq!(
        (p.order_id.as_str(), p.offer_id.as_str(), p.account_id.as_str()),
        ("V-9", "1", "ACC")
    );
    assert_eq!(routes.routes().get("V-9").map(String::as_str), Some("c-1"));
}

/// ...and every other answer is SILENCE, never a terminal: the order may be booked late, and the row
/// stays for the watchdog's next confirm. An order client id this process never left unknown is not looked up.
#[test]
fn a_confirm_that_cannot_find_the_order_says_nothing_and_keeps_the_row() {
    for answer in [Ok(None), Err("timeout waiting for the refresh-table response".to_string())] {
        let mut unknown = unknown_with("c-1");
        let (mut placed, mut routes) = (std::collections::HashMap::new(), RouteTable::default());
        let evs = super::confirm_unknown(&mut unknown, &mut placed, &mut routes, "c-1", |_| answer);
        assert!(evs.is_empty(), "no verdict on an absence: {evs:?}");
        assert!(
            unknown.as_map().contains_key("c-1") && placed.is_empty() && routes.routes().is_empty()
        );
    }
    let mut unknown = UnknownTable::default();
    let (mut placed, mut routes) = (std::collections::HashMap::new(), RouteTable::default());
    let evs = super::confirm_unknown(&mut unknown, &mut placed, &mut routes, "c-x", |_| {
        panic!("an order client id never left unknown must not reach the venue")
    });
    assert!(evs.is_empty());
}

/// The `confirm` worker never waits on a dead exec thread, and returns once the thread acks.
#[test]
fn the_confirm_worker_asks_the_execution_thread_and_survives_its_absence() {
    let (tx, rx) = std::sync::mpsc::channel::<super::ConfirmRequest>();
    drop(rx);
    let t0 = std::time::Instant::now();
    super::ask_execution_thread(&tx, "c-1");
    assert!(t0.elapsed() < std::time::Duration::from_secs(5), "a closed lane returns at once");

    let (tx, rx) = std::sync::mpsc::channel::<super::ConfirmRequest>();
    let t = std::thread::spawn(move || {
        let req = rx.recv().expect("one request");
        assert_eq!(req.order_client_id, "c-7");
        req.reply.send(()).expect("the worker is still waiting for its ack");
    });
    super::ask_execution_thread(&tx, "c-7");
    t.join().expect("the exec side");
}

fn request(order_client_id: &str) -> (super::ConfirmRequest, std::sync::mpsc::Receiver<()>) {
    let (reply, ack) = std::sync::mpsc::channel();
    (super::ConfirmRequest { order_client_id: order_client_id.to_string(), reply }, ack)
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
        |event| out.push(event),
    );
    assert!(
        matches!(out.as_slice(), [Event::OrderAccepted(a)] if a.order_client_id == "c-1"),
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
        |_| panic!("no order client id here is unknown"),
        |_| {},
    );
    assert!(acks[0].try_recv().is_ok(), "the first was served");
    assert!(
        acks[1].try_recv().is_err() && acks[2].try_recv().is_err(),
        "the rest wait for the next pass"
    );
    assert_eq!(rx.try_iter().count(), 2, "two requests are still queued");
}

/// A lookup that lands on an order id this session already holds for ANOTHER order client id (a RequestID
/// reused across sessions) must not accept it for this order client id, however it is held. The same order client id
/// finding its own id again is not a collision.
#[test]
fn a_confirm_never_adopts_an_order_id_held_under_another_order_client_id() {
    use crate::transport::sys::PlacedOrder;
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
    assert!(unknown.as_map().contains_key("c-1"), "the row stays for the watchdog's next confirm");
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
/// removal takes the row once (that the queue and the map hold the same set is `FifoMap`'s own property).
#[test]
fn the_unknown_table_evicts_oldest_first_and_stays_consistent() {
    let mut t = UnknownTable::default();
    for i in 0..MAX_UNKNOWN + 2 {
        t.insert(format!("c-{i}"), lost_placement());
    }
    assert_eq!(t.as_map().len(), MAX_UNKNOWN);
    assert!(
        !t.as_map().contains_key("c-0") && !t.as_map().contains_key("c-1"),
        "the oldest two are gone"
    );
    assert!(
        t.as_map().contains_key("c-2")
            && t.as_map().contains_key(&format!("c-{}", MAX_UNKNOWN + 1))
    );
    assert!(t.remove("c-5").is_some() && t.remove("c-5").is_none());
    assert_eq!(t.as_map().len(), MAX_UNKNOWN - 1);
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

// ---- the reason an instrument could not be subscribed rides on the order's refusal ---------------------

fn unknown_note() -> crate::instrument::subscription::Failure {
    crate::instrument::subscription::Failure {
        why: "fxcm: the bridge cannot subscribe `EUR/JPY` for trading itself: the shim is older"
            .to_string(),
        transient: true,
    }
}

fn refused_by_the_venue() -> super::Placement {
    Err((false, "FXCM native error -3: offer not found".to_string(), None))
}

/// A refusal by the venue of an order whose subscription could not be made (unknown outcome, an older shim, a
/// connection that is not `Demo`) carries BOTH reasons, so the operator is never left with the bare "offer not found".
#[test]
fn a_refused_order_carries_why_its_instrument_was_not_subscribed() {
    let note = unknown_note();
    let merged = super::with_subscription_note(Some(&note), refused_by_the_venue());
    let Err((unknown, reason, _)) = merged else { panic!("still a refusal") };
    assert!(!unknown, "a refusal stays a refusal");
    assert_eq!(
        reason,
        "FXCM native error -3: offer not found; fxcm: the bridge cannot subscribe `EUR/JPY` for trading itself: the shim is older"
    );
}

/// Everything else is left exactly as it is: no note, an accepted order, a placement whose OUTCOME is unknown (it was
/// not refused: nothing to explain), and a DEFINITE subscription failure (it ended the order in its own words).
#[test]
fn only_a_refusal_with_an_unknown_subscription_outcome_takes_the_note() {
    let note = unknown_note();
    let placed = || {
        Ok(super::PlacedOrder {
            order_id: "416153400".into(),
            offer_id: "9".into(),
            account_id: "1".into(),
            rate: 0.0,
        })
    };
    assert!(
        matches!(super::with_subscription_note(Some(&note), placed()), Ok(p) if p.order_id == "416153400")
    );
    let lost: super::Placement = Err((true, "no answer".to_string(), None));
    assert!(
        matches!(super::with_subscription_note(Some(&note), lost), Err((true, r, _)) if r == "no answer")
    );
    assert!(matches!(
        super::with_subscription_note(None, refused_by_the_venue()),
        Err((false, r, _)) if r == "FXCM native error -3: offer not found"
    ));
    let definite =
        crate::instrument::subscription::Failure { why: "refused".to_string(), transient: false };
    assert!(matches!(
        super::with_subscription_note(Some(&definite), refused_by_the_venue()),
        Err((false, r, _)) if r == "FXCM native error -3: offer not found"
    ));
}

/// **A close never subscribes**, an opening order does once per instrument per session, and a session that cannot
/// subscribe at all answers without asking. The closure stands for the shim: it counts how often it was asked.
#[test]
fn only_an_opening_order_asks_the_venue_to_subscribe_and_only_once() {
    let asked = std::cell::Cell::new(0);
    let ask = || {
        asked.set(asked.get() + 1);
        Ok(())
    };
    let mut memory = crate::instrument::subscription::Attempts::default();

    // A close (a flatten after a restart is the first order of its session): no round trip, no refusal.
    let (note, was_asked) = super::ensure_for_order(true, None, "EUR/JPY", &mut memory, ask);
    assert_eq!((note, was_asked, asked.get()), (None, false, 0));

    // The first opening order asks; the second finds it done.
    let (note, was_asked) = super::ensure_for_order(false, None, "EUR/JPY", &mut memory, ask);
    assert_eq!((note, was_asked, asked.get()), (None, true, 1));
    let (note, was_asked) = super::ensure_for_order(false, None, "EUR/JPY", &mut memory, ask);
    assert_eq!((note, was_asked, asked.get()), (None, false, 1), "once per instrument per session");

    // A session that cannot subscribe: a TRANSIENT note naming the gap, nothing asked, the order still goes.
    let (note, was_asked) =
        super::ensure_for_order(false, Some("the shim is older"), "EUR/CAD", &mut memory, ask);
    let note = note.expect("the gap is said");
    assert!(
        note.transient && note.why.contains("EUR/CAD") && note.why.contains("the shim is older"),
        "{note:?}"
    );
    assert_eq!((was_asked, asked.get()), (false, 1));
}

/// A refusal is remembered (no retry storm: the second order does not ask again and does not claim to have asked) and
/// a DEFINITE one is the order's refusal.
#[test]
fn a_refused_subscription_is_not_asked_again_by_the_next_order() {
    let refusal = crate::instrument::subscription::Failure {
        why: "FXCM said no".to_string(),
        transient: false,
    };
    let asked = std::cell::Cell::new(0);
    let mut memory = crate::instrument::subscription::Attempts::default();
    for round in 1..=3 {
        let (note, was_asked) =
            super::ensure_for_order(false, None, "EUR/AUD", &mut memory, || {
                asked.set(asked.get() + 1);
                Err(refusal.clone())
            });
        assert_eq!(note.as_ref(), Some(&refusal), "round {round}");
        assert_eq!(was_asked, round == 1, "only the first order asks (and so only it logs)");
    }
    assert_eq!(asked.get(), 1);
}

// ---- an exit over several trades, through the exec thread's own glue ---------------------------------------------------
//
// `execution/exit/group_tests.rs` holds the group; these hold what the exec thread does around it: the routes of every leg, the
// folding of the drain, the following of a split trade, and the cancel and the amend that reach the group and drop or
// add the routes of the legs they remove or add.

use crate::execution::exit::ExitLeg;
use crate::execution::exit::group::ExitGroup;
use crate::exit_script::Script;
use vike_bridge_core::exit::ExitKind;

fn stop_leg(order: &str, trade: &str, units: i64) -> ExitLeg {
    ExitLeg {
        order_id: order.into(),
        trade_id: trade.into(),
        units,
        rate: 1.1175,
        account_id: "7".into(),
        offer_id: "1".into(),
        taken_over: false,
    }
}

/// The exec thread's four maps for an exit test: routes, cancel ids, closes in progress, exit groups.
type StopBook = (
    RouteTable,
    HashMap<String, super::PlacedOrder>,
    HashMap<String, CloseTracker>,
    HashMap<String, ExitGroup>,
);

/// What the exec thread holds for ONE stop over three 1000-unit trades: every leg routed to the order, the group, the first
/// leg in `placed`.
fn three_leg_stop() -> StopBook {
    let legs =
        vec![stop_leg("s1", "101", 1000), stop_leg("s2", "102", 1000), stop_leg("s3", "103", 1000)];
    let mut routes = RouteTable::default();
    for leg in &legs {
        routes.insert(leg.order_id.clone(), "c-stop".to_string());
    }
    let mut placed = HashMap::new();
    placed.insert("c-stop".to_string(), legs[0].as_placed());
    let mut exits = HashMap::new();
    exits.insert("c-stop".to_string(), ExitGroup::new(ExitKind::Stop, false, "EUR/USD", legs));
    (routes, placed, HashMap::new(), exits)
}

/// The new quantity the first `OrderModified` among `evs` states (`exit_group_tests` reads it too).
pub(crate) fn modified_to(evs: &[Event]) -> Option<f64> {
    evs.iter().find_map(|e| match e {
        Event::OrderModified(m) => m.new_qty,
        _ => None,
    })
}

/// **Every leg's fill reaches the one order and reads as ONE.** Drop the `exits` branch of `fold_envelope` and the first
/// leg's `OrderFilled` ends the order, so the second is an illegal edge.
#[test]
fn the_legs_of_a_stop_fill_one_order_and_prune_only_at_the_last() {
    let (mut routes, mut placed, mut closes, mut exits) = three_leg_stop();
    let mut fold = |v: serde_json::Value| {
        super::fold_envelope(&v, &mut routes, &mut placed, &mut closes, &mut exits)
    };
    assert_eq!(
        event_names(&fold(fill_envelope("s1", "101-close-s1", "S", 1000))),
        ["Fill", "OrderPartiallyFilled"]
    );
    assert_eq!(
        event_names(&fold(fill_envelope("s3", "103-close-s3", "S", 1000))),
        ["Fill", "OrderPartiallyFilled"]
    );
    assert!(exits.contains_key("c-stop") && placed.contains_key("c-stop"), "still resting");
    let last = super::fold_envelope(
        &fill_envelope("s2", "102-close-s2", "S", 1000),
        &mut routes,
        &mut placed,
        &mut closes,
        &mut exits,
    );
    assert_eq!(event_names(&last), ["Fill", "OrderFilled"]);
    assert!(exits.is_empty() && placed.is_empty(), "the order ended: nothing left to cancel");
    assert!(routes.routes().contains_key("s2"), "a fill keeps its route (a reconnect replays it)");
}

/// The venue removes one leg (its trade closed another way): the leg's route goes, the core is told the cover that remains
/// and the order lives; the last leg ends it cancelled.
#[test]
fn the_venues_removal_of_one_leg_narrows_the_order_and_drops_that_legs_route() {
    let (mut routes, mut placed, mut closes, mut exits) = three_leg_stop();
    let evs = super::fold_envelope(
        &canceled_envelope("s2"),
        &mut routes,
        &mut placed,
        &mut closes,
        &mut exits,
    );
    assert_eq!(modified_to(&evs), Some(2000.0));
    assert!(!routes.routes().contains_key("s2"), "the cancelled leg's route is dead");
    assert!(exits.contains_key("c-stop") && placed.contains_key("c-stop"));
    for last in ["s1", "s3"] {
        let evs = super::fold_envelope(
            &canceled_envelope(last),
            &mut routes,
            &mut placed,
            &mut closes,
            &mut exits,
        );
        if last == "s3" {
            assert_eq!(event_names(&evs), ["OrderCanceled"]);
        }
    }
    assert!(exits.is_empty() && placed.is_empty() && routes.routes().is_empty());
}

/// A closing fill that left a remainder: the group's leg follows to the remainder's id, and the process learns that the
/// remainder is the same trade (the reconcile fill report reads it as the root).
#[test]
fn a_partial_close_is_followed_by_the_legs_and_by_the_trade_roots() {
    let (mut routes, mut placed, mut closes, mut exits) = three_leg_stop();
    routes.insert("close-1".to_string(), "c-close".to_string());
    let mut split = fill_envelope("close-1", "xs-102-close-close-1", "S", 400);
    split["remain_trade_id"] = serde_json::json!("xs-202");
    let evs = super::fold_envelope(&split, &mut routes, &mut placed, &mut closes, &mut exits);
    assert_eq!(event_names(&evs), ["Fill", "OrderFilled"], "the close's own fill is untouched");
    // the envelope's trade id is `<closed trade>-close-<closing order>`: here the closed trade is `xs-102`
    assert_eq!(crate::execution::trade_roots::process().root_of("xs-202"), "xs-102");
    let legs: Vec<&str> = exits["c-stop"].legs().iter().map(|l| l.trade_id.as_str()).collect();
    assert_eq!(legs, ["101", "102", "103"], "no leg covers `xs-102`: none moved");

    // a leg that DOES cover the closed trade moves to the remainder and shrinks to its size
    let mut split = fill_envelope("close-1", "102-close-close-1", "S", 400);
    split["remain_trade_id"] = serde_json::json!("202");
    super::fold_envelope(&split, &mut routes, &mut placed, &mut closes, &mut exits);
    let moved: Vec<(&str, i64)> =
        exits["c-stop"].legs().iter().map(|l| (l.trade_id.as_str(), l.units)).collect();
    assert_eq!(moved, [("101", 1000), ("202", 600), ("103", 1000)]);
    assert_eq!(crate::execution::trade_roots::process().root_of("202"), "102");
}

/// **The venue's trim of a partially closed trade's exits reaches the core** (the demo measurement of 2026-11-09, replayed):
/// a BUY 2000 (trade `187012853`) with a stop and a take-profit of 2000 each, then a reduce-only close of 1000 that left the
/// remainder `187013019`. The venue kept both exits under their order ids at 1000 and said nothing under either, so the core
/// is told ONCE per exit, after the close's own fill, and a replay of the envelope tells it nothing more. Drop the
/// `trimmed` events in `fold_envelope` and the core's stop and take-profit stay at 2000 while the venue holds 1000.
#[test]
fn a_partial_close_tells_the_core_the_size_each_exit_it_cut_now_covers() {
    let leg = |order: &str| stop_leg(order, "187012853", 2000);
    let mut routes = RouteTable::default();
    routes.insert("416261870".to_string(), "c-stop".to_string());
    routes.insert("416261872".to_string(), "c-tp".to_string());
    routes.insert("close-9".to_string(), "c-close".to_string());
    let mut exits = HashMap::new();
    exits.insert(
        "c-stop".to_string(),
        ExitGroup::new(ExitKind::Stop, false, "EUR/USD", vec![leg("416261870")]),
    );
    exits.insert(
        "c-tp".to_string(),
        ExitGroup::new(ExitKind::Limit, false, "EUR/USD", vec![leg("416261872")]),
    );
    let (mut placed, mut closes) = (HashMap::new(), HashMap::new());
    let mut split = fill_envelope("close-9", "187012853-close-close-9", "S", 1000);
    split["remain_trade_id"] = serde_json::json!("187013019");

    let evs = super::fold_envelope(&split, &mut routes, &mut placed, &mut closes, &mut exits);
    assert_eq!(event_names(&evs), ["Fill", "OrderFilled", "other", "other"]);
    let told: Vec<(&str, Option<f64>, Option<f64>)> = evs
        .iter()
        .filter_map(|e| match e {
            Event::OrderModified(m) => Some((m.order_client_id.as_str(), m.new_qty, m.new_price)),
            _ => None,
        })
        .collect();
    assert_eq!(told, [("c-stop", Some(1000.0), None), ("c-tp", Some(1000.0), None)]);
    for order_client_id in ["c-stop", "c-tp"] {
        let legs: Vec<(&str, i64)> =
            exits[order_client_id].legs().iter().map(|l| (l.trade_id.as_str(), l.units)).collect();
        assert_eq!(legs, [("187013019", 1000)], "{order_client_id} follows the remainder");
    }

    // a ForexConnect reconnect replays the closing fill: no exit is cut twice, and the core hears no second resize
    let again = super::fold_envelope(&split, &mut routes, &mut placed, &mut closes, &mut exits);
    assert_eq!(modified_to(&again), None, "{again:?}");
    assert_eq!(exits["c-stop"].legs()[0].units, 1000);
}

/// An envelope that says no remainder, one that is not a closing fill's and one whose remainder is the trade itself leave
/// the group and the table alone.
#[test]
fn an_envelope_with_no_real_remainder_follows_nothing() {
    let (_, _, _, mut exits) = three_leg_stop();
    for (trade, remain) in
        [("102-close-x1", ""), ("102", "202-no-close-suffix"), ("102-close-x1", "102")]
    {
        let mut v = fill_envelope("x1", trade, "S", 400);
        v["remain_trade_id"] = serde_json::json!(remain);
        assert!(super::note_split(&v, &mut exits).is_empty(), "{trade} / {remain}: nothing cut");
    }
    let units: Vec<i64> = exits["c-stop"].legs().iter().map(|l| l.units).collect();
    assert_eq!(units, [1000, 1000, 1000]);
    assert_eq!(
        crate::execution::trade_roots::process().root_of("202-no-close-suffix"),
        "202-no-close-suffix"
    );
}

/// A cancel removes every leg: the routes go (the venue's echo of the node's own cancel is then unroutable), the group is
/// gone and the core hears one cancel.
#[test]
fn a_cancel_of_a_stop_over_three_trades_drops_every_route_and_ends_it() {
    let (mut routes, _placed, _closes, mut exits) = three_leg_stop();
    let mut venue = Script::new("[]", "[]");
    let group = exits.get_mut("c-stop").expect("the group");
    let (evs, gone) = super::cancel_exit(group, &mut routes, "c-stop", &mut venue);
    assert!(gone);
    assert_eq!(event_names(&evs), ["OrderCanceled"], "one cancel, whatever the number of legs");
    assert!(matches!(&evs[0], Event::OrderCanceled(c) if c.order_client_id == "c-stop"), "{evs:?}");
    assert!(routes.routes().is_empty());
    assert_eq!(venue.log, ["delete s1", "delete s2", "delete s3"]);
}

/// A cancel that removes only some: the routes of those go, the order lives on with the cover that remains, and the
/// refusal is the NON-terminal advisory.
#[test]
fn a_cancel_that_removes_some_legs_tells_what_remains_and_refuses_the_rest() {
    let (mut routes, _placed, _closes, mut exits) = three_leg_stop();
    let mut venue = Script::new("[]", "[]");
    venue.delete_refused.insert("s2".into());
    let group = exits.get_mut("c-stop").expect("the group");
    let (evs, gone) = super::cancel_exit(group, &mut routes, "c-stop", &mut venue);
    assert!(!gone);
    assert_eq!(modified_to(&evs), Some(1000.0));
    assert!(
        matches!(evs.last(), Some(Event::OrderCancelRejected(r)) if r.order_client_id == "c-stop"),
        "{evs:?}"
    );
    assert_eq!(routes.routes().keys().cloned().collect::<Vec<_>>(), ["s2"]);
}

/// An amend of the size reaches the group, and the routes follow the legs it removes and adds.
#[test]
fn an_amend_of_the_size_drops_and_adds_the_routes_of_the_legs_it_changes() {
    let (mut routes, _placed, _closes, mut exits) = three_leg_stop();
    let group = exits.get_mut("c-stop").expect("the group");
    let order =
        OrderRequest { order_client_id: "c-stop".into(), qty: 3000.0, ..OrderRequest::default() };
    let mut venue = Script::new("[]", "[]");
    let evs = super::amend_exit(group, &mut routes, &order, Some(1000.0), None, &mut venue);
    assert_eq!(modified_to(&evs), Some(1000.0));
    assert_eq!(routes.routes().keys().cloned().collect::<Vec<_>>(), ["s1"]);

    // and a request this venue cannot read is a non-terminal refusal, nothing sent
    let evs = super::amend_exit(group, &mut routes, &order, None, None, &mut venue);
    assert!(
        matches!(&evs[..], [Event::OrderModifyRejected(m)] if m.reason.contains("names neither"))
    );
    assert_eq!(venue.log, ["delete s3", "delete s2"]);
}

// ---- an exit a previous process placed, after the core re-taught it (a gatewayhub restart) ------------------------------

/// The venue's book as the shim writes it: `trades` long EUR/USD trades of 1000, and `exits` (order, trade, type, rate),
/// each a SELL exit of 1000 with the account and offer ids.
fn venue_book(trades: &[&str], exits: &[(&str, &str, &str, f64)]) -> (String, String) {
    let trades: Vec<serde_json::Value> = trades
        .iter()
        .map(|id| {
            serde_json::json!({"trade_id":id,"instrument":"EUR/USD","buysell":"B","amount":1000,"open_time":46321.5})
        })
        .collect();
    let exits: Vec<serde_json::Value> = exits
        .iter()
        .map(|(order, trade, kind, rate)| {
            serde_json::json!({"order_id":order,"trade_id":trade,"type":kind,"buysell":"S","amount":1000,
                "rate":rate,"account_id":"7","offer_id":"1"})
        })
        .collect();
    (serde_json::Value::from(trades).to_string(), serde_json::Value::from(exits).to_string())
}

/// The Offers table as `forex_connect_offers` writes it: one forex row per `(instrument, offer id)` (no `offer_id` key for `None`,
/// as a shim older than the instrument terms writes it).
fn offers_json(rows: &[(&str, Option<&str>)]) -> String {
    let rows: Vec<serde_json::Value> = rows
        .iter()
        .map(|(instrument, offer_id)| {
            let mut row = serde_json::json!({"instrument":instrument,"bid":1.1,"ask":1.1002,"digits":5,
                "instrument_type":1,"contract_currency":"EUR","contract_multiplier":1.0,"subscription":"T",
                "trading_status":"O","bid_tradable":"T","ask_tradable":"T","base_unit":1000,"quote_age_ms":0});
            if let Some(id) = offer_id {
                row["offer_id"] = serde_json::json!(id);
            }
            row
        })
        .collect();
    serde_json::Value::from(rows).to_string()
}

/// One teaching, `(venue order id, order client id, symbol)` each, as `adopt_restored` hands it to the session thread.
fn teaching(orders: &[(&str, &str, &str)]) -> Vec<vike_execution::RestoredOrder> {
    orders
        .iter()
        .map(|(venue_order_id, order_client_id, symbol)| vike_execution::RestoredOrder {
            venue_order_id: Some(venue_order_id.to_string()),
            ..vike_execution::RestoredOrder::new(*order_client_id, *symbol)
        })
        .collect()
}

/// What the session thread makes of one teaching over `book` (the exits read) and `entry` (the account id and the
/// Offers table, or why they were not read), read the way `run` reads them (the shim's JSON through the bridge's own
/// parsers): its maps, and the reasons of what it could not rebuild.
fn adopt_with(
    taught: Vec<vike_execution::RestoredOrder>,
    book: &(String, String),
    entry: Result<(&str, &str), &str>,
) -> (StopBook, HashMap<String, String>) {
    let (tx, rx) = std::sync::mpsc::channel();
    tx.send(taught).expect("the session thread's lane");
    let (mut routes, mut placed, mut exits, mut unbuilt) =
        (RouteTable::default(), HashMap::new(), HashMap::new(), HashMap::new());
    super::adopt_taught(
        &rx,
        &mut routes,
        &mut placed,
        &mut exits,
        &mut unbuilt,
        || {
            Ok((
                crate::execution::close::parse_open_trades(&book.0)?,
                crate::execution::exit::parse_open_exits(&book.1)?,
            ))
        },
        || {
            let (account, offers) = entry.map_err(str::to_string)?;
            Ok((account.to_string(), crate::account::quote_rate::parse_offers(offers)?))
        },
    );
    ((routes, placed, HashMap::new(), exits), unbuilt)
}

/// [`adopt_with`] of EURUSD orders (`venue order id -> order_client_id`) whose entry reads fail: the exit tests.
fn adopt(taught: &[(&str, &str)], book: &(String, String)) -> StopBook {
    let orders: Vec<(&str, &str, &str)> = taught.iter().map(|(v, c)| (*v, *c, "EURUSD")).collect();
    adopt_with(teaching(&orders), book, Err("not read in this test")).0
}

/// **A cancel of a pre-restart exit deletes it at the venue.** The core re-taught the stop and the take-profit of one
/// trade by their venue order ids; the venue's listing names each one's trade, rate and the account and offer ids a delete
/// needs, so both are rebuilt (in `placed` and as one-leg groups) and are this process's own from then on. The stop's
/// cancel deletes it at the venue and ends it; the take-profit's, refused by the venue, is the NON-terminal advisory in the
/// venue's words and keeps its route. An order the venue lists no exit for is an entry, and its reads fail here: routed,
/// not rebuilt (the entry tests below).
#[test]
fn a_re_taught_exit_is_rebuilt_from_the_listing_and_its_cancel_deletes_it_at_the_venue() {
    let book = venue_book(
        &["187100001"],
        &[("416262401", "187100001", "S", 1.085), ("416262402", "187100001", "L", 1.125)],
    );
    let (mut routes, placed, _closes, mut exits) = adopt(
        &[("416262401", "fxt2fx1001"), ("416262402", "fxt2fx1002"), ("416262403", "fxt2fx1003")],
        &book,
    );
    let ids = |order_client_id: &str| {
        placed
            .get(order_client_id)
            .map(|p| (p.order_id.clone(), p.account_id.clone(), p.offer_id.clone()))
    };
    assert_eq!(ids("fxt2fx1001"), Some(("416262401".into(), "7".into(), "1".into())));
    assert!(ids("fxt2fx1002").is_some() && ids("fxt2fx1003").is_none(), "an entry is not rebuilt");
    assert!(!exits["fxt2fx1001"].unknown_legs() && !exits["fxt2fx1002"].unknown_legs());
    assert!(
        routes.placed_here("416262401"),
        "a second order client id's exit of its rate does not take it over"
    );
    assert!(!routes.placed_here("416262403") && routes.routes().contains_key("416262403"));

    let mut venue = Script::new(&book.0, &book.1);
    let group = exits.get_mut("fxt2fx1001").expect("the stop");
    let (evs, gone) = super::cancel_exit(group, &mut routes, "fxt2fx1001", &mut venue);
    assert!(gone);
    assert!(
        matches!(&evs[..], [Event::OrderCanceled(c)] if c.order_client_id == "fxt2fx1001"),
        "{evs:?}"
    );
    assert_eq!(venue.log, ["delete 416262401"]);
    assert!(!routes.routes().contains_key("416262401"), "the venue's echo of it is unroutable");
    assert_eq!(
        crate::execution::registry::process().order_client_id_of("416262401"),
        None,
        "and the listing names it no more"
    );

    venue.delete_refused.insert("416262402".into());
    let group = exits.get_mut("fxt2fx1002").expect("the take-profit");
    let (evs, gone) = super::cancel_exit(group, &mut routes, "fxt2fx1002", &mut venue);
    assert!(!gone);
    assert!(
        matches!(&evs[..], [Event::OrderCancelRejected(r)] if r.reason.contains("ORA-refused")),
        "the venue's own words: {evs:?}"
    );
    assert!(routes.routes().contains_key("416262402"));
}

/// **One leg of several is never told as the whole order filled.** The core holds the first leg's id of a stop over two
/// trades; the venue lists the other leg too, at the same rate, and nothing names it (it may be this order's or anyone's).
/// So the re-taught leg's fill is a PARTIAL fill and the other exit stays unrouted (its fill publishes nothing, as before
/// the re-teaching). The control: the same stop with no such exit beside it fills the order.
#[test]
fn a_re_taught_exit_beside_an_unnamed_one_like_it_tells_its_fill_as_partial() {
    let two = venue_book(
        &["187100011", "187100012"],
        &[("416262411", "187100011", "S", 1.085), ("416262412", "187100012", "S", 1.085)],
    );
    let (mut routes, mut placed, mut closes, mut exits) =
        adopt(&[("416262411", "fxt2fx1011")], &two);
    assert!(exits["fxt2fx1011"].unknown_legs());
    let mut fold = |v: serde_json::Value| {
        super::fold_envelope(&v, &mut routes, &mut placed, &mut closes, &mut exits)
    };
    let evs = fold(fill_envelope("416262411", "187100011-close-416262411", "S", 1000));
    assert_eq!(event_names(&evs), ["Fill", "OrderPartiallyFilled"]);
    assert!(
        matches!(&evs[1], Event::OrderPartiallyFilled(p) if p.order_client_id == "fxt2fx1011"),
        "{evs:?}"
    );
    assert!(fold(fill_envelope("416262412", "187100012-close-416262412", "S", 1000)).is_empty());
    assert!(exits.contains_key("fxt2fx1011"), "the order lives until the core cancels it");

    // the other leg named by another order of this process is not unknown: the fill is the order's own
    let (_, _, _, exits) = adopt(&[("416262411", "fxt2fx1021"), ("416262412", "fxt2fx1022")], &two);
    assert!(!exits["fxt2fx1021"].unknown_legs() && !exits["fxt2fx1022"].unknown_legs());

    let one = venue_book(&["187100011"], &[("416262411", "187100011", "S", 1.085)]);
    let (mut routes, mut placed, mut closes, mut exits) =
        adopt(&[("416262411", "fxt2fx1031")], &one);
    let evs = super::fold_envelope(
        &fill_envelope("416262411", "187100011-close-416262411", "S", 1000),
        &mut routes,
        &mut placed,
        &mut closes,
        &mut exits,
    );
    assert_eq!(event_names(&evs), ["Fill", "OrderFilled"]);
    assert!(exits.is_empty() && placed.is_empty(), "the order ended");
}

/// An exits listing that cannot be read rebuilds nothing (it is not known whether the order is an exit or an entry; the
/// routes stand, as before the rebuild) and says why at the cancel; an order this thread already holds is never
/// overwritten by a teaching of it.
#[test]
fn a_teaching_rebuilds_nothing_over_an_unread_listing_or_an_order_held_here() {
    let (tx, rx) = std::sync::mpsc::channel();
    let one = || teaching(&[("416262441", "fxt2fx1041", "EURUSD")]);
    tx.send(one()).expect("lane");
    let (mut routes, mut placed, mut exits, mut unbuilt) =
        (RouteTable::default(), HashMap::new(), HashMap::new(), HashMap::new());
    super::adopt_taught(
        &rx,
        &mut routes,
        &mut placed,
        &mut exits,
        &mut unbuilt,
        || Err("timeout".to_string()),
        || panic!("an order that may be an exit is never read as an entry"),
    );
    assert!(routes.routes().contains_key("416262441") && placed.is_empty() && exits.is_empty());
    assert!(super::nothing_to_act_on(&unbuilt, "fxt2fx1041").contains("not known whether"));

    let book = venue_book(&["187100041"], &[("416262441", "187100041", "S", 1.085)]);
    placed.insert("fxt2fx1041".to_string(), exit_order("416262441"));
    tx.send(one()).expect("lane");
    super::adopt_taught(
        &rx,
        &mut routes,
        &mut placed,
        &mut exits,
        &mut unbuilt,
        || {
            Ok((
                crate::execution::close::parse_open_trades(&book.0)?,
                crate::execution::exit::parse_open_exits(&book.1)?,
            ))
        },
        || panic!("nothing to read"),
    );
    assert!(exits.is_empty(), "held here already: left as it is");
}

/// **A cancel of a pre-restart resting ENTRY deletes it at the venue.** The exits read lists neither order, so both are
/// entries: each is rebuilt from the login's account id and the offer id of its own symbol's Offers row, and a cancel
/// sends `forex_connect_delete_order` with exactly those ids and ends the order; a venue refusal is the NON-terminal advisory in
/// the venue's words and the order stays held.
#[test]
fn a_re_taught_entry_is_rebuilt_from_the_account_and_offers_and_its_cancel_deletes_it_at_the_venue()
{
    let offers = offers_json(&[("EUR/USD", Some("1")), ("GBP/USD", Some("2"))]);
    let ((mut routes, mut placed, _, exits), unbuilt) = adopt_with(
        teaching(&[("416262501", "fxt2fx1101", "EURUSD"), ("416262502", "fxt2fx1102", "GBPUSD")]),
        &venue_book(&[], &[]),
        Ok(("7", &offers)),
    );
    assert!(exits.is_empty() && unbuilt.is_empty(), "{unbuilt:?}");
    let ids =
        |p: &super::PlacedOrder| (p.order_id.clone(), p.account_id.clone(), p.offer_id.clone());
    assert_eq!(ids(&placed["fxt2fx1102"]), ("416262502".into(), "7".into(), "2".into()));

    let mut asked = Vec::new();
    let event = super::cancel_resting(&mut placed, &mut routes, &unbuilt, "fxt2fx1101", |p| {
        asked.push(ids(p));
        Ok(())
    });
    assert!(
        matches!(&event, Event::OrderCanceled(c) if c.order_client_id == "fxt2fx1101"),
        "{event:?}"
    );
    assert_eq!(asked, [("416262501".to_string(), "7".to_string(), "1".to_string())]);
    assert!(!placed.contains_key("fxt2fx1101") && !routes.routes().contains_key("416262501"));

    let event = super::cancel_resting(&mut placed, &mut routes, &unbuilt, "fxt2fx1102", |_| {
        Err(crate::transport::sys::FxcmError::Native {
            code: -6,
            message: "ORA-20103: order not found".into(),
        })
    });
    assert!(
        matches!(&event, Event::OrderCancelRejected(r) if r.reason.contains("ORA-20103")),
        "the venue's own words: {event:?}"
    );
    assert!(placed.contains_key("fxt2fx1102") && routes.routes().contains_key("416262502"));
}

/// **An entry that cannot be rebuilt is refused in words that say why** (the Offers read failed, its symbol has no row,
/// the row has no offer id from a shim older than the instrument terms), never with the bare "no resting order", and
/// nothing is sent; an amend reads the same words. An order no teaching named keeps the three-case sentence.
#[test]
fn a_re_taught_entry_that_cannot_be_rebuilt_is_refused_in_words() {
    let refused = |entry: Result<(&str, &str), &str>, symbol: &str, says: &str| {
        let ((mut routes, mut placed, _, _), unbuilt) = adopt_with(
            teaching(&[("416262601", "fxt2fx1201", symbol)]),
            &venue_book(&[], &[]),
            entry,
        );
        assert!(placed.is_empty(), "{says}");
        let event = super::cancel_resting(&mut placed, &mut routes, &unbuilt, "fxt2fx1201", |_| {
            unreachable!("nothing is sent for an order that was not rebuilt")
        });
        match &event {
            Event::OrderCancelRejected(r) => {
                assert!(r.reason.contains(says) && r.reason.contains("by hand"), "{says}: {r:?}");
                assert!(!r.reason.contains("never placed here"), "{r:?}");
            }
            other => panic!("{says}: {other:?}"),
        }
        assert!(super::nothing_to_act_on(&unbuilt, "fxt2fx1201").contains(says));
    };
    refused(Err("the Offers table could not be read: timeout"), "EURUSD", "timeout");
    let with_ids = offers_json(&[("EUR/USD", Some("1"))]);
    refused(Ok(("7", &with_ids)), "USDZAR", "not in the venue's Offers table");
    let without = offers_json(&[("EUR/USD", None)]);
    refused(Ok(("7", &without)), "EURUSD", "carries no offer id");
    assert!(super::nothing_to_act_on(&HashMap::new(), "c-x").contains("never placed here"));
}
