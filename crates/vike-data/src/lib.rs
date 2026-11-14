//! vike-data — historical market-data access for the trading core.
//!
//! THE CONTRACT: one seam for "give me history" so backtests, the bench, and (later) the live
//! feed all load bars/ticks the same way instead of wiring bespoke readers.
//!
//! - [`HistoryStore`] — the unified seam over bars AND ticks (the design of record:
//!   `docs/superpowers/specs/2026-07-05-tickstore-datafusion-spec.md`): read (`load_bars` /
//!   `scan_*`), idempotent ingest (`append_*`, manifest file-index), and derive
//!   (`resample_*_to_bars`, via the parity-tested `vike_model::consolidate_*`).
//! - The DataFusion + Parquet engine behind `HistoryStore` — `HistoryStoreDataFusion` and its maintenance, the
//!   archive decoders, the live recorder `RecorderSink` and the bulk-ingest session — is the separate
//!   crate `vike-store` (layer 25); this crate is the seam and the engine-free contracts around it.
//! - [`live`] — [`DataClient`] + [`DataClientSink`], the LIVE mirror of `HistoryStore`: venue feeds
//!   implement `DataClient` directly (fire-and-forget subscribe/unsubscribe); all streamed data
//!   returns through the `DataClientSink` given to the client at construction. Model-only deps,
//!   same as `HistoryStore`. The closed `quotes`/`trades`/`book`/`depth` vocabulary of the `stream`
//!   argument that `DataClientSink::stream_status` and `data_delay` carry is `vike_model::StreamLabel`.
//!
//! The original SQLite bar store (PR #9) is RETIRED — DataFusion+Parquet covers bars and ticks.
//!
//! Layering: depends on vike-model ONLY. Venue fetchers (the per-venue crates under
//! crates/bridges/) and consumers (backtests, benches, GUI) sit above.

#![warn(unreachable_pub)]

// Option-chain snapshot row type (kind=chain) — model-only, so the `HistoryStore` trait signatures can
// name it (like `execution_log`).
pub mod chain_log;
// Cohort open-interest row type (kind=cohort) — model-only, so the `HistoryStore` trait signatures can
// name it (like `chain_log`).
pub mod cohort_log;
// One archive-import day's refusal and bar count: the store half (`vike-backfill`) mints them and the
// datahub's archive verb carries them, unchanged.
pub mod archive_day;
// The ONE CSV bar decoder (decision 0268): the `csv-bars` import and `HistoryStoreCsv` both read it.
pub mod csv_bars;
pub mod datasets;

/// The TEST tape — deterministic synthetic bars the suites seed a store with. Behind
/// `test-support`, so no shipped binary links it: the product carries no demo or fake data (the
/// owner's ruling of 2026-11-13). Pure: no clock, no RNG, no path.
#[cfg(feature = "test-support")]
pub mod demo;
// Execution trade-log row types (kind=exec_fill / kind=execution_order) — model-only, so the `HistoryStore`
// trait signatures can name them.
pub mod exec_log;
pub mod execution_index;
// Perp market-CONTEXT metrics row type (kind=perp_metrics) — model-only, so the `HistoryStore` trait
// signatures can name it (like `cohort_log`).
pub mod perp_metrics_log;
// Realized perp funding-payment row type (kind=exec_funding) — model-only, so the `HistoryStore` trait
// signatures can name it (like `execution_log`).
pub mod funding_log;
pub mod live;
// The per-event delivery class (lossless / conflatable / control) of each `DataClientSink` verb and the
// per-subscriber floor over it — the one table every live-data tube reads, beside the seam it classifies.
pub mod delivery;
pub mod source;
// The STORE: the `HistoryStore` seam and the pure layout/cadence/identity/coverage/quality/removal
// contracts around it — `store/mod.rs`'s module doc is the map.
pub mod store;
// `find_gaps` comes from `coverage`.
// ⚠ `SeriesMissingSpan`/`SeriesShortfall`/`window_shortfall`/`missing_ms` answer a DIFFERENT question from
// everything else in that module and from `find_gaps` above: not "what does this store hold" but
// "does it hold what I am about to read". They are re-exported here rather than left behind the
// module path because the consumer is a RUN pre-flight (`vike_backtest::data_plan`), and a caller
// reaching for a hole-finder will find `find_gaps` first — which is blind to the leading and
// trailing case by construction. Sitting in the same re-export line is what puts the honest answer
// next to the tempting one.
pub use store::coverage::{
    InstrumentCoverage, InstrumentCoverageKey, InstrumentCoverageKindDays,
    InstrumentCoveragePartialDay, SeriesMissingSpan, SeriesShortfall, find_gaps, missing_ms,
    window_shortfall,
};
// The shared stoppable-periodic-worker harness (pure std: stop flag + Condvar interruptible sleep +
// spawn/stop/join), shared by `vike-store`'s `MaintenanceScheduler` and the journal materializer.
pub mod worker;

