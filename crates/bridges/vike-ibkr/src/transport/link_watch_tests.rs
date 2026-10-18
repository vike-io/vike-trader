//! The link monitor and the watch, on a FAKE clock (`Instant` plus an offset) and scripted notices, with the
//! texts the the CI box gateway really sends (the journal of 2026-10-17 and the days around it).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use super::*;

fn s(n: u64) -> Duration {
    Duration::from_secs(n)
}

const SERVER_BROKEN: &str = "Connectivity between Trader Workstation and server is broken. It will be restored automatically.";
const USFARM_BROKEN: &str = "Market data farm connection is broken:usfarm";
const USFARM_OK: &str = "Market data farm connection is OK:usfarm";
const SECDEF_BROKEN: &str = "Sec-def data farm connection is broken:secdefil";
const SECDEF_OK: &str = "Sec-def data farm connection is OK:secdefil";
const HMDS_INACTIVE: &str =
    "HMDS data farm connection is inactive but should be available upon demand.ushmds";

/// THE INCIDENT'S HANDSHAKE BURST: the gateway without its link announces its broken parts on every new
/// connection and nothing restores them; the monitor calls it lost at the threshold, not before. Only the
/// server link counts: the farms are kept to clear it, not to condemn the session.
#[test]
fn a_server_link_broken_past_the_threshold_with_no_restore_is_lost() {
    let t = Instant::now();
    let mut m = LinkMonitor::default();
    m.notice(2110, SERVER_BROKEN, t);
    m.notice(2103, USFARM_BROKEN, t);
    m.notice(2107, HMDS_INACTIVE, t);
    m.notice(2157, SECDEF_BROKEN, t);
    assert!(m.lost(t + LOST_AFTER - s(1), LOST_AFTER).is_none(), "one second short");
    let out = m.lost(t + LOST_AFTER, LOST_AFTER).expect("lost at the threshold");
    assert_eq!(out.age, LOST_AFTER);
    assert_eq!(out.began, t);
    assert_eq!(out.parts.len(), 1, "the server link only; the farms and 2107 are not breaks");
    let reason = out.reason();
    assert!(reason.starts_with("IB Gateway has no link to IBKR for 300 s"), "{reason}");
    assert!(reason.contains("1100/2110"), "{reason}");
}

/// The threshold is 5 minutes because 90 s is inside the normal range: the CI box saw 1100 at 04:24:18 and 1102 at
/// 04:25:52 (94 s, healed by itself) and outages of 27 s and 31 s.
#[test]
fn an_outage_that_heals_by_itself_inside_the_normal_range_is_never_lost() {
    assert_eq!(LOST_AFTER, s(300));
    let t = Instant::now();
    let mut m = LinkMonitor::default();
    m.notice(1100, "Connectivity between IB and Trader Workstation has been lost.", t);
    assert!(m.lost(t + s(94), LOST_AFTER).is_none());
    m.notice(
        1102,
        "Connectivity between IB and Trader Workstation has been restored - data maintained.",
        t + s(94),
    );
    assert!(m.is_clear());
    assert!(m.lost(t + s(1000), LOST_AFTER).is_none());
}

/// THE SECOND DATA POINT: `[2103] usfarm` broken at 19:33:02 and OK at 19:35:18 (136 s) with the server link
/// fine. A farm alone is not a lost link, however long it lasts.
#[test]
fn a_farm_only_break_is_never_a_lost_link_but_a_server_break_is() {
    let t = Instant::now();
    let mut farms = LinkMonitor::default();
    farms.notice(2103, USFARM_BROKEN, t);
    farms.notice(2157, SECDEF_BROKEN, t);
    assert!(farms.lost(t + s(301), LOST_AFTER).is_none(), "a farm-only break for 301 s");
    assert!(farms.lost(t + s(36_000), LOST_AFTER).is_none(), "nor for ten hours");
    assert!(farms.is_clear(), "farms do not count");

    let mut server = LinkMonitor::default();
    server.notice(2110, SERVER_BROKEN, t);
    assert!(server.lost(t + s(299), LOST_AFTER).is_none());
    assert!(server.lost(t + s(301), LOST_AFTER).is_some(), "a server-link break for 301 s");
}

#[test]
fn a_restore_before_the_threshold_clears_it() {
    let t = Instant::now();
    let mut m = LinkMonitor::default();
    m.notice(2110, SERVER_BROKEN, t);
    assert!(!m.is_clear());
    m.notice(
        1102,
        "Connectivity between IB and Trader Workstation has been restored - data maintained.",
        t + s(30),
    );
    assert!(m.is_clear());
    m.notice(2110, SERVER_BROKEN, t + s(600));
    m.notice(
        1101,
        "Connectivity between IB and Trader Workstation has been restored - data lost.",
        t + s(610),
    );
    assert!(m.is_clear(), "1101 restores too");
}

