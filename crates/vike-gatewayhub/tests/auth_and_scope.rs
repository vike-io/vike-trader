//! The handshake and the scopes, on the wire: the always-keyed handshake in the gatewayhub's own
//! domain, the COARSE denial, the scope table enforced per verb by the server (not only by the client's
//! courtesy check), decode-versus-drop, the frame caps and the connection cap.

#[path = "support/fake_lanes.rs"]
#[allow(dead_code, clippy::allow_attributes)]
mod fake_lanes;
#[path = "support/harness.rs"]
#[allow(dead_code, clippy::allow_attributes)]
mod harness;
#[path = "support/raw_peer.rs"]
#[allow(dead_code, clippy::allow_attributes)]
mod raw_peer;

use std::time::Duration;

use vike_gatewayhub::server::limits::ServerConfig;
use vike_gatewayhub_client::client::{GatewayClient, GatewayLink};
use vike_gatewayhub_client::proto::{
    FetchWhat, GATEWAYHUB_DOMAIN, GATEWAYHUB_PLANE_SENTINEL, GATEWAYHUB_PROTO_VERSION,
    GW_UP_FRAME_CEILING_BYTES, Request, Response,
};
use vike_hub_proto::auth::{self, HubKeys, Scope};

use fake_lanes::{FakeData, FakeRest};
use harness::{Harness, full_facts, read_only_keys, test_keys};
use raw_peer::Raw;

fn credentialed_klines() -> Request {
    Request::FetchCredentialed {
        rid: 1,
        what: FetchWhat::Klines {
            venue: "oanda".into(),
            symbol: "EUR_USD".into(),
            interval: "S5".into(),
            start_ms: 0,
            end_ms: 1,
        },
    }
}

#[test]
fn the_welcome_names_the_plane_before_anything_is_signed() {
    let h = Harness::start();
    let mut raw = Raw::connect(h.addr);
    let (version, features) = raw.hello();
    assert_eq!(version, GATEWAYHUB_PROTO_VERSION);
    assert_eq!(features.first().map(String::as_str), Some(GATEWAYHUB_PLANE_SENTINEL));
}

#[test]
fn a_wrong_mac_is_the_coarse_denial_and_the_connection_closes() {
    let h = Harness::start();
    let mut raw = Raw::connect(h.addr);
    raw.hello();
    match raw.authenticate(b"not-the-key", Scope::Read) {
        Response::AuthDenied { reason } => assert_eq!(reason, "bad mac"),
        other => panic!("{other:?}"),
    }
    assert!(raw.closed_within(Duration::from_secs(3)));
}

#[test]
fn a_scope_with_no_key_is_the_same_coarse_denial_not_a_hint() {
    // A daemon holding only the observe key; the peer asks for control signing with anything.
    let h = Harness::start_with(
        FakeData::new(&["binance"]),
        FakeRest::new(full_facts()),
        ServerConfig::PRODUCTION,
        read_only_keys(),
    );
    let mut raw = Raw::connect(h.addr);
    raw.hello();
    let wrong_key = raw.authenticate(b"control-key-of-the-tests", Scope::Write);
    assert!(
        matches!(&wrong_key, Response::AuthDenied { reason } if reason == "bad mac"),
        "{wrong_key:?}"
    );
    // And it is word for word what a wrong key for a scope the daemon DOES hold gets.
    let mut other = Raw::connect(h.addr);
    other.hello();
    assert!(matches!(
        other.authenticate(b"wrong", Scope::Read),
        Response::AuthDenied { reason } if reason == "bad mac"
    ));
}

#[test]
fn the_account_scope_is_never_served_even_with_an_admin_key_configured() {
    let keys = HubKeys::new(b"obs".to_vec(), b"ctl".to_vec()).with_admin(b"admin-key".to_vec());
    let h = Harness::start_with(
        FakeData::new(&["binance"]),
        FakeRest::new(full_facts()),
        ServerConfig::PRODUCTION,
        keys,
    );
    let mut raw = Raw::connect(h.addr);
    raw.hello();
    assert!(matches!(
        raw.authenticate(b"admin-key", Scope::Account),
        Response::AuthDenied { reason } if reason == "bad mac"
    ));
}

