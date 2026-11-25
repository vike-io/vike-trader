//! Aster's USDⓈ-M perp ORDER_TRADE_UPDATE → vike event mapper: the venue face of the shared
//! [`vike_binance::family::execution::event_mapper::perp`].
//!
//! Aster's perp user-data stream is Binance-verbatim (same event names/field letters), so this
//! module's mapping logic — previously a byte-for-byte copy of Binance's — now lives once in the
//! Binance-wire-grammar core and is called here under Aster's name (F0, dedup rung 1):
//! `map_aster_perp`, the twin of Binance's `map_binance_perp` (`vike_binance::family::execution::event_mapper::perp`'s
//! `map_perp` doc records the pair), hands it Aster's one fact the family takes as a parameter: Aster
//! places no broker prefix, so it has no link id.
//! `venue` stays a caller-supplied parameter. The contract and the fixtures
//! (`tests/offline/aster_mapper.rs`) are unchanged — see the family module for the liquidation/funding/
//! dual-publish notes.

use serde_json::Value;
use vike_model::events::Event;

/// `vike_binance::family::execution::event_mapper::perp`'s `map_perp` with no broker link id (Aster stamps none).
pub fn map_aster_perp(frame: &Value, venue: &str, symbol: &str) -> Vec<Event> {
    vike_binance::family::execution::event_mapper::perp::map_perp(frame, venue, symbol, None)
}
