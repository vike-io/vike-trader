//! Aster's USDⓈ-M perp ORDER_TRADE_UPDATE → vike event mapper: the venue face of the shared
//! [`vike_binance::family::perp_mapper`].
//!
//! Aster's perp user-data stream is Binance-verbatim (same event names/field letters), so this
//! module's mapping logic — previously a byte-for-byte copy of Binance's — now lives once in the
//! Binance-wire-grammar core and is re-exported here under Aster's name (F0, dedup rung 1).
//! `venue` stays a caller-supplied parameter. The contract, the fixtures (`tests/aster_mapper.rs`),
//! and the public path here are unchanged — see the family module for the liquidation/funding/
//! dual-publish notes.

pub use vike_binance::family::perp_mapper::map_perp as map_aster_perp;
