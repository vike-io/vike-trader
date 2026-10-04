//! Coverage for finding A8's central deliverable: the `Weak`/`drop` self-exit trick and the
//! stop/join semantics live ONCE in [`spawn_pump_with_resync`], driven here without any venue
//! I/O (the pump body is a plain closure, no WS).
use super::*;
use std::sync::Mutex;
use vike_exec::event_channel;

/// A pump body that bumps the reconnect hook `bumps` times (spaced so the supervisor observes
/// each), then RETURNS on its own — never touching `stop`. This is the crux: because the
/// builder dropped its `session_gen` strong ref, the hook is the ONLY strong owner, so when the
/// body returns the supervisor's `Weak` upgrade fails and it self-exits WITHOUT a shutdown.
#[test]
fn resync_self_exits_when_the_pump_body_returns() {
    let (events, _ingest) = event_channel(16);
    let fetches = Arc::new(AtomicU64::new(0));
    let fetches_f = Arc::clone(&fetches);

    let feed = spawn_pump_with_resync(ResyncPumpSpec {
        pump_thread_name: "test-pump-selfexit".into(),
        resync_thread_name: "test-resync-selfexit".into(),
        poll: Duration::from_millis(1),
        settle: Duration::from_millis(0),
        events,
        resync_fetch: move || {
            fetches_f.fetch_add(1, Ordering::Relaxed);
            Vec::new() // nothing to emit — this test cares about the lifetime, not replay content
        },
        on_reconcile: None,
        pump_body: move |_stop: Arc<AtomicBool>, mut on_reconnect: ReconnectHook| {
            on_reconnect(); // simulate a WS re-open → gen bump
            std::thread::sleep(Duration::from_millis(30));
            on_reconnect();
            std::thread::sleep(Duration::from_millis(30));
            Ok(()) // pump ends ON ITS OWN — no stop was ever raised
        },
    });

    // The resync supervisor must finish on its own, purely because the pump's hook (the sole
    // strong `session_gen` ref) was dropped when the body returned.
    let deadline = Instant::now() + TEST_HANDSHAKE_DEADLINE;
    while !feed.resync.is_finished() {
        assert!(Instant::now() < deadline, "supervisor did not self-exit when the pump ended");
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(fetches.load(Ordering::Relaxed) >= 1, "a reconnect bump must have driven a resync");
    // shutdown is still safe (idempotent) and joins both threads without hanging.
    feed.shutdown().unwrap();
}

/// Stop/join semantics: a pump body that loops until `stop` fires; `shutdown()` raises stop,
/// joins the pump (returning its `Ok`), then the supervisor — no hang.
#[test]
fn shutdown_stops_and_joins_both_threads() {
    let (events, _ingest) = event_channel(16);
    let feed = spawn_pump_with_resync(ResyncPumpSpec {
        pump_thread_name: "test-pump-stop".into(),
        resync_thread_name: "test-resync-stop".into(),
        poll: Duration::from_millis(1),
        settle: Duration::from_millis(0),
        events,
        resync_fetch: Vec::<Event>::new,
        on_reconcile: None,
        pump_body: move |stop: Arc<AtomicBool>, _on_reconnect| {
            while !stop.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(1));
            }
            Ok(())
        },
    });
    // Not finished until we ask.
    std::thread::sleep(Duration::from_millis(20));
    assert!(!feed.pump.is_finished(), "pump must run until stop is raised");
    feed.shutdown().unwrap();
}

/// The `on_reconcile` poke is forwarded straight into the supervisor and fires once per
/// reconnect — the shared-builder home of the wiring each venue's `_with_resync` used to paste
/// (was binance's `resync_trigger_tests`, now covered here for ALL venues at once).
#[test]
fn on_reconcile_is_forwarded_and_fires_once_per_reconnect() {
    let (events, _ingest) = event_channel(16);
    let (recon_tx, recon_rx) = mpsc::channel::<()>();
    let armed = Arc::new(AtomicBool::new(false));
    let armed_p = Arc::clone(&armed);

    let feed = spawn_pump_with_resync(ResyncPumpSpec {
        pump_thread_name: "test-pump-recon".into(),
        resync_thread_name: "test-resync-recon".into(),
        poll: Duration::from_millis(1),
        settle: Duration::from_millis(0),
        events,
        resync_fetch: Vec::<Event>::new,
        on_reconcile: Some(recon_tx),
        pump_body: move |stop: Arc<AtomicBool>, mut on_reconnect: ReconnectHook| {
            // Wait to be armed, then bump exactly once, then idle until shutdown.
            while !armed_p.load(Ordering::Relaxed) && !stop.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(1));
            }
            on_reconnect();
            while !stop.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(1));
            }
            Ok(())
        },
    });

    // No reconnect yet → the trigger must not fire.
    std::thread::sleep(Duration::from_millis(15));
    assert!(recon_rx.try_recv().is_err(), "reconcile must not fire before a reconnect");

    armed.store(true, Ordering::Relaxed); // let the pump bump once
    let deadline = Instant::now() + TEST_HANDSHAKE_DEADLINE;
    loop {
        match recon_rx.try_recv() {
            Ok(()) => break,
            Err(mpsc::TryRecvError::Empty) => {
                assert!(Instant::now() < deadline, "reconcile trigger never fired");
                std::thread::sleep(Duration::from_millis(2));
            }
            Err(mpsc::TryRecvError::Disconnected) => panic!("supervisor dropped the sender"),
        }
    }
    assert!(recon_rx.try_recv().is_err(), "reconcile must fire exactly once per reconnect");
    feed.shutdown().unwrap();
}

