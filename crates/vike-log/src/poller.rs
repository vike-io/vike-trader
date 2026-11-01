//! `poller`: the ONE stop-aware background-poller scaffold every opt-in poller thread is built from
//! (the venue bridges' settlement pollers and depth pumps, and the tradehub's Telegram long poll):
//! the [`sleep_stop_aware`] tick sleep, the stop-flag + `Drop`-joining owner handle [`StopHandle`],
//! and the named-thread spawn [`spawn_poller`]. Gating and each poller's tick logic stay at the call
//! sites, which spawn only what their caller already decided to start.
//!
//! ## Why it lives in the logging crate
//!
//! It is shipped code, not `test-support`. It was `vike_bridge_core::poller`'s, and the tradehub
//! carried a verbatim copy because that daemon may not link `vike-bridge-core`
//! (`crates/vike-ops/tests/architecture/named_run_closure_gate/fences.rs`, decision 0164). This
//! crate is the lowest one both already link: `vike-model` sits in
//! `crates/vike-ops/tests/architecture/clock_pin.rs`'s `DETERMINISM_CRITICAL_CRATES`, where the
//! deadline's `Instant` read would count as a library clock read, and `vike-marketdata` is published
//! to crates.io. It also fits: the panic hook here reports a thread by its NAME, and
//! [`spawn_poller`] is how a poller thread gets one. The bridges' at-most-once ledger reader
//! (`vike_bridge_core::poller::load_ledger_keys`) is theirs alone and stayed there.
//!
//! ## The contract
//! - **Slice-granular responsiveness.** [`sleep_stop_aware`] naps `slice.min(total)` at a time and
//!   re-checks `stop` between naps, so a raised stop is honoured within ~one slice
//!   ([`STOP_POLL_SLICE`]). The last nap can overshoot by up to one slice, so `total` is a CADENCE,
//!   not a deadline; that overshoot and the `bool` return are why this is NOT
//!   `vike_bridge_core::user_data::sleep_unless_stopped`, the fixed-delay twin that sleeps the exact
//!   total and reports nothing.
//! - **`Drop` joins.** [`StopHandle`] signals stop and JOINS on `Drop`, like
//!   [`shutdown`](StopHandle::shutdown), so a dropped handle never leaks the thread. Contrast
//!   `vike_bridge_core::net_probe::NetProbeThread`, whose `Drop` deliberately does not join (an
//!   in-flight DNS resolve must not park an unrelated shutdown): a different contract, not this
//!   scaffold.
//! - **Named threads.** [`spawn_poller`] names the thread (`vike-…`), hands the body an owned clone
//!   of the stop flag, and returns the joining handle.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// The stop-poll granularity every poller passes as `slice`: a raised stop is honoured within
/// ~100 ms of wherever the tick sleep happens to be.
pub const STOP_POLL_SLICE: Duration = Duration::from_millis(100);

/// Stop-aware sleep: naps `slice.min(total)` at a time, re-checking `stop` between naps, up to
/// `total`. Returns true if `stop` fired during the sleep (the poller loops' `break` signal). The
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
#[path = "poller_tests.rs"]
mod poller_tests;