#[test]
fn a_mac_signed_under_another_protocol_version_is_denied() {
    let h = Harness::start();
    let mut raw = Raw::connect(h.addr);
    // The daemon still answers with ITS version (the client names both numbers); the mac binds the
    // version the peer signed, so it fails.
    let (version, _) = raw.hello_with_version(99);
    assert_eq!(version, GATEWAYHUB_PROTO_VERSION);
    let key = test_keys().key_for(Scope::Read).to_vec();
    assert!(matches!(
        raw.authenticate_versioned(&key, Scope::Read, 99),
        Response::AuthDenied { .. }
    ));
}

#[test]
fn a_frame_before_the_handshake_is_refused_and_the_connection_closes() {
    let h = Harness::start();
    let mut raw = Raw::connect(h.addr);
    raw.send(&Request::Ping);
    match raw.recv::<Response>() {
        Response::AuthDenied { reason } => {
            assert!(reason.contains("expected Hello first"), "{reason}")
        }
        other => panic!("{other:?}"),
    }
    assert!(raw.closed_within(Duration::from_secs(3)));
}

#[test]
fn a_read_key_cannot_open_a_credentialed_verb_and_the_refusal_names_both_scopes() {
    let h = Harness::start();
    let mut raw = Raw::open(h.addr, &test_keys(), Scope::Read);
    raw.send(&credentialed_klines());
    match raw.recv::<Response>() {
        Response::Error(text) => {
            assert!(
                text.contains("FetchCredentialed")
                    && text.contains("control")
                    && text.contains("observe"),
                "{text}"
            );
        }
        other => panic!("{other:?}"),
    }
    assert!(
        h.rest.fetches.lock().unwrap_or_else(std::sync::PoisonError::into_inner).is_empty(),
        "the lane was reached by an over-scoped verb"
    );
    // The connection survived the refusal, and its own scope still works.
    raw.send(&Request::Ping);
    assert!(matches!(raw.recv::<Response>(), Response::Pong));
}

#[test]
fn a_write_key_may_open_the_credentialed_verb() {
    let h = Harness::start();
    h.rest.set_fetch(|_, _, credentialed, _| {
        assert!(credentialed);
        Ok(vike_gatewayhub_client::proto::FetchRows::Bars(Vec::new()))
    });
    let mut raw = Raw::open(h.addr, &test_keys(), Scope::Write);
    raw.send(&credentialed_klines());
    loop {
        match raw.recv::<Response>() {
            Response::FetchProgress(_) => {}
            Response::FetchDone { .. } => break,
            other => panic!("{other:?}"),
        }
    }
}

#[test]
fn the_real_client_with_a_wrong_key_is_denied_with_the_daemons_words() {
    let h = Harness::start();
    let link =
        GatewayLink::new(h.addr_string(), HubKeys::new(b"wrong".to_vec(), b"wrong".to_vec()));
    let err = GatewayClient::connect(&link, Scope::Read).expect_err("a wrong key");
    assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);
    assert!(
        err.to_string().contains("auth denied") && err.to_string().contains("bad mac"),
        "{err}"
    );
    // A link with only the observe key cannot even dial for control (nothing is sent).
    let read_only = h.read_link();
    let err = GatewayClient::connect(&read_only, Scope::Write).expect_err("no control key");
    assert!(err.to_string().contains("nothing was dialled"), "{err}");
}

#[test]
fn a_second_handshake_frame_on_an_established_connection_is_refused_not_obeyed() {
    let h = Harness::start();
    let mut raw = Raw::open(h.addr, &test_keys(), Scope::Read);
    raw.send(&Request::Hello { proto_version: 1 });
    match raw.recv::<Response>() {
        Response::Error(text) => assert!(text.contains("already authenticated"), "{text}"),
        other => panic!("{other:?}"),
    }
    raw.send(&Request::Auth { scope: Scope::Write, mac: vec![0; 32] });
    assert!(matches!(raw.recv::<Response>(), Response::Error(_)));
    // A scope that can be renegotiated is not a ceiling: Write verbs are still refused.
    raw.send(&credentialed_klines());
    assert!(matches!(raw.recv::<Response>(), Response::Error(t) if t.contains("control")));
}

