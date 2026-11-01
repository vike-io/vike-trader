//! The helpers this crate's unit-test modules share ACROSS module boundaries — they compile into one
//! test binary, so each is spelled once here. `#[cfg(test)]`, declared once in `lib.rs`; a helper
//! only one module's tests use stays beside them.

use vike_store::DataFusionHist;

/// A store over its own temp dir, returned with the dir so the dir outlives it.
pub(crate) fn store() -> (tempfile::TempDir, DataFusionHist) {
    let dir = tempfile::tempdir().expect("temp dir");
    let hist = DataFusionHist::open(dir.path()).expect("open temp store");
    (dir, hist)
}
