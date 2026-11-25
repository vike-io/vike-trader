//! The sidecar is SUPERVISED: a JForex sidecar that dies after `ready` is started again by the client itself
//! (measured 2026-10-26: the real one died at 08:15:03 UTC on `JForex session disconnected` and nothing started
//! another for 2.5 hours while the node still counted the account as running).
//!
//! Driven through real child processes of the scripted fake bridge (`src/bin/fake_jforex_bridge.rs`: `die-once`,
//! `die-then-refuse`, `die-then-hang`, and for the idle limit `wedge-once` and `quiet`, each counting its launches in a
//! log file), with a restart policy and a liveness lane in milliseconds. What these prove is the Rust half: the client reports the sidecar DOWN, refuses every command by
//! name while it is, rejects an in-flight order exactly once, backs off within bounds, starts the next sidecar only
//! after the dead one is reaped, serves a submit after the restart, and stops the loop when it is detached. They do
//! not prove the JForex half (what a real session loss looks like, whether the venue lets a login back in): that is
//! the live check in `crates/bridges/dukascopy/CLAUDE.md`'s "The sidecar is restarted by itself".

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::support::{BRIDGE, config, drain, engine_holding, launches, resting_stop, wait_until};
use vike_bridge_core::venue_link::{VenueLinkDown, VenueLinkState, venue_link};
use vike_dukascopy::ExecutionClientDukascopy;
use vike_dukascopy::feed::QuoteBus;
use vike_dukascopy::reconcile::query::PROBE_ID;
use vike_dukascopy::test_support::order;
use vike_execution::testing::{RecordingClient, recv_event};
use vike_execution::{
    Engine, EventBusFold, EventBusOutbox, EventHandler, ExecutionClient, Ingest, event_channel,
};
use vike_model::events::Event;
use vike_model::{OrderRequest, OrderStatus};
use vike_venues::venue_timings::{VenueTimingsLane, VenueTimingsReconnect, VenueTimingsValue};

/// A restart policy in milliseconds: 40 ms, doubling to 160 ms.
const FAST: VenueTimingsReconnect =
    VenueTimingsReconnect::new(Duration::from_millis(40), Duration::from_millis(160), "a test");

fn resting_limit(order_client_id: &str) -> OrderRequest {
    OrderRequest {
        order_type: vike_model::OrderType::Limit,
        price: Some(1.1),
        ..order(order_client_id)
    }
}

/// The ladder a healthy sidecar gives a market order: Submitted, Accepted, the bare Fill, the OrderFilled wrap.
fn expect_filled(rx: &mut tokio::sync::mpsc::Receiver<Ingest>, order_client_id: &str) {
    match recv_event(rx) {
        Event::OrderSubmitted(e) => assert_eq!(e.order_client_id, order_client_id),
        other => panic!("expected OrderSubmitted for {order_client_id}, got {other:?}"),
    }
    match recv_event(rx) {
        Event::OrderAccepted(e) => assert_eq!(e.order_client_id, order_client_id),
        other => panic!("expected OrderAccepted for {order_client_id}, got {other:?}"),
    }
    match recv_event(rx) {
        Event::Fill(f) => assert_eq!(f.order_client_id, order_client_id),
        other => panic!("expected the bare Fill for {order_client_id}, got {other:?}"),
    }
    match recv_event(rx) {
        Event::OrderFilled(e) => assert_eq!(e.order_client_id, order_client_id),
        other => panic!("expected OrderFilled for {order_client_id}, got {other:?}"),
    }
}

fn scratch(tag: &str) -> vike_paths::scratch::ScratchDir {
    vike_paths::scratch::ScratchDir::create_in(&std::env::temp_dir(), tag).expect("scratch dir")
}

/// THE regression test for the incident: a sidecar that dies while idle is started again by itself, and the
/// first submit after it is filled. On the base (no supervisor) the client stays dead and the submit is rejected.
#[test]
fn a_sidecar_that_dies_idle_is_restarted_by_itself_and_a_later_submit_is_filled() {
    let dir = scratch("vike-duka-restart-idle");
    let log = dir.path().join("launches.txt");
    let (events, mut rx) = event_channel(64);
    let mut client = ExecutionClientDukascopy::spawn_with_restart(
        BRIDGE,
        &["die-once".into(), log.display().to_string()],
        &config(),
        events,
        FAST,
    )
    .expect("the first launch reaches ready");
    assert_eq!(client.sidecar_state(), VenueLinkState::Up);

    // The first launch dies on its first command, the way the real sidecar does when JForex ends the session.
    client.cancel("DIE");

    // A second launch exists only because the supervisor made it; `Up` after that is the new sidecar's ready.
    wait_until("the second sidecar to be up", || {
        launches(&log).len() == 2 && client.sidecar_state() == VenueLinkState::Up
    });

    client.submit(&order("c1"));
    expect_filled(&mut rx, "c1");
    assert_eq!(launches(&log).len(), 2, "one death, one restart, and no more");
    client.detach();
}

/// `state` with its wait zeroed: the cell counts a `retry_in` down on every read, so two readings equal each other only
/// once the wait is set aside.
fn waitless(state: VenueLinkState) -> VenueLinkState {
    match state {
        VenueLinkState::Down(down) => {
            VenueLinkState::Down(VenueLinkDown { retry_in: Duration::ZERO, ..down })
        }
        other => other,
    }
}

