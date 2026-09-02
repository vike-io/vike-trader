//! The ALWAYS-ON collector supervisor — the long-running counterpart to this crate's ~15 one-shot
//! `*_backfill` bins.
//!
//! WHAT WAS MISSING. Gap DETECTION already existed (`vike_data::DataFusionHist::series_gaps` derives
//! the missing UTC days of a series straight from its manifest `date=` index — no Parquet scan) and a
//! coverage DASHBOARD already existed (`vike-data-manager` + vike-app's Data Manager). What did not
//! exist is anything that RUNS on its own: every collector here is a one-shot process an operator
//! invokes by hand, and the only in-repo scheduler is store MAINTENANCE
//! (`vike_data::MaintenanceScheduler` — compaction/retention, not collection). This module is that
//! missing third leg: a declared roster of series, a cadence per source, and a per-pass gap-heal.
//!
//! THE CONTRACT.
//! - **It reimplements NO collector.** Every dispatch goes through the SAME `backfill_*` function
//!   the matching one-shot bin calls ([`registry`]), so idempotency (the `{venue}:{symbol}:
//!   {interval}:{start}-{end}` commit key), paging, and the still-forming-candle guard are inherited
//!   verbatim. Adding a collector is one [`registry::Collector`] row plus a config line.
//!   READ THAT KEY CAREFULLY: the store's dedup is BATCH-COMMIT-KEY only — `commit_rows`'s own
//!   comment is "a batch key already committed is a no-op (NEVER dedup by row value)". A window that
//!   moves with the clock is a DIFFERENT key, so re-fetching an overlapping span is NOT free: it
//!   appends the rows again. That is why [`heal::fresh_window`] anchors the window's start on the
//!   series' stored watermark, and why the planner — not the store — is what keeps the supervisor
//!   from duplicating its own data. (A second consequence, in the other direction: a fetch that
//!   returns 0 rows never records its key at all, so a hole the venue cannot serve is never
//!   "closed" — hence [`heal::plan_heal_jobs`]'s rotating cursor.)
//! - **It is entirely OPT-IN and purely additive.** Nothing in vike-backfill calls into this module;
//!   it is reachable only from the new `collector_supervisor` bin (or an explicit library call). No
//!   existing collector, bin, commit key, or stored byte changes — with the supervisor not running,
//!   the crate behaves exactly as before. [`config::SupervisorConfig::default`] is inert (zero
//!   sources ⇒ a pass dispatches nothing), which `run`'s off-path test pins.
//! - **A failing source never kills the loop.** Fetch/ingest/`series_gaps` errors are recorded on
//!   that source's status row and the pass moves on; the next attempt is deferred by a bounded
//!   exponential backoff ([`schedule::effective_interval_ms`]), optionally parking the source after
//!   `max_consecutive_failures` ([`schedule::is_parked`]). Only the FRESHNESS lane feeds those two —
//!   a failing historical heal must never push "now" out to the backoff cap (see [`run`]'s
//!   two-lane note).
//! - **Teardown is deterministic.** The loop runs on the shared `vike_data::worker::Worker` harness
//!   (the same one `MaintenanceScheduler` uses): a Condvar-interruptible inter-pass sleep, `stop()` =
//!   flag-then-join, and a `Drop` that mirrors it so the thread is never leaked. Deterministic needs
//!   BOUNDED, so a pass's work is bounded too: `max_heal_jobs` counts [`heal::HEAL_CHUNK_MS`]-wide
//!   sub-windows, not gap ranges (one contiguous multi-year hole is otherwise ONE unbounded job).
//!
//! SPLIT (everything decision-shaped is a PURE function, so the whole brain is unit-testable with no
//! network, no store and no sleeping):
//! - [`config`] — the TOML roster (`[[source]]` tables) + its validation.
//! - [`schedule`] — "is this source due?" / "when next?" / backoff / parking.
//! - [`heal`] — gap set (+ freshness lookback) → the concrete backfill jobs a pass would dispatch.
//! - [`registry`] — collector name → the existing `backfill_*` fn, its venue and its data kind.
//! - [`status`] — the JSON status surface (last-run / next-run / heal-queue / last-error per source).
//! - [`run`] — the only impure part: the `Worker` loop that glues the four together over a store.

use std::fmt;

pub mod config;
pub mod heal;
pub mod registry;
pub mod run;
pub mod schedule;
pub mod status;

pub use config::{
    load_supervisor_config, parse_supervisor_config, validate, SourceConfig, SupervisorConfig,
};
pub use heal::{
    chunked_gaps, fresh_window, plan_heal_jobs, plan_pass, HealJob, JobReason, HEAL_CHUNK_MS,
};
pub use registry::{collector_by_name, Collector, COLLECTORS};
pub use run::{run_once, CollectorSupervisor};
pub use schedule::{effective_interval_ms, is_due, is_parked, next_run_ms};
pub use status::{write_status, SourceStatus, SupervisorStatus};

/// Errors from loading, validating or driving the supervisor. Deliberately its OWN type rather than
/// a new [`crate::CollectError`] variant: `CollectError` is the per-collect-run error every existing
/// collector returns and callers match on exhaustively, so widening it would be a breaking change to
/// an unrelated seam. Per-source RUN failures never surface here at all — they are recorded on the
/// status row and the loop continues (see the module contract).
#[derive(Debug)]
pub enum SupervisorError {
    /// The config file could not be read, or the status file could not be written.
    Io(String),
    /// The config text is not valid TOML / does not deserialize into a [`SupervisorConfig`].
    Parse(String),
    /// The config deserialized but violates a supervisor invariant (unknown collector, duplicate
    /// source name, zero cadence, …).
    Invalid(String),
}

impl fmt::Display for SupervisorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SupervisorError::Io(e) => write!(f, "supervisor io: {e}"),
            SupervisorError::Parse(e) => write!(f, "supervisor config parse: {e}"),
            SupervisorError::Invalid(e) => write!(f, "supervisor config invalid: {e}"),
        }
    }
}

impl std::error::Error for SupervisorError {}