/// **The permanently-lost edge, end to end.** The pump is spawned BEFORE the supervisor, so a
/// pump body whose very first act is a reconnect races the supervisor's startup. While the
/// baseline was sampled on the supervisor's own thread, losing that race did not delay the
/// reconcile — it deleted it: the supervisor adopted the already-bumped value, and its
/// `cur == last_gen` check then held for the life of the session. Both flakes #1020 widened
/// deadlines for were this, which is why the widening changed nothing (one blew at 30.083 s).
///
/// With the baseline sampled on the builder thread before `Builder::spawn`, this is
/// DETERMINISTIC — the supervisor cannot observe anything but 0, so the bump always registers,
/// and the loop below is not a flaky retry: it is a mutation gate. Restore the old
/// sample-on-the-supervisor-thread behavior and the rounds start losing the coin flip.
#[test]
fn a_reconnect_racing_the_supervisor_spawn_still_reconciles() {
    let deadline = Instant::now() + TEST_HANDSHAKE_DEADLINE;
    for round in 0..64 {
        let (events, _ingest) = event_channel(16);
        let (recon_tx, recon_rx) = mpsc::channel::<()>();

        let feed = spawn_pump_with_resync(ResyncPumpSpec {
            pump_thread_name: format!("test-pump-startrace-{round}"),
            resync_thread_name: format!("test-resync-startrace-{round}"),
            poll: Duration::from_millis(1),
            settle: Duration::from_millis(0),
            events,
            resync_fetch: Vec::<Event>::new,
            on_reconcile: Some(recon_tx),
            pump_body: move |stop: Arc<AtomicBool>, mut on_reconnect: ReconnectHook| {
                // No arming handshake, no sleep: bump as the FIRST act, so this reconnect is
                // as early as a reconnect can possibly be relative to the supervisor's start.
                on_reconnect();
                while !stop.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(1));
                }
                Ok(())
            },
        });

        loop {
            match recon_rx.try_recv() {
                Ok(()) => break,
                Err(mpsc::TryRecvError::Empty) => {
                    assert!(
                        Instant::now() < deadline,
                        "round {round}: a reconnect that beat the supervisor's start was LOST \
                             — the gen baseline is being sampled on the supervisor thread again"
                    );
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    panic!("supervisor dropped the sender")
                }
            }
        }
        feed.shutdown().unwrap();
    }
}

/// **The builder actually ARMS the history floor** — the half a direct
/// [`run_resync_supervisor`] test cannot see. `spawn_ms` is a parameter with `0` meaning "no
/// floor", so every venue's real path depends on this ONE line sampling a live clock; a builder
/// that passed `0` (or forwarded a stale value) would leave all eight production consumers
/// replaying previous sessions' fills while `resync_supervisor_tests` stayed green, because
/// those drive the supervisor directly and supply their own floor.
///
/// Drives the REAL builder — its own pump body, its own reconnect hook, its own supervisor
/// thread — with a resync that returns one fill stamped an hour ago and one stamped an hour
/// ahead, and reads the ingest the builder wired itself.
#[test]
fn the_builder_arms_the_history_floor_for_every_venue() {
    let (events, mut ingest) = event_channel(16);
    let now = vike_model::now_ms();
    let rows = Arc::new(Mutex::new(Some(vec![
        super::fill_at("c_prev", "e_old", now - 3_600_000),
        super::fill_at("c_this", "e_new", now + 3_600_000),
    ])));
    let rows_f = Arc::clone(&rows);

    let feed = spawn_pump_with_resync(ResyncPumpSpec {
        pump_thread_name: "test-pump-floor".into(),
        resync_thread_name: "test-resync-floor".into(),
        poll: Duration::from_millis(1),
        settle: Duration::from_millis(0),
        events,
        resync_fetch: move || rows_f.lock().unwrap().take().unwrap_or_default(),
        on_reconcile: None,
        pump_body: move |stop: Arc<AtomicBool>, mut on_reconnect: ReconnectHook| {
            on_reconnect(); // the WS re-open this lane replays on
            while !stop.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(1));
            }
            Ok(())
        },
    });

    // Wait for the post-spawn fill to arrive; the pre-spawn one would precede it in the same
    // batch, so seeing `e_new` first proves `e_old` was dropped rather than merely late.
    let deadline = Instant::now() + TEST_HANDSHAKE_DEADLINE;
    let mut seen: Vec<String> = Vec::new();
    while !seen.iter().any(|t| t == "e_new") {
        assert!(Instant::now() < deadline, "the builder's resync never delivered: {seen:?}");
        while let Ok(ev) = ingest.try_recv() {
            if let vike_exec::Ingest::Event(Event::Fill(f)) = ev {
                seen.push(f.trade_id.to_string());
            }
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    feed.shutdown().unwrap();
    while let Ok(ev) = ingest.try_recv() {
        if let vike_exec::Ingest::Event(Event::Fill(f)) = ev {
            seen.push(f.trade_id.to_string());
        }
    }
    assert!(
        !seen.iter().any(|t| t == "e_old"),
        "`spawn_pump_with_resync` did not arm the floor it samples — every venue's resync is \
             still replaying previous sessions' fills into `apply_fill`: {seen:?}"
    );
}