/// **The client's state is on the account's cell the gatewayhub's lane reads** (the start's route key's,
/// `vike_bridge_core::venue_link::venue_link`): `Down` with the sidecar's own words from the death until the next
/// sidecar's `ready`, `Up` before and after, the same answer as the client's own `sidecar_state` (its wait counted down,
/// never more than said), and a NEW client on the same account starts from `Unknown` and reads `Up` at its first
/// `ready`, never the last one's outage (the cell outlives its clients). Under the production policy, whose first wait
/// (5 s) is long enough to read.
#[test]
fn the_clients_sidecar_state_is_on_the_cell_the_gatewayhub_lane_reads() {
    const ROUTE: &str = "dukascopy#RESTARTCELL";
    let dir = scratch("vike-duka-restart-bus");
    let log = dir.path().join("launches.txt");
    let (mut events, _rx) = event_channel(64);
    events.route_key = Some(vike_marketdata::Symbol::new(ROUTE));
    let bus = QuoteBus::new();
    let mut client = ExecutionClientDukascopy::spawn_with_program_quoting(
        BRIDGE,
        &["die-once".into(), log.display().to_string()],
        &config(),
        events,
        Arc::clone(&bus),
    )
    .expect("the first launch reaches ready");
    assert_eq!(venue_link(ROUTE).state(), VenueLinkState::Up);

    client.cancel("DIE");
    wait_until("the death to reach the cell", || {
        matches!(venue_link(ROUTE).state(), VenueLinkState::Down(_))
    });
    match venue_link(ROUTE).state() {
        VenueLinkState::Down(VenueLinkDown { why, failed_restarts, retry_in, effect }) => {
            assert!(why.contains("JForex session disconnected"), "the sidecar's own reason: {why}");
            // The death is the first failure (`Error`, said at once), and no restart failed yet.
            assert_eq!(failed_restarts, 1, "the death alone");
            assert!(retry_in <= Duration::from_secs(5), "counted down: {retry_in:?}");
            assert_eq!(effect, "", "no effect words");
        }
        VenueLinkState::Up | VenueLinkState::Unknown => unreachable!("waited for Down"),
    }
    assert_eq!(
        waitless(venue_link(ROUTE).state()),
        waitless(client.sidecar_state()),
        "one state, read two ways"
    );

    wait_until("the cell to say the sidecar is back", || {
        venue_link(ROUTE).state() == VenueLinkState::Up
    });
    assert_eq!(client.sidecar_state(), VenueLinkState::Up);
    client.detach();

    // A client started on an account whose cell still holds an earlier client's outage does not inherit it.
    venue_link(ROUTE).set(
        VenueLinkState::Down(VenueLinkDown {
            why: "an earlier client's outage".to_string(),
            failed_restarts: 3,
            retry_in: Duration::from_secs(40),
            effect: "",
        }),
        None,
    );
    let (mut events, _rx) = event_channel(64);
    events.route_key = Some(vike_marketdata::Symbol::new(ROUTE));
    let mut second = ExecutionClientDukascopy::spawn_with_program_quoting(
        BRIDGE,
        &["die-once".into(), dir.path().join("second.txt").display().to_string()],
        &config(),
        events,
        Arc::clone(&bus),
    )
    .expect("the second client's first launch reaches ready");
    assert_eq!(
        venue_link(ROUTE).state(),
        VenueLinkState::Up,
        "a new client does not inherit the last one's outage"
    );
    second.detach();
}

/// **Nothing known yet is not up** (E18, owner ruling U-6): the cell of an account no client has started reads
/// `Unknown`, where the process's bus once said `Up` before any client, and so says no start failure.
#[test]
fn a_cell_no_client_has_started_reads_unknown() {
    const ROUTE: &str = "dukascopy#NEVERSTARTED";
    assert_eq!(venue_link(ROUTE).state(), VenueLinkState::Unknown);
    assert_eq!(vike_dukascopy::feed::start_failure(ROUTE), None);
}

/// The same incident under the PRODUCTION policy (`DUKASCOPY`'s row: 5 s, doubling to 60 s), end to end through the
/// public events only: after a sidecar's death the next orders are refused and, within the first wait and the
/// start, one is FILLED. Written against nothing but `spawn_with_program` and the event lane, so the very same
/// body runs on the sources before the supervisor and fails there by assertion (no order is ever filled).
#[test]
fn the_production_policy_brings_a_dead_sidecar_back_within_its_first_wait() {
    let dir = scratch("vike-duka-restart-prod");
    let log = dir.path().join("launches.txt");
    let (events, mut rx) = event_channel(256);
    let mut client = ExecutionClientDukascopy::spawn_with_program(
        BRIDGE,
        &["die-once".into(), log.display().to_string()],
        &config(),
        events,
    )
    .expect("the first launch reaches ready");

    client.cancel("DIE");
    let end = Instant::now() + Duration::from_secs(25);
    let mut filled = false;
    let mut n = 0;
    while !filled && Instant::now() < end {
        n += 1;
        client.submit(&order(&format!("p{n}")));
        std::thread::sleep(Duration::from_millis(400));
        filled = drain(&mut rx).iter().any(|e| matches!(e, Event::OrderFilled(_)));
    }
    assert!(filled, "no order was filled in 25 s after the sidecar died ({n} submits)");
    assert_eq!(launches(&log).len(), 2, "one death, one restart");
    client.detach();
}

/// While no sidecar runs the client is DOWN and says so: a submit is a terminal reject, a cancel and an amend are
/// non-terminal refusals, all by name, and nothing is sent to anything (no `OrderSubmitted`, no new launch yet).
#[test]
fn while_the_sidecar_is_down_every_command_is_refused_by_name() {
    let dir = scratch("vike-duka-restart-down");
    let log = dir.path().join("launches.txt");
    let (events, mut rx) = event_channel(64);
    // A first wait long enough to act inside the outage.
    let slow =
        VenueTimingsReconnect::new(Duration::from_millis(1500), Duration::from_secs(3), "a test");
    let mut client = ExecutionClientDukascopy::spawn_with_restart(
        BRIDGE,
        &["die-once".into(), log.display().to_string()],
        &config(),
        events,
        slow,
    )
    .expect("ready");

    client.cancel("DIE");
    wait_until("the death to be noticed", || {
        matches!(client.sidecar_state(), VenueLinkState::Down(_))
    });
    match client.sidecar_state() {
        VenueLinkState::Down(VenueLinkDown { why, failed_restarts, retry_in, .. }) => {
            assert!(why.contains("JForex session disconnected"), "the sidecar's own reason: {why}");
            assert_eq!(failed_restarts, 1, "the death alone");
            assert_eq!(retry_in, Duration::from_millis(1500));
        }
        VenueLinkState::Up | VenueLinkState::Unknown => unreachable!("waited for Down"),
    }

    client.submit(&order("c1"));
    match recv_event(&mut rx) {
        Event::OrderRejected(e) => {
            assert_eq!(e.order_client_id, "c1");
            assert!(e.reason.starts_with("bridge unavailable"), "{}", e.reason);
            assert!(e.reason.contains("JForex session disconnected"), "{}", e.reason);
            assert!(e.reason.contains("NOT sent"), "{}", e.reason);
        }
        other => panic!("the FIRST event of a refused submit is its rejection, got {other:?}"),
    }
    client.cancel("c1");
    match recv_event(&mut rx) {
        Event::OrderCancelRejected(r) => {
            assert_eq!(r.order_client_id, "c1");
            assert!(
                r.reason.contains("bridge unavailable") && r.reason.contains("NOT sent"),
                "{}",
                r.reason
            );
        }
        other => panic!("a cancel while down is refused by name, got {other:?}"),
    }
    client.modify(&resting_limit("c1"), None, Some(1.2));
    match recv_event(&mut rx) {
        Event::OrderModifyRejected(r) => {
            assert_eq!(r.order_client_id, "c1");
            assert!(
                r.reason.contains("bridge unavailable") && r.reason.contains("NOT sent"),
                "{}",
                r.reason
            );
        }
        other => panic!("an amend while down is refused by name, got {other:?}"),
    }
    assert_eq!(launches(&log).len(), 1, "nothing started the next sidecar before its wait");

    // ...and it comes back by itself, after which the same submit works.
    wait_until("the sidecar to be back", || client.sidecar_state() == VenueLinkState::Up);
    client.submit(&order("c2"));
    expect_filled(&mut rx, "c2");
    client.detach();
}

