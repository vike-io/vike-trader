//! The store modules — what a hist store holds and how it is read, written, laid out and kept.
//!
//! `hist` (the `HistStore` seam) and its two engines, `datafusion_hist` (DataFusion + Parquet) and
//! `archive_store` (the published archive, read in place), with `hist_maint`/`hist_sched` (the
//! maintenance knobs and the timer that drives them) and `backtest_store` (which of the two a run
//! reads). Beside them sit the pure, engine-free contracts a producer and a consumer of a store root
//! agree on — `store_kind` (the layout), `series_cadence` (the expected rate), `series` (identity),
//! `coverage`/`quality` (what a store holds, and how good it is) and `removal` (selecting series to
//! delete and proving their provenance first).
//!
//! The row types (`*_log`), the live seam (`live`) and the recorders (`rec`) stay at the crate root:
//! they are vocabulary the `HistStore` signatures name, or producers that call it.
//!
//! The crate-root re-exports (`vike_data::HistStore`, `vike_data::DataFusionHist`,
//! `vike_data::SeriesId`, …) are what callers use for the types; the module path
//! (`vike_data::store::removal::describe_id`, `vike_data::store::store_kind::STORE_KINDS`) is for the
//! items that are not re-exported. Which of these modules sit behind `hist-datafusion` is declared
//! below, one attribute per declaration — the gating is exactly what it was at the crate root.

pub mod hist;
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
// Pure series identity + coverage types (no Arrow/DataFusion) — so downstream crates can group a
// hist inventory without pulling the DataFusion tree. Produced by the gated `datafusion_hist`.
pub mod series;
// The LAYOUT authority: one declared row per stored `kind=` (columns, partitioning, grouped form,
// identity semantics, producer commit-key shapes), with a source-derived exhaustiveness gate in
// `crates/vike-ops/tests/store_kind_gate.rs`. Dependency-free by design — the whole point is that
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

// The `data.vike.io` archive READER — a second `HistStore` over the published Parquet day files,
// read IN PLACE. It arrived here on 2026-09-20 from `vike-backfill`, where it had been declared
// beside the HTTP downloader that fetches those files.
//
// ⚠ That placement was not untidy, it was INVERTED: a collector ranks ABOVE the venue bridges it
// drives (layer 42), so a store held there sat above `vike-backtest` (50) and the engine could not
// open it. `crates/vike-backfill/src/bin/poly_ch_backtest.rs` existed for exactly that reason — its
// own doc called itself "the leaf binary that can depend on it directly" — which is a workaround
// around a layering accident rather than a component. Storage belongs with storage; ACQUISITION
// (network, credentials, per-venue quirks) stays above.
//
// Gated on `hist-datafusion` because it reads Parquet through the same stack `DataFusionHist` does.
#[cfg(feature = "hist-datafusion")]
pub mod archive_store;
// WHICH of the two stores a run reads — a `DataFusionHist` root or archive Parquet in place. It
// came down with the reader on 2026-09-20 and lost the `poly` in its name, which was never true of
// the code: it selects between two general backends. Holding the SELECTOR above the engine is what
// stopped the engine choosing its own store.
#[cfg(feature = "hist-datafusion")]
pub mod backtest_store;
#[cfg(feature = "hist-datafusion")]
pub mod datafusion_hist;
#[cfg(feature = "hist-datafusion")]
pub mod hist_maint;
#[cfg(feature = "hist-datafusion")]
pub mod hist_sched;
