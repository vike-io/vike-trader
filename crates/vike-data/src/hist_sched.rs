//! Background maintenance scheduler for the DataFusion hist store (feature `hist-datafusion`).
//!
//! [`MaintenanceScheduler`] owns a single `std::thread` that loops
//! `{ store.run_maintenance(cfg); sleep(interval) }` — periodically compacting (and, if the config
//! carries a retention policy, pruning) EVERY series without the caller having to enumerate them or
//! call `compact_series` / `apply_retention` by hand. The thread itself — the stop flag, the
//! interruptible sleep, `stop()` = set-the-flag-then-join, and the `Drop` that does the same so the
//! thread is never leaked — is the shared [`crate::worker`] harness (this module was where that
//! harness grew up; it now lives next door so vike-app-core's journal materializer shares it).
//!
//! STOP IS DETERMINISTIC + PROMPT. The inter-pass sleep is an interruptible wait on a `Condvar`
//! (not a bare `thread::sleep`), so `stop()` — which sets the flag then notifies — wakes the thread
//! out of its sleep immediately instead of blocking a whole `interval` for shutdown. The loop always
//! re-reads the flag before starting another pass, so a stop signalled mid-pass ends the loop the
//! moment that pass returns, and a stop that beats the first tick runs zero passes.
//!
//! LAYERING / CONCURRENCY. The scheduler only ever calls [`DataFusionHist::run_maintenance`], which
//! per series goes through `compact_series` / `apply_retention` — and THOSE take the per-series
//! lock (the manifest read-modify-write barrier). So the scheduler is safe to run alongside live
//! appends: a racing append just serializes on the same per-series lock and loses no rows. And
//! because the single thread visits series sequentially and never re-enters `run_maintenance` until
//! the previous pass returns, maintenance never runs concurrently with itself.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use crate::datafusion_hist::DataFusionHist;
use crate::hist_maint::MaintenanceConfig;
use crate::worker::{Shared, Worker};

/// Monotonic progress counters shared between the scheduler handle and its worker thread. Purely
/// this scheduler's domain state — the stop/sleep machinery lives in [`Worker`]/[`Shared`].
#[derive(Default)]
struct Counters {
    /// Count of maintenance passes that completed successfully (Ok). Monotonic → safe to poll.
    passes: AtomicU64,
    /// Running total of sealed parts produced by compaction across all passes. Monotonic → once it
    /// crosses a threshold it stays there, so a test can poll it to observe "a compaction happened".
    parts_compacted: AtomicU64,
    /// Count of passes that returned `Err`. Monotonic. Exists so "maintenance is silently doing
    /// nothing" is DISTINGUISHABLE from "maintenance has nothing to do" without reading logs:
    /// `passes() > 0 && parts_compacted() == 0` is idle, `failed_passes() > 0` is broken.
    failed_passes: AtomicU64,
}

/// A running background maintenance loop over a shared [`DataFusionHist`]. Construct with
/// [`MaintenanceScheduler::start`]; end with [`MaintenanceScheduler::stop`] (or just drop it — the
/// [`Worker`] field's own `Drop` stops and joins, so the thread is never leaked).
pub struct MaintenanceScheduler {
    counters: Arc<Counters>,
    worker: Worker,
}

impl MaintenanceScheduler {
    /// Spawn the loop: an immediate first `run_maintenance` pass, then one every `interval` until
    /// [`stop`](Self::stop). `store` is shared (`Arc`) so the caller keeps using it for live appends
    /// while the scheduler maintains it. `cfg` is moved into the thread.
    ///
    /// A `run_maintenance` error is swallowed (the pass is retried next tick) — a transient IO/query
    /// failure must not tear down the maintenance thread; only successful passes bump the counters.
    pub fn start(store: Arc<DataFusionHist>, cfg: MaintenanceConfig, interval: Duration) -> Self {
        let counters = Arc::new(Counters::default());
        let c = Arc::clone(&counters);
        let worker = Worker::spawn("vike-data-maint", move |shared| {
            run_loop(shared, &c, &store, &cfg, interval)
        });
        Self { counters, worker }
    }

    /// Signal the loop to stop and JOIN its thread — deterministic teardown (the thread has returned
    /// when this returns). Idempotent: a second call (or the `Drop`) is a no-op. Wakes the thread out
    /// of its inter-pass sleep immediately rather than waiting the interval.
    pub fn stop(&mut self) {
        self.worker.stop();
    }

    /// Number of maintenance passes that have completed successfully so far. Monotonic.
    pub fn passes_completed(&self) -> u64 {
        self.counters.passes.load(Ordering::SeqCst)
    }

    /// Running total of sealed parts produced by compaction across all passes so far. Monotonic, so
    /// a caller can poll `parts_compacted() > 0` to know at least one compaction has landed.
    pub fn parts_compacted(&self) -> u64 {
        self.counters.parts_compacted.load(Ordering::SeqCst)
    }

    /// Number of passes that returned `Err`. Monotonic. Non-zero means maintenance is BROKEN, not
    /// merely idle — the distinction `parts_compacted() == 0` alone cannot make.
    pub fn failed_passes(&self) -> u64 {
        self.counters.failed_passes.load(Ordering::SeqCst)
    }
}

/// The worker body: run a pass, record progress, then interruptibly sleep — until stopped. The flag
/// is checked at the top (so a stop before the first tick still runs zero passes) and the sleep is
/// itself stop-aware, so shutdown is prompt from anywhere in the cycle.
fn run_loop(
    shared: &Shared,
    counters: &Counters,
    store: &DataFusionHist,
    cfg: &MaintenanceConfig,
    interval: Duration,
) {
    while !shared.is_stopped() {
        match store.run_maintenance(cfg) {
            Ok(report) => {
                counters
                    .parts_compacted
                    .fetch_add(report.compaction.parts_written as u64, Ordering::SeqCst);
                counters.passes.fetch_add(1, Ordering::SeqCst);
            }
            // Still non-fatal — a transient error must not kill the loop — but no longer SILENT.
            // An unlogged `Err(_)` meant a store whose maintenance had stopped entirely looked
            // exactly like one with nothing to do: no parts merged, no retention, no log line, and
            // the only symptom was an ever-growing part count nobody was watching. Observed on
            // the CI box (2026-08-02): 15+ minutes and ~270 uncompacted parts in one date with a 300s
            // interval, and not one line explaining why.
            Err(e) => {
                counters.failed_passes.fetch_add(1, Ordering::SeqCst);
                tracing::warn!(error = %e, "maintenance: pass failed — retrying next tick");
            }
        }
        shared.sleep_interruptible(interval);
    }
}
