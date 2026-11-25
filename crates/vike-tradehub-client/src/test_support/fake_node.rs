//! The ONE scripted node of this crate's negotiation suites: it runs the real handshake, then
//! RECORDS a name for every post-auth frame, so "nothing was sent" is `report.recv()` being empty.
//! A real node cannot prove that: it answers the frame either way, which is exactly what a
//! client-side refusal must prevent.
//!
use std::net::{SocketAddr, TcpListener};
use std::sync::mpsc;
use std::thread;

use crate::auth;
use crate::proto::{NODE_PROTO_VERSION, Request, Response};
use vike_hub_protocol::frame::{
    BodyCodec, decode_body, read_frame, read_frame_raw, write_frame, write_reply_as,
};
use vike_hub_protocol::handshake::{HandshakeFrame, HandshakeReply};

/// Accept ONE client on loopback and answer its `Hello` with a `Welcome` advertising exactly
/// `features`; verify its `Auth` mac with `key` for whatever scope it claims (scope separation is
/// the real server's concern). Every later frame goes to `reply`, which returns the name to record
/// and the answer to write. When the client hangs up, the recorded names arrive on the channel.
pub fn scripted_node(
    key: &'static [u8],
    features: Vec<String>,
    mut reply: impl FnMut(Request) -> (String, Response) + Send + 'static,
) -> (SocketAddr, mpsc::Receiver<Vec<String>>) {
    scripted_node_in_codecs(key, features, move |request, _| reply(request))
}

/// [`scripted_node`] whose `reply` is also told the codec each request came in, answered in that
/// codec as a node of this build answers: what proves which codec a client chose.
pub fn scripted_node_in_codecs(
    key: &'static [u8],
    features: Vec<String>,
    mut reply: impl FnMut(Request, BodyCodec) -> (String, Response) + Send + 'static,
) -> (SocketAddr, mpsc::Receiver<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let addr = listener.local_addr().expect("local addr");
    let (report_tx, report_rx) = mpsc::channel();
    // NAMED, because `src/` is production to `crates/vike-ops/tests/hygiene/bare_thread_spawn_gate.rs`
    // even behind `test-support`. Detached, as before: the test never joins it.
    let spawned = thread::Builder::new().name("thc-fake-node".into()).spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        match read_frame::<_, HandshakeFrame>(&mut stream) {
            Ok(HandshakeFrame::Hello { .. }) => {}
            other => panic!("expected Hello, got {other:?}"),
        }
        let nonce = [7u8; 32];
        write_frame(
            &mut stream,
            &HandshakeReply::Welcome {
                proto_version: NODE_PROTO_VERSION,
                features,
                nonce: Some(nonce),
            },
        )
        .expect("write Welcome");
        let Ok(HandshakeFrame::Auth { scope, mac }) = read_frame::<_, HandshakeFrame>(&mut stream)
        else {
            panic!("expected Auth after Welcome");
        };
        assert!(
            auth::verify(key, &nonce, NODE_PROTO_VERSION, scope, &mac),
            "the scripted node's one key must verify"
        );
        write_frame(&mut stream, &HandshakeReply::AuthOk { scope, admitted: None })
            .expect("write AuthOk");

        // Serve + record until the client hangs up (a read error is EOF: the connection is over).
        let mut seen: Vec<String> = Vec::new();
        while let Ok(body) = read_frame_raw(&mut stream) {
            let codec = BodyCodec::of(&body);
            let Ok(req) = decode_body::<Request>(&body) else { break };
            let (name, answer) = reply(req, codec);
            seen.push(name);
            write_reply_as(&mut stream, codec, &answer).expect("write scripted reply");
        }
        // A test that never reads the report has dropped the receiver; that is not a failure.
        let _ = report_tx.send(seen);
    });
    spawned.expect("spawn the scripted node thread");
    (addr, report_rx)
}

/// A `Welcome.features` list spelled as `&str`s: each suite's "old node" names its exact set.
pub fn features(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| (*name).to_string()).collect()
}