/// After a socket reconnect `ibapi` does not replay restoration notices, so a recorded 1100 could never learn
/// the link is back: its own `-8` notice starts the monitor from a clean slate.
#[test]
fn a_socket_reconnect_forgets_what_was_recorded() {
    let t = Instant::now();
    let mut m = LinkMonitor::default();
    m.notice(1100, "Connectivity between IB and Trader Workstation has been lost.", t);
    m.notice(2103, USFARM_BROKEN, t);
    m.notice(ibapi::TRANSPORT_RECONNECT_CODE, "transport reconnected to TWS/Gateway", t + s(10));
    assert!(m.is_clear());
    assert!(m.broken.is_empty(), "the farm keys are gone too");
    assert!(m.lost(t + s(1000), LOST_AFTER).is_none());
}

/// The OK of a farm that was broken also proves the server link is back; the OK of a farm that was never broken
/// (every healthy handshake sends them, and the 10:28:57 burst had secdefil OK beside a broken server link)
/// says nothing.
#[test]
fn a_farm_ok_clears_only_what_it_names_and_an_unbroken_one_clears_nothing() {
    let t = Instant::now();
    let mut m = LinkMonitor::default();
    m.notice(2110, SERVER_BROKEN, t);
    m.notice(2103, USFARM_BROKEN, t);
    m.notice(2158, SECDEF_OK, t + s(1));
    m.notice(2106, "HMDS data farm connection is OK:ushmds", t + s(1));
    assert!(
        m.lost(t + LOST_AFTER, LOST_AFTER).is_some(),
        "an OK for a farm never broken restores nothing"
    );
    m.notice(2104, "Market data farm connection is OK:eufarm", t + s(2));
    assert!(m.lost(t + LOST_AFTER, LOST_AFTER).is_some(), "another farm's OK clears no usfarm");
    m.notice(2104, USFARM_OK, t + s(50));
    assert!(
        m.is_clear() && m.broken.is_empty(),
        "usfarm back: the farm and the server link are back"
    );

    m.notice(2157, SECDEF_BROKEN, t + s(60));
    assert!(!m.broken.is_empty());
    m.notice(2158, SECDEF_OK, t + s(70));
    assert!(m.broken.is_empty());
}

/// The clock of a part starts when it FIRST broke: a repeated notice (the burst repeats per connection) does not
/// restart it.
#[test]
fn a_repeated_break_keeps_its_first_time() {
    let t = Instant::now();
    let mut m = LinkMonitor::default();
    m.notice(2110, SERVER_BROKEN, t);
    m.notice(2110, SERVER_BROKEN, t + s(280));
    assert!(m.lost(t + LOST_AFTER, LOST_AFTER).is_some());
}

#[test]
fn the_socket_and_a_snapshot_are_parts_of_the_link_too() {
    let t = Instant::now();
    let mut m = LinkMonitor::default();
    m.set(SOCKET, true, t);
    assert!(m.lost(t + LOST_AFTER, LOST_AFTER).is_some(), "ibapi is not connected");
    m.set(SOCKET, false, t + s(10));
    assert!(m.is_clear());
    m.set(SNAPSHOT, true, t + s(400));
    assert!(
        m.lost(t + s(400) + LOST_AFTER, LOST_AFTER).is_some(),
        "the venue did not answer a request"
    );
    m.set(SNAPSHOT, false, t + s(450));
    assert!(m.is_clear());
}

// ---- the watch ----------------------------------------------------------------------------------

type Queue = Arc<Mutex<VecDeque<(i32, String)>>>;

struct Script(Queue);

impl Notices for Script {
    fn next(&mut self) -> Option<(i32, String)> {
        self.0.lock().unwrap().pop_front()
    }
}

fn watch(gate: &Arc<GatewayGate>, t: Instant) -> (Watch, Queue) {
    let queue = Arc::new(Mutex::new(VecDeque::new()));
    let setup = Watching { gate: Arc::clone(gate), times: WatchTimes::PRODUCTION };
    (Watch::new(setup, Some(Box::new(Script(Arc::clone(&queue)))), t), queue)
}

fn gate() -> Arc<GatewayGate> {
    Arc::new(GatewayGate::new("127.0.0.1:9", Duration::from_millis(10), false))
}

