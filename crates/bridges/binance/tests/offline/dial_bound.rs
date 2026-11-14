//! Both binance user-data sockets (the spot WS-API session and the listenKey stream perp and aster ride)
//! connect bounded through the handshake. They carry the fills, and a bare `tungstenite::connect` waits
//! for ever on a peer that accepts and says nothing, with the pump unable to reconnect or see its `stop`.
//! `crates/bridges/deribit/tests/offline/dial_bound.rs` is the same proof for that venue.

use std::sync::atomic::AtomicBool;

use vike_binance::execution::user_data::perp::LISTENKEY_PATH;
use vike_binance::execution::user_data::spot::open_binance_user_data_websocket;
use vike_binance::family::execution::user_data::{
    ListenKeyAuthentication, open_user_data_websocket,
};
use vike_binance::transport::ratelimit::websocket_rate_gate;
use vike_bridge_core::silent_peer::open_silent_peer;
use vike_log::loopback::serve_replies;

const LISTEN_KEY: &str = "the-listen-key";

struct NoAuth;

impl ListenKeyAuthentication for NoAuth {
    fn query(&self, _method: &str) -> String {
        String::new()
    }

    fn header(&self) -> Option<(&'static str, String)> {
        None
    }
}

#[test]
fn a_silent_peer_cannot_hold_the_spot_session_open() {
    open_silent_peer(|url| {
        open_binance_user_data_websocket(
            &url,
            "key",
            "secret",
            || 0,
            &AtomicBool::new(false),
            &websocket_rate_gate(),
        )
    });
}

#[test]
fn a_silent_peer_cannot_hold_the_listen_key_stream_open() {
    let (rest, _) = serve_replies("", vec![(200, format!(r#"{{"listenKey":"{LISTEN_KEY}"}}"#))]);
    let dial = open_silent_peer(move |url| {
        open_user_data_websocket(&rest, LISTENKEY_PATH, &url, "", &NoAuth, &AtomicBool::new(false))
    });
    assert!(dial.request.contains(LISTEN_KEY), "the key is the auth: {}", dial.request);
    assert!(!dial.failure.contains(LISTEN_KEY), "{}", dial.failure);
}
