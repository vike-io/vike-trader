//! The sidecar is SUPERVISED: a JForex sidecar that dies after `ready` is started again by the client itself
//! (measured 2026-10-26: the real one died at 08:15:03 UTC on `JForex session disconnected` and nothing started
//! another for 2.5 hours while the node still counted the account as running).
//!
//! Driven through real child processes of the scripted fake bridge (`src/bin/fake_jforex_bridge.rs`: `die-once`,
//! `die-then-refuse`, `die-then-hang`, each counting its launches in a log file), with a restart policy in
//! milliseconds. What these prove is the Rust half: the client reports the sidecar DOWN, refuses every command by
//! name while it is, rejects an in-flight order exactly once, backs off within bounds, starts the next sidecar only
//! after the dead one is reaped, serves a submit after the restart, and stops the loop when it is detached. They do
//! not prove the JForex half (what a real session loss looks like, whether the venue lets a login back in): that is
//! the live check in `crates/bridges/dukascopy/CLAUDE.md`'s "The sidecar is restarted by itself".

use std::sync::Arc;
use std::time::{Duration, Instant};

use vike_dukascopy::quotes::QuoteBus;
use vike_dukascopy::{DukascopyConfig, DukascopyExecutionClient, SidecarState};
use vike_exec::testing::RecordingClient;
use vike_exec::{
    Account, BalanceMode, EventHandler, ExecutionClient, ExecutionEngine, Fold, Ingest, Outbox,
    RiskGate, event_channel,
};
use vike_model::events::Event;
use vike_model::{ManagedOrder, OrderRequest, OrderStatus, RiskLimits};
use vike_venues::venue_timings::Reconnect;

/// Path of the fake bridge binary (cargo builds crate bins for integration tests).
const BRIDGE: &str = env!("CARGO_BIN_EXE_fake_jforex_bridge");

/// A restart policy in milliseconds: 40 ms, doubling to 160 ms.
const FAST: Reconnect =
    Reconnect::new(Duration::from_millis(40), Duration::from_millis(160), "a test");

fn config() -> DukascopyConfig {
    DukascopyConfig {
        login: "test-login".into(),
        password: "test-password".into(),
        server: String::new(),
        ledger_mount: None,
    }
}

fn order(coid: &str) -> OrderRequest {
    OrderRequest {
        data_delay: None,
        account: None,
        combo_legs: Vec::new(),
        client_order_id: coid.into(),
        venue: "dukascopy".into(),
        symbol: "EURUSD".into(),
        side: 1,
        qty: 1000.0,
        order_type: vike_model::OrderType::Market,
        price: None,
        trigger_price: None,
        reduce_only: false,
        time_in_force: Default::default(),
        gtd_expiry: None,
        ts: 1,
        parent_order_id: None,
        linked_order_ids: vec![],
        order_list_id: None,
        contingency_type: None,
        weight: 0.0,
        stop: None,
        trail: None,
        extreme: None,
        on_close: false,
        margin_mode: None,
        trigger_by: None,
    }
}

fn resting_limit(coid: &str) -> OrderRequest {
    OrderRequest { order_type: vike_model::OrderType::Limit, price: Some(1.1), ..order(coid) }
}

/// The next event of the ingest lane, failing loudly after 10 s.
fn recv_event(rx: &mut tokio::sync::mpsc::Receiver<Ingest>) -> Event {
    let rt = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
    let ingest = rt
        .block_on(async { tokio::time::timeout(Duration::from_secs(10), rx.recv()).await })
        .expect("timed out waiting for event")
        .expect("ingest channel closed");
    match ingest {
        Ingest::Event(ev) => ev,
        other => panic!("expected Ingest::Event, got {other:?}"),
    }
}

/// Every event that is on the lane right now.
fn drain(rx: &mut tokio::sync::mpsc::Receiver<Ingest>) -> Vec<Event> {
    let mut out = Vec::new();
    while let Ok(Ingest::Event(ev)) = rx.try_recv() {
        out.push(ev);
    }
    out
}

