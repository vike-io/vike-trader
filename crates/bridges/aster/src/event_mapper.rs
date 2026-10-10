//! Aster's spot executionReport → vike event mapper: the venue face of the shared
//! [`vike_binance::family::event_mapper`].
//!
//! Aster's spot user-data stream is Binance-verbatim (same field letters, listenKey transport), so
//! this module's mapping logic — previously a byte-for-byte copy of Binance's — now lives once in
//! the Binance-wire-grammar core and is re-exported here under Aster's names (F0, dedup rung 1).
//! `venue` stays a caller-supplied parameter, so the shared code never knows which venue it serves.
//! The contract, the mapper fixtures (`tests/aster_mapper.rs`) and the public paths here are
//! unchanged — see the family module for the dual-publish/commission/trade_id notes.
//! `tests/aster_userdata.rs` proves this mapper plugs into the shared user-data pump; its module
//! doc says which of the pump's reliability gates live in `vike-binance` instead.

pub use vike_binance::family::event_mapper::{
    map_execution_report, map_private as map_aster_private,
};
