//! vike-store — the DataFusion + Parquet backend of the [`vike_data::HistStore`] seam: the SINGLE
//! engine for all historical data (pure Rust; no C++/server/Python/SQLite).
//!
//! THE CONTRACT: `vike-data` (layer 20) owns the seam — the trait, the row types, the live sink,
//! the pure store contracts and the recorders that write through an `Arc<dyn HistStore>` — and this
//! crate (layer 25) implements it. It depends on `vike-data`, never the reverse, so a crate that
//! only reads, produces or tests history names `vike-data` and links no Arrow/DataFusion/tokio tree.
//! It is a crate rather than a cargo feature of `vike-data` because cargo unifies features across a
//! build, so one dependent enabling the engine turned it on for every crate in that build, while a
//! dependency edge is held by the crate graph and the layer gate.
//!
//! - [`DataFusionHist`] — the store. Parts are `date=`-partitioned; [`hist_maint`] holds the
//!   compaction/retention knobs (`compact_series` / `apply_retention`), the whole-store
//!   [`DataFusionHist::run_maintenance`] pass runs both across every series, and
//!   [`MaintenanceScheduler`] (in [`hist_sched`]) drives that pass on a background timer.
//! - [`live_rec`] — [`RecorderSink`], a `LiveDataSink` that persists live quotes+trades into
//!   [`DataFusionHist`] via a buffered writer actor (tick-producer T3). Books are a documented
//!   no-op (no book schema in the store). Rows it LOSES — never enqueued, or discarded by a failed
//!   flush — are announced as a periodic [`DropReport`] (one `warn!` plus an optional
//!   [`DropObserver`]) rather than only counted, so a gapping tape is visible. The never-enqueued
//!   half is counted per [`vike_data::SeriesId`], so the report NAMES which tape gapped: one
//!   bounded queue carries every series, and a low-rate producer loses the largest SHARE of its own
//!   rows while still looking alive to [`Liveness`].
//! - [`BulkIngestSession`] (via [`DataFusionHist::bulk_session`]) — the OPT-IN bulk/offline write
//!   profile for backfilling from an already-on-disk, re-fetchable source: batches many `stage_*`
//!   calls into one commit per series per window and skips the live path's per-commit WAL, never
//!   the default and never reachable through ordinary `append_*`/`HistStore` calls. See
//!   `datafusion_hist::bulk`'s module doc for the crash-safety argument and the profiling report it
//!   answers.
//! - [`archive_store`] — the second `HistStore`, over the published archive Parquet read in place;
//!   [`backtest_store`] selects which of the two a run reads.
//! - [`open_properties_recorder`] opens a `vike_data::PropertiesRecorder` over a store root, and
//!   `DataFusionHist`'s `*_series_quality` methods score a recorded series with
//!   `vike_data::store::quality`'s pure scorers.
//!
//! The crate-root re-exports (`vike_store::DataFusionHist`, `vike_store::RecorderSink`, …) are what
//! callers use for the types; the module path (`vike_store::datafusion_hist::RepairPlan`) is for the
//! items that are not re-exported.
//!
//! Layout and the traps a change here meets: `crates/vike-store/CLAUDE.md`.

#![warn(unreachable_pub)]

// The `data.vike.io` archive READER — a second `HistStore` over the published Parquet day files,
// read IN PLACE. It arrived in `vike-data` on 2026-09-20 from `vike-backfill`, where it had been
// declared beside the HTTP downloader that fetches those files.
//
// ⚠ That placement was not untidy, it was INVERTED: a collector ranks ABOVE the venue bridges it
// drives (layer 42), so a store held there sat above `vike-backtest` (50) and the engine could not
// open it. `crates/vike-backfill/src/bin/poly_ch_backtest.rs` existed for exactly that reason — its
// own doc called itself "the leaf binary that can depend on it directly" — which is a workaround
// around a layering accident rather than a component. Storage belongs with storage; ACQUISITION
// (network, credentials, per-venue quirks) stays above.
pub mod archive_store;
// WHICH of the two stores a run reads — a `DataFusionHist` root or archive Parquet in place. It
// came down with the reader on 2026-09-20 and lost the `poly` in its name, which was never true of
// the code: it selects between two general backends. Holding the SELECTOR above the engine is what
// stopped the engine choosing its own store.
pub mod backtest_store;
pub mod datafusion_hist;
pub mod hist_maint;
pub mod hist_sched;
// The buffered live-tick writer actor. The other three recorders (`chain_rec`, `cohort_rec`,
// `properties_rec`) hold only an `Arc<dyn HistStore>` and stay in `vike-data`.
pub mod live_rec;
// `vike_data::PropertiesRecorder`'s store-opening constructor, which names the engine.
mod properties_rec;
// The `*_series_quality` methods on `DataFusionHist`: `vike_data::store::quality`'s pure scorers
// over one `scan_*` of a recorded series.
mod series_quality;

pub use datafusion_hist::sources::{StoreSourcePolicy, load_policy, save_policy};
pub use datafusion_hist::{
    BulkConfig, BulkFlushReport, BulkIngestSession, DataFusionHist, GroupResolver,
    write_bars_parquet,
};
pub use hist_maint::{
    CompactionConfig, CompactionReport, MaintenanceConfig, MaintenanceReport, PruneReport,
    RetentionPolicy, SeriesMaintenance, SourceRankPolicy,
};
pub use hist_sched::MaintenanceScheduler;
pub use live_rec::{
    DropObserver, DropReport, LiveMap, Liveness, RecorderConfig, RecorderHandle, RecorderSink,
    SeriesDrops,
};
pub use properties_rec::open_properties_recorder;

// `vike_data::ChainRecorder`'s round-trip tests: they open a `DataFusionHist`, so they live here.
#[path = "chain_rec_tests.rs"]
#[cfg(test)]
mod chain_rec_tests;
