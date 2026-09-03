//! Task 13 (reconnect-triggered reconcile): a scripted-stream test proving `run_resync_supervisor`'s
//! new `on_reconcile` trigger fires exactly once per reconnect (session-gen bump), AFTER the
//! event-replay it already performs, and NEVER on the initial connect. Mirrors the harness used by
//! `resync_supervisor_tests::resync_fires_on_gen_bump_and_exits_with_the_pump` in
//! `src/user_data.rs` — this test is the `on_reconcile`-focused twin, offline (no venue I/O).

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use vike_bridge_core::user_data::run_resync_supervisor;
use vike_model::events::{Event, OrderSubmitted};

/// How long a CROSS-THREAD handshake below may take before the test declares failure — the twin of
/// `user_data.rs`'s own `TEST_HANDSHAKE_DEADLINE`, which is `#[cfg(test)]`-private to that crate's
/// unit tests and so cannot be imported here.
///
/// Every use sits in a poll-until-success loop that sleeps ~2 ms and breaks the instant the
/// condition holds, so this bounds only how long an actually broken test takes to fail — never the
/// happy path. It was five hardcoded 3 s literals over the same ~15 ms window; #1020 widened the
/// sibling module's copies to 30 s for a flake whose real cause was `run_resync_supervisor` sampling
/// its gen baseline on its own thread (a lost edge, not a slow one — the 30 s deadline blew at
/// 30.083 s) and never reached this file at all, leaving these latent. The baseline is a caller-owned
/// parameter now, so what remains is one `poll` (1 ms) plus `settle` plus a scheduling hop.
const TEST_HANDSHAKE_DEADLINE: Duration = Duration::from_secs(5);

#[test]
fn reconcile_trigger_fires_once_after_settle_and_never_on_initial_connect() {
    let gen = Arc::new(AtomicU64::new(0));
    let weak = Arc::downgrade(&gen);
    let stop = Arc::new(AtomicBool::new(false));
    let fetches = Arc::new(AtomicU64::new(0));
    // WHAT HAPPENED, IN THE ORDER IT HAPPENED — the one log both halves append to: the emit closure
    // pushes each replayed event's id (on the supervisor's own thread), and the observer thread
    // below pushes "recon" when the reconcile poke lands. `run_resync_supervisor` emits the replayed
    // events and only THEN sends the poke, so the order recorded here is fixed by the program's own
    // happens-before edges — it cannot come out differently on a slow box, which is the whole reason
    // this test reads a SEQUENCE rather than a clock.
    let seq = Arc::new(Mutex::new(Vec::<String>::new()));
    // How long after the reconnect each poke arrived, pushed by the observer BEFORE its "recon"
    // marker — so an arrival is always readable once the marker it precedes is visible.
    let arrivals = Arc::new(Mutex::new(Vec::<Duration>::new()));
    let (recon_tx, recon_rx) = mpsc::channel::<()>();

    let (stop_t, fetches_t, seq_t) = (stop.clone(), fetches.clone(), seq.clone());
    let settle = Duration::from_millis(30);
    let handle = std::thread::spawn(move || {
        run_resync_supervisor(
            weak,
            0, // the gen as it stood when this test created it, sampled by the caller
            0, // no history floor: this test is about the reconcile trigger, not the restart law
            &stop_t,
            Duration::from_millis(1),
            settle,
            || {
                fetches_t.fetch_add(1, Ordering::Relaxed);
                vec![Event::OrderSubmitted(OrderSubmitted {
                    client_order_id: "resynced".into(),
                    ts: 0,
                })]
            },
            |ev| {
                if let Event::OrderSubmitted(e) = ev {
                    seq_t.lock().unwrap().push(e.client_order_id);
                }
                true
            },
            Some(recon_tx),
        );
    });

    // No reconnect yet → no event-replay AND no reconcile trigger (not on the initial connect).
    // Sound however far this sleep overruns: with no gen bump the supervisor has nothing to fetch
    // and nothing to send, so a late wake-up cannot put either into evidence.
    std::thread::sleep(Duration::from_millis(15));
    assert_eq!(fetches.load(Ordering::Relaxed), 0, "no event-replay without a reconnect");
    assert!(recon_rx.try_recv().is_err(), "reconcile trigger must not fire on the initial connect");

    // From here the channel belongs to the observer, which BLOCKS on `recv` — so it timestamps the
    // poke's ARRIVAL rather than discovering it on a later poll, and it ends itself when the
    // supervisor drops the sender on exit. `t0` is taken before the bump, so a recorded wait can
    // only over-state the real one: the safe direction for the settle assertion below.
    let t0 = Instant::now();
    let (seq_o, arrivals_o) = (seq.clone(), arrivals.clone());
    let observer = std::thread::spawn(move || {
        while recon_rx.recv().is_ok() {
            arrivals_o.lock().unwrap().push(t0.elapsed());
            seq_o.lock().unwrap().push("recon".to_string());
        }
    });

    // Simulate a reconnect (session-gen bump).
    gen.fetch_add(1, Ordering::Relaxed);

    // After settle, both the event-replay and the reconcile trigger fire, in that order. Poll the
    // SEQUENCE, never the `fetches` counter: the fetch closure bumps `fetches` as its FIRST act,
    // before it has even returned the events the supervisor then hands to the emit closure — so a
    // `fetches != 0` poll can win that race by an instant and observe an empty log. That is a race
    // in this test's observation point, not in the supervisor; polling the value actually being
    // asserted removes it.
    let deadline = Instant::now() + TEST_HANDSHAKE_DEADLINE;
    loop {
        let seen = seq.lock().unwrap().clone();
        if seen.iter().any(|marker| marker == "recon") {
            assert_eq!(
                seen,
                ["resynced", "recon"],
                "the event-replay must precede the reconcile trigger"
            );
            break;
        }
        assert!(Instant::now() < deadline, "no reconcile trigger on the gen bump — saw {seen:?}");
        std::thread::sleep(Duration::from_millis(2));
    }

    // The settle window, asserted in the ONE direction that cannot be wrong here: the supervisor
    // arms an ABSOLUTE `Instant::now() + settle` deadline AFTER it observes the bump, so a poke can
    // never legitimately arrive sooner than `settle` from `t0`, while an overrun only makes this
    // number LARGER and is evidence of nothing. The probe this replaces asserted the mirror image —
    // sleep half the window, require the channel still empty — which had ~16ms of margin against a
    // `thread::sleep` that is a FLOOR, not a ceiling, so a box that overran it accused the
    // supervisor of firing early (and `try_recv` CONSUMES on `Ok`, taking the poke with it).
    let waited = *arrivals.lock().unwrap().first().expect("an arrival precedes its marker");
    assert!(
        waited >= settle,
        "reconcile trigger fired {waited:?} after the bump — inside the {settle:?} settle window"
    );

    // Teardown: pump gone → supervisor self-exits, dropping the `on_reconcile` sender, which is
    // what ends the observer's `recv` loop.
    drop(gen);
    let deadline = Instant::now() + TEST_HANDSHAKE_DEADLINE;
    while !handle.is_finished() {
        assert!(Instant::now() < deadline, "supervisor did not exit when the pump was gone");
        std::thread::sleep(Duration::from_millis(2));
    }
    handle.join().unwrap();
    observer.join().unwrap();

    // Exactly once — read after BOTH threads are done, so no second poke can still be in flight
    // behind the first. The old post-hoc `try_recv().is_err()` assumed that instead of proving it.
    let seen = seq.lock().unwrap().clone();
    assert_eq!(
        seen,
        ["resynced", "recon"],
        "exactly one event-replay and one reconcile trigger per reconnect"
    );
    assert_eq!(fetches.load(Ordering::Relaxed), 1, "exactly one event-replay for one reconnect");
}