// In-memory HistoryStore for downstream tests (venue filter-recording wiring). Behind
// `test-support` so it never ships in a normal build (`any(test, …)` so this crate's own unit
// tests use the same doubles without a self-referential dev-dep).
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
#[cfg(any(test, feature = "test-support"))]
pub use test_support::{HistoryStoreMemory, NoopSink, RecordingSink, SinkCall};
// `hist_store_stubs!` (`#[macro_export]`, so it re-exports nothing) compiles in EVERY build: the
// shipped `HistoryStoreCsv` spells its refusals with it. The file stays beside the doubles, its other users.
#[path = "test_support/stubs.rs"]
mod stubs;
// The read-only store over one CSV file, crate-root vocabulary like `HistoryStoreMemory` above.
pub use store::csv_store::HistoryStoreCsv;

// WHICH CLOCK a Polymarket backtest window is scoped on, and the counter that measures the
// resulting disagreement. It depends on nothing but `TimestampRange`, and its own doc argues that the
// divergence it measures is a property of the RECORDER's tape rather than of any one store.
pub mod window_clock;
// The three recorders (`chain_rec`, `cohort_rec`, `properties_rec`), apart from the `*_log` row
// types above; each holds only `Arc<dyn HistoryStore>`.
pub mod rec;

pub use chain_log::ChainRow;
pub use cohort_log::CohortRow;
pub use exec_log::{ExecFillRow, ExecOrderRow};
pub use funding_log::ExecutionHistoryFundingRow;
pub use live::{
    DataClient, DataClientBuilder, DataClientError, DataClientRegistry, DataClientSink,
    DataClientSinkTee, DataClientSubscriptionId, require_live_verb,
};
pub use perp_metrics_log::PerpMetricRow;
pub use rec::chain_rec::ChainRecorder;
pub use rec::cohort_rec::{CohortFetch, CohortRecorder};
pub use store::hist::{BarEdges, HistoryStore, HistoryStoreError, TimestampRange};
// The pure identity/coverage types (see `series`).
pub use store::series::{SeriesCoverage, SeriesId};
// The store-layout table is what a producer and a consumer agree on, and either end must be able to
// read it without the engine. Only the kind TYPE is re-exported here; the table, the rate
// expectations (`store::series_cadence`), the day-quality record (`store::quality`) and the removal
// plan (`store::removal`, which `vike-datahub-client` carries on the wire) are named by their module
// path, the one spelling their callers use.
pub use store::store_kind::StoreKindId;

pub use rec::properties_rec::PropertiesRecorder;

/// The source declaring the one backtest-profile table that IS this crate's type, verbatim, beside its
/// repo-relative path: `[data] file_format` is `csv_bars::CsvBarsFormatWords`.
/// `vike_backtest::profile_surface` publishes the profile's schema by parsing the sources that declare
/// it, and reads this through the dependency edge, as it reads `[engine.in_bar]` through
/// `vike_simulator::PROFILE_TABLE_SOURCES` (an `include_str!` across a package boundary is not the way:
/// `vike_model::risk::surface`'s doc says why). Its parser panics on a field line it cannot read, so a
/// refactor of the struct fails that gate instead of silently shortening a published table.
#[cfg(feature = "profile-schema")]
pub const PROFILE_TABLE_SOURCES: &[(&str, &str)] =
    &[("crates/vike-data/src/csv_bars/format_words.rs", include_str!("csv_bars/format_words.rs"))];
