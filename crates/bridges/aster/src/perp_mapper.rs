//! Aster's USDⓈ-M perp ORDER_TRADE_UPDATE → vike event mapper: the venue face of the shared
//! [`vike_binance::family::perp_mapper`].
//!
//! Aster's perp user-data stream is Binance-verbatim (same event names/field letters), so this
//! module's mapping logic — previously a byte-for-byte copy of Binance's — now lives once in the
//! Binance-wire-grammar core and is re-exported here under Aster's name (F0, dedup rung 1). The
//! alias is deliberate, not a move shim: it is a different-word name (`map_perp` ->
//! `map_aster_perp`), the twin of Binance's `map_binance_perp`, and
//! `vike_binance::family::perp_mapper`'s `map_perp` doc records the pair.
//! `venue` stays a caller-supplied parameter. The contract and the fixtures
//! (`tests/offline/aster_mapper.rs`) are unchanged — see the family module for the liquidation/funding/
//! dual-publish notes.

pub use vike_binance::family::perp_mapper::map_perp as map_aster_perp;
