//! The AUTHENTICATION gate for the datahub wire (`docs/decisions/0025-datahub-remote-posture.md`,
//! the adopting PR): hermetic, loopback-only, over the in-memory `MemHistStore` double.
//!
//! Five properties, each in its own section below:
//!
//! 1. **A key-LESS server is unchanged** — not "still works", but BYTE-IDENTICAL on the handshake
//!    frame and serving every verb. This is the backward-compatibility contract every existing
//!    local flow (`vike-cli backtest`, the Studio's `Backend::Remote`, `RemoteHistStore`, the GUI's
//!    store branch) depends on, and it is the one an implementation is most likely to break by
//!    accident.
//! 2. **A KEYED server refuses every verb pre-auth** — exhaustively, driven off
//!    [`vike_datahub::required_scope`]'s own classification rather than a hand-written list, so the
//!    table cannot silently fall behind the enum.
//! 3. **The scope split is enforced** — Observe reads but is refused `Backfill` AND the
//!    Rhai-compiling `Run*` verbs; Control does both.
//! 4. **A bad mac is denied** — wrong key, wrong scope, a tag from the tradehub domain, and a mac
//!    replayed from another connection.
//! 5. **The pre-auth bounds hold** — the frame cap and the handshake deadline.
//!
//! The composed AUTHED round trip carrying real data lives with its DataFusion sibling at the
//! bottom of the file, behind `serve-datafusion`.

use std::io::Write;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use vike_data::{HistStore, MemHistStore, SeriesId};
use vike_datahub::server::{HANDSHAKE_DEADLINE, HANDSHAKE_MAX_FRAME_LEN};
use vike_datahub::{required_scope, serve_authed, VerbScope};
use vike_datahub_client::node_auth::{self, NodeKeys, Scope, DATAHUB_DOMAIN};
use vike_datahub_client::proto::{
    read_frame, write_frame, Request, Response, FEATURE_AUTH, PROTO_VERSION,
};
use vike_datahub_client::DatahubClient;

const OBSERVE_KEY: &[u8] = b"datahub-observe-key";
const CONTROL_KEY: &[u8] = b"datahub-control-key";

/// A minimal, valid bar-mode profile — enough to exercise `RunBacktest`'s full path over an empty
/// store (it loads no bars, so it closes no trades; what matters here is that the verb is REACHED
/// or REFUSED, not what it computes).
const MINIMAL_BAR_PROFILE: &str = r#"
[data]
venue = "binance"
symbols = ["BTCUSDT"]
kind = "bar"
interval = "1d"
from = "0"
to = "100000"

[engine]
cash = 1000.0

[strategy]
name = "buy_hold"
[strategy.params]
size = 1.0
symbol = "BTCUSDT"
"#;

fn keys() -> NodeKeys {
    NodeKeys::new(OBSERVE_KEY.to_vec(), CONTROL_KEY.to_vec())
}

/// Bind an ephemeral loopback listener and spawn `serve_authed` over a fresh in-memory store.
fn spawn(keys: Option<NodeKeys>) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let store: Arc<dyn HistStore + Send + Sync> = Arc::new(MemHistStore::new());
    thread::spawn(move || {
        let _ = serve_authed(listener, store, None, keys);
    });
    addr
}

