//! `run_keyed_handshake` over a real loopback socket, one refusal at a time: the bytes the client
//! reads back, the notes the daemon is told, and the outcome. The datahub's, the backtesthub's and the
//! gatewayhub's copies were this function three times; a mutant that reorders a refusal, changes a
//! reason or drops a note goes red here. Each [`HandshakeSpec`] term a daemon sets (the domain, the
//! plane, the reply ceiling) and the served-scope check have a test of their own, and the last test
//! holds the phase deadline against a peer that trickles.

use std::io::Write as _;
use std::net::TcpListener;

use super::*;
use crate::auth::{DATAHUB_DOMAIN, sign};
use crate::frame::{MAX_FRAME_LEN, read_frame};

const READ_KEY: &[u8] = b"handshake-test-read-key";
const WRITE_KEY: &[u8] = b"handshake-test-write-key";
const PROTO_VERSION: u32 = 7;
/// Production's deadline: every client here answers at once, so it never fires.
const DEADLINE: Duration = crate::limits::SERVER_HANDSHAKE_DEADLINE;
/// A domain separator that is not the data plane's, for the daemon on no data plane.
const OTHER_DOMAIN: Domain = Domain::new(b"vike-handshake-test-auth\0");

/// The terms a test daemon handshakes under: [`DATA_DAEMON`] unless the test says otherwise.
#[derive(Clone, Copy)]
struct Daemon {
    domain: Domain,
    plane: Option<Plane>,
    reply_ceiling: u32,
    served: fn(Scope) -> bool,
}

fn every_scope(_: Scope) -> bool {
    true
}

/// The datahub's terms: its domain and plane, the frame codec's ceiling, every scope it has a key for.
const DATA_DAEMON: Daemon = Daemon {
    domain: DATAHUB_DOMAIN,
    plane: Some(Plane::Data),
    reply_ceiling: MAX_FRAME_LEN,
    served: every_scope,
};

fn keys() -> HubKeys {
    HubKeys::new(READ_KEY.to_vec(), WRITE_KEY.to_vec())
}

/// A daemon's own request enum, as far as this suite needs one: one verb.
#[derive(Debug, Deserialize)]
enum OwnVerb {
    Ping,
}

fn verb_name(_: &OwnVerb) -> &'static str {
    "Ping"
}

/// `body` as one frame: its big-endian `u32` length, then the bytes (decodable or not).
fn framed(body: &[u8]) -> Vec<u8> {
    let mut frame = u32::try_from(body.len()).unwrap().to_be_bytes().to_vec();
    frame.extend_from_slice(body);
    frame
}

fn json(frame: &HandshakeFrame) -> Vec<u8> {
    serde_json::to_vec(frame).unwrap()
}

const PING: &[u8] = br#""Ping""#;

/// [`handshake_with`] the datahub's terms.
fn handshake(
    first: Vec<u8>,
    second: impl FnOnce([u8; 32]) -> Option<Vec<u8>> + Send + 'static,
) -> (HandshakeOutcome, Vec<HandshakeNote>, Vec<HandshakeReply>) {
    handshake_with(DATA_DAEMON, first, second)
}

