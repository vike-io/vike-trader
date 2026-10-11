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
//!   default so the base crate stays lean & sync. Parts are `date=`-partitioned; [`store::hist_maint`]
//!   holds the compaction/retention knobs (`compact_series` / `apply_retention`), the whole-store
//!   [`DataFusionHist::run_maintenance`] pass runs both across every series, and
//!   [`MaintenanceScheduler`] (in [`store::hist_sched`]) drives that pass on a background timer.
//! - [`live`] — [`DataClient`] + [`LiveDataSink`], the LIVE mirror of `HistStore`: venue feeds
//!   implement `DataClient` directly (fire-and-forget subscribe/unsubscribe); all streamed data
//!   returns through the `LiveDataSink` given to the client at construction. Model-only deps,
//!   same as `HistStore`.
//! - [`rec::live_rec`] (feature `hist-datafusion`) — [`RecorderSink`], a `LiveDataSink` that persists
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
//!   `append_*`/`HistStore` calls. See `store::datafusion_hist::bulk`'s module doc for the crash-safety
//!   argument and the profiling report it answers.
//!
//! The original SQLite bar store (PR #9) is RETIRED — DataFusion+Parquet covers bars and ticks.
//!
//! Layering: depends on vike-model ONLY. Venue fetchers (the per-venue crates under
//! crates/bridges/) and consumers (backtests, benches, GUI) sit above.

#![warn(unreachable_pub)]

// Option-chain snapshot row type (kind=chain) — always compiled (model-only), so the `HistStore`
// trait signatures can name it without the `hist-datafusion` feature (like `exec_log`).
pub mod chain_log;
// Cohort open-interest row type (kind=cohort) — always compiled (model-only), so the `HistStore`
// trait signatures can name it without the `hist-datafusion` feature (like `chain_log`).
pub mod cohort_log;
// Reconstructing a recorded Polymarket L2 book from the `kind=book` archive — the fold, the
// venue-authoritative ghost repair and the checkpoint integrity gate, which its own module doc
// insists stay together. UNGATED and model-only, like the two rows above: it names `L2Book` and
// the tick/quote types through vike-model and opens no store itself, so it costs a default build
// nothing and needs no `hist-datafusion`.
//
// ⚠ It arrived here from `vike-backtest` rather than being written here, and the detour is the
// point: its module doc records that it was hoisted out of the `cheap_np_depth` BIN when a second
// bin needed the same fold, which put archive-reconstruction code in the simulator crate. Reading
// the archive is this crate's job.
pub mod cheap_np_book;
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
// Realized perp funding-payment row type (kind=exec_funding) — always compiled (model-only), so the
// `HistStore` trait signatures can name it without the `hist-datafusion` feature (like `exec_log`).
pub mod funding_log;
pub mod live;
pub mod source;
// The STORE: the `HistStore` seam, its two engines, and the pure layout/cadence/identity/coverage/
// quality/removal contracts around them — `store/mod.rs`'s module doc is the map, and each module's
// `hist-datafusion` gating is declared there on the module's own line.
pub mod store;
// `find_gaps` was previously re-exported here from the GATED `datafusion_hist`; it now comes from
// `coverage`, so `vike_data::find_gaps` resolves in EVERY build rather than only under
// `hist-datafusion`. Existing call sites are unchanged.
// ⚠ `MissingSpan`/`Shortfall`/`window_shortfall`/`missing_ms` answer a DIFFERENT question from
// everything else in that module and from `find_gaps` above: not "what does this store hold" but
// "does it hold what I am about to read". They are re-exported here rather than left behind the
// module path because the consumer is a RUN pre-flight (`vike_backtest::data_plan`), and a caller
// reaching for a hole-finder will find `find_gaps` first — which is blind to the leading and
// trailing case by construction. Sitting in the same re-export line is what puts the honest answer
// next to the tempting one.
pub use store::coverage::{
    InstrumentCoverage, InstrumentKey, KindDays, MissingSpan, PartialDay, Shortfall, find_gaps,
    missing_ms, window_shortfall,
};
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

// WHICH CLOCK a Polymarket backtest window is scoped on, and the counter that measures the
// resulting disagreement. NOT gated: it depends on nothing but `TsRange`, and its own doc argues
// that the divergence it measures is a property of the RECORDER's tape rather than of any one
// store — so the one authority for it must stay reachable without the feature.
pub mod window_clock;
// `PropertiesRecorder` holds only `Arc<dyn HistStore>` (the always-available seam), so it is NOT gated
// on `hist-datafusion` — venue bridges reference it in their default build without pulling
// DataFusion. Only its tests (which construct `DataFusionHist`) require the feature.
//
// The four recorders (`live_rec`, `chain_rec`, `cohort_rec`, `properties_rec`), apart from the
// `*_log` row types above. The `hist-datafusion` gate on `live_rec` and the reason the other three
// carry none are on their declarations in `rec/mod.rs`.
pub mod rec;

pub use chain_log::ChainRow;
pub use cohort_log::CohortRow;
pub use exec_log::{ExecFillRow, ExecOrderRow};
pub use funding_log::FundingRow;
pub use live::{
    DataClient, FeedRegistry, LiveDataError, LiveDataSink, StreamStatus, SubscriptionId, TeeSink,
    require_live_verb,
};
pub use perp_metrics_log::PerpMetricRow;
pub use rec::chain_rec::ChainRecorder;
pub use rec::cohort_rec::{CohortFetch, CohortRecorder};
pub use store::hist::{BarEdges, DataError, HistStore, TsRange};
pub use store::quality::{DayQuality, QualityConfig, QualityLane};
// Feature-free (see `series`): the pure identity/coverage types, available with OR without the
// DataFusion engine.
pub use store::series::{SeriesCoverage, SeriesId};
// Feature-free for the same reason: the store-layout table is what a producer and a consumer agree
// on, and either end must be able to read it without the engine.
pub use store::store_kind::{CommitKey, Partition, STORE_KINDS, StoreKind, store_kind};
// Feature-free for the same reason as the layout table beside it: a rate expectation is read by a
// live recorder and by an offline quality scorer, and neither may be made to link the engine.
pub use store::series_cadence::{
    Cadence, SERIES_CADENCE, SeriesCadence, cadence_for, cadence_for_series_key,
};
// Feature-free for the third time, and here the reason is the WIRE: `vike-datahub-client` carries
// these types in a request and a response, and it cannot link the DataFusion engine.
pub use store::removal::{
    PlannedSeries, RemovalOutcome, RemovalPlan, SeriesSelector, execute_removal, plan_removal,
    select_series,
};

#[cfg(feature = "hist-datafusion")]
pub use rec::live_rec::{
    DropObserver, DropReport, LiveMap, Liveness, RecorderConfig, RecorderHandle, RecorderSink,
    SeriesDrops,
};
pub use rec::properties_rec::PropertiesRecorder;
#[cfg(feature = "hist-datafusion")]
pub use store::datafusion_hist::{
    BulkConfig, BulkFlushReport, BulkIngestSession, DataFusionHist, GroupResolver,
    write_bars_parquet,
};
#[cfg(feature = "hist-datafusion")]
pub use store::datafusion_hist::{StoreSourcePolicy, load_policy, save_policy};
#[cfg(feature = "hist-datafusion")]
pub use store::hist_maint::{
    CompactionConfig, CompactionReport, MaintenanceConfig, MaintenanceReport, PruneReport,
    RetentionPolicy, SeriesMaintenance, SourceRankPolicy,
};
#[cfg(feature = "hist-datafusion")]
pub use store::hist_sched::MaintenanceScheduler;
