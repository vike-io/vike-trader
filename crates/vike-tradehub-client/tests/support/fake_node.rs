//! The ONE scripted node of this crate's negotiation suites: it runs the real handshake, then
//! RECORDS a name for every post-auth frame, so "nothing was sent" is `report.recv()` being empty.
//! A real node cannot prove that: it answers the frame either way, which is exactly what a
//! client-side refusal must prevent.
//!
//! Each caller `#[path]`-includes it (a bare `mod` would resolve beside the binary root); a file
//! under `tests/support/` is no test target of its own. It holds only what EVERY includer calls,
//! so no binary warns on dead code under `-D warnings`.

use std::net::{SocketAddr, TcpListener};
use std::sync::mpsc;
use std::thread;

use vike_tradehub_client::auth;
use vike_tradehub_client::proto::{NODE_PROTO_VERSION, Request, Response, read_frame, write_frame};

/// Accept ONE client on loopback and answer its `Hello` with a `Welcome` advertising exactly
/// `features`; verify its `Auth` mac with `key` for whatever scope it claims (scope separation is
/// the real server's concern). Every later frame goes to `reply`, which returns the name to record
/// and the answer to write. When the client hangs up, the recorded names arrive on the channel.
pub fn scripted_node(
    key: &'static [u8],
    features: Vec<String>,
    mut reply: impl FnMut(Request) -> (String, Response) + Send + 'static,
) -> (SocketAddr, mpsc::Receiver<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let addr = listener.local_addr().expect("local addr");
    let (report_tx, report_rx) = mpsc::channel();
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        match read_frame::<_, Request>(&mut stream) {
            Ok(Request::Hello { .. }) => {}
            other => panic!("expected Hello, got {other:?}"),
        }
        let nonce = [7u8; 32];
        write_frame(
            &mut stream,
            &Response::Welcome { proto_version: NODE_PROTO_VERSION, nonce, features },
        )
        .expect("write Welcome");
        let Ok(Request::Auth { scope, mac }) = read_frame::<_, Request>(&mut stream) else {
            panic!("expected Auth after Welcome");
        };
        assert!(
            auth::verify(key, &nonce, NODE_PROTO_VERSION, scope, &mac),
            "the scripted node's one key must verify"
        );
        write_frame(&mut stream, &Response::AuthOk { scope }).expect("write AuthOk");

        // Serve + record until the client hangs up (a read error is EOF: the connection is over).
        let mut seen: Vec<String> = Vec::new();
        while let Ok(req) = read_frame::<_, Request>(&mut stream) {
            let (name, answer) = reply(req);
            seen.push(name);
            write_frame(&mut stream, &answer).expect("write scripted reply");
        }
        // A test that never reads the report has dropped the receiver; that is not a failure.
        let _ = report_tx.send(seen);
    });
    (addr, report_rx)
}

/// A `Welcome.features` list spelled as `&str`s: each suite's "old node" names its exact set.
pub fn features(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| (*name).to_string()).collect()
}