/// THE WATCHDOG: a fake gateway that reports [2110] and never restores is declared LOST at the threshold with
/// the words an operator reads, the gate is armed (rebuilds are spaced from then on), and the first rebuild is
/// refused until the schedule allows it. The farms broken beside it change nothing.
#[test]
fn a_gateway_that_reports_2110_and_never_restores_is_marked_lost_and_arms_the_schedule() {
    let t = Instant::now();
    let gate = gate();
    let (mut w, queue) = watch(&gate, t);
    {
        let mut q = queue.lock().unwrap();
        q.push_back((2110, SERVER_BROKEN.to_string()));
        q.push_back((2103, USFARM_BROKEN.to_string()));
        q.push_back((2157, SECDEF_BROKEN.to_string()));
    }
    assert_eq!(w.step(true, t), None);
    assert_eq!(w.step(true, t + s(150)), None);
    assert_eq!(w.step(true, t + LOST_AFTER - s(1)), None);
    assert!(!gate.is_armed());
    let reason = w.step(true, t + LOST_AFTER + s(1)).expect("lost after the threshold");
    assert!(reason.contains("IB Gateway has no link to IBKR for 301 s"), "{reason}");
    assert!(gate.is_armed(), "the loss armed the rebuild schedule");
    assert!(gate.permit_exec().is_err(), "the rebuild is not at once");
}

/// Farms broken for far longer than the threshold declare nothing and arm nothing.
#[test]
fn a_gateway_with_only_a_farm_down_is_left_alone() {
    let t = Instant::now();
    let gate = gate();
    let (mut w, queue) = watch(&gate, t);
    queue.lock().unwrap().push_back((2103, USFARM_BROKEN.to_string()));
    queue.lock().unwrap().push_back((2157, SECDEF_BROKEN.to_string()));
    assert_eq!(w.step(true, t), None);
    assert_eq!(w.step(true, t + s(301)), None);
    assert_eq!(w.step(true, t + s(7_200)), None);
    assert!(!gate.is_armed());
}

/// A fake that restores in time: nothing is declared, nothing is armed, and a session that then runs clean
/// through its probation makes the gate forget an earlier outage.
#[test]
fn a_gateway_that_restores_heals_and_resets_the_schedule() {
    let t = Instant::now();
    let gate = gate();
    {
        let mut h = gate.heal();
        h.lost(t, t, 0);
        assert!(h.is_armed());
    }
    let (mut w, queue) = watch(&gate, t);
    queue.lock().unwrap().push_back((2110, SERVER_BROKEN.to_string()));
    assert_eq!(w.step(true, t), None);
    queue.lock().unwrap().push_back((
        1102,
        "Connectivity between IB and Trader Workstation has been restored".to_string(),
    ));
    assert_eq!(w.step(true, t + s(60)), None, "restored in time: not lost");
    assert_eq!(w.step(true, t + s(60) + PROBATION - s(1)), None);
    assert!(gate.is_armed(), "not yet through probation");
    assert_eq!(w.step(true, t + s(60) + PROBATION), None);
    assert!(!gate.is_armed(), "a clean probation resets the schedule");
}

/// A break inside the probation restarts it: a link that flaps never resets the schedule.
#[test]
fn a_break_inside_the_probation_restarts_it() {
    let t = Instant::now();
    let gate = gate();
    gate.heal().lost(t, t, 0);
    let (mut w, queue) = watch(&gate, t);
    assert_eq!(w.step(true, t + s(200)), None);
    queue.lock().unwrap().push_back((2110, SERVER_BROKEN.to_string()));
    assert_eq!(w.step(true, t + s(250)), None);
    queue.lock().unwrap().push_back((1102, "restored".to_string()));
    assert_eq!(w.step(true, t + s(260)), None);
    assert_eq!(w.step(true, t + s(260) + PROBATION - s(1)), None);
    assert!(gate.is_armed());
    assert_eq!(w.step(true, t + s(260) + PROBATION), None);
    assert!(!gate.is_armed());
}

#[test]
fn an_unconnected_socket_and_an_unanswered_snapshot_are_lost_too() {
    let t = Instant::now();
    let gate = gate();
    let (mut w, _queue) = watch(&gate, t);
    assert_eq!(w.step(false, t), None);
    assert!(
        w.step(false, t + LOST_AFTER + s(1)).expect("lost").contains("API socket is not connected")
    );

    let (mut w, _queue) = watch(&gate, t);
    w.snapshot(false, t);
    assert!(
        w.step(true, t + LOST_AFTER + s(1)).expect("lost").contains("did not answer a snapshot")
    );
    let (mut w, _queue) = watch(&gate, t);
    w.snapshot(false, t);
    w.snapshot(true, t + s(20));
    assert_eq!(w.step(true, t + s(1000)), None, "a completed snapshot clears it");
}

/// One incomplete reply (the stranded-fill sweep's `request_executions`) after the gateway has answered a
/// snapshot is not a dead link; before any answer it is (the learning of a gateway with no link).
#[test]
fn an_incomplete_snapshot_is_ignored_once_the_session_has_had_one_answered() {
    let t = Instant::now();
    let gate = gate();
    let (mut w, _queue) = watch(&gate, t);
    w.snapshot(true, t);
    w.snapshot(false, t + s(10));
    assert_eq!(w.step(true, t + s(1000)), None, "the gateway has answered before");
    assert!(gate.heal().failures() == 0 && !gate.is_armed());
}
