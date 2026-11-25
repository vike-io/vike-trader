//! Aster's user-data socket connects bounded through the handshake, on the signed listenKey it mints
//! (the shared pump's dial is `vike_binance::family::execution::user_data`'s `open_user_data_websocket`). It carries the
//! fills, and a bare dial waits for ever on a peer that accepts and says nothing. A loopback peer only:
//! no aster host is dialled.

use std::sync::atomic::AtomicBool;

use vike_aster::execution::user_data::perp::LISTENKEY_PATH;
use vike_aster::transport::listenkey_auth::ListenKeyAuthenticationAster;
use vike_binance::family::execution::user_data::open_user_data_websocket;
use vike_bridge_core::silent_peer::open_silent_peer;
use vike_log::loopback::serve_replies;
use vike_secrets::credentials::Credentials;

#[test]
fn a_silent_peer_cannot_hold_the_listen_key_stream_open() {
    let (rest, _) = serve_replies("", vec![(200, r#"{"listenKey":"the-key"}"#.to_string())]);
    let creds = Credentials {
        api_key: "0x000000000000000000000000000000000000aa".to_string().into(),
        api_secret: "0x0123456789012345678901234567890123456789012345678901234567890a"
            .to_string()
            .into(),
        passphrase: None,
    };
    let dial = open_silent_peer(move |url| {
        let clock = vike_bridge_core::signer::VenueClock::default();
        let auth = ListenKeyAuthenticationAster::new(&creds, LISTENKEY_PATH, &clock);
        open_user_data_websocket(&rest, LISTENKEY_PATH, &url, "/ws", &auth, &AtomicBool::new(false))
    });
    assert!(dial.request.contains("/ws/the-key"), "aster's path: {}", dial.request);
    assert!(!dial.failure.contains("the-key"), "{}", dial.failure);
}
