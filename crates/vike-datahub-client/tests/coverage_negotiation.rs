//! The cross-kind coverage verb's CAPABILITY NEGOTIATION (split-plane spec §6-Q2), driven
//! end-to-end — the mirror image of `backfill_negotiation.rs`. `Request::Coverage` is negotiated by
//! its `Welcome.features` string, never by a `PROTO_VERSION` bump
//! (`docs/decisions/0112-an-additive-verb-is-negotiated-by-a-feature-string-never-by-a-version-bump.md`).
//!
//! It differs from backfill in exactly one way, and that difference is what these tests pin: there
//! is no table to mount. Coverage is a plain `vike_data::HistStore` trait verb, so EVERY server
//! that serves at all advertises it unconditionally — a missing advertisement means only *this
//! peer predates the verb*, which the GUI can turn into an honest note instead of a blank column.
//!
//! Loopback + in-memory only — no store on disk.

// `common::spawn_server` over a `MemHistStore`, which inherits the trait's empty `coverage_report`
// default: these tests are about the NEGOTIATION, not the report (the real DataFusion answer
// crosses the wire in `crates/vike-datahub/tests/composed_store_roundtrip.rs`).
mod common;
#[path = "support/fake_peer.rs"]
mod fake_peer;

use std::net::TcpStream;
use std::sync::Arc;

use vike_data::{HistStore, MemHistStore};
use vike_datahub_client::{
    DatahubClient, FEATURE_COVERAGE, RemoteHistStore,
    proto::{Request, Response, read_frame, write_frame},
};

use common::spawn_server;
use fake_peer::spawn_fake_peer;

/// The OLD-SERVER case, which is the only case this negotiation exists for: a matching protocol
/// version (so the handshake succeeds and gives no warning) but no `coverage` advertisement. The
/// client refuses locally and sends NOTHING — the fake peer records no frame after the `Hello`.
#[test]
fn a_welcome_without_the_capability_is_refused_client_side_without_sending() {
    let (addr, rx, handle) =
        spawn_fake_peer(vec!["backtest".to_string(), "inventory".to_string()], |_| None);

    let mut client = DatahubClient::connect(addr).expect("handshake succeeds — same version");
    let err =
        client.coverage_report().expect_err("an unadvertised verb must be refused client-side");
    assert!(err.contains(FEATURE_COVERAGE), "the refusal names the missing capability: {err}");
    assert!(
        err.contains("nothing was sent"),
        "the refusal says it did not reach the server, so a reader knows it is not a store fault: \
         {err}"
    );
    drop(client); // EOF ends the fake peer's loop

    assert!(
        rx.recv().expect("count").is_empty(),
        "the client sent NOTHING after the refused negotiation"
    );
    handle.join().expect("fake server thread joins cleanly");
}

/// `RemoteHistStore::coverage_report` — the seam the GUI actually calls — surfaces that refusal as
/// an `Err`, NOT as the trait's empty `Ok(vec![])` default. This is the load-bearing half for the
/// Partial column: an inherited empty would render as "nothing is partial", a claim about the
/// server's data that the client is in no position to make.
#[test]
fn the_remote_store_surfaces_an_old_servers_refusal_as_an_error() {
    let (addr, rx, handle) = spawn_fake_peer(vec!["inventory".to_string()], |_| None);
    let store = RemoteHistStore::new(addr.to_string());

    let err = store
        .coverage_report()
        .expect_err("an unanswerable report must not degrade to an empty Ok");
    let msg = err.to_string();
    assert!(msg.contains(FEATURE_COVERAGE), "the cause survives to the caller's log: {msg}");

    // The per-call connection is dropped inside the verb, so the fake has already seen EOF: the
    // refusal happened before any request frame, through this seam too.
    assert!(rx.recv().expect("count").is_empty(), "RemoteHistStore sent nothing either");
    handle.join().expect("fake server thread joins cleanly");
}

/// The server half: the real `serve` advertises the capability UNCONDITIONALLY — no table, no
/// feature gate, because `coverage_report` is a trait verb every build can call. (Contrast
/// `backfill`, advertised per mounted collector table.)
#[test]
fn every_serving_build_advertises_the_coverage_capability() {
    let addr = spawn_server(Arc::new(MemHistStore::new()));
    let client = DatahubClient::connect(addr).expect("handshake on connect");
    assert!(
        client.features().iter().any(|f| f == FEATURE_COVERAGE),
        "a plain `serve` over an in-memory store still advertises `{FEATURE_COVERAGE}`: {:?}",
        client.features()
    );
}

/// ...and having advertised it, it answers: an advertised verb on a store with nothing to report
/// comes back as an EMPTY `Ok`, never an error. "Nothing is partial" and "I cannot ask" are
/// different facts and the wire keeps them different.
#[test]
fn an_advertising_server_answers_an_empty_report_rather_than_an_error() {
    let addr = spawn_server(Arc::new(MemHistStore::new()));
    let mut client = DatahubClient::connect(addr).expect("handshake on connect");
    let report = client.coverage_report().expect("an advertised verb answers");
    assert!(report.is_empty(), "MemHistStore inherits the empty default: {report:?}");
}

/// The decode-side safety net underneath the negotiation: a client that SKIPS the capability check
/// (a raw frame, a hand-rolled client) and sends `Coverage` to a server that never heard of it gets
/// a clean `Response::Error` and keeps its connection — the framing/decode split. The mechanism is
/// variant-name dispatch, so an unknown FUTURE tag exercises precisely the path our tag takes on an
/// older build.
#[test]
fn an_unknown_coverage_shaped_tag_gets_a_clean_error_not_a_hang() {
    use std::io::Write;

    let addr = spawn_server(Arc::new(MemHistStore::new()));
    let mut stream = TcpStream::connect(addr).expect("connect");

    // Well-formed JSON, externally-tagged unit-variant shape (what `Request::Coverage` looks like
    // on the wire), with a tag this server does not know.
    let body: &[u8] = br#""CoverageVNext""#;
    let len = u32::try_from(body.len()).expect("test body fits u32");
    stream.write_all(&len.to_be_bytes()).expect("write length prefix");
    stream.write_all(body).expect("write body");
    stream.flush().expect("flush");

    match read_frame::<_, Response>(&mut stream).expect("server answers, never hangs") {
        Response::Error(msg) => assert!(!msg.is_empty(), "the decode failure carries a message"),
        other => panic!("an unknown variant must get Response::Error, got {other:?}"),
    }

    // The refusal was a bad REQUEST, not a bad CONNECTION.
    write_frame(&mut stream, &Request::Ping).expect("send Ping");
    match read_frame::<_, Response>(&mut stream).expect("read Pong") {
        Response::Pong => {}
        other => panic!("expected Pong after the unknown variant, got {other:?}"),
    }
}
