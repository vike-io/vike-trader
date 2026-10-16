//! Opt-in, best-effort recorder that persists observed `SymbolProperties` into the `kind=properties`
//! PIT series (see the PIT symbol-properties design). Capture is gated by the `enabled` PARAMETER —
//! a root hands in the `flags.record_properties` row; this crate reads no environment (decision
//! 0111) — and disabled is a no-op. A store error is logged and dropped — recording MUST never fail
//! or block a venue's startup. Called once per venue at instrument-fetch time, never on the hot
//! path. A composition root builds one over the DataFusion store with
//! `vike_store::open_properties_recorder`.

use std::sync::Arc;

use vike_model::SymbolProperties;

use crate::store::hist::HistStore;

pub struct PropertiesRecorder {
    store: Arc<dyn HistStore + Send + Sync>,
    enabled: bool,
}

impl PropertiesRecorder {
    pub fn new(store: Arc<dyn HistStore + Send + Sync>, enabled: bool) -> Self {
        Self { store, enabled }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Record one symbol's properties. `ts_ns` is the observation wall-clock (nanoseconds); the row ts
    /// and the per-day commit_key derive from it. No-op when disabled; store errors are logged.
    pub fn record(&self, venue: &str, symbol: &str, properties: SymbolProperties, ts_ns: i64) {
        if !self.enabled {
            return;
        }
        let ts_ms = ts_ns / 1_000_000;
        let date = vike_model::time::epoch_ms_to_utc_date(ts_ms);
        let key = format!("{venue}:{symbol}:{date}");
        if let Err(e) =
            self.store.append_symbol_properties(venue, symbol, &[(ts_ms, properties)], Some(&key))
        {
            tracing::warn!(venue, symbol, error = %e, "properties recording failed (dropped)");
        }
    }

    /// Best-effort record through an `Option<Arc<Self>>` handle — the shape every venue adapter holds
    /// (a recorder is threaded in only while `flags.record_properties` is on). Dedupes the identical
    /// `if let Some(r) = rec { r.record(..) }` wrapper each bridge (bybit/okx/deribit) hand-rolled;
    /// bridge-core can't host it (it can't depend on vike-data), so the helper lives with the type.
    pub fn record_opt(
        rec: &Option<Arc<Self>>,
        venue: &str,
        symbol: &str,
        properties: SymbolProperties,
        ts_ns: i64,
    ) {
        if let Some(r) = rec {
            r.record(venue, symbol, properties, ts_ns);
        }
    }

    pub fn record_all(
        &self,
        venue: &str,
        entries: impl IntoIterator<Item = (String, SymbolProperties)>,
        ts_ns: i64,
    ) {
        if !self.enabled {
            return;
        }
        for (symbol, properties) in entries {
            self.record(venue, &symbol, properties, ts_ns);
        }
    }
}
