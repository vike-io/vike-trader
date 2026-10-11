//! Reconnect-triggered reconcile, offline: `run_resync_supervisor`'s `on_reconcile` trigger fires
//! exactly once per reconnect (session-gen bump), AFTER its event replay, and NEVER on the initial
//! connect. The `on_reconcile`-focused twin of the unit test
//! `resync_fires_on_gen_bump_and_exits_with_the_pump`.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use vike_bridge_core::user_data::run_resync_supervisor;
use vike_model::events::{Event, OrderSubmitted};

/// How long a CROSS-THREAD handshake below may take before the test fails: the twin of
/// `user_data.rs`'s `#[cfg(test)]`-private `TEST_HANDSHAKE_DEADLINE`, not importable here. Every
/// use is a poll-until-success loop, so this bounds only how long a broken test takes to fail,
/// never the happy path (one 1 ms `poll` plus `settle` plus a scheduling hop).
const TEST_HANDSHAKE_DEADLINE: Duration = Duration::from_secs(5);

#[test]
fn reconcile_trigger_fires_once_after_settle_and_never_on_initial_connect() {
    let generation = Arc::new(AtomicU64::new(0));
    let weak = Arc::downgrade(&generation);
    let stop = Arc::new(AtomicBool::new(false));
    let fetches = Arc::new(AtomicU64::new(0));
    // WHAT HAPPENED, IN ORDER: the emit closure pushes each replayed event's id (supervisor
    // thread), the observer pushes "recon" when the poke lands. The supervisor emits and only THEN
    // pokes, so happens-before fixes this order on any box: the test reads a SEQUENCE, not a clock.
    let seq = Arc::new(Mutex::new(Vec::<String>::new()));
    // Each poke's arrival time, pushed BEFORE its "recon" marker, so readable once the marker is.
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

    // No reconnect yet → no event replay AND no reconcile trigger. Sound however far this sleep
    // overruns: with no gen bump there is nothing to fetch or send.
    std::thread::sleep(Duration::from_millis(15));
    assert_eq!(fetches.load(Ordering::Relaxed), 0, "no event-replay without a reconnect");
    assert!(recon_rx.try_recv().is_err(), "reconcile trigger must not fire on the initial connect");

    // The observer BLOCKS on `recv`, so it timestamps the poke's ARRIVAL, and ends when the
    // supervisor drops the sender. `t0` precedes the bump, so a recorded wait can only over-state
    // the real one: the safe direction for the settle assertion.
    let t0 = Instant::now();
    let (seq_o, arrivals_o) = (seq.clone(), arrivals.clone());
    let observer = std::thread::spawn(move || {
        while recon_rx.recv().is_ok() {
            arrivals_o.lock().unwrap().push(t0.elapsed());
            seq_o.lock().unwrap().push("recon".to_string());
        }
    });

    // Simulate a reconnect (session-gen bump).
    generation.fetch_add(1, Ordering::Relaxed);

    // After settle, the event replay and the reconcile trigger fire, in that order. Poll the
    // SEQUENCE, never `fetches`: the fetch closure bumps it before returning the events, so a
    // `fetches != 0` poll can observe an empty log (a race in the observation point, not the
    // supervisor).
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

    // The settle window, asserted in the ONE direction that cannot be wrong: the supervisor arms
    // `Instant::now() + settle` AFTER it observes the bump, so a poke never arrives sooner than
    // `settle` from `t0`, while an overrun only makes the number larger. Never assert the mirror
    // image (sleep half the window, require an empty channel): `thread::sleep` is a FLOOR, so a
    // slow box would accuse the supervisor of firing early.
    let waited = *arrivals.lock().unwrap().first().expect("an arrival precedes its marker");
    assert!(
        waited >= settle,
        "reconcile trigger fired {waited:?} after the bump — inside the {settle:?} settle window"
    );

    // Teardown: pump gone → supervisor self-exits, dropping the sender that ends the observer.
    drop(generation);
    let deadline = Instant::now() + TEST_HANDSHAKE_DEADLINE;
    while !handle.is_finished() {
        assert!(Instant::now() < deadline, "supervisor did not exit when the pump was gone");
        std::thread::sleep(Duration::from_millis(2));
    }
    handle.join().unwrap();
    observer.join().unwrap();

    // Exactly once: read after BOTH threads are done, so no second poke can still be in flight.
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
    // `on_reconcile: None` must not change the event-replay path at all.
    let generation = Arc::new(AtomicU64::new(0));
    let weak = Arc::downgrade(&generation);
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

    // No sleep before the bump, deliberately: the gen baseline is the caller's `0` above, so there
    // is no supervisor-side sample for the bump to race (that race lost a reconnect PERMANENTLY).
    generation.fetch_add(1, Ordering::Relaxed);
    let deadline = Instant::now() + TEST_HANDSHAKE_DEADLINE;
    while fetches.load(Ordering::Relaxed) == 0 {
        assert!(Instant::now() < deadline, "resync never fired on the gen bump");
        std::thread::sleep(Duration::from_millis(2));
    }

    drop(generation);
    stop.store(true, Ordering::Relaxed);
    let deadline = Instant::now() + TEST_HANDSHAKE_DEADLINE;
    while !handle.is_finished() {
        assert!(Instant::now() < deadline, "supervisor did not exit");
        std::thread::sleep(Duration::from_millis(2));
    }
    handle.join().unwrap();
}
