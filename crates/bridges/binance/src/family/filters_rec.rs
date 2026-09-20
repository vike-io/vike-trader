//! Opt-in PIT instrument-properties recording glue for the Binance-grammar venues (spot + perp).
//! Shared by vike-binance (`crate::filters_rec`) and vike-aster (`vike_aster::filters_rec`).
//!
//! These venues are unlike bybit/okx here: their instrument properties come from FREE FUNCTIONS
//! returning maps (each venue's `spot::parse_symbol_properties` / `perp::parse_*_perp_instruments`),
//! not a method on a live exec object. So this is deliberately just the recording glue over both
//! parse outputs, rather than any `spawn_with_recorder`/constructor plumbing.
//!
//! NOTE the one shape change vs the two copies this replaces: `venue` was a HARDCODED literal in
//! each venue's body (`"binance"` / `"aster"`), not a parameter — the only one of the four shared
//! mappers where that was true. It is a parameter here; each venue's thin wrapper passes its own
//! literal, so both public signatures stay exactly as they were.

/// Record parsed instrument properties into the PIT store when a recorder is present (opt-in),
/// keyed by the caller's `venue`. Best-effort. Each venue's `filters_rec::record_properties_all`
/// is the entrypoint the composition root / `exec::run` calls with its own venue string.
pub fn record_properties_all(
    rec: &Option<std::sync::Arc<vike_data::PropertiesRecorder>>,
    venue: &str,
    entries: impl IntoIterator<Item = (String, vike_model::SymbolProperties)>,
    ts_ns: i64,
) {
    if let Some(r) = rec {
        r.record_all(venue, entries, ts_ns);
    }
}
