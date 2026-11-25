//! The ONE fake peer of this crate's negotiation suites: it answers the `Hello` with the
//! `Welcome.features` a test chooses, then RECORDS every frame the client sends, so "nothing was sent"
//! is `received.is_empty()`. A real `serve` cannot prove that: it answers the frame either way, which is
//! exactly what a client-side refusal must prevent.
//!
//! Compiled ONCE by the lib behind the `test-support` feature (only dev edges turn it on).

use std::net::{SocketAddr, TcpListener};
use std::sync::mpsc;
use std::thread;

use vike_hub_protocol::frame::{BodyCodec, decode_body, read_frame, read_frame_raw, write_frame};
use vike_hub_protocol::handshake::{HandshakeFrame, HandshakeReply};

use crate::msgpack_body::write_reply;
use crate::proto::{PROTO_VERSION, Request, Response};

/// Accept ONE client and answer its `Hello` with a KEY-LESS `Welcome` (no nonce) advertising
/// `features`. Every later frame is recorded and answered: `Ping` with `Pong`, anything else with
/// `reply(&request)`, or a `Response::Error` where that is `None`. When the client hangs up (EOF), the
/// recorded requests arrive on the returned channel; the join handle lets a test assert a clean thread
/// exit.
///
/// Every frame gets an answer, so a client that DOES send never parks in `read_frame`, and a test whose
/// fake runs nothing still proves the write by the client failing on the ANSWER.
pub fn spawn_fake_peer(
    features: Vec<String>,
    reply: impl Fn(&Request) -> Option<Response> + Send + 'static,
) -> (SocketAddr, mpsc::Receiver<Vec<Request>>, thread::JoinHandle<()>) {
    fake_peer(features, reply, |request, _| request)
}

/// [`spawn_fake_peer`] that records the codec each request came in (`vike_hub_protocol::frame::BodyCodec`) instead.
/// Either peer answers each request in its codec through the backtesthub's own writer (`msgpack_body::write_reply`).
pub fn spawn_fake_peer_with_codecs(
    features: Vec<String>,
    reply: impl Fn(&Request) -> Option<Response> + Send + 'static,
) -> (SocketAddr, mpsc::Receiver<Vec<BodyCodec>>, thread::JoinHandle<()>) {
    fake_peer(features, reply, |_, codec| codec)
}

/// The one body of both peers: `record` keeps what a test reads of each request.
fn fake_peer<T: Send + 'static>(
    features: Vec<String>,
    reply: impl Fn(&Request) -> Option<Response> + Send + 'static,
    record: fn(Request, BodyCodec) -> T,
) -> (SocketAddr, mpsc::Receiver<Vec<T>>, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let (tx, rx) = mpsc::channel::<Vec<T>>();
    // NAMED, because `src/` is production to `crates/vike-ops/tests/hygiene/bare_thread_spawn_gate.rs`
    // even behind `test-support`.
    let spawned = thread::Builder::new().name("bhc-fake-peer".into()).spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept the client");
        match read_frame::<_, HandshakeFrame>(&mut stream) {
            Ok(HandshakeFrame::Hello { .. }) => {}
            other => panic!("fake peer expected Hello, got {other:?}"),
        }
        write_frame(
            &mut stream,
            &HandshakeReply::Welcome { proto_version: PROTO_VERSION, features, nonce: None },
        )
        .expect("fake peer writes Welcome");
        let mut received = Vec::new();
        while let Ok(body) = read_frame_raw(&mut stream) {
            let codec = BodyCodec::of(&body);
            let Ok(request) = decode_body::<Request>(&body) else { break };
            let answer = match &request {
                Request::Ping => Response::Pong,
                other => reply(other)
                    .unwrap_or_else(|| Response::Error("fake peer: unexpected frame".to_string())),
            };
            received.push(record(request, codec));
            if write_reply(&mut stream, codec, &answer).is_err() {
                break;
            }
        }
        tx.send(received).expect("report what arrived");
    });
    (addr, rx, spawned.expect("spawn the fake peer thread"))
}

/// The one-symbol bar slice the Studio wire suites run their sweeps and walk-forwards over: binance
/// `BTCUSDT` at `1m`, unbounded both ends. This crate's `tests/walkforward_search_negotiation.rs` and
/// vike-studio-core's `tests/studio_wire_parity.rs` each built it by hand until 2026-10-31.
pub fn btcusdt_1m_bars() -> crate::wire_studio::DataSliceWire {
    crate::wire_studio::DataSliceWire {
        venue: "binance".to_string(),
        symbols: vec!["BTCUSDT".to_string()],
        interval: "1m".to_string(),
        start: None,
        end: None,
        kind: crate::wire_studio::ReplayKindWire::Bars,
    }
}