/// One sample of EVERY `Request` variant. ⚠ Kept in step with the enum by
/// `the_sample_set_covers_every_verb_scope_classification` below, not by hope: that test asserts
/// this set hits all three [`VerbScope`]s and both Control sub-families (the write verb AND the
/// Rhai-compiling ones), so a new verb that changes the shape of the classification cannot leave
/// this file quietly under-covering.
fn every_request() -> Vec<Request> {
    let series = SeriesId::per_symbol("bar", "binance", "BTCUSDT", Some("1h".to_string()));
    vec![
        Request::Hello { proto_version: PROTO_VERSION },
        Request::Auth { scope: Scope::Observe, mac: vec![0u8; 32] },
        Request::Ping,
        Request::RunBacktest(MINIMAL_BAR_PROFILE.to_string()),
        Request::RunSweepProfile { profile_toml: MINIMAL_BAR_PROFILE.to_string(), rank_by: None },
        Request::RunWalkforwardProfile { profile_toml: MINIMAL_BAR_PROFILE.to_string() },
        Request::LoadBars {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            interval: "1h".into(),
            start: None,
            end: None,
        },
        Request::ScanQuotes {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            start: None,
            end: None,
        },
        Request::ScanTrades {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            start: None,
            end: None,
        },
        Request::PropertiesAsOf { venue: "binance".into(), symbol: "BTCUSDT".into(), ts: 0 },
        Request::ListSeries,
        Request::Inventory,
        Request::SeriesGaps { id: series },
        Request::ListStrategies,
        Request::Backfill {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            interval: "1h".into(),
            start: 0,
            end: 1,
        },
    ]
}

/// Send one raw request on an already-established stream and read the answer.
fn exchange(stream: &mut TcpStream, request: &Request) -> Response {
    write_frame(stream, request).expect("write request");
    read_frame::<_, Response>(stream).expect("read response")
}

/// Open a socket and complete the handshake as `scope`, returning the authenticated stream.
fn authed_stream(addr: SocketAddr, keys: &NodeKeys, scope: Scope) -> TcpStream {
    let mut s = TcpStream::connect(addr).expect("connect");
    write_frame(&mut s, &Request::Hello { proto_version: PROTO_VERSION }).expect("hello");
    let nonce = match read_frame::<_, Response>(&mut s).expect("welcome") {
        Response::Welcome { nonce, .. } => nonce.expect("a keyed server's Welcome carries a nonce"),
        other => panic!("expected Welcome, got {other:?}"),
    };
    let mac = node_auth::sign(DATAHUB_DOMAIN, keys.key_for(scope), &nonce, PROTO_VERSION, scope);
    write_frame(&mut s, &Request::Auth { scope, mac }).expect("auth");
    match read_frame::<_, Response>(&mut s).expect("auth answer") {
        Response::AuthOk { scope: granted } => assert_eq!(granted, scope),
        other => panic!("expected AuthOk, got {other:?}"),
    }
    s
}

// ---- 1. the key-LESS server is unchanged --------------------------------------------------------

/// ⚠ **The backward-compatibility proof, and it is a BYTE comparison, not a behavioural one.**
///
/// A key-less server's `Welcome` must serialize to the EXACT bytes the pre-auth protocol produced:
/// no `nonce` field on the wire at all (hence `skip_serializing_if` on an `Option`, not a
/// `[0u8;32]` sentinel), and no `auth` string in `features`. A test that merely asserted "an old
/// client still works" would pass against a `Welcome` that had grown a `"nonce":null` field — which
/// is a wire change, and the kind that breaks a strict decoder somewhere downstream.
///
/// The expectation is built from the frame the SERVER actually sends, compared against a
/// hand-rolled JSON object carrying only the two pre-auth fields.
#[test]
fn a_keyless_servers_welcome_is_byte_identical_to_the_pre_auth_protocol() {
    let addr = spawn(None);
    let mut s = TcpStream::connect(addr).expect("connect");
    write_frame(&mut s, &Request::Hello { proto_version: PROTO_VERSION }).expect("hello");
    let welcome = read_frame::<_, Response>(&mut s).expect("welcome");

    let (features, nonce) = match &welcome {
        Response::Welcome { features, nonce, .. } => (features.clone(), *nonce),
        other => panic!("expected Welcome, got {other:?}"),
    };
    assert!(nonce.is_none(), "a key-less server must mint no nonce");
    assert!(
        !features.iter().any(|f| f == FEATURE_AUTH),
        "a key-less server must not advertise `{FEATURE_AUTH}`: {features:?}"
    );

    // The BYTES. `nonce` is absent from the encoding entirely — not present-and-null.
    let json = serde_json::to_string(&welcome).expect("encode");
    assert!(!json.contains("nonce"), "the key-less Welcome must carry no `nonce` field: {json}");
    assert!(!json.contains(FEATURE_AUTH), "...and no auth advertisement: {json}");
}

