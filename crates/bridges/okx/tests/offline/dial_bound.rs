//! OKX's private socket connects bounded through the handshake, and its upgrade carries this
//! client's own `User-Agent`, never a browser's. It carries the fills, and a bare
//! `tungstenite::connect` waits for ever on a peer that accepts and says nothing, with the pump unable
//! to reconnect or see its `stop`. `crates/bridges/deribit/tests/offline/dial_bound.rs` is the same
//! proof for that venue.

use std::sync::atomic::AtomicBool;

use vike_bridge_core::silent_peer::open_silent_peer;
use vike_okx::execution::user_data::open_okx_user_data_websocket;
use vike_okx::transport::ratelimit::websocket_rate_gate;
use vike_transport::http::USER_AGENT;

#[test]
fn a_silent_peer_cannot_hold_the_fill_pump_open_and_the_honest_user_agent_still_goes_out() {
    let dial = open_silent_peer(|url| {
        open_okx_user_data_websocket(
            &url,
            "key",
            "secret",
            "passphrase",
            || 0,
            "orders",
            "ANY",
            &AtomicBool::new(false),
            &websocket_rate_gate(),
        )
    });
    let request = dial.request.to_ascii_lowercase();
    let header = format!("user-agent: {}", USER_AGENT.to_ascii_lowercase());
    assert!(request.contains(&header), "the upgrade lost the User-Agent: {}", dial.request);
    assert!(!request.contains("mozilla"), "a browser string went out: {}", dial.request);
}
