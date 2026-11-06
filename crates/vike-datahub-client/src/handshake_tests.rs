//! `run_keyed_handshake` over a real loopback socket, one refusal at a time: the bytes the client
//! reads back, the notes the daemon is told, and the outcome. The datahub's and the backtesthub's
//! copies were this function twice; a mutant that reorders a refusal, changes a reason or drops a
//! note goes red here.

use std::io::Write as _;
use std::net::{TcpListener, TcpStream};

use super::*;
use crate::proto::read_frame;
use vike_hub_proto::auth::sign;

const READ_KEY: &[u8] = b"handshake-test-read-key";
const WRITE_KEY: &[u8] = b"handshake-test-write-key";

/// `body` as one frame: its big-endian `u32` length, then the bytes (decodable or not).
fn framed(body: &[u8]) -> Vec<u8> {
    let mut frame = u32::try_from(body.len()).unwrap().to_be_bytes().to_vec();
    frame.extend_from_slice(body);
    frame
}

fn json(request: &Request) -> Vec<u8> {
    serde_json::to_vec(request).unwrap()
}

/// One handshake: a client thread sends `first`, answers the server's `Welcome` (given its nonce)
/// with `second` when that is `Some`, and reads every reply until the server hangs up.
fn handshake(
    first: Vec<u8>,
    second: impl FnOnce([u8; 32]) -> Option<Vec<u8>> + Send + 'static,
) -> (HandshakeOutcome, Vec<HandshakeNote>, Vec<Response>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let client = std::thread::Builder::new()
        .name("handshake-test-client".into())
        .spawn(move || {
            let mut stream = TcpStream::connect(addr).unwrap();
            stream.write_all(&framed(&first)).unwrap();
            let mut second = Some(second);
            let mut replies = Vec::new();
            while let Ok(reply) = read_frame::<_, Response>(&mut stream) {
                if let Response::Welcome { nonce: Some(nonce), .. } = &reply
                    && let Some(body) = second.take().and_then(|answer| answer(*nonce))
                {
                    stream.write_all(&framed(&body)).unwrap();
                }
                replies.push(reply);
            }
            replies
        })
        .unwrap();
    let (mut server, _) = listener.accept().unwrap();
    let keys = HubKeys::new(READ_KEY.to_vec(), WRITE_KEY.to_vec());
    let mut notes = Vec::new();
    let outcome = run_keyed_handshake(&mut server, &keys, &["f".to_string()], |n| notes.push(n));
    drop(server);
    (outcome, notes, client.join().unwrap())
}

fn hello(proto_version: u32) -> Vec<u8> {
    json(&Request::Hello { proto_version })
}

fn auth(scope: Scope, mac: Vec<u8>) -> Option<Vec<u8>> {
    Some(json(&Request::Auth { scope, mac }))
}

/// The refusal a reply carries, `None` for any other reply.
fn denied(reply: &Response) -> Option<&str> {
    match reply {
        Response::AuthDenied { reason } => Some(reason),
        _ => None,
    }
}

fn is_welcome(reply: &Response) -> bool {
    matches!(
        reply,
        Response::Welcome { proto_version: PROTO_VERSION, features, nonce: Some(_) }
            if features == &["f".to_string()]
    )
}

#[test]
fn a_verb_before_hello_is_refused_by_name_and_noted() {
    let (outcome, notes, replies) = handshake(json(&Request::Ping), |_| None);
    assert!(matches!(outcome, HandshakeOutcome::Closed));
    assert_eq!(notes, [HandshakeNote::VerbRefused(request_kind(&Request::Ping))]);
    assert_eq!(
        replies.iter().map(denied).collect::<Vec<_>>(),
        [Some("not authenticated: expected Hello first")]
    );
}

#[test]
fn an_undecodable_first_frame_is_refused_unnoted() {
    let (outcome, notes, replies) = handshake(b"not json".to_vec(), |_| None);
    assert!(matches!(outcome, HandshakeOutcome::Closed));
    assert!(notes.is_empty(), "{notes:?}");
    assert_eq!(
        replies.iter().map(denied).collect::<Vec<_>>(),
        [Some("expected Hello (undecodable request)")]
    );
}

#[test]
fn a_second_frame_that_is_not_auth_is_refused_after_the_welcome() {
    for (second, reason) in [
        (json(&Request::Ping), "expected Auth after Welcome"),
        (b"not json".to_vec(), "expected Auth (undecodable request)"),
    ] {
        let (outcome, notes, replies) = handshake(hello(PROTO_VERSION), move |_| Some(second));
        assert!(matches!(outcome, HandshakeOutcome::Closed));
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(replies.len(), 2, "{replies:?}");
        assert!(is_welcome(&replies[0]), "{replies:?}");
        assert_eq!(denied(&replies[1]), Some(reason));
    }
}

#[test]
fn a_scope_with_no_key_gets_the_bad_mac_reason_and_the_daemon_the_truth() {
    let (outcome, notes, replies) =
        handshake(hello(PROTO_VERSION), |_| auth(Scope::Trade, vec![0; 32]));
    assert!(matches!(outcome, HandshakeOutcome::Closed));
    assert_eq!(notes, [HandshakeNote::NoKey(Scope::Trade)]);
    assert!(is_welcome(&replies[0]), "{replies:?}");
    assert_eq!(denied(&replies[1]), Some("bad mac"));
}

#[test]
fn a_bad_mac_is_refused_and_a_version_skew_is_noted_first() {
    let (outcome, notes, replies) =
        handshake(hello(PROTO_VERSION + 1), |_| auth(Scope::Read, vec![0; 32]));
    assert!(matches!(outcome, HandshakeOutcome::Closed));
    assert_eq!(
        notes,
        [HandshakeNote::VersionDiffers(PROTO_VERSION + 1), HandshakeNote::BadMac(Scope::Read)]
    );
    assert!(is_welcome(&replies[0]), "{replies:?}");
    assert_eq!(denied(&replies[1]), Some("bad mac"));
}

#[test]
fn a_mac_signed_over_the_nonce_under_the_datahub_domain_authenticates() {
    let (outcome, notes, replies) = handshake(hello(PROTO_VERSION), |nonce| {
        auth(Scope::Write, sign(DATAHUB_DOMAIN, WRITE_KEY, &nonce, PROTO_VERSION, Scope::Write))
    });
    assert!(matches!(outcome, HandshakeOutcome::Authed(Scope::Write)), "{outcome:?}");
    assert_eq!(notes, [HandshakeNote::Authenticated(Scope::Write)]);
    assert_eq!(replies.len(), 2, "{replies:?}");
    assert!(is_welcome(&replies[0]), "{replies:?}");
    assert!(matches!(replies[1], Response::AuthOk { scope: Scope::Write }), "{replies:?}");
}
