//! `poller`: the stop-aware background-poller scaffold the Telegram long-poll thread is built from —
//! the [`sleep_stop_aware`] tick sleep, the stop-flag + `Drop`-joining owner handle [`StopHandle`],
//! and the named-thread spawn [`spawn_poller`]. Gating and the tick logic stay at the call site
//! ([`super::maybe_spawn`]), which spawns only what its caller already decided to start.
//!
//! ## Where this came from
//!
//! The scaffold is `vike_bridge_core::poller`'s, restated here (wave 2, W15): this daemon links no
//! venue bridge and no `vike-bridge-core` any more (the venue half is the gatewayhub's, decision
//! 0164, and `crates/vike-ops/tests/architecture/named_run_closure_gate/fences.rs` fences the tradehub's
//! closure against the transport core), and the Telegram channel was the one remaining caller. The
//! three items and their contract below are the originals' unchanged; the bridges' settlement
//! pollers keep using the bridge-core copy. The ledger-file reader that module also holds is the
//! bridges' alone and did not come.
//!
//! ## The contract
//! - **Slice-granular responsiveness.** [`sleep_stop_aware`] naps `slice.min(total)` at a time and
//!   re-checks `stop` between naps, so a raised stop is honoured within ~one slice
//!   ([`STOP_POLL_SLICE`]). The last nap can overshoot by up to one slice, so `total` is a CADENCE,
//!   not a deadline.
//! - **`Drop` joins.** [`StopHandle`] signals stop and JOINS on `Drop`, like
//!   [`shutdown`](StopHandle::shutdown), so a dropped handle never leaks the thread.
//! - **Named threads.** [`spawn_poller`] names the thread (`vike-…`), hands the body an owned clone
//!   of the stop flag, and returns the joining handle.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// The stop-poll granularity the poller passes as `slice`: a raised stop is honoured within
/// ~100 ms of wherever the tick sleep happens to be.
pub const STOP_POLL_SLICE: Duration = Duration::from_millis(100);

/// Stop-aware sleep: naps `slice.min(total)` at a time, re-checking `stop` between naps, up to
/// `total`. Returns true if `stop` fired during the sleep (the poller loop's `break` signal). The
/// nap schedule's overshoot: module doc.
pub fn sleep_stop_aware(stop: &AtomicBool, total: Duration, slice: Duration) -> bool {
    let deadline = Instant::now() + total;
    while Instant::now() < deadline {
        if stop.load(Ordering::Relaxed) {
            return true;
        }
        thread::sleep(slice.min(total));
    }
    false
}

/// Owner-side handle to a spawned poller thread: stop-aware shutdown, `Drop`-joining like
/// [`shutdown`](Self::shutdown).
pub struct StopHandle {
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl StopHandle {
    /// Signal the thread to stop, without waiting for it. The thread observes the flag within ~one
    /// sleep slice; a later [`shutdown`](Self::shutdown) or `Drop` still joins it.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    /// Signal the thread to stop and join it.
    pub fn shutdown(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(j) = self.join.take() {
            // A `join` Err is a panic the thread already reported; nothing to recover here.
            let _ = j.join();
        }
    }
}

impl Drop for StopHandle {
    fn drop(&mut self) {
        if let Some(j) = self.join.take() {
            self.stop.store(true, Ordering::Relaxed);
            // As in `shutdown`: a panic was already reported by the thread.
            let _ = j.join();
        }
    }
}

/// Spawn a named poller thread wired to a fresh stop flag; `body` receives its owned clone of the
/// flag (loop `while !stop.load(Ordering::Relaxed)`, tick, [`sleep_stop_aware`], repeat). Every
/// opt-in gate stays at the call site.
pub fn spawn_poller<F>(name: &str, body: F) -> StopHandle
where
    F: FnOnce(Arc<AtomicBool>) + Send + 'static,
{
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = Arc::clone(&stop);
    let join = thread::Builder::new()
        .name(name.to_string())
        .spawn(move || body(thread_stop))
        .unwrap_or_else(|e| panic!("spawn {name} thread: {e}"));
    StopHandle { stop, join: Some(join) }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let name = Arc::new(std::sync::Mutex::new(None::<String>));
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
}