/// A resting LIMIT in flight when the sidecar dies is rejected EXACTLY ONCE (the reader's drain and the failed write
/// both take it out of the in-flight set first; its venue state is unknown by design, so it keeps the rejection a
/// market order no longer gets once accepted, see the entry test below); the restarted sidecar does not know it, so a
/// cancel of it is refused by name instead of being skipped in silence by Java; a new order is served.
#[test]
fn a_death_with_an_order_in_flight_rejects_it_once_and_the_new_sidecar_is_not_asked_about_it() {
    let dir = scratch("vike-duka-restart-inflight");
    let log = dir.path().join("launches.txt");
    let (events, mut rx) = event_channel(64);
    let mut client = ExecutionClientDukascopy::spawn_with_restart(
        BRIDGE,
        &["die-once".into(), log.display().to_string()],
        &config(),
        events,
        FAST,
    )
    .expect("ready");

    // Accepted, then the sidecar dies without a terminal event.
    client.submit(&resting_limit("c1"));
    let mut seen = Vec::new();
    for _ in 0..3 {
        seen.push(recv_event(&mut rx));
    }
    assert!(matches!(&seen[0], Event::OrderSubmitted(e) if e.order_client_id == "c1"), "{seen:?}");
    assert!(matches!(&seen[1], Event::OrderAccepted(e) if e.order_client_id == "c1"), "{seen:?}");
    match &seen[2] {
        Event::OrderRejected(e) => {
            assert_eq!(e.order_client_id, "c1");
            assert!(e.reason.starts_with("bridge died"), "{}", e.reason);
        }
        other => panic!("the death must reject the in-flight order, got {other:?}"),
    }

    wait_until("the second sidecar to be up", || {
        launches(&log).len() == 2 && client.sidecar_state() == VenueLinkState::Up
    });

    client.cancel("c1");
    match recv_event(&mut rx) {
        Event::OrderCancelRejected(r) => {
            assert_eq!(r.order_client_id, "c1");
            assert!(r.reason.contains("restarted after this order was placed"), "{}", r.reason);
        }
        other => panic!("a cancel of a stranded order is refused by name, got {other:?}"),
    }
    client.modify(&resting_limit("c1"), None, Some(1.2));
    match recv_event(&mut rx) {
        Event::OrderModifyRejected(r) => {
            assert!(r.reason.contains("restarted after this order was placed"), "{}", r.reason);
            assert_eq!(r.refusal_id.as_deref(), Some(vike_model::OrderRefusalId::NotSent.word()));
        }
        other => panic!("an amend of a stranded order is refused by name, got {other:?}"),
    }
    client.submit(&order("c2"));
    expect_filled(&mut rx, "c2");
    client.detach();

    // Nothing else was said about c1: the one rejection is the only terminal it ever got.
    let rest = drain(&mut rx);
    let again = rest
        .iter()
        .filter(|e| matches!(e, Event::OrderRejected(r) if r.order_client_id == "c1"))
        .count();
    assert_eq!(again, 0, "c1 was rejected a second time: {rest:?}");
}

/// Fold `event` into the real engine and say whether the order FSM took it.
fn folds(engine: &mut Engine<RecordingClient>, event: &Event) -> bool {
    matches!(engine.on_event(event, &mut EventBusOutbox::default()), EventBusFold::Applied)
}

fn status(engine: &Engine<RecordingClient>, order_client_id: &str) -> OrderStatus {
    engine.registry[order_client_id].status
}

