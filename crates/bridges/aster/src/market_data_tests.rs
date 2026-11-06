//! What THIS crate proves of the shared market-data stack: Aster's own stream URL, perp-suffix
//! split and depth-snapshot URL. The frame-routing tests (quote, trade, depth sync, unknown stream)
//! call `vike_binance::family::depth::route_frame` directly, not an Aster wrapper, so they live
//! once, in `crates/bridges/binance/src/market_data.rs`'s tests.

use super::*;

#[test]
fn combined_stream_url_uses_futures_ws_and_lowercased_symbol() {
    let demo = urls::urls_for(AccountTier::Demo);
    assert_eq!(
        combined_stream_url(demo.fapi_ws, "BTCUSDT"),
        "wss://fstream.asterdex-testnet.com/stream?streams=btcusdt@bookTicker/btcusdt@trade/btcusdt@depth@100ms"
    );
    let live = urls::urls_for(AccountTier::Live);
    assert_eq!(
        combined_stream_url(live.fapi_ws, "ETHUSDT"),
        "wss://fstream.asterdex.com/stream?streams=ethusdt@bookTicker/ethusdt@trade/ethusdt@depth@100ms"
    );
}

/// The `.P` START spelling (`crates/vike-venues/src/venue_wiring.rs`'s `VENUE_WIRING` pins `BTCUSDT.P` for this venue) must
/// reach the wire as the exchange's own `btcusdt`: a raw `.P` in the stream name dials a
/// stream nothing serves, so the pump connects, reports healthy and delivers nothing forever.
/// A bare symbol splits to itself — spot-spelled callers are byte-identical to before.
#[test]
fn pump_wire_strips_the_perp_suffix_for_the_stream_and_snapshot() {
    let (url, snap) = pump_wire(AccountTier::Live, "BTCUSDT.P");
    assert_eq!(
        url,
        "wss://fstream.asterdex.com/stream?streams=btcusdt@bookTicker/btcusdt@trade/btcusdt@depth@100ms"
    );
    assert_eq!(snap, "BTCUSDT", "the REST depth seed must ask for the exchange symbol");
    // The identity half: a suffix-less symbol changes nothing.
    assert_eq!(pump_wire(AccountTier::Live, "BTCUSDT"), (url, snap));
}

#[test]
fn depth_snapshot_url_perp_is_futures_v3_spot_is_api_v3() {
    let demo = urls::urls_for(AccountTier::Demo);
    assert_eq!(
        depth_snapshot_url(demo.fapi_rest, "BTCUSDT", true),
        "https://fapi.asterdex-testnet.com/fapi/v3/depth?symbol=BTCUSDT&limit=1000"
    );
    assert_eq!(
        depth_snapshot_url(demo.sapi_rest, "BTCUSDT", false),
        "https://sapi.asterdex-testnet.com/api/v3/depth?symbol=BTCUSDT&limit=1000"
    );
    let live = urls::urls_for(AccountTier::Live);
    assert_eq!(
        depth_snapshot_url(live.fapi_rest, "BTCUSDT", true),
        "https://fapi.asterdex.com/fapi/v3/depth?symbol=BTCUSDT&limit=1000"
    );
}
