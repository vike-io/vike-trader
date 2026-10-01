//! The backfill-on-demand verb's CAPABILITY NEGOTIATION (split-plane REQ-9), driven end-to-end.
//!
//! The verb shipped WITHOUT a `PROTO_VERSION` bump — an old server and a new client still agree on
//! the version, so the version handshake cannot protect this verb. What protects it is the
//! `Welcome.features` capability list (the designed forward-compat hook): a server advertises
//! [`FEATURE_BACKFILL`] only when it actually holds collectors, and the CLIENT refuses locally —
//! without sending — when the advertisement is absent. These tests pin BOTH halves of that
//! contract plus the decode-side safety net underneath it (an unknown-variant frame is answered
//! with a clean `Response::Error`, never a hang — the PR-2 framing/decode split).
//!
//! Placed in `vike-datahub-client` (the FAST CI lane) for the same reason `server_handshake.rs`
//! is: a proto-only PR does not fire the hist job, and these tests must run every time
//! `-p vike-datahub-client` does. Loopback + in-memory only — no store on disk, no network.

use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::mpsc;
use std::thread;

use vike_data::{HistStore, MemHistStore};
use vike_datahub::serve;
use vike_datahub_client::{
    DatahubClient, FEATURE_BACKFILL, PROTO_VERSION, Request, Response, read_frame, write_frame,
};

/// Bind an ephemeral loopback listener and spawn the REAL `serve` — WITHOUT a backfill table, the
/// exact shape of every pre-verb deployment AND of a new build that lacks `backfill-serve` — over
/// an in-memory store; return the address.
fn spawn_tableless_server() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let store: Arc<dyn HistStore + Send + Sync> = Arc::new(MemHistStore::new());
    thread::spawn(move || {
        let _ = serve(listener, store);
    });
    addr
}

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
/// NOTHING is sent: the fake server counts every frame after the `Hello`, and sees zero.
///
/// Driven against a hand-rolled fake (the `handshake.rs` precedent) because proving "nothing was
/// sent" needs a server that can COUNT — the real `serve` would answer the frame either way.
#[test]
fn a_feature_less_welcome_is_refused_client_side_without_sending() {
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
        // A matching version but NO backfill advertisement — an old server, or a new lean one.
        write_frame(
            &mut stream,
            &Response::Welcome {
                proto_version: PROTO_VERSION,
                features: vec!["backtest".to_string(), "load_bars".to_string()],
                // A KEY-LESS server: no nonce on the wire at all (see the field's docs).
                nonce: None,
            },
        )
        .expect("fake server writes Welcome");
        // Count every frame that arrives after the handshake until the client drops (EOF).
        let mut frames_after_hello = 0usize;
        while read_frame::<_, Request>(&mut stream).is_ok() {
            frames_after_hello += 1;
        }
        tx.send(frames_after_hello).expect("report the count");
    });

    let mut client = DatahubClient::connect(addr).expect("handshake succeeds — same version");
    let err = client
        .backfill("binance", "BTCUSDT", "1h", 0, 3_600_000)
        .expect_err("an unadvertised verb must be refused client-side");
    assert!(
        err.contains(FEATURE_BACKFILL),
        "the refusal names the missing capability string: {err}"
    );
    drop(client); // EOF ends the fake server's counting loop

    let frames_after_hello = rx.recv().expect("fake server reports its count");
    assert_eq!(frames_after_hello, 0, "the client sent NOTHING after the refused negotiation");
    handle.join().expect("fake server thread joins cleanly");
}

/// The real `serve` WITHOUT a backfill table does not advertise [`FEATURE_BACKFILL`] — the
/// server half of the negotiation contract (the composed test proves the with-table half).
#[test]
fn a_tableless_server_does_not_advertise_backfill() {
    let addr = spawn_tableless_server();
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
    let addr = spawn_tableless_server();
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
/// comes back as a clean `Response::Error` (the PR-2 framing/decode split), never a dropped
/// connection and never a hang. The mechanism is variant-name dispatch, so an unknown FUTURE tag
/// here exercises precisely the path our new tag takes on an old build.
#[test]
fn an_unknown_struct_variant_frame_gets_a_clean_error_not_a_hang() {
    use std::io::Write;

    let addr = spawn_tableless_server();
    let mut stream = TcpStream::connect(addr).expect("connect");

    // Well-formed JSON, externally-tagged struct-variant shape, unknown tag.
    let body: &[u8] = br#"{"BackfillVNext":{"venue":"binance","job":true}}"#;
    let len = u32::try_from(body.len()).expect("test body fits u32");
    stream.write_all(&len.to_be_bytes()).expect("write length prefix");
    stream.write_all(body).expect("write body");
    stream.flush().expect("flush");

    match read_frame::<_, Response>(&mut stream).expect("server answers, never hangs") {
        Response::Error(msg) => assert!(!msg.is_empty(), "the decode failure carries a message"),
        other => panic!("an unknown variant must get Response::Error, got {other:?}"),
    }

    // The connection survives for the next (known) request.
    write_frame(&mut stream, &Request::Ping).expect("send Ping");
    match read_frame::<_, Response>(&mut stream).expect("read Pong") {
        Response::Pong => {}
        other => panic!("expected Pong after the unknown variant, got {other:?}"),
    }
}
