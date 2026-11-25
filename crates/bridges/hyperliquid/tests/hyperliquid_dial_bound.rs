//! Hyperliquid's user-data socket connects bounded through the handshake. It carries the fills, and a
//! bare `tungstenite::connect` waits for ever on a peer that accepts and says nothing, with the pump
//! unable to reconnect or see its `stop`. `crates/bridges/deribit/tests/offline/dial_bound.rs` is the
//! same proof for that venue.

use vike_bridge_core::silent_peer::open_silent_peer;
use vike_hyperliquid::execution::user_data::open_websocket;

#[test]
fn a_silent_peer_cannot_hold_the_fill_pump_open() {
    open_silent_peer(|url| open_websocket(&url, "{}", "{}"));
}
