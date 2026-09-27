//! The shared stoppable-periodic-worker harness — one named background thread that loops
//! `{ do a pass; interruptibly sleep }` until stopped, with deterministic teardown.
//!
//! This is the ONE place the pattern lives. It was hand-rolled independently at several sites
//! ([`crate::hist_sched::MaintenanceScheduler`] and vike-app-core's `JournalMaterializer` are the
//! two that share it now), each re-deriving the same four parts: an `AtomicBool` stop flag, a
//! stop-aware inter-pass sleep, `stop()` = flag-then-join, and a `Drop` mirroring `stop` so the
//! thread is never leaked. The hand-rolls diverged on the one part that matters — the sleep — and
//! only the [`Condvar`] flavor below is both prompt and lost-wakeup safe; the others polled.
//!
//! WHY THE CONDVAR (the whole point of sharing this). [`Shared::sleep_interruptible`] is a
//! `Condvar::wait_timeout_while` whose predicate is "not yet stopped", and [`Shared::signal_stop`]
//! sets the flag then notifies UNDER the sleep lock. Two consequences a polling sleep cannot give:
//! shutdown is INSTANT (the worker leaves its sleep the moment `stop` is signalled, instead of
//! waiting out a poll tick or a whole interval), and there is NO lost-wakeup window — the predicate
//! is re-checked under the same lock the notifier holds, so a `stop` racing the park cannot be
//! missed. A polling sleep is also imprecise at short intervals (it rounds the wait up to a whole
//! poll step); this one honors `dur` exactly.
//!
//! THE LOOP SHAPE STAYS AT THE CALL SITE. [`Worker::spawn`] owns the thread, the flag and the
//! join — NOT the body. Callers write their own loop against `&Shared`, because the shape is
//! genuinely per-site and load-bearing: the maintenance scheduler checks the flag BEFORE each pass
//! (a stop before the first tick runs zero passes), while the journal materializer runs a pass
//! FIRST and does one extra final drain after the flag is seen (a clean shutdown must lose no
//! fills). Folding either into a generic `run_every` would have silently changed the other.
//!
//! NOT FEATURE-GATED, deliberately: unlike its first caller ([`crate::hist_sched`], which is behind
//! `hist-datafusion`), this module is pure `std` and must stay visible on a default build — the
//! other caller, vike-app-core, depends on vike-data feature-free.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::Duration;

/// Lock a mutex, recovering the guard even if a prior holder panicked (poison). These mutexes only
/// guard the sleep/notify handshake — a poisoned lock must never wedge `stop()`.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// The worker's stop channel: a flag plus the [`Condvar`] the worker parks on between passes. Handed
/// to the thread body by reference; the loop polls [`is_stopped`](Self::is_stopped) and sleeps via
/// [`sleep_interruptible`](Self::sleep_interruptible).
///
/// Carries NO domain state — progress counters and the like belong to the caller's own struct, so
/// this stays the harness and nothing else.
pub struct Shared {
    /// Set by `signal_stop`; polled by the loop and re-checked inside the sleep predicate.
    stop: AtomicBool,
    /// Guards nothing but the wait/notify handshake (the data lives in `stop`); paired with `wake`.
    sleep_lock: Mutex<()>,
    /// Signalled by `signal_stop` to cut the inter-pass sleep short.
    wake: Condvar,
}

impl Shared {
    fn new() -> Self {
        Self { stop: AtomicBool::new(false), sleep_lock: Mutex::new(()), wake: Condvar::new() }
    }

    /// True once stop has been signalled. Cheap — call it at whatever point in the cycle the loop's
    /// contract wants the flag observed.
    pub fn is_stopped(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }

    /// Sleep up to `dur`, returning EARLY if `stop` is (or becomes) set. Implemented as a
    /// `Condvar::wait_timeout_while` whose predicate is "not yet stopped": it blocks while that
    /// holds, up to `dur`, and returns the instant `signal_stop` flips the flag + notifies (or on
    /// timeout). The predicate is re-checked under the lock, so there is no lost-wakeup window. An
    /// already-set flag (or a zero `dur`) returns immediately without parking.
    pub fn sleep_interruptible(&self, dur: Duration) {
        let guard = lock(&self.sleep_lock);
        let _ = self.wake.wait_timeout_while(guard, dur, |_| !self.stop.load(Ordering::SeqCst));
    }

    /// Set the stop flag, then notify the sleep Condvar UNDER its lock so the worker can't miss the
    /// wakeup in the gap between checking the flag and parking (lost-wakeup safe).
    fn signal_stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
        let _guard = lock(&self.sleep_lock);
        self.wake.notify_all();
    }
}

/// A running background worker thread. Construct with [`Worker::spawn`]; end with
/// [`Worker::stop`] (or just drop it — [`Drop`] stops and joins, so the thread is never leaked).
pub struct Worker {
    shared: Arc<Shared>,
    handle: Option<JoinHandle<()>>,
}

