//! The ONE fake peer of this crate's negotiation suites: it answers the `Hello` with the
//! `Welcome.features` a test chooses, then RECORDS every frame the client sends, so "nothing was
//! sent" is `received.is_empty()`. A real `serve` cannot prove that: it answers the frame either
//! way, which is exactly what a client-side refusal must prevent.
//!
//! Compiled ONCE by the lib behind the `test-support` feature (only dev edges turn it on: this
//! crate's self-referential one, `vike-app-core`'s and `vike-studio-core`'s) instead of once per suite that used to
//! `#[path]`-include `tests/support/fake_peer.rs`. A suite names it as `vike_datahub_client::test_support`.
//! [`accept_keyless`] is the key-less handshake alone, for `vike-app-core`'s scripted fake datahubs,
//! which used to carry it in their own `tests/common/mod.rs`.

use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::mpsc;
use std::thread;

use crate::{
    PROTO_VERSION,
    proto::{Request, Response, read_frame, write_frame},
};

/// Accept ONE client and answer its `Hello` with a KEY-LESS `Welcome` (no nonce) advertising
/// `features`. Every later frame is recorded and answered: `Ping` with `Pong`, anything else with
/// `reply(&request)`, or a `Response::Error` where that is `None`. When the client hangs up (EOF),
/// the recorded requests arrive on the returned channel; the join handle lets a test assert a
/// clean thread exit.
///
/// Every frame gets an answer, so a client that DOES send never parks in `read_frame`, and a test
/// whose fake runs nothing still proves the write by the client failing on the ANSWER.
pub fn spawn_fake_peer(
    features: Vec<String>,
    reply: impl Fn(&Request) -> Option<Response> + Send + 'static,
) -> (SocketAddr, mpsc::Receiver<Vec<Request>>, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let (tx, rx) = mpsc::channel::<Vec<Request>>();
    // NAMED, because `src/` is production to `crates/vike-ops/tests/hygiene/bare_thread_spawn_gate.rs`
    // even behind `test-support`.
    let spawned = thread::Builder::new().name("dhc-fake-peer".into()).spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept the client");
        match read_frame::<_, Request>(&mut stream) {
            Ok(Request::Hello { .. }) => {}
            other => panic!("fake peer expected Hello, got {other:?}"),
        }
        write_frame(
            &mut stream,
            &Response::Welcome { proto_version: PROTO_VERSION, features, nonce: None },
        )
        .expect("fake peer writes Welcome");
        let mut received = Vec::new();
        while let Ok(request) = read_frame::<_, Request>(&mut stream) {
            let answer = match &request {
                Request::Ping => Response::Pong,
                other => reply(other)
                    .unwrap_or_else(|| Response::Error("fake peer: unexpected frame".to_string())),
            };
            received.push(request);
            if write_frame(&mut stream, &answer).is_err() {
                break;
            }
        }
        tx.send(received).expect("report what arrived");
    });
    (addr, rx, spawned.expect("spawn the fake peer thread"))
}

/// The ONE spelling of the KEY-LESS datahub handshake, server side, that `vike-app-core`'s scripted
/// fake datahubs open every connection with: read the client's `Request::Hello`, then answer a
/// `Response::Welcome` advertising `features` with NO nonce — the byte-identical-to-pre-auth shape
/// `Response::Welcome::nonce`'s doc describes. `false` when the first frame is not a `Hello` or the
/// `Welcome` cannot be written; the caller decides whether that is a panic or a quiet hang-up.
///
/// Only the handshake is shared: what each fake answers AFTER it (`Backfill`, `VenueCatalog`,
/// `MdSubscribe`/`MdUpdate`) is that suite's own script and stays in its file.
pub fn accept_keyless(stream: &mut TcpStream, features: Vec<String>) -> bool {
    if !matches!(read_frame::<_, Request>(stream), Ok(Request::Hello { .. })) {
        return false;
    }
    write_frame(stream, &Response::Welcome { proto_version: PROTO_VERSION, features, nonce: None })
        .is_ok()
}

/// The version-skew probe two negotiation suites share: connect to the REAL server at `addr`, send ONE
/// length-prefixed frame whose `body` is well-formed JSON with a tag the server does not know, and assert
/// it answers `Response::Error` (with a message) instead of hanging, and that the connection then still
/// answers a `Ping` with `Pong`: the refusal was a bad REQUEST, not a bad CONNECTION.
pub fn assert_an_unknown_tag_is_refused_and_the_link_survives(addr: SocketAddr, body: &[u8]) {
    use std::io::Write;

    let mut stream = TcpStream::connect(addr).expect("connect");
    let len = u32::try_from(body.len()).expect("test body fits u32");
    stream.write_all(&len.to_be_bytes()).expect("write length prefix");
    stream.write_all(body).expect("write body");
    stream.flush().expect("flush");

    match read_frame::<_, Response>(&mut stream).expect("server answers, never hangs") {
        Response::Error(msg) => assert!(!msg.is_empty(), "the decode failure carries a message"),
        other => panic!("an unknown variant must get Response::Error, got {other:?}"),
    }

    write_frame(&mut stream, &Request::Ping).expect("send Ping");
    match read_frame::<_, Response>(&mut stream).expect("read Pong") {
        Response::Pong => {}
        other => panic!("expected Pong after the unknown variant, got {other:?}"),
    }
}

/// The one-symbol bar slice the Studio wire suites run their sweeps and walk-forwards over: binance
/// `BTCUSDT` at `1m`, unbounded both ends. This crate's `tests/walkforward_search_negotiation.rs` and
/// vike-studio-core's `tests/studio_wire_parity.rs` each built it by hand until 2026-10-31.
pub fn btcusdt_1m_bars() -> crate::wire_studio::WireSlice {
    crate::wire_studio::WireSlice {
        venue: "binance".to_string(),
        symbols: vec!["BTCUSDT".to_string()],
        interval: "1m".to_string(),
        start: None,
        end: None,
        kind: crate::wire_studio::WireSliceKind::Bars,
    }
}
