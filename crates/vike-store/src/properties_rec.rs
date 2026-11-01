//! The store-OPENING half of `vike_data::PropertiesRecorder`: the recorder type holds only an
//! `Arc<dyn HistStore>` and stays in `vike-data`; opening one over a [`DataFusionHist`] root names
//! the engine, so it lives here as a free function.

use std::path::Path;
use std::sync::Arc;

use vike_data::PropertiesRecorder;

use crate::DataFusionHist;

/// Open the opt-in PIT-`SymbolProperties` recorder, best-effort — the ONE gate site a
/// composition root routes through instead of hand-rolling the gate+open+wrap in every venue
/// arm.
///
/// Returns `None` (recording disabled) UNLESS `enabled` — the `flags.record_properties` row the
/// root read. When enabled it opens a [`DataFusionHist`] over `tick_store_root` and wraps
/// it in an enabled [`PropertiesRecorder`]. A store-open failure is logged and folds to `None` —
/// recording MUST never fail or block a venue's startup.
///
/// Disabled, this does ZERO work and touches no store (no store open / WAL recovery / tokio
/// runtime), so the default (off) path is byte-identical to constructing no recorder at all.
pub fn open_properties_recorder(
    tick_store_root: &Path,
    enabled: bool,
) -> Option<Arc<PropertiesRecorder>> {
    if !enabled {
        return None;
    }
    // A second handle over the same root is safe (per-series locks). On open failure the venue
    // just doesn't record (previously the error was silently `.ok()`-dropped at each call site;
    // logging it here is the best-effort unification, and only ever fires on the enabled path).
    match DataFusionHist::open(tick_store_root) {
        Ok(store) => Some(Arc::new(PropertiesRecorder::new(Arc::new(store), true))),
        Err(e) => {
            tracing::warn!(
                error = %e,
                "properties store open failed; recording disabled for this venue"
            );
            None
        }
    }
}

#[path = "properties_rec_tests.rs"]
#[cfg(test)]
mod properties_rec_tests;