/// One handshake with `daemon`: a client thread sends `first`, answers the server's `Welcome` (given
/// its nonce) with `second` when that is `Some`, and reads every reply until the server hangs up.
fn handshake_with(
    daemon: Daemon,
    first: Vec<u8>,
    second: impl FnOnce([u8; 32]) -> Option<Vec<u8>> + Send + 'static,
) -> (HandshakeOutcome, Vec<HandshakeNote>, Vec<HandshakeReply>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let client = std::thread::Builder::new()
        .name("handshake-test-client".into())
        .spawn(move || {
            let mut stream = TcpStream::connect(addr).unwrap();
            stream.write_all(&framed(&first)).unwrap();
            let mut second = Some(second);
            let mut replies = Vec::new();
            while let Ok(reply) = read_frame::<_, HandshakeReply>(&mut stream) {
                if let HandshakeReply::Welcome { nonce: Some(nonce), .. } = &reply
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
    let features = ["f".to_string()];
    let spec = HandshakeSpec {
        domain: daemon.domain,
        proto_version: PROTO_VERSION,
        plane: daemon.plane,
        features: &features,
        deadline: DEADLINE,
        reply_ceiling: daemon.reply_ceiling,
    };
    let mut notes = Vec::new();
    let outcome = run_keyed_handshake(&mut server, &keys(), spec, daemon.served, verb_name, |n| {
        notes.push(n)
    });
    drop(server);
    (outcome, notes, client.join().unwrap())
}

fn hello(proto_version: u32) -> Vec<u8> {
    json(&HandshakeFrame::Hello { proto_version })
}

fn auth(scope: Scope, mac: Vec<u8>) -> Option<Vec<u8>> {
    Some(json(&HandshakeFrame::Auth { scope, mac }))
}

/// The refusal a reply carries, `None` for any other reply.
fn denied(reply: &HandshakeReply) -> Option<&str> {
    match reply {
        HandshakeReply::AuthDenied { reason } => Some(reason),
        _ => None,
    }
}

fn is_welcome(reply: &HandshakeReply) -> bool {
    matches!(
        reply,
        HandshakeReply::Welcome { proto_version: PROTO_VERSION, features, nonce: Some(_) }
            if features == &["f".to_string()]
    )
}

#[test]
fn a_verb_before_hello_is_refused_by_name_and_noted() {
    let (outcome, notes, replies) = handshake(PING.to_vec(), |_| None);
    assert!(matches!(outcome, HandshakeOutcome::Closed));
    assert_eq!(notes, [HandshakeNote::VerbRefused("Ping")]);
    assert_eq!(
        replies.iter().map(denied).collect::<Vec<_>>(),
        [Some("not authenticated: expected Hello first")]
    );
}

/// ⚠ A verb of the OTHER plane gets the same words as one of this daemon's, and is noted under its own
/// wire tag, although this daemon's enum cannot decode it.
#[test]
fn a_verb_of_the_other_plane_before_hello_is_refused_the_same_way() {
    let (outcome, notes, replies) = handshake(br#"{"RunBacktest":"x"}"#.to_vec(), |_| None);
    assert!(matches!(outcome, HandshakeOutcome::Closed));
    assert_eq!(notes, [HandshakeNote::VerbRefused("RunBacktest")]);
    assert_eq!(
        replies.iter().map(denied).collect::<Vec<_>>(),
        [Some("not authenticated: expected Hello first")]
    );
}

#[test]
fn an_auth_before_hello_is_refused_as_a_verb() {
    let (outcome, notes, replies) = handshake(auth(Scope::Read, vec![0; 32]).unwrap(), |_| None);
    assert!(matches!(outcome, HandshakeOutcome::Closed));
    assert_eq!(notes, [HandshakeNote::VerbRefused("Auth")]);
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
        (PING.to_vec(), "expected Auth after Welcome"),
        (hello(PROTO_VERSION), "expected Auth after Welcome"),
        (br#"{"RunBacktest":"x"}"#.to_vec(), "expected Auth after Welcome"),
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
    // The outcome carries the fingerprint of the key that verified, for the daemon's audit trail.
    let expected_id = keys().key_id(Scope::Write);
    assert!(expected_id.is_some());
    assert!(
        matches!(&outcome, HandshakeOutcome::Authed { scope: Scope::Write, key_id } if *key_id == expected_id),
        "{outcome:?}"
    );
    assert_eq!(notes, [HandshakeNote::Authenticated(Scope::Write)]);
    assert_eq!(replies.len(), 2, "{replies:?}");
    assert!(is_welcome(&replies[0]), "{replies:?}");
    assert!(matches!(replies[1], HandshakeReply::AuthOk { scope: Scope::Write }), "{replies:?}");
}

/// A scope the daemon does not SERVE is refused like one it holds no key for, before any key is
/// consulted: a mac that WOULD verify still gets the coarse `bad mac`, and the daemon the truth.
#[test]
fn a_scope_the_daemon_does_not_serve_is_refused_although_its_mac_verifies() {
    let read_only = Daemon { served: |scope| scope == Scope::Read, ..DATA_DAEMON };
    let (outcome, notes, replies) = handshake_with(read_only, hello(PROTO_VERSION), |nonce| {
        auth(Scope::Write, sign(DATAHUB_DOMAIN, WRITE_KEY, &nonce, PROTO_VERSION, Scope::Write))
    });
    assert!(matches!(outcome, HandshakeOutcome::Closed), "{outcome:?}");
    assert_eq!(notes, [HandshakeNote::NoKey(Scope::Write)]);
    assert!(is_welcome(&replies[0]), "{replies:?}");
    assert_eq!(denied(&replies[1]), Some("bad mac"));
}

/// The mac is verified under the DAEMON's domain: a datahub-domain mac does not verify at a daemon
/// with another one, and one signed under that daemon's own does.
#[test]
fn the_mac_is_verified_under_the_daemons_own_domain() {
    let other = Daemon { domain: OTHER_DOMAIN, ..DATA_DAEMON };
    let (outcome, notes, _) = handshake_with(other, hello(PROTO_VERSION), |nonce| {
        auth(Scope::Read, sign(DATAHUB_DOMAIN, READ_KEY, &nonce, PROTO_VERSION, Scope::Read))
    });
    assert!(matches!(outcome, HandshakeOutcome::Closed), "{outcome:?}");
    assert_eq!(notes, [HandshakeNote::BadMac(Scope::Read)]);

    let (outcome, _, _) = handshake_with(other, hello(PROTO_VERSION), |nonce| {
        auth(Scope::Read, sign(OTHER_DOMAIN, READ_KEY, &nonce, PROTO_VERSION, Scope::Read))
    });
    assert!(matches!(outcome, HandshakeOutcome::Authed { scope: Scope::Read, .. }), "{outcome:?}");
}

/// A daemon on NO data plane (the gatewayhub) does not name a data-plane verb it cannot decode: the
/// frame is undecodable, before `Hello` and in place of `Auth` alike.
#[test]
fn a_daemon_on_no_data_plane_calls_a_data_plane_verb_undecodable() {
    let no_plane = Daemon { plane: None, ..DATA_DAEMON };
    let load_bars = br#"{"LoadBars":"x"}"#.to_vec();
    let (outcome, notes, replies) = handshake_with(no_plane, load_bars.clone(), |_| None);
    assert!(matches!(outcome, HandshakeOutcome::Closed));
    assert!(notes.is_empty(), "{notes:?}");
    assert_eq!(
        replies.iter().map(denied).collect::<Vec<_>>(),
        [Some("expected Hello (undecodable request)")]
    );

    let (_, notes, replies) =
        handshake_with(no_plane, hello(PROTO_VERSION), move |_| Some(load_bars));
    assert!(notes.is_empty(), "{notes:?}");
    assert_eq!(denied(&replies[1]), Some("expected Auth (undecodable request)"));
}

/// A pre-auth reply over the daemon's reply ceiling is not written at all: the `Welcome` here is
/// longer than 8 bytes, so the client reads nothing and the daemon closes.
#[test]
fn a_reply_over_the_reply_ceiling_is_not_written() {
    let tiny = Daemon { reply_ceiling: 8, ..DATA_DAEMON };
    let (outcome, notes, replies) = handshake_with(tiny, hello(PROTO_VERSION), |_| None);
    assert!(matches!(outcome, HandshakeOutcome::Closed), "{outcome:?}");
    assert!(notes.is_empty(), "{notes:?}");
    assert!(replies.is_empty(), "{replies:?}");
}

/// **The deadline is for the PHASE, not for each read.** A peer that sends a legal length prefix and
/// then one byte every 50 ms (well inside any per-read timeout) would hold a per-read bound for the
/// 50 s its 1,000 bytes take; it is cut off at the 300 ms deadline instead.
#[test]
fn a_peer_trickling_inside_every_read_timeout_is_cut_off_at_the_phase_deadline() {
    let deadline = Duration::from_millis(300);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let trickler = std::thread::Builder::new()
        .name("handshake-test-trickle".into())
        .spawn(move || {
            let mut stream = TcpStream::connect(addr).unwrap();
            if stream.write_all(&1_000_u32.to_be_bytes()).is_err() {
                return;
            }
            for _ in 0..1_000 {
                if stream.write_all(b"x").is_err() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        })
        .unwrap();
    let (mut server, _) = listener.accept().unwrap();
    let spec = HandshakeSpec {
        domain: DATAHUB_DOMAIN,
        proto_version: PROTO_VERSION,
        plane: Some(Plane::Data),
        features: &[],
        deadline,
        reply_ceiling: MAX_FRAME_LEN,
    };
    let began = Instant::now();
    let outcome = run_keyed_handshake(&mut server, &keys(), spec, every_scope, verb_name, |_| {});
    let held = began.elapsed();
    drop(server);
    assert!(matches!(outcome, HandshakeOutcome::Closed), "{outcome:?}");
    assert!(
        held >= deadline / 2 && held < deadline * 5,
        "a peer trickling a byte per 50 ms was held {held:?}, not the {deadline:?} phase deadline"
    );
    trickler.join().unwrap();
}