/// **A native stop-loss resting at the venue when its sidecar dies ends `OrderCanceled`, in words, and nothing takes
/// it off at the venue.** The defect (read in the code, v0.1.93): the EOF drain sent `OrderRejected "bridge died"` for
/// every order client id in flight, the stop's included, and the core folds a rejection only from `Initialized`/`Submitted`: on
/// the `Accepted` stop it was DROPPED (`closing_events_dropped_order_still_live`), the stop stayed `Accepted` on the node for ever,
/// every cancel of it was refused as stranded, and the venue's stop-loss, firing later, was never heard. Driven here
/// through the real engine's order FSM, so the assertion is what the node ends up BELIEVING, not what the bridge
/// sent. The fake keeps the venue's stop-losses in a file that outlives its launches and logs every cancel a launch
/// receives: after the restart the venue still holds the stop and no cancel was ever sent for it (a restart never
/// cancels a resting order; this changes the node's belief only).
#[test]
fn a_resting_stop_loss_at_a_sidecars_death_ends_canceled_in_words_and_the_venue_keeps_it() {
    let dir = scratch("vike-duka-restart-stop");
    let log = dir.path().join("launches.txt");
    let (events, mut rx) = event_channel(64);
    let mut client = ExecutionClientDukascopy::spawn_with_restart(
        BRIDGE,
        &["stop-die-once".into(), log.display().to_string()],
        &config(),
        events,
        FAST,
    )
    .expect("ready");
    let stop = resting_stop("s1");
    let mut engine = engine_holding(&stop);

    // The venue takes the stop-loss: submitted, then accepted (resting).
    client.submit(&stop);
    for _ in 0..2 {
        let event = recv_event(&mut rx);
        assert!(folds(&mut engine, &event), "{event:?}");
    }
    assert_eq!(status(&engine, "s1"), OrderStatus::Accepted);

    // The sidecar dies with the stop-loss resting.
    client.cancel("DIE");
    let ended = recv_event(&mut rx);
    match &ended {
        Event::OrderCanceled(c) => {
            assert_eq!(c.order_client_id, "s1");
            assert!(c.reason.starts_with("bridge died"), "{}", c.reason);
            assert!(
                c.reason.contains("STILL hold the stop-loss"),
                "the venue may keep it: {}",
                c.reason
            );
            assert!(c.reason.contains("check the JForex platform"), "{}", c.reason);
        }
        other => panic!("a resting stop-loss ends with a cancel the core accepts, got {other:?}"),
    }
    assert!(folds(&mut engine, &ended), "the order FSM must take the end of an Accepted stop");
    assert_eq!(
        status(&engine, "s1"),
        OrderStatus::Canceled,
        "the node no longer believes it is protected"
    );
    assert_eq!(
        engine.closing_events_dropped_order_still_live, 0,
        "no terminal was dropped on a live order"
    );

    wait_until("the second sidecar to be up", || {
        launches(&log).len() == 2 && client.sidecar_state() == VenueLinkState::Up
    });

    // The new sidecar does not know the stop: a cancel of it is refused by name and never reaches it.
    client.cancel("s1");
    match recv_event(&mut rx) {
        Event::OrderCancelRejected(r) => {
            assert_eq!(r.order_client_id, "s1");
            assert!(r.reason.contains("restarted after this order was placed"), "{}", r.reason);
        }
        other => panic!("a cancel of a stranded stop is refused by name, got {other:?}"),
    }
    client.detach();

    // The venue still holds the stop-loss, and the only cancel any launch ever received is the one that killed it.
    let book =
        std::fs::read_to_string(format!("{}.book", log.display())).expect("the venue's book");
    assert_eq!(
        book.lines().collect::<Vec<_>>(),
        ["s1"],
        "the venue's stop-loss survived the restart"
    );
    let cancels =
        std::fs::read_to_string(format!("{}.cancels", log.display())).expect("cancel log");
    assert_eq!(
        cancels.lines().collect::<Vec<_>>(),
        ["DIE"],
        "nothing cancelled the stop at the venue"
    );
    let again = drain(&mut rx)
        .iter()
        .filter(|e| matches!(e, Event::OrderRejected(r) if r.order_client_id == "s1"))
        .count();
    assert_eq!(again, 0, "the stop got one end, the cancel");
}

/// A stop the sidecar never answered may have reached the venue all the same: its outcome is unknown, so the death
/// rejects it no more (decision 0303). It stays `Submitted` on the node, for the stuck-order ladder (this jar does not
/// adopt, so no sidecar can read the venue for it), and a cancel of it is refused by name.
#[test]
fn a_stop_the_sidecar_never_answered_is_not_rejected_at_the_death() {
    let dir = scratch("vike-duka-restart-stop-unanswered");
    let log = dir.path().join("launches.txt");
    let (events, mut rx) = event_channel(64);
    let mut client = ExecutionClientDukascopy::spawn_with_restart(
        BRIDGE,
        &["stop-die-once".into(), log.display().to_string()],
        &config(),
        events,
        FAST,
    )
    .expect("ready");
    let stop = resting_stop("unanswered");
    let mut engine = engine_holding(&stop);

    client.submit(&stop);
    let submitted = recv_event(&mut rx);
    assert!(folds(&mut engine, &submitted), "{submitted:?}");
    assert_eq!(status(&engine, "unanswered"), OrderStatus::Submitted);

    client.cancel("DIE");
    left_submitted_at_the_death(&mut client, &mut rx, &log, &engine);
    client.detach();
    assert!(
        std::fs::read_to_string(format!("{}.book", log.display())).is_err(),
        "the venue never took it"
    );
}

/// A reduce-only MARKET sell: the flatten, the margin call, the market exit, the liquidation.
fn reduce_only_close(order_client_id: &str) -> OrderRequest {
    OrderRequest { side: -1, reduce_only: true, ..order(order_client_id) }
}

