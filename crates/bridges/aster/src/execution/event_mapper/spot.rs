//! Aster's spot executionReport → vike event mapper: the venue face of the shared
//! [`vike_binance::family::execution::event_mapper::spot`].
//!
//! Aster's spot user-data stream is Binance-verbatim (same field letters, listenKey transport), so
//! this module's mapping logic — previously a byte-for-byte copy of Binance's — now lives once in
//! the Binance-wire-grammar core. The ONE function here is `map_aster_private` (the family's
//! `map_private` under Aster's name, the twin of Binance's `map_binance_private`:
//! `vike_binance::family::execution::event_mapper::spot`'s `map_private` doc records the pair), which hands it Aster's
//! one fact the family takes as a parameter: Aster places no broker prefix, so it has no link id;
//! the shared `map_execution_report` is named at its own path, `vike_binance::family::execution::event_mapper::spot`.
//! `venue` stays a caller-supplied parameter, so the shared code never knows which venue it serves.
//! The contract and the mapper fixtures (`tests/offline/aster_mapper.rs`) are unchanged — see the family
//! module for the dual-publish/commission/trade_id notes.
//! `tests/offline/aster_userdata.rs` proves this mapper plugs into the shared user-data pump; its module
//! doc says which of the pump's reliability gates live in `vike-binance` instead.

use serde_json::Value;
use vike_model::events::Event;

/// `vike_binance::family::execution::event_mapper::spot`'s `map_private` with no broker link id (Aster stamps none).
pub fn map_aster_private(frame: &Value, venue: &str, symbol: &str) -> Vec<Event> {
    vike_binance::family::execution::event_mapper::spot::map_private(frame, venue, symbol, None)
}