#[test]
fn an_undecodable_request_is_a_bad_request_not_a_bad_connection() {
    let h = Harness::start();
    let mut raw = Raw::open(h.addr, &test_keys(), Scope::Read);
    raw.send(&"AVerbFromTheFuture");
    match raw.recv::<Response>() {
        Response::Error(text) => assert!(text.contains("undecodable"), "{text}"),
        other => panic!("{other:?}"),
    }
    raw.send(&Request::Ping);
    assert!(matches!(raw.recv::<Response>(), Response::Pong));
}

#[test]
fn a_frame_over_the_client_ceiling_is_a_framing_fault_that_closes_the_connection() {
    let h = Harness::start();
    let mut raw = Raw::open(h.addr, &test_keys(), Scope::Read);
    // Only the length prefix: the daemon refuses the DECLARED length before it allocates anything.
    raw.send_bytes(&(GW_UP_FRAME_CEILING_BYTES + 1).to_be_bytes());
    assert!(raw.closed_within(Duration::from_secs(3)));
}

#[test]
fn a_pre_auth_frame_over_the_handshake_cap_closes_the_connection() {
    let h = Harness::start();
    let mut raw = Raw::connect(h.addr);
    raw.send_bytes(&(1u32 << 20).to_be_bytes());
    assert!(raw.closed_within(Duration::from_secs(3)));
}

#[test]
fn a_silent_peer_is_dropped_when_the_handshake_deadline_passes() {
    let mut cfg = ServerConfig::PRODUCTION;
    cfg.timings.handshake_deadline = Duration::from_millis(250);
    let h = Harness::start_config(cfg);
    let mut raw = Raw::connect(h.addr);
    assert!(raw.closed_within(Duration::from_secs(3)), "a peer that said nothing held a thread");
}

