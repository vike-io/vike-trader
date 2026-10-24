//! **The late arm, with a real thread and a real socket**: the `gateway-late-arm` thread polls the
//! gatewayhub, fires exactly when it answers, and raises the process's one stop flag. The
//! boundaries (the 90 s guard, the backoff, the once-only latch) are the machine's own tests on a
//! virtual clock (`src/late_arm_tests.rs`); this holds that the THREAD does what the machine says.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use vike_tradehub::late_arm::{LateArmPlan, spawn};

#[path = "../../vike-gatewayhub-client/tests/support/fake_trade_peer.rs"]
#[allow(dead_code, clippy::allow_attributes)]
mod fake_trade_peer;

use fake_trade_peer::{TradePeer, TradePeerConfig, link_to};

fn plan(addr: &str) -> LateArmPlan {
    LateArmPlan {
        link: link_to(addr),
        waiting: vec!["binance".to_string(), "bybit#ALT".to_string()],
        gateway: true,
        datahub: None,
        feeds_why: None,
        restarts: vike_tradehub::late_arm::RestartLedger::none(),
    }
}

/// A process old enough, a gatewayhub that answers: the thread fires, sets the `fired` flag the
/// root reads for its exit status, and raises the stop flag the root's teardown waits on.
#[test]
fn a_gatewayhub_that_answers_fires_the_late_arm_and_stops_the_process() {
    let peer = TradePeer::start(TradePeerConfig::new());
    let stop = Arc::new(AtomicBool::new(false));
    let aged = Instant::now().checked_sub(Duration::from_secs(120)).expect("a clock that old");
    let handle = spawn(plan(&peer.addr_string()), aged, Arc::clone(&stop)).expect("a thread");

    assert!(
        TradePeer::wait_until(Duration::from_secs(5), || stop.load(Ordering::SeqCst)),
        "the late arm raised the stop flag"
    );
    assert!(handle.fired.load(Ordering::SeqCst), "and recorded that IT did, for the exit status");
    handle.join();
}

/// A gatewayhub that does not answer fires nothing: the thread keeps polling on its backoff and ends
/// at the process's own stop (here, ours), leaving `fired` clear.
#[test]
fn a_gatewayhub_that_stays_down_fires_nothing_and_the_thread_ends_at_stop() {
    let stop = Arc::new(AtomicBool::new(false));
    let aged = Instant::now().checked_sub(Duration::from_secs(120)).expect("a clock that old");
    let handle =
        spawn(plan(&fake_trade_peer::dead_addr()), aged, Arc::clone(&stop)).expect("a thread");

    std::thread::sleep(Duration::from_millis(700));
    assert!(!stop.load(Ordering::SeqCst), "nothing answered, so nothing stopped the process");
    assert!(!handle.fired.load(Ordering::SeqCst));

    stop.store(true, Ordering::SeqCst);
    let fired = Arc::clone(&handle.fired);
    handle.join();
    assert!(!fired.load(Ordering::SeqCst), "an ordinary stop is not a late arm");
}

/// **A process younger than the guard is never asked to restart**, even by a gatewayhub that is up:
/// the thread idles and `fired` stays clear.
#[test]
fn a_young_process_is_not_restarted() {
    let peer = TradePeer::start(TradePeerConfig::new());
    let stop = Arc::new(AtomicBool::new(false));
    let handle =
        spawn(plan(&peer.addr_string()), Instant::now(), Arc::clone(&stop)).expect("a thread");
    std::thread::sleep(Duration::from_millis(700));
    assert!(!stop.load(Ordering::SeqCst) && !handle.fired.load(Ordering::SeqCst));
    stop.store(true, Ordering::SeqCst);
    handle.join();
}

// ---- the feed-driven restart is counted before it is ordered (W12d) --------------------------------------------

fn feeds_plan(restarts: vike_tradehub::late_arm::RestartLedger) -> LateArmPlan {
    let mut plan = LateArmPlan::of(
        link_to(&fake_trade_peer::dead_addr()),
        Vec::new(),
        Some(vike_tradehub::feeds::DatahubProbe::new(|| None)),
    )
    .expect("the feeds wait");
    plan.restarts = restarts;
    plan
}

/// **A feed-driven fire is counted, then ordered**: the datahub answers, the thread raises the stop flag, and the
/// ledger in the state directory holds the restart (the next process start reads it: the budget).
#[test]
fn a_feed_driven_fire_writes_the_ledger_and_stops_the_process() {
    let dir = tempfile::tempdir().expect("a dir");
    let ledger = vike_tradehub::late_arm::RestartLedger::in_dir(dir.path());
    let stop = Arc::new(AtomicBool::new(false));
    let aged = Instant::now().checked_sub(Duration::from_secs(120)).expect("a clock that old");
    let handle = spawn(feeds_plan(ledger.clone()), aged, Arc::clone(&stop)).expect("a thread");
    assert!(
        TradePeer::wait_until(Duration::from_secs(5), || stop.load(Ordering::SeqCst)),
        "the late arm raised the stop flag"
    );
    assert!(handle.fired.load(Ordering::SeqCst));
    assert_eq!(ledger.recent(vike_model::now_ms()), Ok(1), "the restart is counted");
    handle.join();
}

/// **A restart that cannot be counted is not ordered**: an uncounted restart is how the budget would be
/// bypassed, so a ledger that cannot be written leaves the process up (and says so).
#[test]
fn a_restart_that_cannot_be_counted_is_not_ordered() {
    let dir = tempfile::tempdir().expect("a dir");
    let missing = dir.path().join("no-such-dir");
    let stop = Arc::new(AtomicBool::new(false));
    let aged = Instant::now().checked_sub(Duration::from_secs(120)).expect("a clock that old");
    let handle = spawn(
        feeds_plan(vike_tradehub::late_arm::RestartLedger::in_dir(&missing)),
        aged,
        Arc::clone(&stop),
    )
    .expect("a thread");
    std::thread::sleep(Duration::from_millis(700));
    assert!(!stop.load(Ordering::SeqCst), "no restart was ordered");
    assert!(!handle.fired.load(Ordering::SeqCst));
    stop.store(true, Ordering::SeqCst);
    handle.join();
}
