//! `poller`: the ONE stop-aware background-poller scaffold every opt-in venue poller thread is
//! built from (polymarket `auto_redeem`/`resolve`/`heartbeat`/`chain`, hyperliquid
//! `outcome_settlement`): the [`sleep_stop_aware`] tick sleep, the stop-flag + `Drop`-joining owner
//! handle [`StopHandle`] (also the depth-pump threads' owner handle), the named-thread spawn
//! [`spawn_poller`], and [`load_ledger_keys`], the read half of the settlement pollers'
//! at-most-once ledger. Gating and each poller's tick logic stay at the call sites, which spawn
//! only what their caller already decided to start.
//!
//! ## The contract
//! - **Slice-granular responsiveness.** [`sleep_stop_aware`] naps `slice.min(total)` at a time and
//!   re-checks `stop` between naps, so a raised stop is honoured within ~one slice
//!   ([`STOP_POLL_SLICE`]). The last nap can overshoot by up to one slice, so `total` is a CADENCE,
//!   not a deadline; that overshoot and the `bool` return are why this is NOT
//!   `user_data::sleep_unless_stopped`, the fixed-delay twin that sleeps the exact total
//!   and reports nothing.
//! - **`Drop` joins.** [`StopHandle`] signals stop and JOINS on `Drop`, like
//!   [`shutdown`](StopHandle::shutdown), so a dropped handle never leaks the thread. Contrast
//!   `net_probe::NetProbeThread`, whose `Drop` deliberately does not join (an in-flight
//!   DNS resolve must not park an unrelated shutdown): a different contract, not this scaffold.
//! - **Named threads.** [`spawn_poller`] names the thread (`vike-…`), hands the body an owned clone
//!   of the stop flag, and returns the joining handle.

use std::collections::HashSet;
use std::path::Path;
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

/// Load an at-most-once ledger file: one key per line, each trimmed, blank lines skipped. A missing
/// or unreadable file is an EMPTY ledger — the ordinary first-run state, never an error.
///
/// The read half of the persisted set both settlement pollers keep (`SettlementLedger::open` in
/// polymarket's `exec_plane::settlement::resolve` and hyperliquid's `outcome_settlement`). The write
/// half (key shape, log wording, in-memory guard) stays with each venue.
#[must_use]
pub fn load_ledger_keys(path: &Path) -> HashSet<String> {
    let mut seen = HashSet::new();
    if let Ok(txt) = std::fs::read_to_string(path) {
        for line in txt.lines() {
            let k = line.trim();
            if !k.is_empty() {
                seen.insert(k.to_string());
            }
        }
    }
    seen
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
            *seen.lock().unwrap() = thread::current().name().map(str::to_string);
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
        assert_eq!(name.lock().unwrap().as_deref(), Some("vike-test-poller-shutdown"));
    }

    /// The ledger file is one key per line, trimmed, blanks skipped, repeats collapsed; a file that
    /// does not exist yet is the ordinary first-run EMPTY ledger, not an error.
    #[test]
    fn load_ledger_keys_trims_skips_blanks_and_reads_a_missing_file_as_empty() {
        let dir = tempfile::tempdir().expect("scratch dir");
        let path = dir.path().join("ledger.txt");
        assert!(load_ledger_keys(&path).is_empty(), "a missing file is an empty ledger");
        std::fs::write(&path, "a:1:2\n  b:3:4  \n\n   \na:1:2\n").expect("write the ledger");
        let keys = load_ledger_keys(&path);
        assert_eq!(keys.len(), 2, "{keys:?}");
        assert!(keys.contains("a:1:2") && keys.contains("b:3:4"), "{keys:?}");
    }
}
