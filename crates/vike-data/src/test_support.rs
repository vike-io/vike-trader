//! DataFusion-free in-memory [`HistStore`] for tests — the test double venue bridges build a
//! [`crate::PropertiesRecorder`] over when asserting their opt-in properties-recording wiring, WITHOUT
//! pulling the heavy Arrow/DataFusion tree into their test builds (the `hist-datafusion` feature).
//!
//! Also home to the shared [`RecordingSink`]/[`NoopSink`] `LiveDataSink` doubles (testing-arch
//! Phase 4d) - one capturing sink instead of the ~13 inline copies the venue bridges carried.
//!
//! Behind the `test-support` feature (off by default). The `bars`, `symbol_properties` and
//! `equity` methods carry real behavior — they honor the batch-level `commit_key` idempotency
//! contract (a repeated key is a no-op) and return rows ts-ascending, matching `DataFusionHist`.
//! Unlike `DataFusionHist` (which has no venue/symbol Parquet column and must re-inject
//! `EquitySample.venue` from the caller's `symbol` argument on scan — see
//! `datafusion_hist::codec::batch_to_equities`), this in-memory double holds the real domain value,
//! so `equity` rows come back with whatever `.venue` the caller originally passed in, untouched.
//! Bars take the OPPOSITE care for the same fidelity reason: [`persisted_bar`] erases on append the
//! fields the real bar schema never persists, so the round trip loses exactly what
//! `DataFusionHist`'s does. The still-inert remainder: quotes/trades/book read empty and append
//! zero rows, depth inherits the trait's refusing defaults, and the two `resample_*_to_bars` verbs
//! return `Ok(0)` HONESTLY rather than fabricating — this double never holds ticks, and resampling
//! zero ticks derives zero bars on the real store too. The catalog pair
//! (`list_series`/`inventory`) answers with a REAL fold over exactly the held seams — an honest
//! empty when nothing was seeded — rather than inheriting a default: the trait's old empty default
//! had a SEEDED double answering "no series" while holding rows, and the refusing default that
//! replaced it would deny a fold this double can genuinely perform.
//!
//! Empty-batch fidelity: every real append here returns `Ok(0)` for an empty `rows` slice WITHOUT
//! consuming the `commit_key`, matching `DataFusionHist::commit_rows` (which short-circuits on
//! `ts.is_empty()` before registering the key). A double that burned the key on an empty batch would
//! turn a subsequent real append under that key into a no-op that happens only in tests.

use vike_model::Bar;

use crate::store::hist::HistStore;

mod mem_store;
mod sinks;

pub use mem_store::MemHistStore;
pub use sinks::{NoopSink, RecordingSink, SinkCall};

/// What a bar looks like AFTER the real store's round trip: the bar schema persists
/// ts/OHLC/volume/funding and no bid/ask/symbol column, so
/// `crates/vike-data/src/store/datafusion_hist/codec/market.rs`'s `bars_from_batch` decodes those three as
/// `None` regardless of what was appended. The double erases at the same boundary (the write), so
/// a test asserting `symbol` survives `append_bars` → `load_bars` fails here exactly as it would
/// on `DataFusionHist`.
fn persisted_bar(b: &Bar) -> Bar {
    Bar { bid: None, ask: None, symbol: None, ..b.clone() }
}

// ===================================================================================================
// The double's own unit tests — the BAR verbs. The older seams are covered by the integration
// suites in `tests/` (equity_series.rs's `mem_tests`, exec_log_series.rs, funding_series.rs,
// chain_series.rs, hist_datafusion.rs's seeded-catalog test); bars are tested HERE, beside the
// erasure helper whose behavior half of them pin. Runs in the plain `cargo test -p vike-data`
// roster lane: `lib.rs` compiles this module under `#[cfg(any(test, feature = "test-support"))]`.
// ===================================================================================================

#[path = "test_support_tests.rs"]
#[cfg(test)]
mod test_support_tests;