/// A key-less server serves EVERY verb without any handshake at all — including sending a normal
/// verb as the very FIRST frame, with no `Hello` before it. That "Hello informs, it does not gate"
/// contract is what `vike-cli`, the Studio and the GUI have always relied on.
///
/// The assertion is deliberately "not an auth refusal" rather than "succeeded": several verbs
/// legitimately answer `Response::Error` over an empty `MemHistStore` (or on a build without
/// `serve-datafusion`). What must NEVER appear is `AuthDenied`, or an error mentioning a scope.
#[test]
fn a_keyless_server_serves_every_verb_with_no_handshake() {
    let addr = spawn(None);
    for request in every_request() {
        // A FRESH connection per verb, each sending the verb as its first frame — the strongest
        // form of "no handshake required".
        let mut s = TcpStream::connect(addr).expect("connect");
        let response = exchange(&mut s, &request);
        match (&request, &response) {
            // The one exception, and it is the honest answer rather than a refusal: a client that
            // signed a mac against a server holding no keys is TOLD its key was never checked.
            (Request::Auth { .. }, Response::AuthDenied { reason }) => {
                assert!(
                    reason.contains("no node keys"),
                    "a key-less server's Auth answer must say the keys are absent: {reason}"
                );
            }
            (_, Response::AuthDenied { reason }) => {
                panic!("key-less server refused {request:?} with AuthDenied: {reason}")
            }
            (_, Response::Error(msg)) => assert!(
                !msg.contains("scope"),
                "key-less server refused {request:?} on scope grounds: {msg}"
            ),
            _ => {}
        }
    }
}

/// The unauthenticated `DatahubClient::connect` path — the one every existing caller uses — still
/// connects and serves against a key-less server, with `authenticated_scope() == None`.
#[test]
fn the_plain_client_still_works_against_a_keyless_server() {
    let addr = spawn(None);
    let mut client = DatahubClient::connect(addr).expect("connect must succeed unauthenticated");
    assert_eq!(client.authenticated_scope(), None);
    assert!(!client.requires_auth());
    client.ping().expect("ping");
    assert!(client.list_series().expect("list_series is served").is_empty());
}

/// ...and a client that HOLDS keys degrades cleanly against a key-less server rather than failing:
/// it sees no `auth` advertisement, sends no `Auth`, and returns a working unauthenticated
/// connection. That is what lets one configured GUI work against both a keyed production datahub
/// and a local key-less dev one without branching.
#[test]
fn an_authed_connect_degrades_to_unauthenticated_against_a_keyless_server() {
    let addr = spawn(None);
    let mut client = DatahubClient::connect_authed(addr, &keys(), Scope::Control)
        .expect("connect_authed must not fail against a key-less server");
    assert_eq!(
        client.authenticated_scope(),
        None,
        "no authentication took place, and the client must say so rather than claim a scope"
    );
    client.ping().expect("ping");
}

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

// ---- 3. the scope split -------------------------------------------------------------------------

