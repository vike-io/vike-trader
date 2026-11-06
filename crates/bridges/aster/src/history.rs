//! Aster spot + perp REST order-history → events, for the audit-A3 post-reconnect resync: the
//! venue face of the shared [`vike_binance::family::history`].
//!
//! Aster's order/trade JSON is a Binance fork (identical field names/shapes), so this module's
//! replay logic — previously a byte-for-byte copy of Binance's — now lives once in the
//! Binance-wire-grammar core and is re-exported here under Aster's names (F0, dedup rung 1).
//! `venue` stays a caller-supplied parameter. The contract and the public paths here are
//! unchanged — see the family module for the reconnect-gap/dedup/dual-publish notes. The tests
//! stay in this crate (`history_tests.rs`, `history_perp_tests.rs`), exercising the shared code
//! through Aster's own wrapper: Binance's twin does the same through its own, so the one shared
//! implementation is proven twice.

pub use vike_binance::family::history::{
    map_history as map_aster_history, map_perp_history as map_aster_perp_history,
};

#[path = "history_tests.rs"]
#[cfg(test)]
mod history_tests;

#[path = "history_perp_tests.rs"]
#[cfg(test)]
mod history_perp_tests;
