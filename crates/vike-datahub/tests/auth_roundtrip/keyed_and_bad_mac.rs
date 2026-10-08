//! Sections 2 and 4: a KEYED server refuses every verb pre-auth, and a bad mac is denied.

use super::*;

// ---- 2. a KEYED server refuses every verb pre-auth -----------------------------------------------

/// ⚠ **The exhaustive table.** Every verb, on its own fresh connection, sent WITHOUT a handshake:
/// each must be refused. The list is [`every_request`], and
/// `the_sample_set_covers_every_verb_scope_classification` is what keeps it honest.
///
/// `Hello` is the ONE frame that is answered rather than refused — it is how a connection begins —
/// so it is asserted separately as "answered with a Welcome carrying a nonce".
#[test]
fn a_keyed_server_refuses_every_verb_before_auth() {
    let addr = spawn(Some(keys()));
    for request in every_request() {
        let mut s = TcpStream::connect(addr).expect("connect");
        write_frame(&mut s, &request).expect("write");
        let response = read_frame::<_, Response>(&mut s);
        match (&request, response) {
            (Request::Hello { .. }, Ok(Response::Welcome { nonce, features, .. })) => {
                assert!(nonce.is_some(), "a keyed Welcome must carry a nonce");
                assert!(
                    features.iter().any(|f| f == FEATURE_AUTH),
                    "a keyed server must advertise `{FEATURE_AUTH}`"
                );
            }
            // `Auth` as the FIRST frame (no `Hello`, so no nonce was ever minted) is refused like
            // any other out-of-order frame.
            (Request::Auth { .. }, Ok(Response::AuthDenied { reason })) => {
                assert!(reason.contains("Hello"), "{reason}");
            }
            (_, Ok(Response::AuthDenied { .. })) => {}
            (_, other) => panic!(
                "keyed server did not refuse un-authenticated {request:?}; answered {other:?}"
            ),
        }
    }
}

/// A refused connection is CLOSED, not merely answered — otherwise an unauthenticated peer keeps a
/// connection thread for the idle timeout by sending one bad frame.
#[test]
fn a_refused_handshake_closes_the_connection() {
    let addr = spawn(Some(keys()));
    let mut s = TcpStream::connect(addr).expect("connect");
    write_frame(&mut s, &Request::Ping).expect("write");
    let _ = read_frame::<_, Response>(&mut s).expect("the refusal itself");
    // The server has dropped its end; the next read hits EOF rather than blocking.
    s.set_read_timeout(Some(Duration::from_secs(5))).expect("read timeout");
    let after = read_frame::<_, Response>(&mut s);
    assert!(after.is_err(), "the socket must be closed after a refusal, got {after:?}");
}

/// `DatahubClient::connect` (the keyless constructor) against a KEYED server fails AT CONNECT with
/// an actionable message naming the keys — not one round trip later with a confusing per-verb
/// error.
#[test]
fn the_plain_client_fails_legibly_against_a_keyed_server() {
    let addr = spawn(Some(keys()));
    let err =
        DatahubClient::connect(addr).expect_err("a keyed server must refuse a keyless client");
    let msg = err.to_string();
    assert!(msg.contains("VIKE_DATAHUB_OBSERVE_KEY"), "the error names the keys: {msg}");
    assert!(msg.contains("connect_authed"), "...and the constructor to use: {msg}");
}

// ---- 4. a bad mac is denied ---------------------------------------------------------------------

/// A WRONG key is denied — the ordinary forgery.
#[test]
fn a_wrong_key_is_denied() {
    let addr = spawn(Some(keys()));
    let wrong = NodeKeys::new(b"not-the-observe-key".to_vec(), b"not-the-control-key".to_vec());
    for scope in [Scope::Read, Scope::Write] {
        let err = DatahubClient::connect_authed(addr, &wrong, scope)
            .expect_err("a wrong key must be denied");
        assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied, "{scope:?}: {err}");
    }
}

/// The OBSERVE key cannot buy CONTROL: the scope is signed, and the two scopes sign under different
/// keys, so an observe-only holder presenting a `Control` request is denied. Capability is
/// cryptographic here, not a claim the server takes on trust.
#[test]
fn an_observe_key_cannot_authenticate_control() {
    let addr = spawn(Some(keys()));
    // A client whose CONTROL slot holds the OBSERVE key — i.e. an operator with read credentials
    // trying to sign a control handshake with what they have.
    let observe_only_holder = NodeKeys::new(OBSERVE_KEY.to_vec(), OBSERVE_KEY.to_vec());
    let err = DatahubClient::connect_authed(addr, &observe_only_holder, Scope::Write)
        .expect_err("an observe key must not authenticate Control");
    assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied, "{err}");
}