/// The CLASSIFICATION, pinned as a table — the authority is [`required_scope`] and this is what it
/// says, verb by verb. A change to any row is a deliberate change to what an Observe credential can
/// do, and must show up as a diff here.
///
/// ⚠ The rows that matter most are the six `Run*` ones. They RETURN answers and read like reads,
/// but every one of them compiles CLIENT-SUPPLIED RHAI server-side, so they are Control.
#[test]
fn the_verb_scope_classification_is_pinned() {
    let expect = |request: Request, want: VerbScope| {
        assert_eq!(required_scope(&request), want, "{request:?}");
    };
    expect(Request::Hello { proto_version: PROTO_VERSION }, VerbScope::Handshake);
    expect(Request::Auth { scope: Scope::Observe, mac: vec![] }, VerbScope::Handshake);

    expect(Request::Ping, VerbScope::Observe);
    expect(Request::ListSeries, VerbScope::Observe);
    expect(Request::Inventory, VerbScope::Observe);
    expect(Request::ListStrategies, VerbScope::Observe);
    expect(
        Request::SeriesGaps {
            id: SeriesId::per_symbol("bar", "binance", "BTCUSDT", Some("1h".to_string())),
        },
        VerbScope::Observe,
    );
    for r in every_request() {
        match &r {
            Request::LoadBars { .. }
            | Request::ScanQuotes { .. }
            | Request::ScanTrades { .. }
            | Request::PropertiesAsOf { .. } => {
                assert_eq!(required_scope(&r), VerbScope::Observe, "reads are Observe: {r:?}")
            }
            _ => {}
        }
    }

    // The WRITE verb.
    expect(
        Request::Backfill {
            venue: "binance".into(),
            symbol: "B".into(),
            interval: "1h".into(),
            start: 0,
            end: 1,
        },
        VerbScope::Control,
    );
    // ...and the RHAI-COMPILING verbs. `RunSlice`/`RunSweep`/`RunWalkforward` need studio DTOs to
    // construct, so the profile-shaped three stand for the family here; the family's membership is
    // exhaustive in `required_scope`, whose match has no `_` arm.
    expect(Request::RunBacktest(String::new()), VerbScope::Control);
    expect(
        Request::RunSweepProfile { profile_toml: String::new(), rank_by: None },
        VerbScope::Control,
    );
    expect(Request::RunWalkforwardProfile { profile_toml: String::new() }, VerbScope::Control);
}

/// The sample set used by the pre-auth table actually spans the classification — all three
/// [`VerbScope`]s, and BOTH Control families (the store WRITE and the Rhai-compiling compute). A
/// new verb that shifted the shape of the split would otherwise leave the exhaustive test above
/// exhaustive-looking but blind.
#[test]
fn the_sample_set_covers_every_verb_scope_classification() {
    let scopes: Vec<VerbScope> = every_request().iter().map(required_scope).collect();
    for want in [VerbScope::Handshake, VerbScope::Observe, VerbScope::Control] {
        assert!(scopes.contains(&want), "the sample set never exercises {want:?}");
    }
    let has_write = every_request()
        .iter()
        .any(|r| matches!(r, Request::Backfill { .. }) && required_scope(r) == VerbScope::Control);
    let has_rhai = every_request()
        .iter()
        .any(|r| matches!(r, Request::RunBacktest(_)) && required_scope(r) == VerbScope::Control);
    assert!(has_write, "the sample set must include the store-WRITE Control verb");
    assert!(has_rhai, "the sample set must include a RHAI-COMPILING Control verb");
}

/// An OBSERVE connection reads — every read verb answers on ONE long-lived connection, proving the
/// scope check does not close it.
#[test]
fn observe_reads() {
    let addr = spawn(Some(keys()));
    let mut s = authed_stream(addr, &keys(), Scope::Observe);
    for request in every_request().into_iter().filter(|r| required_scope(r) == VerbScope::Observe) {
        match exchange(&mut s, &request) {
            Response::AuthDenied { reason } => {
                panic!("Observe refused a read {request:?}: {reason}")
            }
            Response::Error(msg) => {
                assert!(!msg.contains("scope"), "Observe refused {request:?} on scope: {msg}")
            }
            _ => {}
        }
    }
}

