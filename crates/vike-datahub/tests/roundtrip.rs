//! Hermetic end-to-end tests for vike-datahub — loopback only, no prod store, no external network.
//!
//! The server is bound on an ephemeral `127.0.0.1:0` port and driven over an in-memory
//! `MemHistStore` (vike-data's `test-support` double), so every test is self-contained and
//! deterministic. Plus direct `proto` framing tests over in-memory buffers (write -> read
//! round-trip, and the oversized-length OOM guard).

use std::io::{self, Cursor};
use std::sync::Arc;

use vike_data::MemHistStore;
use vike_datahub_client::{
    DatahubClient,
    proto::{MAX_FRAME_LEN, Request, Response, write_frame},
    read_frame,
};

mod common;
use common::spawn_server;

// ⚠ `MINIMAL_BAR_PROFILE` moved to `crates/vike-backtest/tests/compute_plane.rs` with the verbs
// that consumed it.

#[test]
fn ping_round_trips() {
    let addr = spawn_server(Arc::new(MemHistStore::new()));
    let mut client = DatahubClient::connect(addr).expect("connect");
    client.ping().expect("ping must get Pong");
}

// ⚠ SIX COMPUTE TESTS LEFT THIS FILE (ruling 7 of
// `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md`): the `RunBacktest`
// round-trip, the invalid-profile refusal, the `list_strategies` roster, the profile-shaped sweep
// and its two rejection cases. They all drove verbs this daemon no longer serves, so keeping them
// here would have meant asserting a wrong-plane refusal while claiming to prove an engine ran. They
// moved WITH the verbs, to `crates/vike-backtest/tests/compute_plane.rs`, over the compute daemon's
// own socket. What this daemon owes about those verbs now is the REFUSAL, and that is
// `tests/plane_split.rs`.

// --- direct proto framing tests (no network) --------------------------------------------------

/// A `Request` and a `Response` survive `write_frame` -> `read_frame` unchanged over an in-memory
/// buffer — the pure framing contract. Uses the payload-free variants so the round-trip is
/// independent of the larger profile/report shapes.
#[test]
fn frame_round_trips_request_and_response() {
    let mut buf: Vec<u8> = Vec::new();
    write_frame(&mut buf, &Request::Ping).unwrap();
    write_frame(&mut buf, &Response::Error("boom".to_string())).unwrap();

    let mut cur = Cursor::new(buf);
    let req: Request = read_frame(&mut cur).unwrap();
    assert!(matches!(req, Request::Ping));
    let resp: Response = read_frame(&mut cur).unwrap();
    match resp {
        Response::Error(m) => assert_eq!(m, "boom"),
        other => panic!("expected Error, got {other:?}"),
    }
}

/// An oversized length prefix is rejected BEFORE the body allocation — the OOM guard. A raw prefix
/// of `MAX_FRAME_LEN + 1` must error on the length alone, before any body is read.
#[test]
fn oversized_length_prefix_is_rejected() {
    let mut framed = (MAX_FRAME_LEN + 1).to_be_bytes().to_vec();
    framed.push(0); // a trailing byte: the guard must fire on the length, not on a short read
    let mut cur = Cursor::new(framed);
    let err = read_frame::<_, Request>(&mut cur).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
}
