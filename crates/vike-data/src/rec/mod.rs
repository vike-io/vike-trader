//! The four recorders — the writers that persist live and batch-fetched data into the store.
//!
//! `live_rec` (the buffered live-tick writer actor), `chain_rec` (option-chain snapshots),
//! `cohort_rec` (cohort open interest) and `properties_rec` (the point-in-time
//! `SymbolProperties` grid) sit here, apart from the `*_log` record types at the crate root
//! (`chain_log`, `cohort_log`, `exec_log`, `funding_log`, `perp_metrics_log`): those are
//! vocabulary the `HistStore` signatures name, these are the producers that call it.
//!
//! The crate-root re-exports (`vike_data::RecorderSink`, `vike_data::ChainRecorder`, …) are what
//! callers use for the types; the module path is for the items that are not re-exported, such as
//! the `RECORD_CHAINS_ENV` / `RECORD_PROPERTIES_ENV` constants.

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