/// ⚠ A tag minted for the TRADEHUB node is denied here, even though the two services share the
/// scheme and could share key bytes. This is the domain separator doing its job at the SERVER, not
/// merely in the crypto unit tests.
#[test]
fn a_foreign_domain_mac_is_denied() {
    let addr = spawn(Some(keys()));
    let mut s = TcpStream::connect(addr).expect("connect");
    write_frame(&mut s, &Request::Hello { proto_version: PROTO_VERSION }).expect("hello");
    let nonce = match read_frame::<_, Response>(&mut s).expect("welcome") {
        Response::Welcome { nonce, .. } => nonce.expect("nonce"),
        other => panic!("expected Welcome, got {other:?}"),
    };
    // Correct key, correct nonce, correct version, correct scope — WRONG domain.
    let tradehub_domain = auth::Domain::new(b"vike-tradehub-auth\0");
    let mac = auth::sign(tradehub_domain, OBSERVE_KEY, &nonce, PROTO_VERSION, Scope::Read);
    write_frame(&mut s, &Request::Auth { scope: Scope::Read, mac }).expect("auth");
    match read_frame::<_, Response>(&mut s).expect("answer") {
        Response::AuthDenied { .. } => {}
        other => panic!("a tradehub-domain tag was ACCEPTED at the datahub: {other:?}"),
    }
}

/// A mac REPLAYED from another connection is denied: each connection mints a fresh nonce, so a
/// captured transcript is worthless against the next socket. This is the anti-replay property the
/// nonce challenge exists for, proven end to end rather than in the signing unit test.
#[test]
fn a_replayed_mac_from_another_connection_is_denied() {
    let addr = spawn(Some(keys()));

    // Connection A: capture a legitimate, ACCEPTED mac.
    let mut a = TcpStream::connect(addr).expect("connect A");
    write_frame(&mut a, &Request::Hello { proto_version: PROTO_VERSION }).expect("hello A");
    let nonce_a = match read_frame::<_, Response>(&mut a).expect("welcome A") {
        Response::Welcome { nonce, .. } => nonce.expect("nonce"),
        other => panic!("expected Welcome, got {other:?}"),
    };
    let captured = auth::sign(DATAHUB_DOMAIN, OBSERVE_KEY, &nonce_a, PROTO_VERSION, Scope::Read);
    write_frame(&mut a, &Request::Auth { scope: Scope::Read, mac: captured.clone() })
        .expect("auth A");
    assert!(
        matches!(read_frame::<_, Response>(&mut a).expect("A"), Response::AuthOk { .. }),
        "guard: the captured mac must be a VALID one, or the replay test proves nothing"
    );

    // Connection B: replay it verbatim.
    let mut b = TcpStream::connect(addr).expect("connect B");
    write_frame(&mut b, &Request::Hello { proto_version: PROTO_VERSION }).expect("hello B");
    let nonce_b = match read_frame::<_, Response>(&mut b).expect("welcome B") {
        Response::Welcome { nonce, .. } => nonce.expect("nonce"),
        other => panic!("expected Welcome, got {other:?}"),
    };
    assert_ne!(nonce_a, nonce_b, "each connection must mint a FRESH nonce");
    write_frame(&mut b, &Request::Auth { scope: Scope::Read, mac: captured }).expect("auth B");
    match read_frame::<_, Response>(&mut b).expect("B") {
        Response::AuthDenied { .. } => {}
        other => panic!("a replayed mac was ACCEPTED on a second connection: {other:?}"),
    }
}

/// A truncated / empty mac is denied (the constant-time compare rejects a length mismatch rather
/// than panicking or short-circuiting).
#[test]
fn a_truncated_mac_is_denied() {
    let addr = spawn(Some(keys()));
    for mac in [vec![], vec![0u8; 16], vec![0xFFu8; 32]] {
        let mut s = TcpStream::connect(addr).expect("connect");
        write_frame(&mut s, &Request::Hello { proto_version: PROTO_VERSION }).expect("hello");
        let _ = read_frame::<_, Response>(&mut s).expect("welcome");
        write_frame(&mut s, &Request::Auth { scope: Scope::Read, mac: mac.clone() }).expect("auth");
        match read_frame::<_, Response>(&mut s).expect("answer") {
            Response::AuthDenied { .. } => {}
            other => panic!("a {}-byte mac was accepted: {other:?}", mac.len()),
        }
    }
}

/// The refusal reason is deliberately COARSE: it never distinguishes "no key for that scope" from
/// "wrong key for that scope", so an unauthenticated peer cannot enumerate which scopes a server
/// offers. The operator's log carries the real answer.
#[test]
fn the_refusal_reason_does_not_enumerate_the_servers_scopes() {
    let observe_only = spawn(Some(NodeKeys::new(OBSERVE_KEY.to_vec(), Vec::new())));
    let both = spawn(Some(keys()));
    let reason_for = |addr: SocketAddr| -> String {
        let mut s = TcpStream::connect(addr).expect("connect");
        write_frame(&mut s, &Request::Hello { proto_version: PROTO_VERSION }).expect("hello");
        let _ = read_frame::<_, Response>(&mut s).expect("welcome");
        // A deliberately bogus Control mac on both servers: one lacks the key, one has it.
        write_frame(&mut s, &Request::Auth { scope: Scope::Write, mac: vec![7u8; 32] })
            .expect("auth");
        match read_frame::<_, Response>(&mut s).expect("answer") {
            Response::AuthDenied { reason } => reason,
            other => panic!("expected AuthDenied, got {other:?}"),
        }
    };
    assert_eq!(
        reason_for(observe_only),
        reason_for(both),
        "the refusal must not tell an unauthenticated peer whether the scope has a key at all"
    );
}