/// ...and an OBSERVE connection is REFUSED every Control verb — the write AND the Rhai-compiling
/// compute. This is the property the whole scope split exists for: history without remote code
/// execution, which was not expressible before.
#[test]
fn observe_is_refused_the_write_verb_and_the_rhai_compiling_verbs() {
    let addr = spawn(Some(keys()));
    let mut s = authed_stream(addr, &keys(), Scope::Observe);
    let controls: Vec<Request> =
        every_request().into_iter().filter(|r| required_scope(r) == VerbScope::Control).collect();
    assert!(!controls.is_empty(), "guard: the filter must not be vacuous");
    for request in controls {
        match exchange(&mut s, &request) {
            Response::Error(msg) => {
                assert!(msg.contains("Control scope"), "the refusal names the scope: {msg}");
                assert!(msg.contains("Rhai"), "...and why Run* is Control: {msg}");
            }
            other => panic!("Observe was ALLOWED the Control verb {request:?}: {other:?}"),
        }
    }
    // The connection SURVIVED every refusal — an over-scoped verb is a bad request, not a bad
    // connection, so a mixed-verb client is not disconnected for asking.
    match exchange(&mut s, &Request::Ping) {
        Response::Pong => {}
        other => panic!("the connection did not survive the scope refusals: {other:?}"),
    }
}

/// A CONTROL connection does both: the reads AND the Control verbs. (`Backfill` answers a clean
/// refusal here because no collector table is mounted — the point is that it REACHED the verb
/// rather than being stopped at the scope check, which the message distinguishes.)
#[test]
fn control_does_both() {
    let addr = spawn(Some(keys()));
    let mut s = authed_stream(addr, &keys(), Scope::Control);
    for request in every_request()
        .into_iter()
        .filter(|r| matches!(required_scope(r), VerbScope::Observe | VerbScope::Control))
    {
        match exchange(&mut s, &request) {
            Response::AuthDenied { reason } => panic!("Control refused {request:?}: {reason}"),
            Response::Error(msg) => assert!(
                !msg.contains("Control scope"),
                "Control was refused {request:?} on scope grounds: {msg}"
            ),
            _ => {}
        }
    }
}

/// An observe-ONLY server (a `NodeKeys` with no control key) refuses a `Control` handshake outright
/// — the closed-gate shape: an absent key is never consulted, so it cannot be an open door.
#[test]
fn a_server_with_no_control_key_refuses_control_auth() {
    let addr = spawn(Some(NodeKeys::new(OBSERVE_KEY.to_vec(), Vec::new())));
    let err = DatahubClient::connect_authed(addr, &keys(), Scope::Control)
        .expect_err("control must be refused on an observe-only server");
    assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied, "{err}");
    // ...and Observe on the same server still works, so the refusal is the missing key and not the
    // server being broken.
    let mut ok = DatahubClient::connect_authed(addr, &keys(), Scope::Observe).expect("observe");
    assert_eq!(ok.authenticated_scope(), Some(Scope::Observe));
    ok.ping().expect("ping");
}

// ---- 4. a bad mac is denied ---------------------------------------------------------------------

