//! The store modules — what a hist store holds and how it is read, written, laid out and kept.
//!
//! `hist` (the `HistStore` seam) and the pure, engine-free contracts a producer and a consumer of a
//! store root agree on — `store_kind` (the layout), `series_cadence` (the expected rate), `series`
//! (identity), `coverage`/`quality` (what a store holds, and how good it is) and `removal` (selecting
//! series to delete and proving their provenance first). The engines behind the seam — DataFusion +
//! Parquet, the published archive read in place, their maintenance and the selector between them —
//! live in the crate `vike-store`.
//!
//! The row types (`*_log`), the live seam (`live`) and the recorders (`rec`) stay at the crate root:
//! they are vocabulary the `HistStore` signatures name, or producers that call it.
//!
//! The crate-root re-exports (`vike_data::HistStore`, `vike_data::SeriesId`, …) are what callers use
//! for the types; the module path (`vike_data::store::removal::describe_id`,
//! `vike_data::store::store_kind::STORE_KINDS`) is for the items that are not re-exported.

pub mod hist;
// Pure per-captured-day DATA-QUALITY scoring (gap % / seq resets / staleness) over already-scanned
// rows — no Arrow/DataFusion. The store-scanning convenience methods on `DataFusionHist` live in
// `vike-store`.
pub mod quality;
// CROSS-KIND coverage: the join that lines `kind=trade`/`kind=quote`/`kind=book` up PER INSTRUMENT,
// so a day with a full trade tape and NO book shows as one row instead of three unremarkable
// per-series facts. Pure (a fold over day sets — no Arrow/DataFusion), beside `quality`/`series`; it
// also OWNS the pure `find_gaps` rule that `vike-store`'s `datafusion_hist::gaps` re-uses, since a
// second copy of the rule would be a duplicate.
pub mod coverage;
// Pure series identity + coverage types (no Arrow/DataFusion) — so downstream crates can group a
// hist inventory without pulling the DataFusion tree. Produced by `vike-store`'s `datafusion_hist`.
pub mod series;
// The LAYOUT authority: one declared row per stored `kind=` (columns, partitioning, grouped form,
// identity semantics, producer commit-key shapes), with a source-derived exhaustiveness gate in
// `crates/vike-ops/tests/venues/store_kind_gate.rs`. Dependency-free by design — the whole point is that
// a producer or a consumer can read the contract without linking the engine that implements it, so
// it lives beside `series`/`coverage`.
pub mod store_kind;
// The expected-CADENCE authority: one declared row per stored `kind=`, refined per venue where a
// venue contradicts its kind, with a source-derived completeness gate in
// `crates/vike-data/tests/series_cadence_gate.rs`. Engine-free for `store_kind`'s reason and one
// more: the consumer that needs it most is the live recorder's watchdog.
pub mod series_cadence;
// Selecting series for REMOVAL, and proving their provenance before any of them goes. Engine-free
// for `series`' reason and one more: the plan is what a client RENDERS and what the datahub wire
// carries, so a caller must be able to hold one without linking the engine that executes it.
pub mod removal;
