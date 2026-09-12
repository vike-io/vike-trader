//! The AUTHENTICATION gate for the datahub wire (`docs/decisions/0025-datahub-remote-posture.md`,
//! the adopting PR): hermetic, loopback-only, over the in-memory `MemHistStore` double.
//!
//! Five properties, each in its own section below:
//!
//! 1. **A key-LESS server's HANDSHAKE is unchanged, and its verb set is unchanged but for one** —
//!    not "still works", but BYTE-IDENTICAL on the `Welcome` frame, and serving every verb EXCEPT
//!    the destructive `DeleteSeries` with no handshake at all. This is the backward-compatibility
//!    contract every existing local flow (`vike-cli backtest`, the Studio's `Backend::Remote`,
//!    `RemoteHistStore`, the GUI's store branch) depends on, and it is the one an implementation is
//!    most likely to break by accident.
//!
//!    ⚠ **This property read "BYTE-IDENTICAL … and serving every verb" until 2026-09-07, and its
//!    second half is now false — the tests below say so.** `a_keyless_server_serves_no_delete_verb`
//!    drives the wire with the server built BOTH ways and asserts that a key-less one neither
//!    advertises `delete_series` nor answers the request, and
//!    `a_keyless_server_serves_every_verb_with_no_handshake` carries a NAMED arm for that verb
//!    rather than passing by substring accident. The BYTE half is untouched and still exact: a
//!    key-less `Welcome` encodes with no `nonce` field and no auth advertisement, which is what
//!    `a_keyless_servers_welcome_is_byte_identical_to_the_pre_auth_protocol` compares. `Backfill`
//!    is `Scope::Control` too and is NOT withheld for key-lessness — only `delete_series_verb`
//!    gates on `keyed` — which is the asymmetry the record argues: a backfill writes rows a
//!    re-fetch restores, a removal takes the only copy
//!    (`docs/decisions/0050-a-key-less-datahub-serves-no-delete-verb.md`).
//!    ⚠ Do not read that as "these tests see a backfill served". They do not, and cannot: `spawn`
//!    passes `None` for the backfill table, so `backfill_verb` answers the missing-feature error on
//!    the very arm they drive. What `a_keyless_server_serves_every_verb_with_no_handshake` proves
//!    about `Backfill` is the narrower and load-bearing thing — the refusal it gets does NOT mention
//!    a scope, so it is not being refused for key-lessness.
//! 2. **A KEYED server refuses every verb pre-auth** — exhaustively, driven off
//!    `vike_datahub_client::proto`'s `required_scope`'s own classification rather than a hand-written list, so the
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

use vike_data::removal::SeriesSelector;
use vike_data::{HistStore, MemHistStore, SeriesId};
use vike_datahub::md::MdHub;
use vike_datahub::serve_authed;
use vike_datahub::server::{HANDSHAKE_DEADLINE, HANDSHAKE_MAX_FRAME_LEN};
// ⚠ `VerbScope`/`required_scope` MOVED to the light client crate when ruling 7 gave the compute
// verbs a second daemon: one table below both servers, so `vike-backtest` (layer 50) enforces the
// same classification `vike-datahub` (layer 65) does. No `pub use` shim was left behind.
use vike_datahub_client::DatahubClient;
use vike_datahub_client::market::{MdLane, MdSessionId, MdSpec};
use vike_datahub_client::node_auth::{self, DATAHUB_DOMAIN, NodeKeys, Scope};
use vike_datahub_client::proto::{
    FEATURE_AUTH, PROTO_VERSION, Request, Response, read_frame, write_frame,
};
use vike_datahub_client::proto::{VerbScope, required_scope};

const OBSERVE_KEY: &[u8] = b"datahub-observe-key";
const CONTROL_KEY: &[u8] = b"datahub-control-key";

// ⚠ `MINIMAL_BAR_PROFILE` is GONE from this file: it existed to give `RunBacktest` a valid payload,
// and that verb is the COMPUTE daemon's since ruling 7. The profile itself moved with it, to
// `crates/vike-backtest/tests/compute_plane.rs`.

fn keys() -> NodeKeys {
    NodeKeys::new(OBSERVE_KEY.to_vec(), CONTROL_KEY.to_vec())
}

