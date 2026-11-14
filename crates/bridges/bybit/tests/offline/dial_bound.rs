//! Bybit's private socket connects bounded through the handshake. It carries the fills, and a bare
//! `tungstenite::connect` waits for ever on a peer that accepts and says nothing, with the pump unable
//! to reconnect or see its `stop`. `crates/bridges/deribit/tests/offline/dial_bound.rs` is the same
//! proof for that venue.

use std::sync::atomic::AtomicBool;

use vike_bridge_core::silent_peer::open_silent_peer;
use vike_bybit::execution::user_data::open_bybit_user_data_websocket;
use vike_bybit::transport::ratelimit::websocket_rate_gate;

#[test]
fn a_silent_peer_cannot_hold_the_fill_pump_open() {
    open_silent_peer(|url| {
        open_bybit_user_data_websocket(
            &url,
            "key",
            "secret",
            || 0,
            &["order"],
            &AtomicBool::new(false),
            &websocket_rate_gate(),
        )
    });
}
