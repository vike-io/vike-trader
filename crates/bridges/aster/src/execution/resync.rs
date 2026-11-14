//! Aster spot + perp REST order-history → events, for the audit-A3 post-reconnect resync: the
//! venue face of the shared [`vike_binance::family::execution::resync`].
//!
//! Aster's order/trade JSON is a Binance fork (identical field names/shapes), so this module's
//! replay logic — previously a byte-for-byte copy of Binance's — now lives once in the
//! Binance-wire-grammar core and is called here under Aster's names (F0, dedup rung 1), with Aster's
//! one fact the family takes as a parameter: Aster places no broker prefix, so it has no link id.
//! `venue` stays a caller-supplied parameter. The contract and the public paths here are
//! unchanged — see the family module for the reconnect-gap/dedup/dual-publish notes. The tests
//! stay in this crate (`execution/resync_tests.rs`, `execution/resync_perp_tests.rs`), exercising the shared code
//! through Aster's own wrapper: Binance's twin does the same through its own, so the one shared
//! implementation is proven twice.

use serde_json::Value;
use vike_model::events::Event;

/// `vike_binance::family::execution::resync`'s `map_history` with no broker link id (Aster stamps none).
pub fn map_aster_history(
    all_orders: &Value,
    my_trades: &Value,
    venue: &str,
    symbol: &str,
) -> Vec<Event> {
    vike_binance::family::execution::resync::map_history(all_orders, my_trades, venue, symbol, None)
}

/// `vike_binance::family::execution::resync`'s `map_perp_history` with no broker link id (Aster stamps none).
pub fn map_aster_perp_history(
    all_orders: &Value,
    user_trades: &Value,
    venue: &str,
    symbol: &str,
) -> Vec<Event> {
    vike_binance::family::execution::resync::map_perp_history(
        all_orders,
        user_trades,
        venue,
        symbol,
        None,
    )
}

#[path = "resync_tests.rs"]
#[cfg(test)]
mod resync_tests;

#[path = "resync_perp_tests.rs"]
#[cfg(test)]
mod resync_perp_tests;
