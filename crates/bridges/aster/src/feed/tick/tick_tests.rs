//! What THIS crate proves of the shared market-data stack: Aster's own stream URL, perp-suffix
//! split and depth-snapshot URL. The frame-routing tests (quote, trade, depth sync, unknown stream)
//! call `vike_binance::family::feed::tick::route_frame` directly, not an Aster wrapper, so they live
//! once, in `crates/bridges/binance/src/feed/tick/mod.rs`'s tests.

use super::*;
use crate::transport::urls::seeded::{mainnet, testnet};

#[test]
fn combined_stream_url_uses_futures_websocket_and_lowercased_symbol() {
    assert_eq!(
        combined_stream_url(testnet().fapi_websocket, "BTCUSDT"),
        "wss://fstream.asterdex-testnet.com/stream?streams=btcusdt@bookTicker/btcusdt@trade/btcusdt@depth@100ms"
    );
    assert_eq!(
        combined_stream_url(mainnet().fapi_websocket, "ETHUSDT"),
        "wss://fstream.asterdex.com/stream?streams=ethusdt@bookTicker/ethusdt@trade/ethusdt@depth@100ms"
    );
}

/// The `.P` START spelling (`crates/vike-venues/src/venue_wiring.rs`'s `VENUE_WIRING` pins `BTCUSDT.P` for this venue) must
/// reach the wire as the exchange's own `btcusdt`: a raw `.P` in the stream name dials a
/// stream nothing serves, so the pump connects, reports healthy and delivers nothing forever.
/// A bare symbol splits to itself — spot-spelled callers are byte-identical to before.
#[test]
fn pump_wire_strips_the_perp_suffix_for_the_stream_and_snapshot() {
    let (url, snap) = pump_wire(mainnet(), "BTCUSDT.P");
    assert_eq!(
        url,
        "wss://fstream.asterdex.com/stream?streams=btcusdt@bookTicker/btcusdt@trade/btcusdt@depth@100ms"
    );
    assert_eq!(snap, "BTCUSDT", "the REST depth seed must ask for the exchange symbol");
    // The identity half: a suffix-less symbol changes nothing.
    assert_eq!(pump_wire(mainnet(), "BTCUSDT"), (url, snap));
}

#[test]
fn depth_snapshot_url_perp_is_futures_v3_spot_is_api_v3() {
    assert_eq!(
        depth_snapshot_url(testnet().rest(true), "BTCUSDT", true),
        "https://fapi.asterdex-testnet.com/fapi/v3/depth?symbol=BTCUSDT&limit=1000"
    );
    assert_eq!(
        depth_snapshot_url(testnet().rest(false), "BTCUSDT", false),
        "https://sapi.asterdex-testnet.com/api/v3/depth?symbol=BTCUSDT&limit=1000"
    );
    assert_eq!(
        depth_snapshot_url(mainnet().rest(true), "BTCUSDT", true),
        "https://fapi.asterdex.com/fapi/v3/depth?symbol=BTCUSDT&limit=1000"
    );
}
