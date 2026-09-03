//! PR-2 protocol-hygiene tests driven over a LIVE `vike_datahub::serve` — the handshake and the
//! bad-request-survival behavior, exercised end-to-end against the real server accept loop.
//!
//! These live in `vike-datahub-client` (the FAST CI lane) rather than `vike-datahub` (the hist lane)
//! ON PURPOSE: a `vike-datahub`-only change does NOT trigger the hist job (its trigger set is
//! vike-data/vike-backfill/vike-report), so server tests placed there would silently not run on a
//! proto-only PR. vike-datahub-client already dev-depends on `vike_datahub::serve` + a `MemHistStore`
//! (see `remote_roundtrip.rs`), so spawning the real server here is free and the tests run every time
//! `-p vike-datahub-client` does. No prod store, no external network — loopback + in-memory only.

use std::io::Write;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::thread;

use vike_data::{HistStore, MemHistStore};
use vike_datahub::serve;
use vike_datahub_client::{
    read_frame, write_frame, DatahubClient, Request, Response, PROTO_VERSION,
};

/// Bind an ephemeral loopback listener and spawn the REAL `serve` over an in-memory store on a
/// detached thread; return the assigned address for a client/raw stream to connect to.
fn spawn_server() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let store: Arc<dyn HistStore + Send + Sync> = Arc::new(MemHistStore::new());
    thread::spawn(move || {
        let _ = serve(listener, store);
    });
    addr
}

/// Write a raw length-prefixed frame with an ARBITRARY body — used to inject a body the typed
/// [`write_frame`] could not produce (e.g. an unknown-variant JSON string).
fn write_raw_frame(stream: &mut TcpStream, body: &[u8]) {
    let len = u32::try_from(body.len()).expect("test body fits u32");
    stream.write_all(&len.to_be_bytes()).expect("write length prefix");
    stream.write_all(body).expect("write body");
    stream.flush().expect("flush");
}

/// The OPTIONAL `Hello` handshake over a live `serve`: a raw `Request::Hello` gets a
/// `Response::Welcome` carrying THIS server's `PROTO_VERSION` and its served-verb feature list.
/// Driven over a raw stream (no `DatahubClient`), proving the handshake is an ordinary request.
#[test]
fn hello_handshake_over_serve_returns_version_and_features() {
    let addr = spawn_server();
    let mut stream = TcpStream::connect(addr).expect("connect");
    let hello = Request::Hello { proto_version: PROTO_VERSION };
    write_frame(&mut stream, &hello).expect("send Hello");

    let reply = read_frame::<_, Response>(&mut stream).expect("read the Welcome reply");
    let (proto_version, features) = match reply {
        Response::Welcome { proto_version, features, .. } => (proto_version, features),
        other => panic!("expected Welcome, got {other:?}"),
    };

    assert_eq!(proto_version, PROTO_VERSION, "server echoes its protocol version");
    for verb in ["backtest", "load_bars", "scan_quotes", "scan_trades", "properties_as_of"] {
        assert!(features.iter().any(|f| f == verb), "advertises `{verb}`: {features:?}");
    }
}

/// `DatahubClient::connect` performs the handshake and exposes the server's advertised features —
/// the happy path every caller (`vike-cli`, `RemoteHistStore`) inherits for free.
#[test]
fn connect_handshakes_and_exposes_features() {
    let addr = spawn_server();
    let client = DatahubClient::connect(addr).expect("handshake on connect");
    assert!(
        client.features().iter().any(|f| f == "backtest"),
        "connect captured the advertised features: {:?}",
        client.features()
    );
}

/// A well-framed but UNDECODABLE request gets a `Response::Error` and the connection SURVIVES: a
/// subsequent `Ping` on the SAME stream still answers `Pong`. Driven over a raw stream that never
/// sends `Hello`, which also proves the handshake is optional (the server serves it regardless).
#[test]
fn bad_request_yields_error_and_the_connection_survives() {
    let addr = spawn_server();
    let mut stream = TcpStream::connect(addr).expect("connect");

    // A valid-JSON body that is NOT a known `Request` variant (externally-tagged unit variants are
    // JSON strings; "Bogus" is not one) — it frames fine but fails to decode server-side.
    write_raw_frame(&mut stream, b"\"Bogus\"");
    match read_frame::<_, Response>(&mut stream).expect("server answers the bad request") {
        Response::Error(msg) => assert!(!msg.is_empty(), "the decode failure carries a message"),
        other => panic!("a bad request must get Response::Error, got {other:?}"),
    }

    // The loop survived: the SAME connection still serves a normal request.
    write_frame(&mut stream, &Request::Ping).expect("send Ping on the surviving connection");
    match read_frame::<_, Response>(&mut stream).expect("read Pong") {
        Response::Pong => {}
        other => panic!("expected Pong after a bad request, got {other:?}"),
    }
}