/// **A reduce-only close the sidecar had taken up when it died ends `OrderCanceled`, in words, and the core takes it.**
/// The defect (read in the code, v0.1.95): the sidecar answers a close with `OrderAccepted` (the synthetic net-accept)
/// BEFORE it asks the venue to close, so a death between that and the close's `ORDER_CLOSE_OK` left the close
/// `Accepted`: the EOF drain sent `OrderRejected "bridge died"`, which the core folds only from `Initialized`/`Submitted`
/// and DROPS on an `Accepted` order (`closing_events_dropped_order_still_live`). The dead close then stayed `Accepted` on the node for
/// ever, and `vike-core`'s `closes_in_flight` counted it as cover, so the margin call and the latch's flatten sent
/// nothing for a position that may still be open (or, if the venue had executed the close, that the node still held
/// because the fill died with the sidecar). Driven through the real engine's order FSM, like the stop-loss test above:
/// the assertion is what the node ends up BELIEVING.
#[test]
fn a_reduce_only_close_the_sidecar_had_taken_up_ends_canceled_in_words_at_its_death() {
    let dir = scratch("vike-duka-restart-close");
    let log = dir.path().join("launches.txt");
    let (events, mut rx) = event_channel(64);
    let mut client = ExecutionClientDukascopy::spawn_with_restart(
        BRIDGE,
        &["die-once".into(), log.display().to_string()],
        &config(),
        events,
        FAST,
    )
    .expect("ready");
    let close = reduce_only_close("flat1");
    let mut engine = engine_holding(&close);

    // Submitted, then the sidecar takes the close up (`OrderAccepted`) and dies before it has said more.
    client.submit(&close);
    for _ in 0..2 {
        let event = recv_event(&mut rx);
        assert!(folds(&mut engine, &event), "{event:?}");
    }
    assert_eq!(status(&engine, "flat1"), OrderStatus::Accepted);

    let ended = recv_event(&mut rx);
    match &ended {
        Event::OrderCanceled(c) => {
            assert_eq!(c.order_client_id, "flat1");
            assert!(c.reason.starts_with("bridge died"), "{}", c.reason);
            assert!(
                c.reason.contains("may have executed it"),
                "the venue's state is unknown: {}",
                c.reason
            );
            assert!(c.reason.contains("Look at the position"), "{}", c.reason);
        }
        other => panic!(
            "an accepted reduce-only close ends with a cancel the core accepts, got {other:?}"
        ),
    }
    assert!(folds(&mut engine, &ended), "the order FSM must take the end of an Accepted close");
    assert_eq!(
        status(&engine, "flat1"),
        OrderStatus::Canceled,
        "the node no longer counts a dead close as in flight"
    );
    assert_eq!(
        engine.closing_events_dropped_order_still_live, 0,
        "no terminal was dropped on a live order"
    );

    // The restarted sidecar serves the next close (the venue refuses it in words when nothing is left to close).
    wait_until("the second sidecar to be up", || {
        launches(&log).len() == 2 && client.sidecar_state() == VenueLinkState::Up
    });
    client.submit(&reduce_only_close("flat2"));
    expect_filled(&mut rx, "flat2");
    client.detach();

    // flat1 got ONE end: the cancel.
    let rest = drain(&mut rx);
    let again = rest
        .iter()
        .filter(|e| matches!(e, Event::OrderRejected(r) if r.order_client_id == "flat1"))
        .count();
    assert_eq!(again, 0, "flat1 was rejected as well: {rest:?}");
}

/// A close the sidecar never answered may have reached the venue: it is not rejected at the death (decision 0303) and
/// stays `Submitted` for the stuck-order ladder. The pin for the scope of the test above: only a close the sidecar had
/// taken up ends there.
#[test]
fn a_reduce_only_close_the_sidecar_never_answered_is_not_rejected_at_the_death() {
    let dir = scratch("vike-duka-restart-close-unanswered");
    let log = dir.path().join("launches.txt");
    let (events, mut rx) = event_channel(64);
    let mut client = ExecutionClientDukascopy::spawn_with_restart(
        BRIDGE,
        &["die-once".into(), log.display().to_string()],
        &config(),
        events,
        FAST,
    )
    .expect("ready");
    let close = reduce_only_close("unanswered");
    let mut engine = engine_holding(&close);

    client.submit(&close);
    let submitted = recv_event(&mut rx);
    assert!(folds(&mut engine, &submitted), "{submitted:?}");
    assert_eq!(status(&engine, "unanswered"), OrderStatus::Submitted);
    left_submitted_at_the_death(&mut client, &mut rx, &log, &engine);
    client.detach();
}

/// **A plain MARKET entry the sidecar had acknowledged when it died ends `OrderCanceled`, in words, and nothing is
/// booked.** The defect (v0.1.96): the venue's `ORDER_SUBMIT_OK` is the order's `OrderAccepted`, and a death before its
/// fill reached the node made the EOF drain send `OrderRejected "bridge died"`, which the core drops on an `Accepted`
/// order: the entry stayed `Accepted` on the node for ever. It ends canceled now, the words saying the venue may have
/// FILLED it. The cancel carries no fill, so the account books nothing: a fill that went through is the venue's
/// position alone, which the words send the operator to look for. Driven through the real engine, like the tests above.
#[test]
fn an_accepted_market_entry_at_a_sidecars_death_ends_canceled_in_words_and_books_nothing() {
    let dir = scratch("vike-duka-restart-entry");
    let log = dir.path().join("launches.txt");
    let (events, mut rx) = event_channel(64);
    let mut client = ExecutionClientDukascopy::spawn_with_restart(
        BRIDGE,
        &["die-once".into(), log.display().to_string()],
        &config(),
        events,
        FAST,
    )
    .expect("ready");
    let entry = order("e1");
    let mut engine = engine_holding(&entry);

    client.submit(&entry);
    for _ in 0..2 {
        let event = recv_event(&mut rx);
        assert!(folds(&mut engine, &event), "{event:?}");
    }
    assert_eq!(status(&engine, "e1"), OrderStatus::Accepted);

    let ended = recv_event(&mut rx);
    match &ended {
        Event::OrderCanceled(c) => {
            assert_eq!(c.order_client_id, "e1");
            assert!(c.reason.starts_with("bridge died"), "{}", c.reason);
            assert!(c.reason.contains("may have FILLED it"), "{}", c.reason);
            assert!(
                c.reason.contains("Look at the positions in the JForex platform"),
                "{}",
                c.reason
            );
        }
        other => {
            panic!("an accepted market entry ends with a cancel the core accepts, got {other:?}")
        }
    }
    assert!(folds(&mut engine, &ended), "the order FSM must take the end of an Accepted entry");
    assert_eq!(status(&engine, "e1"), OrderStatus::Canceled);
    assert_eq!(
        engine.closing_events_dropped_order_still_live, 0,
        "no terminal was dropped on a live order"
    );
    assert_eq!(engine.registry["e1"].filled_qty, 0.0, "the order filled nothing on the node");
    assert_eq!(
        engine.position_size_of("EURUSD", vike_model::events::PositionSide::Both),
        0.0,
        "nothing was booked"
    );
    client.detach();
}

/// The control: an entry the sidecar never answered may have reached the venue, so it is not rejected at the death
/// (decision 0303): it stays `Submitted` for the stuck-order ladder.
#[test]
fn an_unanswered_market_entry_is_not_rejected_at_the_death() {
    let dir = scratch("vike-duka-restart-entry-unanswered");
    let log = dir.path().join("launches.txt");
    let (events, mut rx) = event_channel(64);
    let mut client = ExecutionClientDukascopy::spawn_with_restart(
        BRIDGE,
        &["die-once".into(), log.display().to_string()],
        &config(),
        events,
        FAST,
    )
    .expect("ready");
    let entry = order("unanswered");
    let mut engine = engine_holding(&entry);

    client.submit(&entry);
    let submitted = recv_event(&mut rx);
    assert!(folds(&mut engine, &submitted), "{submitted:?}");
    left_submitted_at_the_death(&mut client, &mut rx, &log, &engine);
    client.detach();
}

