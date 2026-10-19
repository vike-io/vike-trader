//! The three light recorders — the writers that persist batch-fetched and snapshot data into the store.
//!
//! `chain_rec` (option-chain snapshots), `cohort_rec` (cohort open interest) and `properties_rec`
//! (the point-in-time `SymbolProperties` grid) sit here, apart from the `*_log` record types at the
//! crate root (`chain_log`, `cohort_log`, `exec_log`, `funding_log`, `perp_metrics_log`): those are
//! vocabulary the `HistStore` signatures name, these are the producers that call it. The buffered
//! live-tick writer actor, `live_rec`, is in the crate `vike-store`.
//!
//! The crate-root re-exports (`vike_data::ChainRecorder`, `vike_data::PropertiesRecorder`, …) are
//! what callers use for the types; the module path is for the items that are not re-exported, such
//! as `chain_rec::DEFAULT_CHAIN_CADENCE_MS`.

// `ChainRecorder` holds only `Arc<dyn HistStore>` (like `PropertiesRecorder` right below), so it
// names no engine.
pub mod chain_rec;
// `CohortRecorder` holds only `Arc<dyn HistStore>` (like the two recorders around it), and unlike
// them it has no on/off gate and does not swallow a store error, because its caller is a batch
// rather than a live mount (its module doc argues both).
pub mod cohort_rec;
pub mod properties_rec;
