//! The COUNTING fake peer of the negotiation suites that drive BOTH directions —
//! `search_method_negotiation.rs`, `seed_class_negotiation.rs`, `study_negotiation.rs` and
//! `walkforward_search_negotiation.rs` — each of which `#[path]`-includes it (a bare `mod` would
//! resolve beside the binary root, not here). A file under `tests/support/` is no test target of
//! its own, and it is not in `tests/common/` because the binaries that declare `mod common;` never
//! call it.
//!
//! Hand-rolled rather than driven through a real `serve`: proving "nothing was sent" needs a
//! server that COUNTS, and a real one would answer the frame either way. `coverage_negotiation.rs`
//! keeps its own fake, which counts and never answers (`spawn_counting_fake`'s doc says why).

use std::net::{SocketAddr, TcpListener};
use std::sync::mpsc;
use std::thread;

use vike_datahub_client::{
    PROTO_VERSION,
    proto::{Request, Response, write_frame},
    read_frame,
};

/// Answer ONE `Hello` with `features` (a KEY-LESS `Welcome`: no nonce), then count every frame the
/// client sends afterwards and report the total on the returned channel when the connection
/// closes. The join handle is returned so a test can assert a clean thread exit.
///
/// ⚠ It COUNTS and then ANSWERS, where `coverage_negotiation.rs`'s fake only counts. That sibling
/// never drives the SENT direction, so its client always refuses before a write and never blocks
/// on a read; these suites do drive it, and a fake that stayed silent would park the client in
/// `read_frame` until nextest's timeout killed it. The answer is a `Response::Error`, which is
/// what a peer that cannot serve the verb would send anyway.
pub fn spawn_counting_fake(
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
        write_frame(
            &mut stream,
            &Response::Welcome { proto_version: PROTO_VERSION, features, nonce: None },
        )
        .expect("fake server writes Welcome");
        let mut frames_after_hello = 0usize;
        while read_frame::<_, Request>(&mut stream).is_ok() {
            frames_after_hello += 1;
            if write_frame(&mut stream, &Response::Error("fake peer".to_string())).is_err() {
                break;
            }
        }
        tx.send(frames_after_hello).expect("report the count");
    });
    (addr, rx, handle)
}
