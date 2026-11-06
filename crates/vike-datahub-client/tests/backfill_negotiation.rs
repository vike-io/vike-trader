//! The backfill-on-demand verb's CAPABILITY NEGOTIATION (split-plane REQ-9), driven end-to-end.
//!
//! The verb is negotiated by `Welcome.features`, never by a `PROTO_VERSION` bump
//! (`docs/decisions/0112-an-additive-verb-is-negotiated-by-a-feature-string-never-by-a-version-bump.md`):
//! a server advertises [`FEATURE_BACKFILL`] only when it actually holds collectors, and the CLIENT
//! refuses locally — without sending — when the advertisement is absent. These tests pin BOTH
//! halves of that contract plus the decode-side safety net underneath it (an unknown-variant frame
//! is answered with a clean `Response::Error`, never a hang).
//!
//! Loopback + in-memory only — no store on disk, no network.

use std::net::TcpStream;
use std::sync::Arc;

use vike_data::MemHistStore;
use vike_datahub_client::{
    DatahubClient, FEATURE_BACKFILL,
    proto::{Request, Response, read_frame, write_frame},
};

// `spawn_server` is the REAL `serve` WITHOUT a backfill table — the exact shape of every
// pre-verb deployment AND of a new build that lacks `backfill-serve`.
use vike_datahub::test_support::spawn_server;
use vike_datahub_client::test_support::{
    assert_an_unknown_tag_is_refused_and_the_link_survives, spawn_fake_peer,
};

/// A `Request::Backfill` with placeholder coordinates — the negotiation tests never expect it to
/// reach a collector.
fn backfill_request() -> Request {
    Request::Backfill {
        venue: "binance".to_string(),
        symbol: "BTCUSDT".to_string(),
        interval: "1h".to_string(),
        start: 0,
        end: 3_600_000,
    }
}

/// A server whose `Welcome` does not advertise [`FEATURE_BACKFILL`] is refused CLIENT-SIDE, and
/// NOTHING is sent: the fake peer records no frame after the `Hello`.
#[test]
fn a_feature_less_welcome_is_refused_client_side_without_sending() {
    // A matching version but NO backfill advertisement — an old server, or a new lean one.
    let (addr, rx, handle) =
        spawn_fake_peer(vec!["backtest".to_string(), "load_bars".to_string()], |_| None);

    let mut client = DatahubClient::connect(addr).expect("handshake succeeds — same version");
    let err = client
        .backfill("binance", "BTCUSDT", "1h", 0, 3_600_000)
        .expect_err("an unadvertised verb must be refused client-side");
    assert!(
        err.contains(FEATURE_BACKFILL),
        "the refusal names the missing capability string: {err}"
    );
    drop(client); // EOF ends the fake peer's loop

    let received = rx.recv().expect("fake server reports what arrived");
    assert!(received.is_empty(), "the client SENT {received:?} after the refused negotiation");
    handle.join().expect("fake server thread joins cleanly");
}

/// The real `serve` WITHOUT a backfill table does not advertise [`FEATURE_BACKFILL`] — the
/// server half of the negotiation contract (the composed test proves the with-table half).
#[test]
fn a_tableless_server_does_not_advertise_backfill() {
    let addr = spawn_server(Arc::new(MemHistStore::new()));
    let client = DatahubClient::connect(addr).expect("handshake on connect");
    assert!(
        !client.features().iter().any(|f| f == FEATURE_BACKFILL),
        "a tableless server must not advertise `{FEATURE_BACKFILL}`: {:?}",
        client.features()
    );
}

/// A `Request::Backfill` that reaches a server with NO collector table (a client that skipped the
/// feature check — a raw frame, a hand-rolled client) gets a clean `Response::Error` NAMING the
/// missing build feature, and the connection SURVIVES for the next request.
#[test]
fn a_tableless_server_answers_backfill_with_a_clean_refusal_naming_the_feature() {
    let addr = spawn_server(Arc::new(MemHistStore::new()));
    let mut stream = TcpStream::connect(addr).expect("connect");

    // Bypass `DatahubClient`'s client-side check on purpose: write the typed frame raw.
    write_frame(&mut stream, &backfill_request()).expect("send Backfill");
    match read_frame::<_, Response>(&mut stream).expect("server answers, never hangs") {
        Response::Error(msg) => {
            assert!(
                msg.contains("backfill-serve"),
                "the refusal names the missing feature (the recorder idiom): {msg}"
            );
        }
        other => panic!("a tableless server must answer Error, got {other:?}"),
    }

    // The refusal was a bad REQUEST, not a bad CONNECTION.
    write_frame(&mut stream, &Request::Ping).expect("send Ping on the surviving connection");
    match read_frame::<_, Response>(&mut stream).expect("read Pong") {
        Response::Pong => {}
        other => panic!("expected Pong after the refusal, got {other:?}"),
    }
}

/// The old-server simulation: a struct-variant frame whose TAG the server does not know — exactly
/// what THIS client's `Backfill` frame looks like to a server built before the verb existed —
/// comes back as a clean `Response::Error` (the framing/decode split), never a dropped connection
/// and never a hang. The mechanism is variant-name dispatch, so an unknown FUTURE tag here
/// exercises precisely the path our new tag takes on an old build.
#[test]
fn an_unknown_struct_variant_frame_gets_a_clean_error_not_a_hang() {
    // Well-formed JSON, externally-tagged struct-variant shape, unknown tag.
    assert_an_unknown_tag_is_refused_and_the_link_survives(
        spawn_server(Arc::new(MemHistStore::new())),
        br#"{"BackfillVNext":{"venue":"binance","job":true}}"#,
    );
}