/// **A clean stop (a detach) does not reject an order the sidecar never answered either** (decision 0303): its submit
/// was WRITTEN, so it may rest at the venue, and it stays `Submitted` for the stuck-order ladder and the restart
/// reconcile. A stop-loss on a jar that does not adopt, and a market entry on one that does.
///
/// ⚠ Mutation proof: the drain's rejection at a clean stop (the code before) ends both `Rejected` "bridge died".
#[test]
fn a_clean_stop_does_not_reject_an_order_the_sidecar_never_answered() {
    for (mode, unanswered) in [
        ("stop-die-once", resting_stop("unanswered")),
        ("adopt-die-once", order("unanswered-entry")),
    ] {
        let dir = scratch(&format!("vike-duka-stop-unanswered-{mode}"));
        let log = dir.path().join("launches.txt");
        let (events, mut rx) = event_channel(64);
        let mut client = ExecutionClientDukascopy::spawn_with_restart(
            BRIDGE,
            &[mode.into(), log.display().to_string()],
            &config(),
            events,
            FAST,
        )
        .expect("ready");
        let id = unanswered.order_client_id.as_str();
        let mut engine = engine_holding(&unanswered);
        client.submit(&unanswered);
        let submitted = recv_event(&mut rx);
        assert!(folds(&mut engine, &submitted), "{mode}: {submitted:?}");
        client.detach();
        let said = drain(&mut rx);
        assert!(
            said.iter().all(|e| e.order_client_id() != Some(id)),
            "{mode}: no word for an order of unknown outcome: {said:?}"
        );
        assert_eq!(status(&engine, id), OrderStatus::Submitted, "{mode}");
    }
}

/// What an order the dying sidecar never answered gets on a jar that does not adopt (decision 0303): no word at all from
/// the death through the next sidecar's `ready`, the node still holding it `Submitted` for the stuck-order ladder, and a
/// cancel of it refused by name (the new sidecar does not know it).
fn left_submitted_at_the_death(
    client: &mut ExecutionClientDukascopy,
    rx: &mut tokio::sync::mpsc::Receiver<Ingest>,
    log: &std::path::Path,
    engine: &Engine<RecordingClient>,
) {
    wait_until("the second sidecar to be up", || {
        launches(log).len() == 2 && client.sidecar_state() == VenueLinkState::Up
    });
    let said = drain(rx);
    assert!(
        said.iter().all(|e| e.order_client_id() != Some("unanswered")),
        "no word for an order of unknown outcome: {said:?}"
    );
    assert_eq!(status(engine, "unanswered"), OrderStatus::Submitted);
    client.cancel("unanswered");
    match recv_event(rx) {
        Event::OrderCancelRejected(r) => assert_eq!(r.order_client_id, "unanswered"),
        other => panic!("a cancel of it is refused by name, got {other:?}"),
    }
}

/// **The netting shadow outlives the sidecar.** The shadow mirrors the core's blended fold, which a sidecar restart
/// does not reset; one born empty at each sidecar would call the next `position` line a size mismatch (or adopt it
/// as a baseline) and never re-anchor the venue's per-order basis. The scenario is the netting module's own: short
/// 1000 @ 100 (A) before the death, short 1000 @ 110 (B) and a buy of 1000 @ 108 after it. The venue closes A at its
/// own entry and keeps B @ 110; the blend keeps 105. Only a shadow that saw A re-anchors, by the two synthesized
/// `NETRA` legs at the venue's 110. On a shadow reset by the restart the same stream gives no leg at all.
#[test]
fn a_restart_does_not_reset_the_netting_shadow_so_the_venues_basis_is_still_re_anchored() {
    let dir = scratch("vike-duka-restart-netting");
    let log = dir.path().join("launches.txt");
    let (events, mut rx) = event_channel(64);
    let mut client = ExecutionClientDukascopy::spawn_with_restart(
        BRIDGE,
        &["netting-die-once".into(), log.display().to_string()],
        &config(),
        events,
        FAST,
    )
    .expect("ready");
    let at = |order_client_id: &str, side: i32, px: f64| OrderRequest {
        side,
        price: Some(px),
        order_type: vike_model::OrderType::Limit,
        ..order(order_client_id)
    };

    // A, before the death: the fake sends the fill and its position line, then dies.
    client.submit(&at("a", -1, 100.0));
    expect_filled(&mut rx, "a");
    wait_until("the second sidecar to be up", || {
        launches(&log).len() == 2 && client.sidecar_state() == VenueLinkState::Up
    });

    // B and the buy, after it. The fake's book survived (a file), as the venue's does.
    client.submit(&at("b", -1, 110.0));
    expect_filled(&mut rx, "b");
    client.submit(&at("c", 1, 108.0));
    expect_filled(&mut rx, "c");

    // The re-anchor: close the blended position at the venue's average, reopen what remains there.
    let mut legs = Vec::new();
    for _ in 0..2 {
        match recv_event(&mut rx) {
            Event::Fill(f) => {
                legs.push((f.order_client_id.to_string(), f.side, f.last_qty, f.last_price))
            }
            other => panic!("expected a re-anchor leg, got {other:?}"),
        }
    }
    assert_eq!(
        legs,
        [
            ("NETRA-EURUSD".to_string(), 1, 1000.0, 110.0),
            ("NETRA-EURUSD".to_string(), -1, 1000.0, 110.0)
        ],
        "close the blended short at the venue's 110, reopen it there"
    );
    client.detach();
}

