//! The cross-kind coverage verb's CAPABILITY NEGOTIATION (split-plane spec §6-Q2), driven
//! end-to-end — the sibling of `backfill_negotiation.rs`, and deliberately its mirror image.
//!
//! `Request::Coverage` / `Response::Coverage` shipped WITHOUT a `PROTO_VERSION` bump (a bump fails
//! every old-client/new-server pair, including the many that never open the Data Manager, to
//! protect one column). So the version handshake cannot protect this verb; the `Welcome.features`
//! capability list does.
//!
//! It differs from backfill in exactly one way, and that difference is what these tests pin: there
//! is no table to mount. Coverage is a plain `vike_data::HistStore` trait verb, so EVERY server
//! that serves at all serves it and advertises it unconditionally — which means a missing
//! advertisement carries exactly one meaning, *this peer predates the verb*, and the GUI can turn
//! that into an honest note instead of a blank column.
//!
//! Placed in `vike-datahub-client` (the FAST CI lane) for the same reason `backfill_negotiation.rs`
//! and `server_handshake.rs` are: a proto-only PR does not fire the hist job, and these must run
//! every time `-p vike-datahub-client` does. Loopback + in-memory only — no store on disk.

use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;

use vike_data::{HistStore, MemHistStore};
use vike_datahub::serve;
use vike_datahub_client::{
    read_frame, write_frame, DatahubClient, RemoteHistStore, Request, Response, FEATURE_COVERAGE,
    PROTO_VERSION,
};

/// Bind an ephemeral loopback listener and spawn the REAL `serve` over an in-memory store; return
/// the address. `MemHistStore` inherits the trait's empty `coverage_report` default, which is the
/// point here: these tests are about the NEGOTIATION, not the report (the real DataFusion answer
/// crosses the wire in `crates/vike-datahub/tests/composed_store_roundtrip.rs`).
fn spawn_server() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let store: Arc<dyn HistStore + Send + Sync> = Arc::new(MemHistStore::new());
    thread::spawn(move || {
        let _ = serve(listener, store);
    });
    addr
}

/// Spawn a fake server that completes the version handshake with a `Welcome` carrying `features`,
/// then COUNTS every frame the client sends afterwards until EOF. The count arrives on the returned
/// channel; the join handle is returned so the test can assert a clean thread exit.
///
/// Hand-rolled rather than driven through the real `serve` (the `handshake.rs` / backfill
/// precedent) because proving "nothing was sent" needs a server that can count — the real server
/// would answer the frame either way, which is exactly what a client-side refusal must prevent.
fn spawn_counting_fake(
    features: Vec<String>,
) -> (SocketAddr, mpsc::Receiver<usize>, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let (tx, rx) = mpsc::channel::<usize>();
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept the client");
        match read_frame::<_, Request>(&mut stream) {
            Ok(Request::Hello { .. }) => {}
            Ok(other) => panic!("fake server expected Hello, got {other:?}"),
            Err(e) => panic!("fake server failed to read Hello: {e}"),
        }
        // A KEY-LESS fake datahub: no nonce, so its Welcome bytes carry no `nonce` field at all.
        write_frame(
            &mut stream,
            &Response::Welcome { proto_version: PROTO_VERSION, features, nonce: None },
        )
        .expect("fake server writes Welcome");
        let mut frames_after_hello = 0usize;
        while read_frame::<_, Request>(&mut stream).is_ok() {
            frames_after_hello += 1;
        }
        tx.send(frames_after_hello).expect("report the count");
    });
    (addr, rx, handle)
}

/// The OLD-SERVER case, which is the only case this negotiation exists for: a matching protocol
/// version (so the handshake succeeds and gives no warning) but no `coverage` advertisement. The
/// client refuses locally and sends NOTHING — the fake server counts zero frames after the `Hello`.
#[test]
fn a_welcome_without_the_capability_is_refused_client_side_without_sending() {
    let (addr, rx, handle) =
        spawn_counting_fake(vec!["backtest".to_string(), "inventory".to_string()]);

    let mut client = DatahubClient::connect(addr).expect("handshake succeeds — same version");
    let err =
        client.coverage_report().expect_err("an unadvertised verb must be refused client-side");
    assert!(err.contains(FEATURE_COVERAGE), "the refusal names the missing capability: {err}");
    assert!(
        err.contains("nothing was sent"),
        "the refusal says it did not reach the server, so a reader knows it is not a store fault: \
         {err}"
    );
    drop(client); // EOF ends the fake server's counting loop

    assert_eq!(
        rx.recv().expect("count"),
        0,
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
    let (addr, rx, handle) = spawn_counting_fake(vec!["inventory".to_string()]);
    let store = RemoteHistStore::new(addr.to_string());

    let err = store
        .coverage_report()
        .expect_err("an unanswerable report must not degrade to an empty Ok");
    let msg = err.to_string();
    assert!(msg.contains(FEATURE_COVERAGE), "the cause survives to the caller's log: {msg}");

    // The per-call connection is dropped inside the verb, so the fake's counting loop has already
    // seen EOF: the refusal happened before any request frame, through this seam too.
    assert_eq!(rx.recv().expect("count"), 0, "RemoteHistStore sent nothing either");
    handle.join().expect("fake server thread joins cleanly");
}

/// The server half: the real `serve` advertises the capability UNCONDITIONALLY — no table, no
/// feature gate, because `coverage_report` is a trait verb every build can call. (Contrast
/// `backfill`, advertised per mounted collector table.)
#[test]
fn every_serving_build_advertises_the_coverage_capability() {
    let addr = spawn_server();
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
    let addr = spawn_server();
    let mut client = DatahubClient::connect(addr).expect("handshake on connect");
    let report = client.coverage_report().expect("an advertised verb answers");
    assert!(report.is_empty(), "MemHistStore inherits the empty default: {report:?}");
}

/// The decode-side safety net underneath the negotiation: a client that SKIPS the capability check
/// (a raw frame, a hand-rolled client) and sends `Coverage` to a server that never heard of it gets
/// a clean `Response::Error` and keeps its connection — the PR-2 framing/decode split. The
/// mechanism is variant-name dispatch, so an unknown FUTURE tag exercises precisely the path our
/// tag takes on an older build.
#[test]
fn an_unknown_coverage_shaped_tag_gets_a_clean_error_not_a_hang() {
    use std::io::Write;

    let addr = spawn_server();
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
