//! Aster's spot executionReport → vike event mapper: the venue face of the shared
//! [`vike_binance::family::event_mapper`].
//!
//! Aster's spot user-data stream is Binance-verbatim (same field letters, listenKey transport), so
//! this module's mapping logic — previously a byte-for-byte copy of Binance's — now lives once in
//! the Binance-wire-grammar core. The ONE re-export here is the different-word alias
//! `map_aster_private` (the family's `map_private` under Aster's name, the twin of Binance's
//! `map_binance_private`: `vike_binance::family::event_mapper`'s `map_private` doc records the pair);
//! the shared `map_execution_report` is named at its own path, `vike_binance::family::event_mapper`.
//! `venue` stays a caller-supplied parameter, so the shared code never knows which venue it serves.
//! The contract and the mapper fixtures (`tests/aster_mapper.rs`) are unchanged — see the family
//! module for the dual-publish/commission/trade_id notes.
//! `tests/aster_userdata.rs` proves this mapper plugs into the shared user-data pump; its module
//! doc says which of the pump's reliability gates live in `vike-binance` instead.

pub use vike_binance::family::event_mapper::map_private as map_aster_private;
