//! vike-data — historical market-data access for the trading core.
//!
//! THE CONTRACT: one seam for "give me history" so backtests, the bench, and (later) the live
//! feed all load bars/ticks the same way instead of wiring bespoke readers.
//!
//! - [`HistStore`] — the unified seam over bars AND ticks (the design of record:
//!   `docs/superpowers/specs/2026-07-05-tickstore-datafusion-spec.md`): read (`load_bars` /
//!   `scan_*`), idempotent ingest (`append_*`, manifest file-index), and derive
//!   (`resample_*_to_bars`, via the parity-tested `vike_model::consolidate_*`).
//! - [`DataFusionHist`] (feature `hist-datafusion`) — the DataFusion + Parquet backend, the
//!   SINGLE engine for all historical data (pure Rust; no C++/server/Python/SQLite). Off by
//!   default so the base crate stays lean & sync. Parts are `date=`-partitioned; [`hist_maint`]
//!   holds the compaction/retention knobs (`compact_series` / `apply_retention`), the whole-store
//!   [`DataFusionHist::run_maintenance`] pass runs both across every series, and
//!   [`MaintenanceScheduler`] (in [`hist_sched`]) drives that pass on a background timer.
//! - [`live`] — [`DataClient`] + [`LiveDataSink`], the LIVE mirror of `HistStore`: venue feeds
//!   implement `DataClient` directly (fire-and-forget subscribe/unsubscribe); all streamed data
//!   returns through the `LiveDataSink` given to the client at construction. Model-only deps,
//!   same as `HistStore`.
//! - [`live_rec`] (feature `hist-datafusion`) — [`RecorderSink`], a `LiveDataSink` that persists
//!   live quotes+trades into [`DataFusionHist`] via a buffered writer actor (tick-producer T3).
//!   Books are a documented no-op (no book schema in the store). Rows it LOSES — never enqueued, or
//!   discarded by a failed flush — are announced as a periodic [`DropReport`] (one `warn!` plus an
//!   optional [`DropObserver`]) rather than only counted, so a gapping tape is visible. The
//!   never-enqueued half is counted per [`SeriesId`], so the report NAMES which tape gapped: one
//!   bounded queue carries every series, and a low-rate producer loses the largest SHARE of its own
//!   rows while still looking alive to [`Liveness`].
//! - [`BulkIngestSession`] (feature `hist-datafusion`, via [`DataFusionHist::bulk_session`]) — the
//!   OPT-IN bulk/offline write profile for backfilling from an already-on-disk, re-fetchable
//!   source: batches many `stage_*` calls into one commit per series per window and skips the
//!   live path's per-commit WAL, never the default and never reachable through ordinary
//!   `append_*`/`HistStore` calls. See `datafusion_hist::bulk`'s module doc for the crash-safety
//!   argument and the profiling report it answers.
//!
//! The original SQLite bar store (PR #9) is RETIRED — DataFusion+Parquet covers bars and ticks.
//!
//! Layering: depends on vike-model ONLY. Venue fetchers (the per-venue crates under
//! crates/bridges/) and consumers (backtests, benches, GUI) sit above.

// Option-chain snapshot row type (kind=chain) — always compiled (model-only), so the `HistStore`
// trait signatures can name it without the `hist-datafusion` feature (like `exec_log`).
pub mod chain_log;
// Cohort open-interest row type (kind=cohort) — always compiled (model-only), so the `HistStore`
// trait signatures can name it without the `hist-datafusion` feature (like `chain_log`).
pub mod cohort_log;
pub mod datasets;

/// The demo tape — deterministic synthetic bars a fresh install is seeded with, so the first
/// command a new user runs has something to run on. Pure: no clock, no RNG, no path, no feature.
pub mod demo;
// Execution trade-log row types (kind=exec_fill / kind=exec_order) — always compiled (model-only),
// so the `HistStore` trait signatures can name them without the `hist-datafusion` feature.
pub mod exec_index;
pub mod exec_log;
// Perp market-CONTEXT metrics row type (kind=perp_metrics) — always compiled (model-only), so the
// `HistStore` trait signatures can name it without the `hist-datafusion` feature (like
// `cohort_log`).
pub mod perp_metrics_log;
// Realized perp funding-payment row type (kind=funding) — always compiled (model-only), so the
// `HistStore` trait signatures can name it without the `hist-datafusion` feature (like `exec_log`).
pub mod funding_log;
pub mod hist;
pub mod live;
// Pure per-captured-day DATA-QUALITY scoring (gap % / seq resets / staleness) over already-scanned
// rows — no Arrow/DataFusion, so it lives in the crate base. The store-scanning convenience methods
// on `DataFusionHist` it also carries are gated on `hist-datafusion` (they need the engine to scan).
pub mod quality;
// CROSS-KIND coverage: the join that lines `kind=trade`/`kind=quote`/`kind=book` up PER INSTRUMENT,
// so a day with a full trade tape and NO book shows as one row instead of three unremarkable
// per-series facts. Pure (a fold over day sets — no Arrow/DataFusion), so it lives in the crate base
// beside `quality`/`series`; it also OWNS the pure `find_gaps` rule that the gated
// `datafusion_hist::gaps` re-uses, since that module is inside the feature-gated tree and a second
// copy of the rule would be a duplicate.
pub mod coverage;
// `find_gaps` was previously re-exported here from the GATED `datafusion_hist`; it now comes from
// `coverage`, so `vike_data::find_gaps` resolves in EVERY build rather than only under
// `hist-datafusion`. Existing call sites are unchanged.
pub use coverage::{InstrumentCoverage, InstrumentKey, KindDays, PartialDay, find_gaps};
// Pure series identity + coverage types (no Arrow/DataFusion) — so downstream crates can group a
// hist inventory without pulling the DataFusion tree. Produced by the gated `datafusion_hist`.
pub mod series;
// The LAYOUT authority: one declared row per stored `kind=` (columns, partitioning, grouped form,
// identity semantics, producer commit-key shapes), with a source-derived exhaustiveness gate in
// `crates/vike-data/tests/store_kind_gate.rs`. Dependency-free by design — the whole point is that
// a producer or a consumer can read the contract without linking the engine that implements it, so
// it lives in the crate base beside `series`/`coverage` and never behind `hist-datafusion`.
pub mod store_kind;
// The expected-CADENCE authority: one declared row per stored `kind=`, refined per venue where a
// venue contradicts its kind, with a source-derived completeness gate in
// `crates/vike-data/tests/series_cadence_gate.rs`. UNGATED for `store_kind`'s reason and one more:
// the consumer that needs it most is the live recorder's watchdog, which runs in a build that has
// no DataFusion engine at all.
pub mod series_cadence;
// Selecting series for REMOVAL, and proving their provenance before any of them goes. Feature-free
// for `series`' reason and one more: the plan is what a client RENDERS and what the datahub wire
// carries, so a caller must be able to hold one without linking the engine that executes it.
pub mod removal;
// The shared stoppable-periodic-worker harness (pure std: stop flag + Condvar interruptible sleep +
// spawn/stop/join). UNGATED on purpose: `hist_sched` (gated) grew it, but vike-app-core's journal
// materializer depends on vike-data feature-free and shares it too.
pub mod worker;

