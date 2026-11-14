use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use super::{STOP_POLL_SLICE, sleep_stop_aware, spawn_poller};

/// The whole point of the helper: a tick sleep must NOT hold shutdown hostage for its full
/// duration. A stop raised mid-sleep is observed within ~a slice, and reported (`true`).
#[test]
fn a_stop_mid_sleep_returns_true_early() {
    let stop = Arc::new(AtomicBool::new(false));
    let st = Arc::clone(&stop);
    thread::spawn(move || {
        thread::sleep(Duration::from_millis(120));
        st.store(true, Ordering::Relaxed);
    });
    let started = Instant::now();
    assert!(
        sleep_stop_aware(&stop, Duration::from_secs(30), STOP_POLL_SLICE),
        "a stop during the sleep must be reported"
    );
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "stop must cut the sleep short, took {:?}",
        started.elapsed()
    );
}

/// Absent a stop it sleeps at least the full total and reports `false`; an already-stopped
/// flag returns `true` immediately without napping at all.
#[test]
fn sleeps_the_full_span_then_returns_instantly_once_stopped() {
    let stop = Arc::new(AtomicBool::new(false));
    let started = Instant::now();
    assert!(!sleep_stop_aware(&stop, Duration::from_millis(250), STOP_POLL_SLICE));
    assert!(started.elapsed() >= Duration::from_millis(250), "must not return early");

    stop.store(true, Ordering::Relaxed);
    let started = Instant::now();
    assert!(sleep_stop_aware(&stop, Duration::from_secs(30), STOP_POLL_SLICE));
    assert!(started.elapsed() < Duration::from_secs(1), "already stopped → no nap");
}

/// Dropping the handle joins the thread: by the time `drop` returns, the body has fully run its
/// post-loop epilogue (a dropped handle never leaks — or races — the background thread).
#[test]
fn drop_joins_the_thread() {
    let finished = Arc::new(AtomicBool::new(false));
    let fin = Arc::clone(&finished);
    let handle = spawn_poller("vike-test-poller-drop", move |stop| {
        while !stop.load(Ordering::Relaxed) {
            if sleep_stop_aware(&stop, Duration::from_secs(30), Duration::from_millis(10)) {
                break;
            }
        }
        fin.store(true, Ordering::Relaxed);
    });
    assert!(!finished.load(Ordering::Relaxed), "the poller is parked in its tick sleep");
    drop(handle);
    assert!(finished.load(Ordering::Relaxed), "Drop must stop AND join the thread");
}

/// `shutdown()` is the explicit spelling of the same stop+join; `stop()` alone flags without
/// consuming the handle, and the spawned thread carries the requested name.
#[test]
fn shutdown_joins_and_stop_alone_only_flags() {
    let finished = Arc::new(AtomicBool::new(false));
    let fin = Arc::clone(&finished);
    let name = Arc::new(Mutex::new(None::<String>));
    let seen = Arc::clone(&name);
    let handle = spawn_poller("vike-test-poller-shutdown", move |stop| {
        *seen.lock().expect("name slot") = thread::current().name().map(str::to_string);
        while !stop.load(Ordering::Relaxed) {
            if sleep_stop_aware(&stop, Duration::from_secs(30), Duration::from_millis(10)) {
                break;
            }
        }
        fin.store(true, Ordering::Relaxed);
    });
    handle.stop(); // flags only — the handle is still owned and joinable
    handle.shutdown();
    assert!(finished.load(Ordering::Relaxed), "shutdown must join the thread");
    assert_eq!(name.lock().expect("name slot").as_deref(), Some("vike-test-poller-shutdown"));
}
