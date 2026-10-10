//! The ONE fake peer of this crate's negotiation suites: it answers the `Hello` with the
//! `Welcome.features` a test chooses, then RECORDS every frame the client sends, so "nothing was
//! sent" is `received.is_empty()`. A real `serve` cannot prove that: it answers the frame either
//! way, which is exactly what a client-side refusal must prevent.
//!
//! Each caller `#[path]`-includes it (a bare `mod` would resolve beside the binary root); a file
//! under `tests/support/` is no test target of its own. It holds only what EVERY includer calls,
//! so no binary warns on dead code under `-D warnings`.

use std::net::{SocketAddr, TcpListener};
use std::sync::mpsc;
use std::thread;

use vike_datahub_client::{
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
    let handle = thread::spawn(move || {
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
    (addr, rx, handle)
}
