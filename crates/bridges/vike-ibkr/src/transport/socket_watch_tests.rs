//! The pump with the link watchdog, over a DOUBLE of the ibapi session that speaks like the gateway of
//! 2026-10-17: it accepts, then reports [2110]/[2103]/[2157] and never restores. The waits are shortened
//! (`WatchTimes`); the rebuild schedule is the real one on a test gate.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::*;
use crate::transport::gateway_gate::GatewayGate;

/// An order-update stream with nothing to say (a gateway without its link is quiet).
struct Quiet;

impl Updates for Quiet {
    fn poll(&mut self, within: Duration) -> Polled {
        std::thread::sleep(within.min(Duration::from_millis(20)));
        Polled::Idle
    }
}

struct Feed(Receiver<(i32, String)>);

impl Notices for Feed {
    fn next(&mut self) -> Option<(i32, String)> {
        self.0.try_recv().ok()
    }
}

struct Ended(bool);

impl SnapshotHandle for Ended {
    fn wait(self: Box<Self>, _within: Duration) -> bool {
        self.0
    }
}

struct Gateway {
    updates: Mutex<Option<Box<dyn Updates>>>,
    notices: Mutex<Option<Box<dyn Notices>>>,
    connected: AtomicBool,
    closed: AtomicUsize,
    snapshot_ends: Mutex<VecDeque<bool>>,
}

impl IbSession for Gateway {
    fn local_next_order_id(&self) -> i32 {
        101
    }
    fn next_valid_order_id(&self) -> Result<i32, String> {
        Ok(9)
    }
    fn submit_order(&self, _: i32, _: &Contract, _: &Order) -> Result<(), String> {
        Ok(())
    }
    fn cancel_order(&self, _: i32) -> Result<(), String> {
        Ok(())
    }
    fn all_open_orders(&self) -> Result<Box<dyn SnapshotHandle>, String> {
        let end = self.snapshot_ends.lock().unwrap().pop_front().unwrap_or(true);
        Ok(Box::new(Ended(end)))
    }
    fn executions(&self, _: i32) -> Result<Box<dyn SnapshotHandle>, String> {
        Err("not scripted".into())
    }
    fn order_updates(&self) -> Result<Box<dyn Updates>, String> {
        self.updates.lock().unwrap().take().ok_or_else(|| "no stream".to_string())
    }
    fn notices(&self) -> Option<Box<dyn Notices>> {
        self.notices.lock().unwrap().take()
    }
    fn connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
    }
    fn close(&self) {
        self.closed.fetch_add(1, Ordering::SeqCst);
    }
}

fn gateway() -> (Arc<Gateway>, Sender<(i32, String)>) {
    let (tx, rx) = channel();
    let session = Arc::new(Gateway {
        updates: Mutex::new(Some(Box::new(Quiet))),
        notices: Mutex::new(Some(Box::new(Feed(rx)))),
        connected: AtomicBool::new(true),
        closed: AtomicUsize::new(0),
        snapshot_ends: Mutex::new(VecDeque::new()),
    });
    (session, tx)
}

/// The pump runs on a real clock, so NO test here asserts a wall-clock margin: a test that expects a death uses a
/// tiny threshold and waits generously for it (it can only be late, never wrong); a test that expects NO death
/// uses a threshold no loaded runner can reach, or a break that never counts (the timing of the threshold itself is
/// pinned on a fake clock in `link_watch_tests.rs`).
const SOON: Duration = Duration::from_millis(50);
const NEVER: Duration = Duration::from_secs(3600);

fn watched(
    session: &Arc<Gateway>,
    lost_after: Duration,
    probation: Duration,
) -> (SocketTransport, Arc<GatewayGate>) {
    let gate = Arc::new(GatewayGate::new("127.0.0.1:9", Duration::from_millis(10), false));
    let watching =
        Watching { gate: Arc::clone(&gate), times: WatchTimes { lost_after, probation } };
    let transport = SocketTransport::start_watched(
        Arc::clone(session) as Arc<dyn IbSession>,
        7,
        "DU1234567",
        Some(watching),
    )
    .expect("the transport starts");
    (transport, gate)
}

/// The reason of the stream death the pump sends, or `None` if none came within `within`.
fn death_within(t: &mut SocketTransport, within: Duration) -> Option<String> {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        if let Some(IbInbound::StreamDead { reason }) = t.next_recv(Duration::from_millis(20)) {
            return Some(reason);
        }
    }
    None
}