/// Refused logins back the restarts off, doubling to the ceiling and no higher; the sidecar's own reason is in the
/// state; the loop never spins (the elapsed time is at least the sum of the waits); and when a login is allowed again
/// the client is up with no hand step.
#[test]
fn refused_restarts_back_off_within_their_bounds_and_the_sidecar_comes_back() {
    let dir = scratch("vike-duka-restart-backoff");
    let log = dir.path().join("launches.txt");
    let (events, mut rx) = event_channel(64);
    let mut client = ExecutionClientDukascopy::spawn_with_restart(
        BRIDGE,
        // launch 1 dies after ready; launches 2..=5 are refused logins; launch 6 is healthy
        &["die-then-refuse".into(), log.display().to_string(), "4".into()],
        &config(),
        events,
        FAST,
    )
    .expect("the first launch reaches ready");

    let began = Instant::now();
    let mut waits: Vec<(u32, Duration)> = Vec::new();
    let mut refusal_named = false;
    wait_until("the sidecar to be back after the refusals", || match client.sidecar_state() {
        VenueLinkState::Down(VenueLinkDown { why, failed_restarts, retry_in, .. }) => {
            if waits.last() != Some(&(failed_restarts, retry_in)) {
                waits.push((failed_restarts, retry_in));
            }
            refusal_named |= failed_restarts > 0 && why.contains("login failed (scripted refusal)");
            false
        }
        VenueLinkState::Up | VenueLinkState::Unknown => launches(&log).len() >= 6,
    });
    let took = began.elapsed();

    let ms = Duration::from_millis;
    assert_eq!(
        waits,
        // The death and the first refused restart both read one failure: the death is a failure of its own.
        [(1, ms(40)), (1, ms(80)), (2, ms(160)), (3, ms(160)), (4, ms(160))],
        "the wait doubles from the first to the ceiling and stays there"
    );
    assert!(refusal_named, "the state never carried the sidecar's own refusal");
    assert!(took >= ms(500), "five restarts waited 40+80+160+160+160 ms, took only {took:?}");
    assert_eq!(launches(&log).len(), 6, "the first, four refused, and the one that came back");

    client.submit(&order("c1"));
    expect_filled(&mut rx, "c1");
    client.detach();
}

/// A venue that accepts the login and ends the session again (`ready`, then `fatal`) is a FLAP, not a fresh outage each
/// time: a restarted sidecar that died before it had lived the policy's ceiling counts as one more failed restart, so
/// the wait keeps doubling exactly as for a refused login. Counted from zero at every death (the supervisor's first
/// version), the same flap waits the SHORTEST wait for ever, which at the production 5 s is a login every few seconds.
#[test]
fn a_sidecar_that_keeps_dying_right_after_ready_is_backed_off_like_a_refused_login() {
    let dir = scratch("vike-duka-restart-flap");
    let log = dir.path().join("launches.txt");
    let (events, mut rx) = event_channel(64);
    // 40 ms doubling; a sidecar counts as having lived only after a full second, far beyond a scheduling stall.
    let policy =
        VenueTimingsReconnect::new(Duration::from_millis(40), Duration::from_secs(1), "a test");
    let mut client = ExecutionClientDukascopy::spawn_with_restart(
        BRIDGE,
        // launches 1..=3 say ready and die at once; launch 4 is healthy
        &["flap".into(), log.display().to_string(), "3".into()],
        &config(),
        events,
        policy,
    )
    .expect("the first launch reaches ready");

    let mut waits: Vec<(u32, Duration)> = Vec::new();
    wait_until("the sidecar to stay up after the flaps", || match client.sidecar_state() {
        VenueLinkState::Down(VenueLinkDown { failed_restarts, retry_in, .. }) => {
            // The reader marks a death with one failure and the first wait before the supervisor publishes its own, and
            // a poll can land between the two (CI read `[(0, 40ms), (1, 80ms), (0, 40ms), (2, 160ms)]` while a death
            // counted nothing). A later `(1, 40ms)` is that mark.
            let the_readers_mark =
                (failed_restarts, retry_in) == (1, Duration::from_millis(40)) && !waits.is_empty();
            if !the_readers_mark && waits.last() != Some(&(failed_restarts, retry_in)) {
                waits.push((failed_restarts, retry_in));
            }
            false
        }
        VenueLinkState::Up | VenueLinkState::Unknown => launches(&log).len() >= 4,
    });

    let ms = Duration::from_millis;
    assert_eq!(
        waits,
        [(1, ms(40)), (1, ms(80)), (2, ms(160))],
        "each flap doubles the wait; restarting the count at every death would read [(1, 40ms)] and stop there"
    );
    assert_eq!(launches(&log).len(), 4, "the first, three flaps, and the one that stayed");

    client.submit(&order("c1"));
    expect_filled(&mut rx, "c1");
    client.detach();
}

/// The account being deactivated ends the client (`detach`), and the client ends the loop: no further sidecar is
/// started, and the detach does not wait for anything.
#[test]
fn a_detached_client_stops_restarting_the_sidecar() {
    let dir = scratch("vike-duka-restart-detach");
    let log = dir.path().join("launches.txt");
    let (events, _rx) = event_channel(64);
    let steady = VenueTimingsReconnect::fixed(Duration::from_millis(100), "a test");
    let mut client = ExecutionClientDukascopy::spawn_with_restart(
        BRIDGE,
        &["die-then-refuse".into(), log.display().to_string(), "100000".into()],
        &config(),
        events,
        steady,
    )
    .expect("ready");

    wait_until("two refused restarts", || launches(&log).len() >= 3);
    let began = Instant::now();
    client.detach();
    assert!(began.elapsed() < Duration::from_secs(3), "the detach waited: {:?}", began.elapsed());

    let after = launches(&log).len();
    std::thread::sleep(Duration::from_millis(600));
    assert_eq!(launches(&log).len(), after, "a sidecar was started after the detach");
}

/// A detach while a restarted sidecar is still LOGGING IN kills and reaps it at once, so no second JForex session
/// ever shares the login and no process outlives the client; it does not wait out the start's time limit.
#[test]
fn a_detach_during_a_restart_login_kills_the_sidecar_that_was_logging_in() {
    let dir = scratch("vike-duka-restart-hang");
    let log = dir.path().join("launches.txt");
    let (events, _rx) = event_channel(64);
    let mut client = ExecutionClientDukascopy::spawn_with_restart(
        BRIDGE,
        &["die-then-hang".into(), log.display().to_string()],
        &config(),
        events,
        FAST,
    )
    .expect("ready");

    // Launch 2 hangs forever (a login that never answers).
    wait_until("the restarted sidecar to be logging in", || launches(&log).len() == 2);
    let began = Instant::now();
    client.detach();
    assert!(
        began.elapsed() < Duration::from_secs(5),
        "the detach waited for the login: {:?}",
        began.elapsed()
    );
    assert_eq!(launches(&log).len(), 2, "nothing else was started");
    // `Child::wait` returned for it: on Linux its /proc entry is gone, not a zombie.
    #[cfg(target_os = "linux")]
    for pid in launches(&log) {
        assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists(), "sidecar {pid} left");
    }
}

