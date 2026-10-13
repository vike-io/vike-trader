//! vike-data — historical market-data access for the trading core.
//!
//! THE CONTRACT: one seam for "give me history" so backtests, the bench, and (later) the live
//! feed all load bars/ticks the same way instead of wiring bespoke readers.
//!
//! - [`HistStore`] — the unified seam over bars AND ticks (the design of record:
//!   `docs/superpowers/specs/2026-07-05-tickstore-datafusion-spec.md`): read (`load_bars` /
//!   `scan_*`), idempotent ingest (`append_*`, manifest file-index), and derive
//!   (`resample_*_to_bars`, via the parity-tested `vike_model::consolidate_*`).
//! - The DataFusion + Parquet engine behind `HistStore` — `DataFusionHist` and its maintenance, the
//!   archive reader, the live recorder `RecorderSink` and the bulk-ingest session — is the separate
//!   crate `vike-store` (layer 25); this crate is the seam and the engine-free contracts around it.
//! - [`live`] — [`DataClient`] + [`LiveDataSink`], the LIVE mirror of `HistStore`: venue feeds
//!   implement `DataClient` directly (fire-and-forget subscribe/unsubscribe); all streamed data
//!   returns through the `LiveDataSink` given to the client at construction. Model-only deps,
//!   same as `HistStore`.
//! - [`stream_label`] — [`StreamLabel`], the closed `quotes`/`trades`/`book`/`depth` vocabulary of the
//!   `stream` argument that `LiveDataSink::stream_status` and `data_delay` carry: the ONE spelling
//!   every feed and every sink names.
//!
//! The original SQLite bar store (PR #9) is RETIRED — DataFusion+Parquet covers bars and ticks.
//!
//! Layering: depends on vike-model ONLY. Venue fetchers (the per-venue crates under
//! crates/bridges/) and consumers (backtests, benches, GUI) sit above.

#![warn(unreachable_pub)]

// Option-chain snapshot row type (kind=chain) — model-only, so the `HistStore` trait signatures can
// name it (like `exec_log`).
pub mod chain_log;
// Cohort open-interest row type (kind=cohort) — model-only, so the `HistStore` trait signatures can
// name it (like `chain_log`).
pub mod cohort_log;
// Reconstructing a recorded Polymarket L2 book from the `kind=book` archive — the fold, the
// venue-authoritative ghost repair and the checkpoint integrity gate, which its own module doc
// insists stay together. Model-only, like the two rows above: it names `L2Book` and the
// tick/quote types through vike-model and opens no store itself.
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
// Execution trade-log row types (kind=exec_fill / kind=exec_order) — model-only, so the `HistStore`
// trait signatures can name them.
pub mod exec_index;
pub mod exec_log;
// Perp market-CONTEXT metrics row type (kind=perp_metrics) — model-only, so the `HistStore` trait
// signatures can name it (like `cohort_log`).
pub mod perp_metrics_log;
// Realized perp funding-payment row type (kind=exec_funding) — model-only, so the `HistStore` trait
// signatures can name it (like `exec_log`).
pub mod funding_log;
pub mod live;
pub mod source;
// The closed tick-lane vocabulary (`quotes`/`trades`/`book`/`depth`) the `stream` argument of
// `LiveDataSink::stream_status` and `data_delay` carries — the one place those four words are
// spelled, beside the seam they label.
pub mod stream_label;
// The STORE: the `HistStore` seam and the pure layout/cadence/identity/coverage/quality/removal
// contracts around it — `store/mod.rs`'s module doc is the map.
pub mod store;
// `find_gaps` comes from `coverage`.
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
// spawn/stop/join), shared by `vike-store`'s `MaintenanceScheduler` and the journal materializer.
pub mod worker;

// In-memory HistStore for downstream tests (venue filter-recording wiring). Behind
// `test-support` so it never ships in a normal build (`any(test, …)` so this crate's own unit
// tests use the same doubles without a self-referential dev-dep).
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
#[cfg(any(test, feature = "test-support"))]
pub use test_support::{MemHistStore, NoopSink, RecordingSink, SinkCall};

// WHICH CLOCK a Polymarket backtest window is scoped on, and the counter that measures the
// resulting disagreement. It depends on nothing but `TsRange`, and its own doc argues that the
// divergence it measures is a property of the RECORDER's tape rather than of any one store.
pub mod window_clock;
// The three recorders (`chain_rec`, `cohort_rec`, `properties_rec`), apart from the `*_log` row
// types above; each holds only `Arc<dyn HistStore>`.
pub mod rec;

pub use chain_log::ChainRow;
pub use cohort_log::CohortRow;
pub use exec_log::{ExecFillRow, ExecOrderRow};
pub use funding_log::FundingRow;
pub use live::{
    DataClient, FeedRegistry, LiveDataError, LiveDataSink, SubscriptionId, TeeSink,
    require_live_verb,
};
pub use perp_metrics_log::PerpMetricRow;
pub use rec::chain_rec::ChainRecorder;
pub use rec::cohort_rec::{CohortFetch, CohortRecorder};
pub use store::hist::{BarEdges, DataError, HistStore, StoreHandle, TsRange};
// Crate-root vocabulary, like `StoreKindId` below: every producer and consumer of a feed's `stream`
// label names it `vike_data::StreamLabel`, never the module path.
pub use stream_label::StreamLabel;
// The pure identity/coverage types (see `series`).
pub use store::series::{SeriesCoverage, SeriesId};
// The store-layout table is what a producer and a consumer agree on, and either end must be able to
// read it without the engine. Only the kind TYPE is re-exported here; the table, the rate
// expectations (`store::series_cadence`), the day-quality record (`store::quality`) and the removal
// plan (`store::removal`, which `vike-datahub-client` carries on the wire) are named by their module
// path, the one spelling their callers use.
pub use store::store_kind::StoreKindId;

pub use rec::properties_rec::PropertiesRecorder;
