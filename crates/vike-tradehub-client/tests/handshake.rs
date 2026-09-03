//! End-to-end handshake composition (PR-10): drive the full Hello → Welcome → Auth → verify flow
//! through the REAL [`proto`] messages and [`auth`] math over the SHARED length-prefixed framing,
//! entirely in-memory (a `Vec<u8>` cursor stands in for the socket — there is no I/O in this crate).
//!
//! This proves the security property at the integration boundary the unit tests assert in isolation:
//! a legitimate client authenticates; a replayed transcript, a wrong-scope client, and a wrong key
//! are all rejected by the server's constant-time verify.

use vike_tradehub_client::{
    auth::{self, NodeKeys},
    proto::{read_frame, write_frame, Request, Response, Scope, NODE_PROTO_VERSION},
};

/// A deterministic "random" nonce for the test server (production mints a CSPRNG nonce per
/// connection; the security property is that it is fresh per connection, which the replay test below
/// exercises by using two DIFFERENT nonces).
fn server_nonce(seed: u8) -> [u8; 32] {
    [seed; 32]
}

/// Run one connection's handshake in memory: the server writes a `Welcome` with `nonce`, the client
/// signs it under `client_key` for `client_scope`, and the server verifies with `NodeKeys`. Returns
/// the server's terminal auth response.
fn run_handshake(
    keys: &NodeKeys,
    nonce: [u8; 32],
    client_key: &[u8],
    client_scope: Scope,
) -> Response {
    // --- server → client: Welcome{ nonce } (framed) ---
    let mut wire: Vec<u8> = Vec::new();
    let welcome = Response::Welcome {
        proto_version: NODE_PROTO_VERSION,
        nonce,
        features: vec!["subscribe".into()],
    };
    write_frame(&mut wire, &welcome).unwrap();

    // client reads the Welcome back off the wire and extracts the challenge nonce
    let mut cursor = std::io::Cursor::new(wire);
    let got: Response = read_frame(&mut cursor).unwrap();
    let (proto_version, challenge) = match got {
        Response::Welcome { proto_version, nonce, .. } => (proto_version, nonce),
        other => panic!("expected Welcome, got {other:?}"),
    };

    // --- client → server: Auth{ scope, mac } (framed) ---
    let mac = auth::sign(client_key, &challenge, proto_version, client_scope);
    let mut wire2: Vec<u8> = Vec::new();
    write_frame(&mut wire2, &Request::Auth { scope: client_scope, mac }).unwrap();

    // server reads the Auth and verifies against its own key for the claimed scope
    let mut cursor2 = std::io::Cursor::new(wire2);
    let auth_req: Request = read_frame(&mut cursor2).unwrap();
    let (scope, mac) = match auth_req {
        Request::Auth { scope, mac } => (scope, mac),
        other => panic!("expected Auth, got {other:?}"),
    };
    if auth::verify(keys.key_for(scope), &nonce, NODE_PROTO_VERSION, scope, &mac) {
        Response::AuthOk { scope }
    } else {
        Response::AuthDenied { reason: "bad mac".into() }
    }
}

#[test]
fn legitimate_control_client_authenticates() {
    let keys = NodeKeys::new(b"observe-key".to_vec(), b"control-key".to_vec());
    let resp = run_handshake(&keys, server_nonce(1), keys.key_for(Scope::Control), Scope::Control);
    assert_eq!(resp, Response::AuthOk { scope: Scope::Control });
}

#[test]
fn legitimate_observe_client_authenticates() {
    let keys = NodeKeys::new(b"observe-key".to_vec(), b"control-key".to_vec());
    let resp = run_handshake(&keys, server_nonce(2), keys.key_for(Scope::Observe), Scope::Observe);
    assert_eq!(resp, Response::AuthOk { scope: Scope::Observe });
}

#[test]
fn observe_key_presented_for_control_is_denied() {
    let keys = NodeKeys::new(b"observe-key".to_vec(), b"control-key".to_vec());
    // Client claims Control but signs with the OBSERVE key it actually holds.
    let resp = run_handshake(&keys, server_nonce(3), keys.key_for(Scope::Observe), Scope::Control);
    assert_eq!(resp, Response::AuthDenied { reason: "bad mac".into() });
}

#[test]
fn wrong_key_is_denied() {
    let keys = NodeKeys::new(b"observe-key".to_vec(), b"control-key".to_vec());
    let resp = run_handshake(&keys, server_nonce(4), b"attacker-guess", Scope::Control);
    assert_eq!(resp, Response::AuthDenied { reason: "bad mac".into() });
}

#[test]
fn a_replayed_mac_from_a_prior_connection_is_denied() {
    let keys = NodeKeys::new(b"observe-key".to_vec(), b"control-key".to_vec());
    let control_key = keys.key_for(Scope::Control);

    // Connection A (nonce seed 10): the client legitimately authenticates; capture its Auth mac.
    let nonce_a = server_nonce(10);
    let captured_mac = auth::sign(control_key, &nonce_a, NODE_PROTO_VERSION, Scope::Control);
    let valid_on_a =
        auth::verify(control_key, &nonce_a, NODE_PROTO_VERSION, Scope::Control, &captured_mac);
    assert!(valid_on_a, "sanity: the captured mac is valid on its OWN connection");

    // Connection B (a DIFFERENT nonce): replaying the captured mac must fail — the whole point of
    // the per-connection nonce challenge.
    let nonce_b = server_nonce(11);
    let valid_on_b =
        auth::verify(control_key, &nonce_b, NODE_PROTO_VERSION, Scope::Control, &captured_mac);
    assert!(!valid_on_b, "a mac captured on connection A must be rejected on connection B");
}
