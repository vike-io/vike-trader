//! `connect_bounded`, the one way a node CLIENT dials (moved here from `vike-datahub-client`, whose
//! copy `vike-strategy-builder` duplicated verbatim, and now also used by `vike-tradehub-client`).
//!
//! The stream it hands back is ALREADY bounded both ways for the handshake and has Nagle off; an
//! address that resolves to nothing is its own `InvalidInput`.

use std::io;
use std::net::{SocketAddr, TcpListener};

use vike_hub_proto::frame::connect_bounded;
use vike_hub_proto::limits::CLIENT_HANDSHAKE_DEADLINE;

/// The stream is bounded both ways at [`CLIENT_HANDSHAKE_DEADLINE`]. Proven against a listener
/// never accepted from: the connect completes anyway (the kernel's backlog), the shape a
/// wrong-service-on-the-port peer has.
#[test]
fn connect_bounded_arms_the_handshake_deadline_both_ways() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let stream = connect_bounded(listener.local_addr().expect("local_addr"))
        .expect("connect to a bound loopback listener");

    assert_eq!(stream.read_timeout().expect("read_timeout"), Some(CLIENT_HANDSHAKE_DEADLINE));
    assert_eq!(stream.write_timeout().expect("write_timeout"), Some(CLIENT_HANDSHAKE_DEADLINE));
}

/// The dialled stream carries `TCP_NODELAY`, so a request is not held for the server's delayed ACK.
#[test]
fn connect_bounded_turns_nagle_off() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let stream = connect_bounded(listener.local_addr().expect("local_addr"))
        .expect("connect to a bound loopback listener");

    assert!(stream.nodelay().expect("read TCP_NODELAY"), "the dialled stream has Nagle on");
}

/// An address that resolves to NOTHING gets its own `InvalidInput`: the walk has no `last_err` to
/// report in that case.
#[test]
fn connect_bounded_reports_an_address_that_resolves_to_nothing() {
    // An empty `SocketAddr` slice yields no candidates, without a DNS lookup in a test.
    let empty: &[SocketAddr] = &[];
    let err = connect_bounded(empty).expect_err("no candidates must not be an Ok connection");
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    assert!(err.to_string().contains("no socket addresses"), "legible message: {err}");
}