fn wait_for(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// THE INCIDENT: the gateway takes the connection, reports its broken link and never restores it. The stream is
/// declared dead with the words an operator reads (so every later submit is refused with a terminal reject by
/// the exec loop's existing latch), the client is CLOSED at once (its id is free for the next attempt), and the
/// gate is armed, so the rebuild is spaced and dials nothing until the schedule allows it.
#[test]
fn a_gateway_that_reports_a_broken_link_and_never_restores_is_declared_dead_and_closed() {
    let (session, notices) = gateway();
    let (mut t, gate) = watched(&session, SOON, NEVER);
    notices
        .send((2110, "Connectivity between Trader Workstation and server is broken.".into()))
        .unwrap();
    notices.send((2103, "Market data farm connection is broken:usfarm".into())).unwrap();
    notices.send((2157, "Sec-def data farm connection is broken:secdefil".into())).unwrap();
    let reason = death_within(&mut t, Duration::from_secs(30)).expect("declared dead");
    assert!(reason.starts_with("IB Gateway has no link to IBKR for "), "{reason}");
    assert!(reason.contains("1100/2110"), "{reason}");
    wait_for("the client to be closed", || session.closed.load(Ordering::SeqCst) == 1);
    wait_for("the closing pump to finish settling", || gate.settling_now() == 0);
    assert!(gate.is_armed());
    assert!(gate.permit_exec().is_err(), "a rebuild right after the loss dials nothing");
}

/// A gateway that restores in time is never declared dead, and a transport that is dropped closes its client.
#[test]
fn a_gateway_that_restores_in_time_is_left_alone_and_a_dropped_transport_closes_its_client() {
    let (session, notices) = gateway();
    // The threshold is out of reach, so this proves the wiring (a break then a restore reach the monitor and
    // nothing is declared); the threshold's own edge is on the fake clock in `link_watch_tests.rs`.
    let (mut t, gate) = watched(&session, NEVER, NEVER);
    notices
        .send((2110, "Connectivity between Trader Workstation and server is broken.".into()))
        .unwrap();
    notices
        .send((1102, "Connectivity between IB and Trader Workstation has been restored".into()))
        .unwrap();
    assert!(death_within(&mut t, Duration::from_millis(300)).is_none(), "nothing is declared");
    assert!(!gate.is_armed());
    drop(t);
    wait_for("the dropped transport to close its client", || {
        session.closed.load(Ordering::SeqCst) == 1
    });
}

/// A data farm (or a sec-def farm) down with the server link fine is not a lost link (the CI box: `[2103] usfarm`
/// broken for 136 s and back, nothing else wrong): the pump declares nothing and arms nothing.
#[test]
fn a_farm_that_is_down_alone_is_not_a_dead_link() {
    let (session, notices) = gateway();
    // A tiny threshold: were a farm counted, the death would come within a couple of polls.
    let (mut t, gate) = watched(&session, SOON, NEVER);
    notices.send((2103, "Market data farm connection is broken:usfarm".into())).unwrap();
    notices.send((2157, "Sec-def data farm connection is broken:secdefil".into())).unwrap();
    assert!(
        death_within(&mut t, Duration::from_millis(500)).is_none(),
        "farms alone never condemn the link"
    );
    assert!(!gate.is_armed());
}

#[test]
fn an_api_socket_that_stays_disconnected_is_declared_dead() {
    let (session, _notices) = gateway();
    let (mut t, _gate) = watched(&session, SOON, NEVER);
    session.connected.store(false, Ordering::SeqCst);
    let reason = death_within(&mut t, Duration::from_secs(30)).expect("declared dead");
    assert!(reason.contains("API socket is not connected"), "{reason}");
}

/// A snapshot request the venue never answered (the learning of a gateway with no link) keeps the link
/// suspect. (That a later one that completes clears it is pinned on the fake clock in `link_watch_tests.rs`.)
#[test]
fn an_unanswered_snapshot_is_declared_dead() {
    let (session, _notices) = gateway();
    let (mut t, _gate) = watched(&session, SOON, NEVER);
    session.snapshot_ends.lock().unwrap().push_back(false);
    t.request_open_orders();
    let reason = death_within(&mut t, Duration::from_secs(30)).expect("declared dead");
    assert!(reason.contains("did not answer a snapshot request"), "{reason}");
}

/// An unwatched transport (a double with no notices) keeps its old behaviour, and still closes its client.
#[test]
fn an_unwatched_transport_is_never_declared_dead_by_the_watchdog() {
    let (session, notices) = gateway();
    let mut t = SocketTransport::start(Arc::clone(&session) as Arc<dyn IbSession>, 7, "DU1234567")
        .expect("starts");
    notices.send((2110, "broken".into())).unwrap();
    assert!(death_within(&mut t, Duration::from_millis(300)).is_none());
    drop(t);
    wait_for("the close", || session.closed.load(Ordering::SeqCst) == 1);
}
