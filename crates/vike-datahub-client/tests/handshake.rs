//! Client handshake: a version-mismatched server makes [`DatahubClient::connect`] fail with a
//! LEGIBLE error that NAMES BOTH the client's and the server's protocol version, rather than
//! desyncing on the first request.
//!
//! Driven against a HAND-ROLLED fake server: a real `vike_datahub::serve` always answers its OWN
//! `PROTO_VERSION`, so only a fake can advertise a DIFFERENT one (the happy path is `vike-datahub`'s
//! `roundtrip.rs`).

use std::net::{SocketAddr, TcpListener};
use std::thread;

use vike_datahub_client::{
    DatahubClient, PROTO_VERSION,
    proto::{Request, Response, read_frame, write_frame},
};

/// Spawn a one-shot fake datahub server that reads the client's `Hello` and replies with a `Welcome`
/// advertising `server_version`, then blocks on a read until the client drops (EOF), so the socket
/// stays open long enough for the client to read the `Welcome` first.
fn spawn_fake_server(server_version: u32) -> (SocketAddr, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept the client");
        match read_frame::<_, Request>(&mut stream) {
            Ok(Request::Hello { .. }) => {}
            Ok(other) => panic!("fake server expected Hello, got {other:?}"),
            Err(e) => panic!("fake server failed to read Hello: {e}"),
        }
        write_frame(
            &mut stream,
            &Response::Welcome { proto_version: server_version, features: vec![], nonce: None },
        )
        .expect("fake server writes Welcome");
        // A server-side close must never race ahead of the client's read of the Welcome.
        let _ = read_frame::<_, Request>(&mut stream);
    });
    (addr, handle)
}

/// A server one version AHEAD of this client makes `connect` fail with a message naming both numbers.
#[test]
fn version_mismatch_is_a_legible_error_naming_both_versions() {
    let server_version = PROTO_VERSION + 1; // a NEWER server than this client
    let (addr, handle) = spawn_fake_server(server_version);

    let err = DatahubClient::connect(addr).expect_err("connect must reject a version mismatch");
    let msg = err.to_string();
    assert!(msg.contains(&PROTO_VERSION.to_string()), "names the client version: {msg}");
    assert!(msg.contains(&server_version.to_string()), "names the server version: {msg}");

    handle.join().expect("fake server thread joins cleanly");
}
