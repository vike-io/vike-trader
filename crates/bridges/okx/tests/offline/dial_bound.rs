//! OKX's private socket connects bounded through the handshake, and still carries the browser
//! `User-Agent` its demo edge asks for (Cloudflare 1010 without it). It carries the fills, and a bare
//! `tungstenite::connect` waits for ever on a peer that accepts and says nothing, with the pump unable
//! to reconnect or see its `stop`. `crates/bridges/deribit/tests/offline/dial_bound.rs` is the same
//! proof for that venue.

use std::sync::atomic::AtomicBool;

use vike_bridge_core::silent_peer::open_silent_peer;
use vike_okx::BROWSER_UA;
use vike_okx::execution::user_data::open_okx_user_data_websocket;
use vike_okx::transport::ratelimit::websocket_rate_gate;

#[test]
fn a_silent_peer_cannot_hold_the_fill_pump_open_and_the_browser_ua_still_goes_out() {
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
    let header = format!("user-agent: {}", BROWSER_UA.to_ascii_lowercase());
    assert!(request.contains(&header), "the upgrade lost the browser UA: {}", dial.request);
}