/// A liveness lane in milliseconds: the probe after 150 ms of silence and the idle limit at 600 ms (four probe
/// intervals, as the row's 210 s is three and a half of its 60 s).
fn liveness_ms() -> VenueTimingsLane {
    VenueTimingsLane {
        keepalive_every: Some(VenueTimingsValue::millis(150, "a test's probe interval")),
        idle_threshold: Some(VenueTimingsValue::millis(600, "a test's idle limit")),
        ..VenueTimingsLane::NONE
    }
}

/// The ids of the `recon` questions the `quiet` launches of `log` were asked (`<log>.recon`).
fn questions(log: &std::path::Path) -> Vec<u64> {
    std::fs::read_to_string(format!("{}.recon", log.display()))
        .unwrap_or_default()
        .lines()
        .map(|l| l.parse().expect("an id line"))
        .collect()
}

/// **A sidecar that writes nothing past the idle limit is WEDGED: it is killed, reaped and started again** on the
/// reconnect ladder, and the path reads Down in the limit's words meanwhile. The fake's first launch says `ready` and then
/// answers nothing, probes included (a stuck JVM or strategy thread); the second answers. Before the limit was enforced,
/// the wedged sidecar stayed the client's for ever and nothing was ever started again.
#[test]
fn a_sidecar_silent_past_the_idle_limit_is_killed_and_restarted() {
    let dir = scratch("vike-duka-idle-wedge");
    let log = dir.path().join("launches.txt");
    let (events, mut rx) = event_channel(64);
    // A first wait long enough to read the Down state in.
    let slow = VenueTimingsReconnect::new(
        Duration::from_millis(300),
        Duration::from_millis(600),
        "a test",
    );
    let mut client = ExecutionClientDukascopy::spawn_with_liveness(
        BRIDGE,
        &["wedge-once".into(), log.display().to_string()],
        &config(),
        events,
        slow,
        liveness_ms(),
    )
    .expect("the first launch reaches ready");
    let began = Instant::now();

    let mut why_down = None;
    wait_until("the wedged sidecar to be replaced by one that is up", || {
        if let VenueLinkState::Down(VenueLinkDown { why, .. }) = client.sidecar_state() {
            why_down = Some(why);
        }
        launches(&log).len() == 2 && client.sidecar_state() == VenueLinkState::Up
    });
    assert!(
        began.elapsed() >= Duration::from_millis(600),
        "killed before its idle limit: {:?}",
        began.elapsed()
    );
    let why = why_down.expect("the path read Down while the sidecar was restarted");
    assert!(why.contains("past its idle limit") && why.contains("killed as wedged"), "{why}");
    // The wedged one was reaped before the next started (one JForex session per login): its /proc entry is gone.
    #[cfg(target_os = "linux")]
    {
        let wedged = launches(&log)[0];
        assert!(
            !std::path::Path::new(&format!("/proc/{wedged}")).exists(),
            "sidecar {wedged} left"
        );
    }

    // The next one answers its probes, so it stays, and it serves orders.
    std::thread::sleep(Duration::from_millis(1_500));
    assert_eq!(launches(&log).len(), 2, "a sidecar that answers its probes is not restarted");
    client.submit(&order("c1"));
    expect_filled(&mut rx, "c1");
    client.detach();
}

/// **The probe keeps a quiet sidecar alive with the node's reconcile pass off.** Nothing asks this sidecar anything (no
/// reconcile client, no order), so its only lines are its answers to the supervisor's own `recon` probe, each asked under
/// `PROBE_ID` once it has been silent for the probe interval; over three idle limits it is never restarted. Without the
/// probe, the limit would kill a healthy idle sidecar every 600 ms here (every 210 s in production).
#[test]
fn the_probe_keeps_a_quiet_sidecar_alive_with_the_nodes_pass_off() {
    let dir = scratch("vike-duka-idle-quiet");
    let log = dir.path().join("launches.txt");
    let (events, _rx) = event_channel(64);
    let mut client = ExecutionClientDukascopy::spawn_with_liveness(
        BRIDGE,
        &["quiet".into(), log.display().to_string()],
        &config(),
        events,
        FAST,
        liveness_ms(),
    )
    .expect("ready");

    std::thread::sleep(Duration::from_millis(2_000));
    assert_eq!(launches(&log).len(), 1, "a sidecar that answers its probes is never restarted");
    assert_eq!(client.sidecar_state(), VenueLinkState::Up);
    let asked = questions(&log);
    assert!(asked.len() >= 3, "probed about every 150 ms of silence, asked only {asked:?}");
    assert!(asked.iter().all(|&id| id == PROBE_ID), "every probe under the probe's id: {asked:?}");
    client.detach();
}

/// **Silence before `ready` never trips the idle limit**: it counts from the sidecar's `ready` (the start is the
/// handshake's to bound). This launch says nothing for 1.5 s, two and a half idle limits, before its `ready`; counted
/// from its start, it would be killed at the first look after it.
#[test]
fn silence_before_ready_never_trips_the_idle_limit() {
    let dir = scratch("vike-duka-idle-login");
    let log = dir.path().join("launches.txt");
    let (events, _rx) = event_channel(64);
    let mut client = ExecutionClientDukascopy::spawn_with_liveness(
        BRIDGE,
        &["quiet".into(), log.display().to_string(), "1500".into()],
        &config(),
        events,
        FAST,
        liveness_ms(),
    )
    .expect("a slow login still reaches ready");

    std::thread::sleep(Duration::from_millis(1_000));
    assert_eq!(launches(&log).len(), 1, "the slow login was not killed after its ready");
    assert_eq!(client.sidecar_state(), VenueLinkState::Up);
    client.detach();
}