fn wait_until(what: &str, mut ready: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(20);
    while !ready() {
        assert!(Instant::now() < end, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// The pids the fake wrote into its launch log, one per launch.
fn launches(log: &std::path::Path) -> Vec<u32> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .map(|l| l.parse().expect("pid line"))
        .collect()
}

/// The ladder a healthy sidecar gives a market order: Submitted, Accepted, the bare Fill, the OrderFilled wrap.
fn expect_filled(rx: &mut tokio::sync::mpsc::Receiver<Ingest>, coid: &str) {
    match recv_event(rx) {
        Event::OrderSubmitted(e) => assert_eq!(e.client_order_id, coid),
        other => panic!("expected OrderSubmitted for {coid}, got {other:?}"),
    }
    match recv_event(rx) {
        Event::OrderAccepted(e) => assert_eq!(e.client_order_id, coid),
        other => panic!("expected OrderAccepted for {coid}, got {other:?}"),
    }
    match recv_event(rx) {
        Event::Fill(f) => assert_eq!(f.client_order_id, coid),
        other => panic!("expected the bare Fill for {coid}, got {other:?}"),
    }
    match recv_event(rx) {
        Event::OrderFilled(e) => assert_eq!(e.client_order_id, coid),
        other => panic!("expected OrderFilled for {coid}, got {other:?}"),
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
    let mut client = DukascopyExecutionClient::spawn_with_restart(
        BRIDGE,
        &["die-once".into(), log.display().to_string()],
        &config(),
        events,
        FAST,
    )
    .expect("the first launch reaches ready");
    assert_eq!(client.sidecar_state(), SidecarState::Up);

    // The first launch dies on its first command, the way the real sidecar does when JForex ends the session.
    client.cancel("DIE");

    // A second launch exists only because the supervisor made it; `Up` after that is the new sidecar's ready.
    wait_until("the second sidecar to be up", || {
        launches(&log).len() == 2 && client.sidecar_state() == SidecarState::Up
    });

    client.submit(&order("c1"));
    expect_filled(&mut rx, "c1");
    assert_eq!(launches(&log).len(), 2, "one death, one restart, and no more");
    client.detach();
}

/// **The client's state is on the bus the gatewayhub's lane reads** (`QuoteBus::sidecar`, the one thing the exec client
/// and the lane share): `Down` with the sidecar's own words from the death until the next sidecar's `ready`, `Up` before
/// and after, the same answer as the client's own `sidecar_state`, and a NEW client on the same bus starts from `Up`
/// (the process's bus outlives its clients). Under the production policy, whose first wait (5 s) is long enough to read.
#[test]
fn the_clients_sidecar_state_is_on_the_bus_the_gatewayhub_lane_reads() {
    let dir = scratch("vike-duka-restart-bus");
    let log = dir.path().join("launches.txt");
    let (events, _rx) = event_channel(64);
    let bus = QuoteBus::new();
    let mut client = DukascopyExecutionClient::spawn_with_program_quoting(
        BRIDGE,
        &["die-once".into(), log.display().to_string()],
        &config(),
        events,
        Arc::clone(&bus),
    )
    .expect("the first launch reaches ready");
    assert_eq!(bus.sidecar(), SidecarState::Up);

    client.cancel("DIE");
    wait_until("the death to reach the bus", || matches!(bus.sidecar(), SidecarState::Down { .. }));
    match bus.sidecar() {
        SidecarState::Down { why, failed_restarts, retry_in } => {
            assert!(why.contains("JForex session disconnected"), "the sidecar's own reason: {why}");
            assert_eq!((failed_restarts, retry_in), (0, Duration::from_secs(5)));
        }
        SidecarState::Up => unreachable!("waited for Down"),
    }
    assert_eq!(bus.sidecar(), client.sidecar_state(), "one state, read two ways");

    wait_until("the bus to say the sidecar is back", || bus.sidecar() == SidecarState::Up);
    assert_eq!(client.sidecar_state(), SidecarState::Up);
    client.detach();

    // A client started on a bus that still holds an earlier client's outage starts from `Up`.
    bus.set_sidecar(SidecarState::Down {
        why: "an earlier client's outage".to_string(),
        failed_restarts: 3,
        retry_in: Duration::from_secs(40),
    });
    let (events, _rx) = event_channel(64);
    let mut second = DukascopyExecutionClient::spawn_with_program_quoting(
        BRIDGE,
        &["die-once".into(), dir.path().join("second.txt").display().to_string()],
        &config(),
        events,
        Arc::clone(&bus),
    )
    .expect("the second client's first launch reaches ready");
    assert_eq!(
        bus.sidecar(),
        SidecarState::Up,
        "a new client does not inherit the last one's outage"
    );
    second.detach();
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
    let mut client = DukascopyExecutionClient::spawn_with_program(
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
    let slow = Reconnect::new(Duration::from_millis(1500), Duration::from_secs(3), "a test");
    let mut client = DukascopyExecutionClient::spawn_with_restart(
        BRIDGE,
        &["die-once".into(), log.display().to_string()],
        &config(),
        events,
        slow,
    )
    .expect("ready");

    client.cancel("DIE");
    wait_until("the death to be noticed", || {
        matches!(client.sidecar_state(), SidecarState::Down { .. })
    });
    match client.sidecar_state() {
        SidecarState::Down { why, failed_restarts, retry_in } => {
            assert!(why.contains("JForex session disconnected"), "the sidecar's own reason: {why}");
            assert_eq!(failed_restarts, 0);
            assert_eq!(retry_in, Duration::from_millis(1500));
        }
        SidecarState::Up => unreachable!("waited for Down"),
    }

    client.submit(&order("c1"));
    match recv_event(&mut rx) {
        Event::OrderRejected(e) => {
            assert_eq!(e.client_order_id, "c1");
            assert!(e.reason.starts_with("bridge unavailable"), "{}", e.reason);
            assert!(e.reason.contains("JForex session disconnected"), "{}", e.reason);
            assert!(e.reason.contains("NOT sent"), "{}", e.reason);
        }
        other => panic!("the FIRST event of a refused submit is its rejection, got {other:?}"),
    }
    client.cancel("c1");
    match recv_event(&mut rx) {
        Event::OrderCancelRejected(r) => {
            assert_eq!(r.client_order_id, "c1");
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
            assert_eq!(r.client_order_id, "c1");
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
    wait_until("the sidecar to be back", || client.sidecar_state() == SidecarState::Up);
    client.submit(&order("c2"));
    expect_filled(&mut rx, "c2");
    client.detach();
}

/// An order in flight when the sidecar dies is rejected EXACTLY ONCE (the reader's drain and the failed write both
/// take it out of the in-flight set first); the restarted sidecar does not know it, so a cancel of it is refused by
/// name instead of being skipped in silence by Java; a new order is served.
#[test]
fn a_death_with_an_order_in_flight_rejects_it_once_and_the_new_sidecar_is_not_asked_about_it() {
    let dir = scratch("vike-duka-restart-inflight");
    let log = dir.path().join("launches.txt");
    let (events, mut rx) = event_channel(64);
    let mut client = DukascopyExecutionClient::spawn_with_restart(
        BRIDGE,
        &["die-once".into(), log.display().to_string()],
        &config(),
        events,
        FAST,
    )
    .expect("ready");

    // Accepted, then the sidecar dies without a terminal event.
    client.submit(&order("c1"));
    let mut seen = Vec::new();
    for _ in 0..3 {
        seen.push(recv_event(&mut rx));
    }
    assert!(matches!(&seen[0], Event::OrderSubmitted(e) if e.client_order_id == "c1"), "{seen:?}");
    assert!(matches!(&seen[1], Event::OrderAccepted(e) if e.client_order_id == "c1"), "{seen:?}");
    match &seen[2] {
        Event::OrderRejected(e) => {
            assert_eq!(e.client_order_id, "c1");
            assert!(e.reason.starts_with("bridge died"), "{}", e.reason);
        }
        other => panic!("the death must reject the in-flight order, got {other:?}"),
    }

    wait_until("the second sidecar to be up", || {
        launches(&log).len() == 2 && client.sidecar_state() == SidecarState::Up
    });

    client.cancel("c1");
    match recv_event(&mut rx) {
        Event::OrderCancelRejected(r) => {
            assert_eq!(r.client_order_id, "c1");
            assert!(r.reason.contains("restarted after this order was placed"), "{}", r.reason);
        }
        other => panic!("a cancel of a stranded order is refused by name, got {other:?}"),
    }
    client.modify(&resting_limit("c1"), None, Some(1.2));
    match recv_event(&mut rx) {
        Event::OrderModifyRejected(r) => {
            assert!(r.reason.contains("restarted after this order was placed"), "{}", r.reason);
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
        .filter(|e| matches!(e, Event::OrderRejected(r) if r.client_order_id == "c1"))
        .count();
    assert_eq!(again, 0, "c1 was rejected a second time: {rest:?}");
}

/// A native stop-loss on a long EURUSD position: a reduce-only SELL stop.
fn resting_stop(coid: &str) -> OrderRequest {
    OrderRequest {
        order_type: vike_model::OrderType::Stop,
        side: -1,
        trigger_price: Some(1.09),
        reduce_only: true,
        ..order(coid)
    }
}

/// The node's real order FSM holding `req` as a registered order, as the engine of a dukascopy account would.
fn engine_holding(req: &OrderRequest) -> ExecutionEngine<RecordingClient> {
    let mut engine = ExecutionEngine::new(
        Account::new(1.0, "dukascopy", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "dukascopy",
        "EURUSD",
    );
    engine.registry.insert(req.client_order_id.clone(), ManagedOrder::new(req.clone()));
    engine
}

/// Fold `event` into the real engine and say whether the order FSM took it.
fn folds(engine: &mut ExecutionEngine<RecordingClient>, event: &Event) -> bool {
    matches!(engine.on_event(event, &mut Outbox::default()), Fold::Applied)
}

fn status(engine: &ExecutionEngine<RecordingClient>, coid: &str) -> OrderStatus {
    engine.registry[coid].status
}

/// **A native stop-loss resting at the venue when its sidecar dies ends `OrderCanceled`, in words, and nothing takes
/// it off at the venue.** The defect (read in the code, v0.1.93): the EOF drain sent `OrderRejected "bridge died"` for
/// every coid in flight, the stop's included, and the core folds a rejection only from `Initialized`/`Submitted`: on
/// the `Accepted` stop it was DROPPED (`dropped_terminal_on_live`), the stop stayed `Accepted` on the node for ever,
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
    let mut client = DukascopyExecutionClient::spawn_with_restart(
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
            assert_eq!(c.client_order_id, "s1");
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
    assert_eq!(engine.dropped_terminal_on_live, 0, "no terminal was dropped on a live order");

    wait_until("the second sidecar to be up", || {
        launches(&log).len() == 2 && client.sidecar_state() == SidecarState::Up
    });

    // The new sidecar does not know the stop: a cancel of it is refused by name and never reaches it.
    client.cancel("s1");
    match recv_event(&mut rx) {
        Event::OrderCancelRejected(r) => {
            assert_eq!(r.client_order_id, "s1");
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
        .filter(|e| matches!(e, Event::OrderRejected(r) if r.client_order_id == "s1"))
        .count();
    assert_eq!(again, 0, "the stop got one end, the cancel");
}

/// A stop the sidecar never answered is not resting as far as anyone knows: it is `Submitted` on the node, where a
/// cancel would be dropped by the FSM (it takes one only from `Accepted` and later), so it is REJECTED at the death
/// like any order in flight, and the order FSM takes that.
#[test]
fn a_stop_the_sidecar_never_answered_is_rejected_at_the_death_like_any_order_in_flight() {
    let dir = scratch("vike-duka-restart-stop-unanswered");
    let log = dir.path().join("launches.txt");
    let (events, mut rx) = event_channel(64);
    let mut client = DukascopyExecutionClient::spawn_with_restart(
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
    let ended = recv_event(&mut rx);
    match &ended {
        Event::OrderRejected(r) => {
            assert_eq!(r.client_order_id, "unanswered");
            assert!(r.reason.starts_with("bridge died"), "{}", r.reason);
        }
        other => panic!("an unanswered stop is rejected, got {other:?}"),
    }
    assert!(folds(&mut engine, &ended), "{ended:?}");
    assert_eq!(status(&engine, "unanswered"), OrderStatus::Rejected);
    assert_eq!(engine.dropped_terminal_on_live, 0);
    client.detach();
    assert!(
        std::fs::read_to_string(format!("{}.book", log.display())).is_err(),
        "the venue never took it"
    );
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
    let mut client = DukascopyExecutionClient::spawn_with_restart(
        BRIDGE,
        &["netting-die-once".into(), log.display().to_string()],
        &config(),
        events,
        FAST,
    )
    .expect("ready");
    let at = |coid: &str, side: i32, px: f64| OrderRequest {
        side,
        price: Some(px),
        order_type: vike_model::OrderType::Limit,
        ..order(coid)
    };

    // A, before the death: the fake sends the fill and its position line, then dies.
    client.submit(&at("a", -1, 100.0));
    expect_filled(&mut rx, "a");
    wait_until("the second sidecar to be up", || {
        launches(&log).len() == 2 && client.sidecar_state() == SidecarState::Up
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
                legs.push((f.client_order_id.to_string(), f.side, f.last_qty, f.last_px))
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
    let mut client = DukascopyExecutionClient::spawn_with_restart(
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
        SidecarState::Down { why, failed_restarts, retry_in } => {
            if waits.last() != Some(&(failed_restarts, retry_in)) {
                waits.push((failed_restarts, retry_in));
            }
            refusal_named |= failed_restarts > 0 && why.contains("login failed (scripted refusal)");
            false
        }
        SidecarState::Up => launches(&log).len() >= 6,
    });
    let took = began.elapsed();

    let ms = Duration::from_millis;
    assert_eq!(
        waits,
        [(0, ms(40)), (1, ms(80)), (2, ms(160)), (3, ms(160)), (4, ms(160))],
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
    let policy = Reconnect::new(Duration::from_millis(40), Duration::from_secs(1), "a test");
    let mut client = DukascopyExecutionClient::spawn_with_restart(
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
        SidecarState::Down { failed_restarts, retry_in, .. } => {
            if waits.last() != Some(&(failed_restarts, retry_in)) {
                waits.push((failed_restarts, retry_in));
            }
            false
        }
        SidecarState::Up => launches(&log).len() >= 4,
    });

    let ms = Duration::from_millis;
    assert_eq!(
        waits,
        [(0, ms(40)), (1, ms(80)), (2, ms(160))],
        "each flap doubles the wait; restarting the count at every death would read [(0, 40ms)] and stop there"
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
    let steady = Reconnect::fixed(Duration::from_millis(100), "a test");
    let mut client = DukascopyExecutionClient::spawn_with_restart(
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
    let mut client = DukascopyExecutionClient::spawn_with_restart(
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