// DataFusion-free in-memory HistStore for downstream tests (venue filter-recording wiring). Behind
// `test-support` so it never ships in a normal build (`any(test, …)` so this crate's own unit
// tests use the same doubles without a self-referential dev-dep).
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
#[cfg(any(test, feature = "test-support"))]
pub use test_support::{MemHistStore, NoopSink, RecordingSink, SinkCall};

#[cfg(feature = "hist-datafusion")]
pub mod datafusion_hist;
// `PropertiesRecorder` holds only `Arc<dyn HistStore>` (the always-available seam), so it is NOT gated
// on `hist-datafusion` — venue bridges reference it in their default build without pulling
// DataFusion. Only its tests (which construct `DataFusionHist`) require the feature.
#[cfg(feature = "hist-datafusion")]
pub mod hist_maint;
#[cfg(feature = "hist-datafusion")]
pub mod hist_sched;
#[cfg(feature = "hist-datafusion")]
pub mod live_rec;
// `ChainRecorder` holds only `Arc<dyn HistStore>` (like `PropertiesRecorder` right below), so it is
// NOT gated on `hist-datafusion` — venue bridges reference it in their default build without
// pulling DataFusion. Only its tests (which construct `DataFusionHist`) require the feature.
pub mod chain_rec;
// `CohortRecorder` holds only `Arc<dyn HistStore>` (like the two recorders above), so it is NOT
// gated on `hist-datafusion` either — and unlike them it is not env-gated and does not swallow a
// store error, because its caller is a batch rather than a live mount (its module doc argues both).
pub mod cohort_rec;
pub mod properties_rec;

pub use chain_log::ChainRow;
pub use chain_rec::ChainRecorder;
pub use cohort_log::CohortRow;
pub use cohort_rec::{CohortFetch, CohortRecorder};
pub use exec_log::{ExecFillRow, ExecOrderRow};
pub use funding_log::FundingRow;
pub use hist::{DataError, HistStore, TsRange};
pub use live::{
    DataClient, FeedRegistry, LiveDataError, LiveDataSink, StreamStatus, SubscriptionId, TeeSink,
    require_live_verb,
};
pub use perp_metrics_log::PerpMetricRow;
pub use quality::{DayQuality, QualityConfig, QualityLane};
// Feature-free (see `series`): the pure identity/coverage types, available with OR without the
// DataFusion engine.
pub use series::{SeriesCoverage, SeriesId};
// Feature-free for the same reason: the store-layout table is what a producer and a consumer agree
// on, and either end must be able to read it without the engine.
pub use store_kind::{CommitKey, Partition, STORE_KINDS, StoreKind, store_kind};
// Feature-free for the same reason as the layout table beside it: a rate expectation is read by a
// live recorder and by an offline quality scorer, and neither may be made to link the engine.
pub use series_cadence::{
    Cadence, SERIES_CADENCE, SeriesCadence, cadence_for, cadence_for_series_key,
};
// Feature-free for the third time, and here the reason is the WIRE: `vike-datahub-client` carries
// these types in a request and a response, and it cannot link the DataFusion engine.
pub use removal::{
    PlannedSeries, RemovalOutcome, RemovalPlan, SeriesSelector, execute_removal, plan_removal,
    select_series,
};

#[cfg(feature = "hist-datafusion")]
pub use datafusion_hist::{
    BulkConfig, BulkFlushReport, BulkIngestSession, DataFusionHist, GroupResolver, RowStream,
};
#[cfg(feature = "hist-datafusion")]
pub use datafusion_hist::{StoreSourcePolicy, load_policy, save_policy};
#[cfg(feature = "hist-datafusion")]
pub use hist_maint::{
    CompactionConfig, CompactionReport, MaintenanceConfig, MaintenanceReport, PruneReport,
    RetentionPolicy, SeriesMaintenance, SourceRankPolicy,
};
#[cfg(feature = "hist-datafusion")]
pub use hist_sched::MaintenanceScheduler;
#[cfg(feature = "hist-datafusion")]
pub use live_rec::{
    DropObserver, DropReport, LiveMap, Liveness, RecorderConfig, RecorderHandle, RecorderSink,
    SeriesDrops,
};
pub use properties_rec::PropertiesRecorder;