impl Worker {
    /// Spawn `body` on a thread named `name`, handing it the [`Shared`] stop channel. `body` is the
    /// caller's whole loop: it must poll [`Shared::is_stopped`] and use
    /// [`Shared::sleep_interruptible`] for its waits, or `stop()` will block until it returns.
    ///
    /// Panics only if the OS refuses the thread (same as the hand-rolled `spawn(..).expect(..)`
    /// sites this replaces — an unspawnable worker is a startup bug, not a runtime condition).
    pub fn spawn<F>(name: &str, body: F) -> Self
    where
        F: FnOnce(&Shared) + Send + 'static,
    {
        let shared = Arc::new(Shared::new());
        let worker = Arc::clone(&shared);
        let handle = std::thread::Builder::new()
            .name(name.to_string())
            .spawn(move || body(&worker))
            .unwrap_or_else(|e| panic!("spawn {name} thread: {e}"));
        Self { shared, handle: Some(handle) }
    }

    /// Signal the loop to stop and JOIN its thread — deterministic teardown (the thread has returned
    /// when this returns). Idempotent: a second call (or the [`Drop`]) is a no-op. Wakes the thread
    /// out of its inter-pass sleep immediately rather than waiting the interval.
    pub fn stop(&mut self) {
        self.shared.signal_stop();
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.stop(); // never leak the worker thread, even if the caller forgot to stop()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;
    use std::time::Instant;

    /// The headline property: `stop()` cuts a long inter-pass sleep short instead of waiting it out.
    /// The worker parks for an hour; if the Condvar wake were broken this would hang (or, with the
    /// polling sleep this harness replaced, wait out a poll tick), so the bound is generous — the
    /// point is "not the hour", not a tight latency claim on a loaded runner.
    #[test]
    fn stop_wakes_the_worker_out_of_a_long_sleep() {
        let passes = Arc::new(AtomicU64::new(0));
        let p = Arc::clone(&passes);
        let mut w = Worker::spawn("test-wake", move |shared| {
            while !shared.is_stopped() {
                p.fetch_add(1, Ordering::SeqCst);
                shared.sleep_interruptible(Duration::from_secs(3600));
            }
        });
        // let the first pass land, so we know we're stopping a thread that is parked in the sleep
        let start = Instant::now();
        while passes.load(Ordering::SeqCst) == 0 {
            assert!(start.elapsed() < Duration::from_secs(10), "worker never ran a pass");
            std::thread::sleep(Duration::from_millis(1));
        }
        let t = Instant::now();
        w.stop();
        assert!(
            t.elapsed() < Duration::from_secs(60),
            "stop() waited out the sleep instead of cutting it short"
        );
        assert_eq!(passes.load(Ordering::SeqCst), 1, "the woken sleep must not start another pass");
    }

    /// `stop()` is idempotent and `Drop` mirrors it — the two properties every call site relies on
    /// (a `stop()`-then-drop, or a bare drop, must both be safe and must both join).
    #[test]
    fn stop_is_idempotent_and_drop_joins() {
        let done = Arc::new(AtomicBool::new(false));
        let d = Arc::clone(&done);
        let mut w = Worker::spawn("test-idem", move |shared| {
            while !shared.is_stopped() {
                shared.sleep_interruptible(Duration::from_secs(3600));
            }
            d.store(true, Ordering::SeqCst);
        });
        w.stop();
        assert!(done.load(Ordering::SeqCst), "stop() joined: the body has returned");
        w.stop(); // second stop: harmless no-op
        drop(w); // Drop after an explicit stop: also a no-op
    }

    /// A bare drop (caller never called `stop`) must still stop AND join the thread — the no-leak
    /// guarantee. Observed via an Arc strong count that can only drop once the body has returned.
    #[test]
    fn drop_alone_stops_and_joins() {
        let live = Arc::new(());
        let held = Arc::clone(&live);
        let w = Worker::spawn("test-drop", move |shared| {
            let _held = held; // moved into the body; released only when the thread returns
            while !shared.is_stopped() {
                shared.sleep_interruptible(Duration::from_secs(3600));
            }
        });
        assert_eq!(Arc::strong_count(&live), 2, "the body holds its clone while running");
        drop(w);
        assert_eq!(Arc::strong_count(&live), 1, "drop stopped AND joined the worker");
    }

    /// An already-stopped `Shared` never parks: the sleep returns immediately. This is what makes a
    /// stop signalled mid-pass end the loop the moment that pass returns.
    #[test]
    fn sleep_returns_immediately_once_stopped() {
        let shared = Shared::new();
        shared.signal_stop();
        let t = Instant::now();
        shared.sleep_interruptible(Duration::from_secs(3600));
        assert!(t.elapsed() < Duration::from_secs(60), "a stopped worker must not park");
        assert!(shared.is_stopped());
    }
}