#[test]
fn an_idle_rpc_connection_is_reaped_but_a_busy_one_is_not() {
    let mut cfg = ServerConfig::PRODUCTION;
    cfg.timings.idle_read_timeout = Duration::from_millis(700);
    let h = Harness::start_config(cfg);
    let mut idle = Raw::open(h.addr, &test_keys(), Scope::Read);
    let mut busy = Raw::open(h.addr, &test_keys(), Scope::Read);
    for _ in 0..8 {
        busy.send(&Request::Ping);
        assert!(matches!(busy.recv::<Response>(), Response::Pong));
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(idle.closed_within(Duration::from_secs(4)), "an idle connection pinned a thread");
}

#[test]
fn past_the_connection_cap_a_connection_is_dropped_without_a_reply() {
    let mut cfg = ServerConfig::PRODUCTION;
    cfg.limits.max_connections = 2;
    let h = Harness::start_config(cfg);
    let _a = Raw::open(h.addr, &test_keys(), Scope::Read);
    let _b = Raw::open(h.addr, &test_keys(), Scope::Read);
    // The kernel accepts the third, the daemon drops it before a byte: not even a Welcome.
    let mut third = Raw::connect(h.addr);
    assert!(third.closed_within(Duration::from_secs(3)), "the third connection was served");
    // A slot freed by a closed connection is reusable.
    drop(_a);
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let mut again = Raw::connect(h.addr);
        again.send(&Request::Hello { proto_version: GATEWAYHUB_PROTO_VERSION });
        if again.recv_within::<Response>(Duration::from_secs(2)).is_ok() {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "a closed connection's slot was never released"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// **The handshake deadline is for the PHASE, not for each read.** The deadline used to be armed once
/// as the socket's read timeout, which bounds a silent peer but not a slow one: a peer that sends a
/// length prefix and then one byte just inside every timeout holds its thread and one of the
/// connection slots for as many timeouts as it has bytes to send. Here the deadline is 600 ms and the
/// peer sends a byte every 100 ms of a body that would take 100 s; it must be cut off at about 600 ms.
#[test]
fn a_peer_that_trickles_bytes_inside_every_read_timeout_is_cut_off_at_the_phase_deadline() {
    use std::io::Write;

    let mut cfg = ServerConfig::PRODUCTION;
    cfg.timings.handshake_deadline = Duration::from_millis(600);
    let h = Harness::start_config(cfg);
    let mut raw = Raw::connect(h.addr);
    let mut trickle = raw.stream.try_clone().expect("clone the socket");
    let began = std::time::Instant::now();
    let sender = std::thread::Builder::new()
        .name("test-trickle".into())
        .spawn(move || {
            // A legal length (under the pre-auth cap), then the body a byte at a time.
            if trickle.write_all(&1_000_u32.to_be_bytes()).is_err() {
                return;
            }
            for _ in 0..1_000 {
                if trickle.write_all(b"x").is_err() || trickle.flush().is_err() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        })
        .expect("spawn");
    assert!(
        raw.closed_within(Duration::from_millis(2_500)),
        "a peer trickling a byte per read timeout held the handshake open for {:?}",
        began.elapsed()
    );
    assert!(began.elapsed() < Duration::from_millis(2_500));
    sender.join().expect("the trickler ends once the daemon closes");
}

// ---- Freshness, replay and the domain, against the running server --------------------------------
//
// `vike_hub_proto::auth` proves these properties of `sign`/`verify` in isolation. What those tests
// cannot see is the SERVER: a `run_handshake` that issued a constant nonce, or verified under a shared
// domain, would pass every suite above and every one of `proto_tests.rs`. These drive the real
// listener with a raw peer, which is the only way to see the nonce it issues.

fn key_of(scope: Scope) -> Vec<u8> {
    test_keys().key_for(scope).to_vec()
}

#[test]
fn every_connection_is_challenged_with_a_fresh_nonce() {
    let h = Harness::start();
    let mut seen = std::collections::HashSet::new();
    for n in 0..24 {
        let mut raw = Raw::connect(h.addr);
        raw.hello();
        assert_ne!(raw.nonce(), [0u8; 32], "connection {n} was handed an all-zero challenge");
        assert!(seen.insert(raw.nonce()), "connection {n} was handed a nonce another one had");
    }
}

/// **A mac captured off one connection is worth nothing on the next.** The legitimate peer's
/// `(nonce, mac)` is exactly what a loopback observer sees; the replay answers a DIFFERENT challenge
/// and must meet the same coarse `bad mac` a wrong key meets.
#[test]
fn a_mac_captured_from_one_connection_is_refused_on_another_with_the_coarse_denial() {
    let h = Harness::start();
    for scope in [Scope::Read, Scope::Write] {
        let mut first = Raw::connect(h.addr);
        first.hello();
        let captured = auth::sign(
            GATEWAYHUB_DOMAIN,
            &key_of(scope),
            &first.nonce(),
            GATEWAYHUB_PROTO_VERSION,
            scope,
        );
        // The control: the same bytes ARE valid on the connection whose challenge they answer.
        assert!(
            matches!(first.authenticate_with_mac(scope, captured.clone()), Response::AuthOk { .. }),
            "{scope:?}: the captured mac is the legitimate peer's own"
        );

        let mut replay = Raw::connect(h.addr);
        replay.hello();
        assert_ne!(first.nonce(), replay.nonce(), "{scope:?}: the replay met the same challenge");
        match replay.authenticate_with_mac(scope, captured) {
            Response::AuthDenied { reason } => assert_eq!(reason, "bad mac", "{scope:?}"),
            other => panic!("{scope:?}: a replayed mac was accepted: {other:?}"),
        }
        assert!(replay.closed_within(Duration::from_secs(3)), "{scope:?}: the replay stayed open");
    }
}

/// **A mac signed under another service's domain is refused by this one, key bytes and all.** The
/// operator who reuses one key across two services (the mistake the domain separator exists for) must
/// find a datahub mac useless here.
#[test]
fn a_mac_signed_under_another_domain_with_the_same_key_bytes_is_refused() {
    use vike_hub_proto::auth::{DATAHUB_DOMAIN, Domain, KEY_ID_DOMAIN};

    let h = Harness::start();
    // The tradehub's separator is spelled out (this crate takes no edge to that client), and the
    // gatewayhub's own WITHOUT its NUL: a domain that is a prefix of ours must not verify either.
    let others: [(&str, Domain); 4] = [
        ("the datahub's", DATAHUB_DOMAIN),
        ("the tradehub's", Domain::new(b"vike-tradehub-auth\0")),
        ("the key-id scheme's", KEY_ID_DOMAIN),
        ("the gatewayhub's own, minus its terminator", Domain::new(b"vike-gatewayhub-auth")),
    ];
    for scope in [Scope::Read, Scope::Write] {
        let key = key_of(scope);
        // The control: the gatewayhub's own domain, the same key, nonce and version, is accepted.
        let mut ok = Raw::connect(h.addr);
        ok.hello();
        let mac = auth::sign(GATEWAYHUB_DOMAIN, &key, &ok.nonce(), GATEWAYHUB_PROTO_VERSION, scope);
        assert!(matches!(ok.authenticate_with_mac(scope, mac), Response::AuthOk { .. }));

        for (whose, domain) in others {
            let mut raw = Raw::connect(h.addr);
            raw.hello();
            let mac = auth::sign(domain, &key, &raw.nonce(), GATEWAYHUB_PROTO_VERSION, scope);
            match raw.authenticate_with_mac(scope, mac) {
                Response::AuthDenied { reason } => {
                    assert_eq!(reason, "bad mac", "{scope:?}: {whose} domain");
                }
                other => panic!("{scope:?}: a mac under {whose} domain was accepted: {other:?}"),
            }
        }
    }
}

// ---- A frame that has begun is bounded by the FRAME timeout, not the idle one --------------------

/// **A frame that has begun must finish within the frame timeout.** The idle timeout is for the GAP
/// between frames (300 s in production, so a quiet-but-alive client is kept); the rest of a frame whose
/// first byte has arrived is `frame_read_timeout`'s (10 s). Here the idle bound is a minute and the frame
/// bound a third of a second: an authenticated peer that stalls at any point inside a frame is cut at
/// the second one, where reading the whole frame under the idle timeout would hold its thread for the
/// minute.
#[test]
fn a_peer_that_stalls_inside_a_frame_is_cut_at_the_frame_timeout_not_the_idle_timeout() {
    let mut cfg = ServerConfig::PRODUCTION;
    cfg.timings.idle_read_timeout = Duration::from_secs(60);
    cfg.timings.frame_read_timeout = Duration::from_millis(300);
    let h = Harness::start_config(cfg);

    // A byte of the length prefix; the whole prefix; the prefix and three bytes of a body it promises.
    let stalls: [(&str, &[u8]); 3] = [
        ("one byte of the length prefix", &[0]),
        ("the whole length prefix", &100u32.to_be_bytes()),
        ("three bytes of a promised body", &[0, 0, 0, 100, b'"', b'P', b'i']),
    ];
    for (where_, bytes) in stalls {
        let mut raw = Raw::open(h.addr, &test_keys(), Scope::Read);
        raw.send_bytes(bytes);
        let began = std::time::Instant::now();
        assert!(
            raw.closed_within(Duration::from_secs(5)),
            "a peer that stalled after {where_} held its thread for {:?} (the idle bound is a minute)",
            began.elapsed()
        );
    }
}

/// The other half of the same rule: the frame bound does NOT shorten the gap between frames, and a
/// frame that arrives in pieces inside the frame bound is served.
#[test]
fn the_gap_between_frames_keeps_the_idle_timeout_and_a_slow_frame_inside_the_bound_is_served() {
    let mut cfg = ServerConfig::PRODUCTION;
    cfg.timings.idle_read_timeout = Duration::from_secs(30);
    cfg.timings.frame_read_timeout = Duration::from_millis(500);
    let h = Harness::start_config(cfg);
    let mut raw = Raw::open(h.addr, &test_keys(), Scope::Read);

    // A gap longer than the frame bound, far under the idle one: the connection is kept.
    std::thread::sleep(Duration::from_millis(900));
    raw.send(&Request::Ping);
    assert!(
        matches!(raw.recv::<Response>(), Response::Pong),
        "an idle gap was cut at the frame bound"
    );

    // A frame sent in two pieces with a pause inside the frame bound: served whole.
    let body = serde_json::to_vec(&Request::Ping).expect("a request encodes");
    let mut frame = u32::try_from(body.len()).expect("small").to_be_bytes().to_vec();
    frame.extend_from_slice(&body);
    let (head, tail) = frame.split_at(3);
    raw.send_bytes(head);
    std::thread::sleep(Duration::from_millis(150));
    raw.send_bytes(tail);
    assert!(
        matches!(raw.recv::<Response>(), Response::Pong),
        "a slow frame inside the bound was cut"
    );
}

// ---- A window the caller names is Write-scope ----------------------------------------------------

/// The three fetches that walk a window the caller names. Every venue is served, so the only thing that
/// can refuse them is the scope.
fn window_fetches() -> Vec<(&'static str, Request)> {
    vec![
        ("klines", unbounded_klines("binance", "BTCUSDT.P", "1m")),
        (
            "funding",
            Request::Fetch {
                rid: 2,
                what: FetchWhat::Funding {
                    venue: "binance".into(),
                    symbol: "BTCUSDT.P".into(),
                    start_ms: 0,
                    end_ms: i64::MAX,
                },
            },
        ),
        (
            "ticks",
            Request::Fetch {
                rid: 3,
                what: FetchWhat::Ticks {
                    venue: "dukascopy".into(),
                    symbol: "EURUSD".into(),
                    start_ms: 0,
                    end_ms: 1,
                },
            },
        ),
    ]
}

/// A keyless klines request over `0..i64::MAX`: the whole of a venue's history.
fn unbounded_klines(venue: &str, symbol: &str, interval: &str) -> Request {
    Request::Fetch {
        rid: 1,
        what: FetchWhat::Klines {
            venue: venue.into(),
            symbol: symbol.into(),
            interval: interval.into(),
            start_ms: 0,
            end_ms: i64::MAX,
        },
    }
}

/// **A Read key cannot make the daemon walk a venue's history through a window it names.** The
/// datahub's `Backfill` was a Write verb; the same work reaches the venue from this box's address, so
/// it is Write here too. Over a raw Read connection the window lanes are refused with both scopes
/// named and the lane is never asked; the connection survives, and the catalog lane (the venue's own
/// universe) is still served at Read.
#[test]
fn a_read_key_cannot_walk_a_venues_history_through_a_window_it_names() {
    let h = Harness::start();
    h.rest.set_fetch(|_, _, _, _| {
        Ok(vike_gatewayhub_client::proto::FetchRows::Instruments(Vec::new()))
    });
    let mut raw = Raw::open(h.addr, &test_keys(), Scope::Read);
    for (lane, request) in window_fetches() {
        raw.send(&request);
        match raw.recv::<Response>() {
            Response::Error(text) => assert!(
                text.contains("Fetch") && text.contains("control") && text.contains("observe"),
                "{lane}: {text}"
            ),
            other => panic!("{lane}: a Read connection was served a window fetch: {other:?}"),
        }
    }
    assert!(
        h.rest.fetches.lock().unwrap_or_else(std::sync::PoisonError::into_inner).is_empty(),
        "the lane was reached by an over-scoped window fetch"
    );

    // The connection survived the refusals, and the cheap catalog lane IS admitted at Read.
    raw.send(&Request::Fetch { rid: 9, what: FetchWhat::CatalogList { venue: "binance".into() } });
    loop {
        match raw.recv::<Response>() {
            Response::FetchProgress(_) => {}
            Response::FetchDone { rid: 9, .. } => break,
            other => panic!("a catalog listing on a Read connection: {other:?}"),
        }
    }
    assert_eq!(
        *h.rest.fetches.lock().unwrap_or_else(std::sync::PoisonError::into_inner),
        ["catalog_list"]
    );
}

/// The control: the same bytes on a Write connection reach the lane, lane by lane.
#[test]
fn a_write_key_is_served_the_same_window_fetches() {
    use vike_gatewayhub_client::proto::FetchRows;

    let h = Harness::start();
    h.rest.set_fetch(|_, what, _, _| {
        Ok(match what {
            FetchWhat::Funding { .. } => FetchRows::Funding(Vec::new()),
            FetchWhat::Ticks { .. } => FetchRows::Quotes(Vec::new()),
            _ => FetchRows::Bars(Vec::new()),
        })
    });
    let mut raw = Raw::open(h.addr, &test_keys(), Scope::Write);
    for (lane, request) in window_fetches() {
        raw.send(&request);
        loop {
            match raw.recv::<Response>() {
                Response::FetchProgress(_) => {}
                Response::FetchDone { .. } => break,
                other => panic!("{lane}: a Write connection was refused a window fetch: {other:?}"),
            }
        }
    }
    assert_eq!(
        *h.rest.fetches.lock().unwrap_or_else(std::sync::PoisonError::into_inner),
        ["klines", "funding", "ticks"]
    );
}