/// Bind an ephemeral loopback listener and spawn `serve_authed` over a fresh in-memory store.
///
/// ⚠ **No `MdHub` — this is the DEFAULT-BUILD path**, and that is what makes
/// `a_hubless_server_refuses_md_subscribe_and_stays_positional` a test of the
/// shipped default rather than of a configuration nobody runs.
fn spawn(keys: Option<NodeKeys>) -> SocketAddr {
    spawn_with_md(keys, None)
}

/// [`spawn`] with an optional market-data hub mounted.
fn spawn_with_md(keys: Option<NodeKeys>, md: Option<Arc<MdHub>>) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let store: Arc<dyn HistStore + Send + Sync> = Arc::new(MemHistStore::new());
    thread::spawn(move || {
        let _ = serve_authed(listener, store, None, keys, md);
    });
    addr
}

/// One sample of every `Request` variant THIS DAEMON SERVES — the DATA plane plus the handshake.
///
/// ⚠ **The seven COMPUTE verbs are deliberately absent since ruling 7** (`docs/superpowers/specs/
/// 2026-09-09-datahub-market-data-wire-design.md`). They still DECODE here — one schema, two
/// daemons — but this server answers them with a wrong-plane refusal before the scope check runs,
/// so driving them through the auth tests below would prove the refusal, not the authentication.
/// Their refusal is `tests/plane_split.rs`'s subject; their SCOPE classification is still pinned
/// below, because `required_scope` is one table for both daemons.
///
/// ⚠ Kept in step with the enum by
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
        Request::Backfill {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            interval: "1h".into(),
            start: 0,
            end: 1,
        },
        Request::DeleteSeries {
            selector: SeriesSelector::new("bar", "binance"),
            produced_by: Some("klines:".to_string()),
            dry_run: true,
        },
        // The market-data SET MUTATION — an ordinary positional Observe verb on a short-lived
        // connection, so it belongs in every sweep below.
        Request::MdUpdate { session: MdSessionId::fresh(), add: Vec::new(), remove: Vec::new() },
        // ⚠⚠ **`Request::MdSubscribe` IS DELIBERATELY ABSENT, AND PUTTING IT HERE BREAKS THIS FILE
        // SILENTLY.** It is this wire's one MODE SWITCH: `observe_reads` and `control_does_both`
        // drive every verb in this list over ONE long-lived connection through `exchange` (write
        // one frame, read one frame), and a subscribed connection stops answering positionally —
        // the loop's next `exchange` would read a pushed `MdFrame::Heartbeat` as if it were the
        // next verb's REPLY, and every remaining assertion in the sweep would pass against the
        // wrong frame. It fails SILENTLY, not loudly, which is the worst way for a test to be
        // wrong.
        //
        // What it costs to leave it out is REAL and is paid for by name elsewhere, because
        // `MdSubscribe` is the one verb in this protocol that converts a connection into an
        // UNBOUNDED WRITER and a pre-auth leak of it would be a market-data firehose plus a venue
        // refcount for an unauthenticated peer:
        //   * the pre-auth refusal — `a_keyed_server_refuses_md_subscribe_before_auth`;
        //   * the mode switch itself — `md_subscribe_is_the_last_positional_frame_on_its_socket`;
        //   * the hub-less refusal — `a_hubless_server_refuses_md_subscribe_and_stays_positional`.
        // And `the_sample_set_covers_every_verb_scope_classification` asserts the mode-switch verbs
        // ARE covered somewhere, so this omission cannot be confused with a forgotten one.
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
///
/// ⚠ **`DeleteSeries` is the ONE verb this claim does not cover, and its exception is spelled here
/// rather than left to a substring match.** That verb is refused OUTRIGHT on a key-less server —
/// see `a_keyless_server_serves_no_delete_verb` — so "every verb" acquired an exception the day it
/// landed. Writing the arm out is what keeps this test's own title honest: without it the refusal
/// passed only because the message happened to spell `Scope::Control` with a capital S.
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
            // The DESTRUCTIVE verb: a key-less server must refuse it, and the refusal must SAY it
            // is about keys rather than about the request.
            (Request::DeleteSeries { .. }, Response::Error(msg)) => assert!(
                msg.contains("no node keys"),
                "a key-less server's delete refusal must name the missing keys: {msg}"
            ),
            (Request::DeleteSeries { .. }, other) => {
                panic!("a key-less server answered DeleteSeries with {other:?}")
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

/// **The narrowing that made the remote delete verb buildable at all**, proved with the server
/// built BOTH ways over one fixture.
///
/// `docs/decisions/0025-datahub-remote-posture.md` records why a datahub write verb is the class
/// change that forces authentication, and the reason it gives — *"A surface with a write verb and no
/// way to say 'reads yes, writes no' cannot be handed even a trusted LAN"* — is sharper for a verb
/// that destroys the only copy than for one that writes rows a re-fetch restores. The design that
/// proposed this verb declined it for exactly that reason: `Scope::Control` is meaningful only on a
/// KEYED server, and a key-less one is the shipped default.
///
/// The rule this test pins is the answer to that, and it is unconditional in both directions:
///
/// * key-LESS — the verb is NOT advertised and is REFUSED, whatever the request says. There is no
///   flag that turns it on.
/// * KEYED — the verb IS advertised, and reaches the store under the Control scope.
///
/// ⚠ Two legs, not one: the advertisement is what stops a well-behaved client sending, and the
/// refusal is what holds for a client that does not check. Neither substitutes for the other, which
/// is why this drives the WIRE rather than calling `served_features`.
#[test]
fn a_keyless_server_serves_no_delete_verb() {
    let request = Request::DeleteSeries {
        selector: SeriesSelector::new("bar", "binance"),
        produced_by: Some("klines:".to_string()),
        dry_run: true,
    };

    // ---- key-LESS: not advertised, and refused on the wire anyway ------------------------------
    let keyless = spawn(None);
    let advertised = DatahubClient::connect(keyless).expect("connect").features().to_vec();
    assert!(
        !advertised.iter().any(|f| f == "delete_series"),
        "a key-less server must not advertise the delete verb: {advertised:?}"
    );
    let mut s = TcpStream::connect(keyless).expect("connect");
    match exchange(&mut s, &request) {
        Response::Error(msg) => {
            assert!(msg.contains("no node keys"), "{msg}");
            assert!(msg.contains("vike-cli data rm --store"), "the refusal says what to do: {msg}");
        }
        other => panic!("a key-less server answered DeleteSeries with {other:?}"),
    }

    // ---- KEYED: advertised, and served under Control -------------------------------------------
    let keyed = spawn(Some(keys()));
    let mut s = authed_stream(keyed, &keys(), Scope::Control);
    match exchange(&mut s, &request) {
        // The empty `MemHistStore` cannot enumerate a provenance, so the PLAN itself fails — which
        // is the store's answer, not a posture refusal. What this arm proves is that the verb was
        // REACHED: a key-less server never gets this far.
        Response::Deleted(done) => assert_eq!(done.plan.matched(), 0),
        Response::Error(msg) => assert!(
            !msg.contains("no node keys"),
            "a KEYED server must reach the verb rather than refusing on keys: {msg}"
        ),
        other => panic!("expected Deleted or a store error, got {other:?}"),
    }

    // ...and an OBSERVE connection on that same keyed server is refused on SCOPE.
    let mut s = authed_stream(keyed, &keys(), Scope::Observe);
    match exchange(&mut s, &request) {
        Response::Error(msg) => assert!(msg.contains("Control scope"), "{msg}"),
        other => panic!("an Observe connection reached the delete verb: {other:?}"),
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
    // ...and the DESTRUCTIVE verb. Control here is NECESSARY and not sufficient: the server refuses
    // it outright when it holds no keys, which `a_keyless_server_serves_no_delete_verb` proves — a
    // scope classification is meaningless on a server that authenticates nothing.
    expect(
        Request::DeleteSeries {
            selector: SeriesSelector::new("bar", "binance"),
            produced_by: Some("klines:".to_string()),
            dry_run: true,
        },
        VerbScope::Control,
    );
    // ...and the MARKET-DATA verbs, which are the rows a reader is most likely to want re-argued:
    // `Backfill` sits three lines up as Control and also "just returns data", so "it reads like a
    // read" cannot be the test. The rule that separates them is BOUNDED-BY-AN-OPERATOR-SET-CEILING
    // versus not — a backfill's venue cost is unbounded per request and the CLIENT names the range,
    // while a subscription's is bounded by MD_MAX_KEYS_PER_VENUE / MD_LINGER, which no request can
    // move. `docs/decisions/0052-a-market-data-subscription-is-an-observe-verb.md` is the record,
    // and it also states the posture this inherits (on a KEY-LESS server every verb but
    // `DeleteSeries` is served to whoever reaches the loopback socket, per 0050).
    expect(Request::MdSubscribe { specs: Vec::new() }, VerbScope::Observe);
    expect(
        Request::MdUpdate { session: MdSessionId::fresh(), add: Vec::new(), remove: Vec::new() },
        VerbScope::Observe,
    );
    // ...and their PLANE, which is the single highest-consequence classification in this change: a
    // `Plane::Compute` answer would have the DATA daemon refuse its own new verbs with a
    // wrong-plane message, compiling perfectly and working not at all.
    for r in [
        Request::MdSubscribe { specs: Vec::new() },
        Request::MdUpdate { session: MdSessionId::fresh(), add: Vec::new(), remove: Vec::new() },
    ] {
        assert_eq!(
            vike_datahub_client::proto::plane_of(&r),
            vike_datahub_client::proto::Plane::Data,
            "{r:?} is served by the DATA daemon"
        );
        assert!(
            matches!(vike_datahub_client::proto::request_kind(&r), "MdSubscribe" | "MdUpdate"),
            "{r:?}"
        );
    }
}

/// The sample set used by the pre-auth table actually spans the classification THIS DAEMON can
/// exercise — all three [`VerbScope`]s, with a store WRITE and a store REMOVAL on the Control side.
/// A new verb that shifted the shape of the split would otherwise leave the exhaustive test above
/// exhaustive-looking but blind.
///
/// ⚠ It used to demand a RHAI-COMPILING Control verb in the set too, and that requirement moved
/// with the verbs (ruling 7): the `Run*` family is the COMPUTE daemon's, and
/// `crates/vike-backtest/tests/compute_plane.rs` is where a Control-scoped Rhai compiler is now
/// driven over a socket. What this file keeps is the half that is still true here — Control on this
/// daemon means "changes the store".
#[test]
fn the_sample_set_covers_every_verb_scope_classification() {
    let scopes: Vec<VerbScope> = every_request().iter().map(required_scope).collect();
    for want in [VerbScope::Handshake, VerbScope::Observe, VerbScope::Control] {
        assert!(scopes.contains(&want), "the sample set never exercises {want:?}");
    }
    let has_write = every_request()
        .iter()
        .any(|r| matches!(r, Request::Backfill { .. }) && required_scope(r) == VerbScope::Control);
    let has_delete = every_request().iter().any(|r| {
        matches!(r, Request::DeleteSeries { .. }) && required_scope(r) == VerbScope::Control
    });
    assert!(has_write, "the sample set must include the store-WRITE Control verb");
    assert!(has_delete, "the sample set must include the store-REMOVAL Control verb");
    // ⚠ **AND THE MODE-SWITCH FAMILY, WHOSE MEMBERSHIP IS SPLIT ON PURPOSE.** `MdUpdate` is an
    // ordinary positional verb and belongs in the sweep; `MdSubscribe` converts the connection into
    // a push stream and would make every later `exchange` in `observe_reads` assert against a
    // heartbeat. Asserting BOTH halves here is what keeps the carve-out distinguishable from an
    // omission — without it, adding two Observe verbs and forgetting to list them reddens nothing,
    // and what goes untested is the pre-auth refusal of the one verb that becomes an unbounded
    // writer.
    assert!(
        every_request().iter().any(|r| matches!(r, Request::MdUpdate { .. })),
        "the sample set must include the market-data SET MUTATION — it is positional and safe here"
    );
    assert!(
        every_request().iter().all(|r| !matches!(r, Request::MdSubscribe { .. })),
        "⚠ `MdSubscribe` must STAY OUT of the sweep — see the comment beside its omission in \
         `every_request`. Its pre-auth refusal, its mode switch and its hub-less refusal each have \
         a dedicated test on a FRESH connection"
    );
    // ...and it must carry NO compute verb: this daemon refuses those before the scope check, so
    // one slipping back into the set would make the auth tests below silently prove a refusal.
    assert!(
        every_request().iter().all(|r| vike_datahub_client::proto::plane_of(r)
            != vike_datahub_client::proto::Plane::Compute),
        "the sample set must stay DATA-plane: the compute verbs are served by `vike-backend \
         backtest --addr` and are refused here before authentication is consulted"
    );
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

/// ...and an OBSERVE connection is REFUSED every Control verb this daemon serves — the store WRITE
/// and the store REMOVAL. This is the property the whole scope split exists for: history without a
/// write, which was not expressible before.
///
/// ⚠ The name still says "the rhai compiling verbs" and that half is now the COMPUTE daemon's
/// (ruling 7). It is KEPT rather than renamed because a test name is what a failure report prints
/// and this one has been quoted in review; the assertion below is what moved, and
/// `crates/vike-backtest/tests/compute_plane.rs` is where an Observe connection meets a Rhai
/// compiler now.
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
                assert!(
                    msg.contains("Backfill store WRITE") && msg.contains("DeleteSeries"),
                    "...and WHICH Control verbs this daemon has: {msg}"
                );
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
            let _ = serve_authed(listener, served, None, Some(keys()), None);
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

// ---- 6. the MARKET-DATA mode switch ------------------------------------------------------------
//
// ⚠ Every test here uses a FRESH connection, deliberately: `MdSubscribe` ends the server's read
// loop on the socket it arrives on, so none of these may share one with a sweep. See the comment
// beside `MdSubscribe`'s omission from `every_request`.

/// A hub over a builder that opens nothing — enough to be MOUNTED, which is what the server keys
/// its advertisement and its mode switch on. No venue is served, so every spec is refused by name;
/// what these tests exercise is the CONNECTION contract, and `crates/vike-datahub/tests/md_hub.rs`
/// is where the hub's own behaviour is driven.
fn mounted_hub() -> Arc<MdHub> {
    MdHub::new(
        Box::new(|venue: &str, _sink: Arc<dyn vike_data::live::LiveDataSink>| {
            Err(format!("no venue client in this test build: {venue}"))
        }),
        vec!["binance".to_string()],
    )
}

/// **A MOUNTED hub advertises the plane and its venues; the mode switch then makes `MdSubscribed`
/// the LAST positional frame that socket carries.**
///
/// The assertion that matters is the second one: after the switch, a `Ping` gets NO `Pong` — the
/// server has left the read loop — and what does arrive inside the read deadline is a pushed
/// `Response::Md`. A server that kept reading would answer the `Ping`, and every "no correlation
/// id" claim on this wire would be false.
#[test]
fn md_subscribe_is_the_last_positional_frame_on_its_socket() {
    let addr = spawn_with_md(None, Some(mounted_hub()));
    let mut s = TcpStream::connect(addr).expect("connect");
    write_frame(&mut s, &Request::Hello { proto_version: PROTO_VERSION }).unwrap();
    let features = match read_frame::<_, Response>(&mut s).unwrap() {
        Response::Welcome { features, .. } => features,
        other => panic!("expected Welcome, got {other:?}"),
    };
    assert!(
        features.iter().any(|f| f == vike_datahub_client::proto::FEATURE_MARKET_DATA),
        "a MOUNTED hub advertises the plane: {features:?}"
    );
    assert_eq!(
        vike_datahub_client::proto::advertised_md_venues(&features),
        vec!["binance".to_string()],
        "...and names the venues this build links: {features:?}"
    );

    // THE SWITCH. One spec, refused (this build links no venue client), which is deliberate: an
    // all-refused subscribe still opens the stream, because `refused` is PER-SPEC and a whole-request
    // failure is a `Response::Error` instead.
    let spec = MdSpec {
        venue: "binance".into(),
        symbol: "BTCUSDT.P".into(),
        lane: MdLane::Depth,
        depth_levels: None,
    };
    write_frame(&mut s, &Request::MdSubscribe { specs: vec![spec] }).unwrap();
    let heartbeat_ms = match read_frame::<_, Response>(&mut s).unwrap() {
        Response::MdSubscribed { heartbeat_ms, accepted, .. } => {
            assert_eq!(accepted.len(), 1, "binance depth is servable by the caps matrix");
            heartbeat_ms
        }
        other => panic!("expected MdSubscribed, got {other:?}"),
    };
    assert!(
        heartbeat_ms > 0,
        "the server SENDS its heartbeat period so the client can deadline it"
    );

    // ⚠ THE POINT. A positional verb now gets NOTHING back positionally.
    //
    // ONE read, not a loop with a deadline: the attach already queued this key's `Status` frame, so
    // the very next frame on this socket is a pushed `Response::Md` and it is there IMMEDIATELY. A
    // server that had kept reading would have answered the `Ping` first, and `Pong` would arrive
    // here instead — which is what makes a single read the sharpest form of the assertion rather
    // than a weaker one. (A deadline loop would also have to outlive one heartbeat to mean
    // anything, and that is 15 s of wall clock for a property that resolves in microseconds.)
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    write_frame(&mut s, &Request::Ping).unwrap();
    match read_frame::<_, Response>(&mut s).expect("a subscribed socket keeps writing") {
        Response::Md(_) => {}
        Response::Pong => panic!(
            "the server answered a positional verb AFTER the mode switch — it is still reading \
             this socket, and the whole no-correlation-id contract rests on it not being"
        ),
        other => panic!("a subscribed socket carries only Response::Md, got {other:?}"),
    }
}

/// **A build with NO hub refuses `MdSubscribe` with `Response::Error`, and the connection SURVIVES
/// POSITIONALLY** — the DEFAULT-BUILD path, so this runs in the roster lane on every PR.
///
/// ⚠ This is the test that holds the deviation from §4.4: that section proposes an
/// `MdRefusal::HubNotMounted`, which — being PER-SPEC — could only be delivered inside an
/// all-refused `MdSubscribed`, and that would MODE-SWITCH a hub-less build into a heartbeat-only
/// writer that will never send a frame. Worse than the refusal it replaces, and it breaks leg (3)
/// of `FEATURE_MARKET_DATA`'s contract. The `Ping`/`Pong` below is the property that variant would
/// have cost.
#[test]
fn a_hubless_server_refuses_md_subscribe_and_stays_positional() {
    let addr = spawn(None);
    let mut s = TcpStream::connect(addr).expect("connect");
    match exchange(&mut s, &Request::MdSubscribe { specs: Vec::new() }) {
        Response::Error(msg) => {
            assert!(msg.contains("live-feeds"), "the refusal names the rebuild: {msg}");
            assert!(msg.contains("VIKE_DATAHUB_LIVE"), "...and the operator's arm: {msg}");
        }
        other => panic!("expected a clean Response::Error, got {other:?}"),
    }
    // ...and NOTHING switched: the same socket still answers positionally.
    assert!(matches!(exchange(&mut s, &Request::Ping), Response::Pong));
    // ...and it never advertised the plane, so a well-behaved client would not have sent one.
    let mut s2 = TcpStream::connect(addr).expect("connect");
    match exchange(&mut s2, &Request::Hello { proto_version: PROTO_VERSION }) {
        Response::Welcome { features, .. } => assert!(
            !features.iter().any(|f| f == vike_datahub_client::proto::FEATURE_MARKET_DATA),
            "{features:?}"
        ),
        other => panic!("expected Welcome, got {other:?}"),
    }
}

/// **THE HIGHEST-STAKES SINGLE TEST IN THIS CHANGE: a KEYED server refuses `MdSubscribe` BEFORE
/// authentication, and closes.**
///
/// `MdSubscribe` is the only verb on this wire that converts a connection into an unbounded writer
/// AND takes a venue refcount. If the `handle_connection` arm ever lands BEFORE the scope check
/// rather than after it, an unauthenticated peer on a keyed server gets a market-data firehose —
/// and every other test in this file still passes, because the sweep that would have caught it
/// cannot carry this verb.
#[test]
fn a_keyed_server_refuses_md_subscribe_before_auth() {
    let addr = spawn_with_md(Some(keys()), Some(mounted_hub()));
    let mut s = TcpStream::connect(addr).expect("connect");
    match exchange(&mut s, &Request::MdSubscribe { specs: Vec::new() }) {
        Response::AuthDenied { reason } => {
            assert!(reason.contains("not authenticated"), "{reason}");
        }
        other => panic!("an unauthenticated MdSubscribe must be DENIED, got {other:?}"),
    }
    // ...and the socket is closed, not switched: a further frame gets nothing back.
    s.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    let _ = write_frame(&mut s, &Request::Ping);
    assert!(
        read_frame::<_, Response>(&mut s).is_err(),
        "a refused pre-auth verb closes the connection"
    );
}

/// **A FOREIGN or expired `MdSessionId` is refused and mutates nothing.**
///
/// Without this, a session id would be a bearer token on a shared authenticated channel and one
/// desktop could unsubscribe another's keys — landing as a silently frozen ladder on a machine
/// whose operator did nothing. The `u128` from the OS generator means guessing is not the threat; a
/// buggy client reusing a stale id after a reconnect is.
#[test]
fn a_foreign_market_data_session_is_refused() {
    let addr = spawn_with_md(None, Some(mounted_hub()));
    let mut s = TcpStream::connect(addr).expect("connect");
    let stranger = MdSessionId::fresh();
    match exchange(
        &mut s,
        &Request::MdUpdate { session: stranger, add: Vec::new(), remove: Vec::new() },
    ) {
        Response::Error(msg) => {
            assert!(msg.contains("no such session"), "{msg}");
            assert!(msg.contains("nothing was changed"), "...and says so: {msg}");
        }
        other => panic!("an unknown session must be a whole-request Error, got {other:?}"),
    }
    // The connection survives — it is a bad *request*, not a bad *connection*.
    assert!(matches!(exchange(&mut s, &Request::Ping), Response::Pong));
}

/// **THE STREAM-CONNECTION CAP IS A REFUSAL, NOT A DROPPED SOCKET** — the connection past
/// `MD_MAX_STREAM_CONNS` gets `Response::Error` and stays POSITIONAL.
///
/// ⚠ This was the one `MdSubscribe` refusal path with no test, and it was the one that got it wrong:
/// `run_market_writer` called `hub.open_session()` itself, wrote the error and then DROPPED the
/// stream. Every doc on this path states the opposite invariant —
/// `vike_datahub_client::market`'s module doc ("A server that answers `Response::Error` … has NOT
/// switched: the connection is still positional and the client must not start a reader thread") and
/// `DatahubClient::md_subscribe`, which promises the caller a client "returned intact, so the caller
/// can keep using it positionally". On this path the returned client sat on a closed socket.
///
/// The `Ping`/`Pong` at the end is the same property
/// `a_hubless_server_refuses_md_subscribe_and_stays_positional` asserts for the OTHER whole-request
/// refusal, and the two must not answer differently.
#[test]
fn the_stream_connection_cap_refuses_and_stays_positional() {
    use vike_datahub::md::MD_MAX_STREAM_CONNS;

    let addr = spawn_with_md(None, Some(mounted_hub()));
    // Fill the stream budget. Each of these mode-switches its own socket and is HELD open for the
    // rest of the test — dropping one would free the slot this test is trying to exhaust.
    let mut held = Vec::new();
    for i in 0..MD_MAX_STREAM_CONNS {
        let mut s = TcpStream::connect(addr).expect("connect");
        write_frame(&mut s, &Request::MdSubscribe { specs: Vec::new() }).unwrap();
        match read_frame::<_, Response>(&mut s) {
            Ok(Response::MdSubscribed { .. }) => {}
            other => panic!("stream {i} of the budget did not open: {other:?}"),
        }
        held.push(s);
    }
    assert_eq!(held.len(), MD_MAX_STREAM_CONNS, "floor: the budget was actually spent");

    // The one over.
    let mut s = TcpStream::connect(addr).expect("connect");
    write_frame(&mut s, &Request::MdSubscribe { specs: Vec::new() }).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    match read_frame::<_, Response>(&mut s).expect("a refusal, not a dropped socket") {
        Response::Error(msg) => {
            assert!(
                msg.contains(&MD_MAX_STREAM_CONNS.to_string()),
                "the refusal names its own number: {msg}"
            );
            assert!(
                msg.contains("nothing was subscribed"),
                "...and says it changed nothing: {msg}"
            );
        }
        other => panic!("expected a clean Response::Error, got {other:?}"),
    }
    // ⚠ THE POINT: NOTHING switched, so the same socket still answers positionally. A writer that
    // had taken this socket would answer a pushed `Response::Md` here, or nothing at all.
    assert!(
        matches!(exchange(&mut s, &Request::Ping), Response::Pong),
        "a cap refusal must leave the connection positional, exactly as the hub-less one does"
    );
    drop(held);
}
