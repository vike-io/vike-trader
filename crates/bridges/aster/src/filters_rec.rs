//! Opt-in PIT instrument-properties recording glue for Aster (spot + perp): the venue face of the
//! shared [`vike_binance::family::filters_rec`], binding it to the `"aster"` venue key.
//!
//! Aster is unlike bybit/okx here: its instrument properties come from FREE FUNCTIONS returning
//! maps (`vike_binance::family::order_map::parse_symbol_properties`,
//! `perp::parse_aster_perp_instruments`), not a method on a live exec object. This module is deliberately just the recording glue over both parse outputs —
//! the entrypoint `exec::run` calls once the REAL fetched grid resolves (never the fallback).
//!
//! The glue body is shared with vike-binance (F0, dedup rung 1); the venue key is the only thing
//! that differed between the two copies, so it is a parameter in `family` and this wrapper pins
//! it. The tests below stay HERE — they drive Aster's OWN `spot`/`perp` parsers, which are
//! venue-specific and deliberately out of the shared core.

/// Record parsed Aster instrument properties into the PIT store when a recorder is present
/// (opt-in). Entrypoint for `exec::run` when the real instrument grid is fetched. Best-effort.
pub fn record_properties_all(
    rec: &Option<std::sync::Arc<vike_data::PropertiesRecorder>>,
    entries: impl IntoIterator<Item = (String, vike_model::SymbolProperties)>,
    ts_ns: i64,
) {
    vike_binance::family::filters_rec::record_properties_all(rec, "aster", entries, ts_ns);
}

#[path = "filters_rec_tests.rs"]
#[cfg(test)]
mod filters_rec_tests;
