use super::{
    ConfirmRequest, ExecCommand, MAX_ROUTES, MAX_UNKNOWN, RouteTable, Spell, UnknownTable, coid_of,
    open_session, refuse_every_command, to_fxcm_instrument,
};
use crate::event_mapper::map_fxcm_event;
use std::time::{Duration, Instant};
use vike_exec::CancelIntent;
use vike_exec::Ingest;
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

    assert_eq!(refuse_every_command(&events, &rx, REASON, far(), || {}), Spell::Stop);

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
    let (events, _ingest) = vike_exec::event_channel(8);
    let (_tx, rx) = std::sync::mpsc::channel::<ExecCommand>();
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

fn submit(coid: &str) -> ExecCommand {
    ExecCommand::Submit(Box::new(OrderRequest {
        client_order_id: coid.into(),
        venue: "fxcm".into(),
        symbol: "EURUSD".into(),
        side: 1,
        qty: 1000.0,
        order_type: vike_model::OrderType::Limit,
        ts: 7,
        ..Default::default()
    }))
}

fn rejection_of<'a>(seen: &'a [Event], coid: &str) -> &'a str {
    seen.iter()
        .find_map(|ev| match ev {
            Event::OrderRejected(e) if e.client_order_id == coid => Some(e.reason.as_str()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no OrderRejected for {coid} in {seen:?}"))
}

fn drained(ingest: &mut tokio::sync::mpsc::Receiver<Ingest>) -> Vec<Event> {
    let mut seen = Vec::new();
    while let Ok(Ingest::Event(ev)) = ingest.try_recv() {
        seen.push(ev);
    }
    seen
}

/// **A session that failed to open is retried, and the one that finally opens is used.** Attempt 1
/// and 2 fail (the second with another reason), attempt 3 opens. Across them: a command sent while
/// down is refused with the LATEST reason and the SESSION_UNAVAILABLE marker (the live smoke relies
/// on it); one that queued during the login that succeeded is refused too, not placed (the watchdog
/// may already have timed it out); and the thread keeps serving afterwards.
#[test]
fn a_session_that_failed_to_open_is_retried_until_it_opens() {
    let (events, mut ingest) = vike_exec::event_channel(64);
    let (tx, rx) = std::sync::mpsc::channel::<ExecCommand>();
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
    let seen = drained(&mut ingest);
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
    let (events, mut ingest) = vike_exec::event_channel(8);
    let (tx, rx) = std::sync::mpsc::channel::<ExecCommand>();
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
    assert!(drained(&mut ingest).is_empty(), "nothing is refused when the first login works");
    assert!(
        matches!(rx.try_recv(), Ok(ExecCommand::Submit(_))),
        "...and the command is still queued"
    );
}

/// Run `open_session` over an opener that ALWAYS fails with `failure`, pausing 20 ms between attempts, and
/// shut the thread down 300 ms in. Returns how many logins were attempted and what the one queued
/// command was refused with.
fn attempts_before_shutdown(failure: &'static str) -> (u32, String) {
    let (events, mut ingest) = vike_exec::event_channel(64);
    let (tx, rx) = std::sync::mpsc::channel::<ExecCommand>();
    let (_confirm_tx, confirms) = std::sync::mpsc::channel::<ConfirmRequest>();
    tx.send(submit("c-refused")).expect("the loop holds the receiver");
    let stopper = tx.clone();
    let stop = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        stopper.send(ExecCommand::Shutdown).expect("the loop is still waiting");
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
        |_| Duration::from_millis(20),
    );
    stop.join().expect("the stopper thread");
    assert_eq!(opened, None);
    let seen = drained(&mut ingest);
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

/// `Shutdown` ends the wait between attempts at once and no login follows it: `ExecActor::detach`
/// joins this thread, and the shipped pause is minutes.
#[test]
fn a_shutdown_during_the_pause_leaves_without_another_login() {
    let (events, _ingest) = vike_exec::event_channel(8);
    let (tx, rx) = std::sync::mpsc::channel::<ExecCommand>();
    let (_confirm_tx, confirms) = std::sync::mpsc::channel::<ConfirmRequest>();
    tx.send(ExecCommand::Shutdown).expect("the loop holds the receiver");
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
    let (events, _ingest) = vike_exec::event_channel(8);
    let (_tx, rx) = std::sync::mpsc::channel::<ExecCommand>();
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

// ---- the wiring of a close, driven by SCRIPTED shim envelopes (what `drain_events` does with each one) ----------------
//
// The pure pieces (`close.rs`) have their own tests; these hold the glue the exec thread owns: that every further leg's
// closing order id is ROUTED to the order, that the order's tracker is REGISTERED and read by the drain, and the pruning
// the drain does around it. Before they existed that glue was proven by the ignored live smoke alone.

use crate::close::CloseTrack;
use std::collections::HashMap;

/// One shim fill envelope, in the grammar `fcshim.cpp`'s `pushFill` writes.
fn fill_envelope(order_id: &str, trade_id: &str, side: &str, amount: i64) -> serde_json::Value {
    serde_json::json!({"kind":"fill","order_id":order_id,"trade_id":trade_id,"instrument":"EUR/USD",
        "side":side,"amount":amount,"rate":1.1,"commission":0.0,"ts":0})
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
) -> (RouteTable, HashMap<String, super::PlacedOrder>, HashMap<String, CloseTrack>) {
    let mut routes = RouteTable::default();
    let placed = HashMap::new();
    let mut closes = HashMap::new();
    routes.insert("o1".to_string(), "c-close".to_string());
    super::register_close(
        &mut routes,
        &mut closes,
        "c-close",
        vec!["o2".to_string()],
        CloseTrack::new(sent, stopped),
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

    let first = super::fold_envelope(
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

    let second = super::fold_envelope(
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
    assert_eq!(coid_of(&second), Some("c-close"));
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
    let evs = super::fold_envelope(
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
    let evs = super::fold_envelope(
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
    let evs = super::fold_envelope(
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
    serde_json::json!({"kind":"canceled","order_id":order_id,"ts":0})
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

    let evs = super::fold_envelope(
        &canceled_envelope("416154901"),
        &mut routes,
        &mut placed,
        &mut closes,
    );
    assert_eq!(event_names(&evs), ["OrderCanceled"]);
    assert_eq!(coid_of(&evs), Some("take-profit"));
    assert!(!placed.contains_key("take-profit") && !routes.routes().contains_key("416154901"));
    assert!(placed.contains_key("stop") && routes.routes().contains_key("416154904"));

    let evs = super::fold_envelope(
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
    assert_eq!(coid_of(&evs), Some("stop"));
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
    let evs = super::fold_envelope(
        &canceled_envelope("416154901"),
        &mut routes,
        &mut placed,
        &mut closes,
    );
    assert!(evs.is_empty());
    assert!(placed.contains_key("stop") && routes.routes().contains_key("416154904"));
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

// ---- the reason an instrument could not be subscribed rides on the order's refusal ---------------------

fn unknown_note() -> crate::subscription::Failure {
    crate::subscription::Failure {
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
    let definite = crate::subscription::Failure { why: "refused".to_string(), transient: false };
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
    let mut memory = crate::subscription::Attempts::default();

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
    let refusal =
        crate::subscription::Failure { why: "FXCM said no".to_string(), transient: false };
    let asked = std::cell::Cell::new(0);
    let mut memory = crate::subscription::Attempts::default();
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
