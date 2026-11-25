//! End-to-end handshake composition: drive the full Hello → Welcome → Auth → verify flow through
//! the REAL shared handshake frames (`vike_hub_protocol::handshake`, the ONE handshake every hub runs,
//! decision 0288) and [`auth`] math over the SHARED length-prefixed framing, entirely
//! in-memory (a `Vec<u8>` cursor stands in for the socket). ⚠ The crate DOES dial a node, through
//! `crates/vike-tradehub-client/src/handshake.rs`'s `hub_handshake`, which is crate-private and not
//! what this file drives.
//!
//! The security property at the integration boundary: a legitimate client authenticates; a
//! replayed transcript, a wrong-scope client and a wrong key are all rejected by the server's
//! constant-time verify.

use vike_hub_protocol::auth::{HubKeys, Scope};
use vike_hub_protocol::frame::{read_frame, write_frame};
use vike_hub_protocol::handshake::{HandshakeFrame, HandshakeReply};
use vike_tradehub_client::{auth, proto::NODE_PROTO_VERSION};

/// A deterministic nonce for the test server (production mints a fresh CSPRNG nonce per
/// connection; the replay test below uses two DIFFERENT nonces).
fn server_nonce(seed: u8) -> [u8; 32] {
    [seed; 32]
}

/// Run one connection's handshake in memory: the server writes a `Welcome` with `nonce`, the client
/// signs it under `client_key` for `client_scope`, and the server verifies with `HubKeys`. Returns
/// the server's terminal auth response.
fn run_handshake(
    keys: &HubKeys,
    nonce: [u8; 32],
    client_key: &[u8],
    client_scope: Scope,
) -> HandshakeReply {
    // --- server → client: Welcome{ nonce } (framed) ---
    let mut wire: Vec<u8> = Vec::new();
    let welcome = HandshakeReply::Welcome {
        proto_version: NODE_PROTO_VERSION,
        features: vec!["subscribe".into()],
        nonce: Some(nonce),
    };
    write_frame(&mut wire, &welcome).unwrap();

    // client reads the Welcome back off the wire and extracts the challenge nonce
    let mut cursor = std::io::Cursor::new(wire);
    let got: HandshakeReply = read_frame(&mut cursor).unwrap();
    let (proto_version, challenge) = match got {
        HandshakeReply::Welcome { proto_version, nonce: Some(nonce), .. } => (proto_version, nonce),
        other => panic!("expected Welcome, got {other:?}"),
    };

    // --- client → server: Auth{ scope, mac } (framed) ---
    let mac = auth::sign(client_key, &challenge, proto_version, client_scope);
    let mut wire2: Vec<u8> = Vec::new();
    write_frame(&mut wire2, &HandshakeFrame::Auth { scope: client_scope, mac }).unwrap();

    // server reads the Auth and verifies against its own key for the claimed scope
    let mut cursor2 = std::io::Cursor::new(wire2);
    let auth_request: HandshakeFrame = read_frame(&mut cursor2).unwrap();
    let (scope, mac) = match auth_request {
        HandshakeFrame::Auth { scope, mac } => (scope, mac),
        other => panic!("expected Auth, got {other:?}"),
    };
    if auth::verify(keys.key_for(scope), &nonce, NODE_PROTO_VERSION, scope, &mac) {
        HandshakeReply::AuthOk { scope, admitted: None }
    } else {
        HandshakeReply::AuthDenied { reason: "bad mac".into() }
    }
}

/// `response` is `AuthOk` for exactly `scope` (`HandshakeReply` derives no `PartialEq`).
fn granted(response: &HandshakeReply, scope: Scope) -> bool {
    matches!(response, HandshakeReply::AuthOk { scope: s, .. } if *s == scope)
}

/// `response` is the coarse `bad mac` refusal.
fn bad_mac(response: &HandshakeReply) -> bool {
    matches!(response, HandshakeReply::AuthDenied { reason } if reason == "bad mac")
}

#[test]
fn legitimate_control_client_authenticates() {
    let keys = HubKeys::new(b"observe-key".to_vec(), b"control-key".to_vec());
    let response = run_handshake(&keys, server_nonce(1), keys.key_for(Scope::Write), Scope::Write);
    assert!(granted(&response, Scope::Write), "{response:?}");
}

#[test]
fn legitimate_observe_client_authenticates() {
    let keys = HubKeys::new(b"observe-key".to_vec(), b"control-key".to_vec());
    let response = run_handshake(&keys, server_nonce(2), keys.key_for(Scope::Read), Scope::Read);
    assert!(granted(&response, Scope::Read), "{response:?}");
}

#[test]
fn observe_key_presented_for_control_is_denied() {
    let keys = HubKeys::new(b"observe-key".to_vec(), b"control-key".to_vec());
    // Client claims Control but signs with the OBSERVE key it actually holds.
    let response = run_handshake(&keys, server_nonce(3), keys.key_for(Scope::Read), Scope::Write);
    assert!(bad_mac(&response), "{response:?}");
}

#[test]
fn wrong_key_is_denied() {
    let keys = HubKeys::new(b"observe-key".to_vec(), b"control-key".to_vec());
    let response = run_handshake(&keys, server_nonce(4), b"attacker-guess", Scope::Write);
    assert!(bad_mac(&response), "{response:?}");
}

#[test]
fn a_replayed_mac_from_a_prior_connection_is_denied() {
    let keys = HubKeys::new(b"observe-key".to_vec(), b"control-key".to_vec());
    let control_key = keys.key_for(Scope::Write);

    // Connection A (nonce seed 10): the client legitimately authenticates; capture its Auth mac.
    let nonce_a = server_nonce(10);
    let captured_mac = auth::sign(control_key, &nonce_a, NODE_PROTO_VERSION, Scope::Write);
    let valid_on_a =
        auth::verify(control_key, &nonce_a, NODE_PROTO_VERSION, Scope::Write, &captured_mac);
    assert!(valid_on_a, "sanity: the captured mac is valid on its OWN connection");

    // Connection B (a DIFFERENT nonce): replaying the captured mac must fail — the whole point of
    // the per-connection nonce challenge.
    let nonce_b = server_nonce(11);
    let valid_on_b =
        auth::verify(control_key, &nonce_b, NODE_PROTO_VERSION, Scope::Write, &captured_mac);
    assert!(!valid_on_b, "a mac captured on connection A must be rejected on connection B");
}
