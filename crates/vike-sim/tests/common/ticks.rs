//! Quote and trade tick builders shared by this crate's test binaries.
// Each binary that includes this uses a subset (`controller_harness` builds no trade print), hence
// the `allow`, as `crates/vike-ops/tests/common/test_module_names.rs` does for the same reason.
#![allow(dead_code)]

use vike_model::{QuoteTick, TradeTick};
use vike_sim::Tick;

/// One L1 quote for `symbol` at `ts` (ms), with no local receive time (`local_ts: 0`).
pub fn quote(symbol: &str, ts: i64, bid: f64, ask: f64, bid_size: f64, ask_size: f64) -> Tick {
    Tick::Quote(QuoteTick {
        ts,
        local_ts: 0,
        bid,
        ask,
        bid_size,
        ask_size,
        symbol: symbol.to_string(),
    })
}

/// One trade print for `symbol` at `ts` (ms): a taker BUY (`is_buyer_maker: false`), no local
/// receive time (`local_ts: 0`).
pub fn trade(symbol: &str, ts: i64, price: f64, size: f64) -> Tick {
    Tick::Trade(TradeTick {
        ts,
        local_ts: 0,
        price,
        size,
        is_buyer_maker: false,
        symbol: symbol.to_string(),
    })
}