/// A WRONG key is denied — the ordinary forgery.
#[test]
fn a_wrong_key_is_denied() {
    let addr = spawn(Some(keys()));
    let wrong = NodeKeys::new(b"not-the-observe-key".to_vec(), b"not-the-control-key".to_vec());
    for scope in [Scope::Observe, Scope::Control] {
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
    let err = DatahubClient::connect_authed(addr, &observe_only_holder, Scope::Control)
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
    let tradehub_domain = node_auth::Domain::new(b"vike-tradehub-auth\0");
    let mac = node_auth::sign(tradehub_domain, OBSERVE_KEY, &nonce, PROTO_VERSION, Scope::Observe);
    write_frame(&mut s, &Request::Auth { scope: Scope::Observe, mac }).expect("auth");
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
    let captured =
        node_auth::sign(DATAHUB_DOMAIN, OBSERVE_KEY, &nonce_a, PROTO_VERSION, Scope::Observe);
    write_frame(&mut a, &Request::Auth { scope: Scope::Observe, mac: captured.clone() })
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
    write_frame(&mut b, &Request::Auth { scope: Scope::Observe, mac: captured }).expect("auth B");
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
        write_frame(&mut s, &Request::Auth { scope: Scope::Observe, mac: mac.clone() })
            .expect("auth");
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
        write_frame(&mut s, &Request::Auth { scope: Scope::Control, mac: vec![7u8; 32] })
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

// ---- 5. the pre-auth bounds ---------------------------------------------------------------------

/// The PRE-AUTH frame cap: a peer that declares a length above [`HANDSHAKE_MAX_FRAME_LEN`] is cut
/// off BEFORE the allocation, so it cannot make the server reserve 64 MiB per connection by sending
/// four bytes.
///
/// The length prefix is written by hand — the whole point is a declared length with no body behind
/// it, which no honest client can produce.
#[test]
fn a_pre_auth_frame_above_the_cap_is_refused_before_allocation() {
    let addr = spawn(Some(keys()));
    let mut s = TcpStream::connect(addr).expect("connect");
    let over = HANDSHAKE_MAX_FRAME_LEN + 1;
    s.write_all(&over.to_be_bytes()).expect("write the length prefix and NOTHING else");
    s.flush().expect("flush");
    s.set_read_timeout(Some(Duration::from_secs(10))).expect("read timeout");
    // The server closes without answering rather than waiting on `over` bytes that never come.
    let answer = read_frame::<_, Response>(&mut s);
    assert!(answer.is_err(), "an over-cap pre-auth frame must not be honoured: {answer:?}");

    // …and the cap leaves ample room for both REAL handshake frames, which is the other half of
    // choosing it (a cap that clipped a legitimate handshake would be a broken server, not a safe
    // one). Measured against the real encoder, not guessed.
    let mut buf: Vec<u8> = Vec::new();
    write_frame(&mut buf, &Request::Hello { proto_version: PROTO_VERSION }).expect("hello");
    write_frame(&mut buf, &Request::Auth { scope: Scope::Control, mac: vec![0u8; 32] })
        .expect("auth");
    assert!(
        buf.len() * 8 < HANDSHAKE_MAX_FRAME_LEN as usize,
        "both handshake frames are {} bytes; the cap ({HANDSHAKE_MAX_FRAME_LEN}) must keep an \
         order of magnitude of headroom over them",
        buf.len()
    );
}

/// The HANDSHAKE DEADLINE: a peer that connects and says NOTHING is dropped within
/// [`HANDSHAKE_DEADLINE`], not held for the 300 s idle timeout. Without it, an unauthenticated peer
/// parks a connection thread for five minutes per socket.
///
/// Asserted with generous slack on the upper bound (CI schedulers are not real-time) but a HARD
/// upper bound well under `IDLE_READ_TIMEOUT`, which is the property that matters: if the deadline
/// were not applied, this test would hang for five minutes rather than fail by a margin.
#[test]
fn a_silent_peer_is_dropped_at_the_handshake_deadline() {
    let addr = spawn(Some(keys()));
    let mut s = TcpStream::connect(addr).expect("connect");
    // Say nothing at all. The read below returns EOF when the server gives up on us.
    s.set_read_timeout(Some(HANDSHAKE_DEADLINE * 6)).expect("read timeout");
    let started = Instant::now();
    let answer = read_frame::<_, Response>(&mut s);
    let waited = started.elapsed();
    assert!(answer.is_err(), "a silent peer must be dropped, not served: {answer:?}");
    assert!(
        waited < HANDSHAKE_DEADLINE * 5,
        "the silent peer was held {waited:?}, which is not the {HANDSHAKE_DEADLINE:?} handshake \
         deadline — the pre-auth read timeout is not being applied"
    );
}

/// An AUTHENTICATED connection is NOT held to the short handshake deadline: the timeout is reset to
/// the ordinary idle one once `AuthOk` is written, so a legitimately idle client is not clipped.
/// (This is the regression the deadline could easily introduce — a short timeout left armed.)
#[test]
fn an_authenticated_connection_is_not_clipped_by_the_handshake_deadline() {
    let addr = spawn(Some(keys()));
    let mut s = authed_stream(addr, &keys(), Scope::Observe);
    // Idle for longer than the handshake deadline, then use the connection.
    thread::sleep(HANDSHAKE_DEADLINE + Duration::from_secs(2));
    match exchange(&mut s, &Request::Ping) {
        Response::Pong => {}
        other => panic!("an idle AUTHENTICATED connection was clipped: {other:?}"),
    }
}

// ---- the composed AUTHED round trip, with real data ---------------------------------------------

/// The composed proof, in the shape of `composed_store_roundtrip.rs`: a real `DataFusionHist`
/// seeded over a temp dir, served with keys, and read back through
/// `RemoteHistStore::with_keys` — REAL bars crossing an AUTHENTICATED wire and coming back equal.
///
/// Why it belongs here rather than only in the unit tests above: everything above proves the
/// handshake in isolation over an empty `MemHistStore` (which stores no bars and can only ever
/// assert `is_empty()`). The question this answers is whether a scoped, authenticated connection
/// still CARRIES data — i.e. that the auth layer did not quietly break the thing the server is for.
#[cfg(feature = "serve-datafusion")]
mod composed {
    use super::*;
    use tempfile::TempDir;
    // `TsRange` is used ONLY by this module's reads, so it is imported HERE rather than at the
    // file head: a default (DataFusion-free) build compiles none of `composed`, and a top-level
    // import would be an unused-import warning — which is a `-D warnings` clippy failure.
    use vike_data::{DataFusionHist, TsRange};
    use vike_datahub_client::RemoteHistStore;
    use vike_model::Bar;

    const VENUE: &str = "binance";
    const SYMBOL: &str = "AUTHEDUSDT";
    const INTERVAL: &str = "1h";

    fn seeded_bars() -> Vec<Bar> {
        (0..3)
            .map(|i| Bar {
                ts: i * 3_600_000,
                open: 100.0 + i as f64,
                high: 110.0 + i as f64,
                low: 90.0 + i as f64,
                close: 105.0 + i as f64,
                volume: 10.0 + i as f64,
                funding: None,
                bid: None,
                ask: None,
                symbol: None,
            })
            .collect()
    }

    #[test]
    fn real_bars_cross_an_authenticated_wire_unchanged() {
        let dir = TempDir::new().expect("temp store root");
        let store = DataFusionHist::open(dir.path()).expect("open");
        let bars = seeded_bars();
        let n = store
            .append_bars(VENUE, SYMBOL, INTERVAL, &bars, Some("seed-authed-bars"))
            .expect("seed");
        assert_eq!(n, bars.len(), "every seeded bar lands");

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let served: Arc<dyn HistStore + Send + Sync> = Arc::new(store);
        thread::spawn(move || {
            let _ = serve_authed(listener, served, None, Some(keys()));
        });

        // An UNAUTHENTICATED reader gets nothing — the guard that this server really is keyed.
        let bare = RemoteHistStore::new(addr.to_string());
        assert!(
            bare.load_bars(VENUE, SYMBOL, INTERVAL, TsRange { start: None, end: None }).is_err(),
            "an unauthenticated RemoteHistStore must not read from a keyed server"
        );

        // …and the OBSERVE-keyed one reads the seeded bars back, equal.
        let remote = RemoteHistStore::with_keys(addr.to_string(), keys());
        let got = remote
            .load_bars(VENUE, SYMBOL, INTERVAL, TsRange { start: None, end: None })
            .expect("authenticated read");
        assert_eq!(got, seeded_bars(), "the bars must survive the authenticated wire unchanged");

        // The catalog verbs too — the Data-Manager reads, on the same authed seam.
        let series = remote.list_series().expect("list_series");
        assert!(
            series.iter().any(|s| s.symbol == SYMBOL),
            "the seeded series must be visible over the authed connection: {series:?}"
        );
    }
}