#[test]
fn none_trigger_is_byte_identical_to_pre_task13_behavior() {
    // Back-compat: `on_reconcile: None` must not change the event-replay path at all.
    let gen = Arc::new(AtomicU64::new(0));
    let weak = Arc::downgrade(&gen);
    let stop = Arc::new(AtomicBool::new(false));
    let fetches = Arc::new(AtomicU64::new(0));

    let (stop_t, fetches_t) = (stop.clone(), fetches.clone());
    let handle = std::thread::spawn(move || {
        run_resync_supervisor(
            weak,
            0, // the gen as it stood when this test created it, sampled by the caller
            0, // no history floor: this test is about the reconcile trigger, not the restart law
            &stop_t,
            Duration::from_millis(1),
            Duration::from_millis(0),
            || {
                fetches_t.fetch_add(1, Ordering::Relaxed);
                Vec::new()
            },
            |_ev| true,
            None,
        );
    });

    // No sleep here, deliberately. This bump used to need one — "give the supervisor time to capture
    // its baseline `last_gen` before bumping", the comment said, "a bug in this test, not the
    // supervisor". It was not a test bug: the supervisor sampling its own baseline is precisely the
    // defect, and losing that race lost the reconnect PERMANENTLY rather than briefly. The baseline
    // is the caller's `0` above now, so there is nothing left for this bump to race.
    gen.fetch_add(1, Ordering::Relaxed);
    let deadline = Instant::now() + TEST_HANDSHAKE_DEADLINE;
    while fetches.load(Ordering::Relaxed) == 0 {
        assert!(Instant::now() < deadline, "resync never fired on the gen bump");
        std::thread::sleep(Duration::from_millis(2));
    }

    drop(gen);
    stop.store(true, Ordering::Relaxed);
    let deadline = Instant::now() + TEST_HANDSHAKE_DEADLINE;
    while !handle.is_finished() {
        assert!(Instant::now() < deadline, "supervisor did not exit");
        std::thread::sleep(Duration::from_millis(2));
    }
    handle.join().unwrap();
}
