// Shared by several of this crate's integration tests, which cargo compiles as INDEPENDENT test
// binaries. Each uses a subset of these items, so per-binary dead-code analysis flags the rest —
// expected, and the same rationale `crates/vike-cli/tests/common/mod.rs` carries for the same shape.
#![allow(dead_code)]
//! The ONE spelling of the test scaffolding two or more of this crate's integration binaries
//! carried byte-for-byte: the loopback, key and frame helpers. (The inert half of a `HistStore`
//! double moved to `vike_data::hist_store_stubs!`, the workspace-wide spelling.) Each user declares
//! it with `mod common;` — a `tests/` SUBDIRECTORY module is not a test target, so this file builds
//! no binary and runs no test of its own.
//!
//! Only a helper whose body was identical in every file it came from lives here. A same-named
//! helper whose numbers or wiring DIFFER per file (`spawn`, `planted`, `bar`, `trade`, `quote`,
//! `book`, …) stays local: each pins its own values.

use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::thread;

use vike_data::{HistStore, TsRange};
use vike_datahub::serve;
use vike_datahub_client::proto::{Request, Response, write_frame};
use vike_datahub_client::{PROTO_VERSION, proto::read_frame};
use vike_node_proto::auth::{self, DATAHUB_DOMAIN, NodeKeys, Scope};

// ------------------------------------------------------------------------------------------------
// Keys
// ------------------------------------------------------------------------------------------------

/// The observe key of the keyed servers in `auth_roundtrip.rs`, `backfill_cancel.rs` and
/// `oanda_history_lane.rs` (a file whose keys differ, `delete_gate.rs`, keeps its own).
pub const OBSERVE_KEY: &[u8] = b"datahub-observe-key";
/// The control key beside [`OBSERVE_KEY`].
pub const CONTROL_KEY: &[u8] = b"datahub-control-key";

pub fn keys() -> NodeKeys {
    NodeKeys::new(OBSERVE_KEY.to_vec(), CONTROL_KEY.to_vec())
}

/// Open a socket and complete the handshake as `scope` with `keys`, returning the authenticated
/// stream (every caller today passes [`keys`]).
pub fn authed_stream(addr: SocketAddr, keys: &NodeKeys, scope: Scope) -> TcpStream {
    let mut s = TcpStream::connect(addr).expect("connect");
    write_frame(&mut s, &Request::Hello { proto_version: PROTO_VERSION }).expect("hello");
    let nonce = match read_frame::<_, Response>(&mut s).expect("welcome") {
        Response::Welcome { nonce, .. } => nonce.expect("a keyed server's Welcome carries a nonce"),
        other => panic!("expected Welcome, got {other:?}"),
    };
    let mac = auth::sign(DATAHUB_DOMAIN, keys.key_for(scope), &nonce, PROTO_VERSION, scope);
    write_frame(&mut s, &Request::Auth { scope, mac }).expect("auth");
    match read_frame::<_, Response>(&mut s).expect("auth answer") {
        Response::AuthOk { scope: granted } => assert_eq!(granted, scope),
        other => panic!("expected AuthOk, got {other:?}"),
    }
    s
}

// ------------------------------------------------------------------------------------------------
// Loopback
// ------------------------------------------------------------------------------------------------

/// Bind an ephemeral loopback listener, spawn `serve` over `store` on a detached thread, and return
/// the assigned address for a client to connect to (the sibling suites' convention). A suite that
/// needs a live server and no rows hands it `Arc::new(MemHistStore::new())`.
pub fn spawn_server(store: Arc<dyn HistStore + Send + Sync>) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    thread::spawn(move || {
        // The serve loop runs for the lifetime of the test process; its Result is only Err on an
        // impossible listener close, which we don't assert on here.
        let _ = serve(listener, store);
    });
    addr
}

/// Send one raw request on an already-established stream and read the answer.
pub fn exchange(stream: &mut TcpStream, request: &Request) -> Response {
    write_frame(stream, request).expect("write request");
    read_frame::<_, Response>(stream).expect("read response")
}

// ------------------------------------------------------------------------------------------------
// Frames and ranges
// ------------------------------------------------------------------------------------------------

/// The frame body the server writes for `response` — `write_frame`'s own encoding.
pub fn frame_of(response: &Response) -> Vec<u8> {
    serde_json::to_vec(response).expect("serialize a Response")
}

/// The ranges every limited request in `load_bars_bounded.rs` and `scan_ceilings.rs` is asked
/// over: the whole series, one starting INSIDE the three-copy group at `2_000`'s neighbour, and one
/// bounded on both sides.
pub fn ranges() -> [TsRange; 3] {
    [TsRange::all(), TsRange { start: Some(1_500), end: None }, TsRange::of(4_000, 16_000)]
}

// ------------------------------------------------------------------------------------------------
// The credentialed history lane
// ------------------------------------------------------------------------------------------------

/// The OANDA token provider every REAL table a test here builds is handed. It answers
/// `NotConfigured`, so the credentialed row — built and never called — could not reach OANDA's
/// practice host even if a test did call it.
#[cfg(feature = "backfill-serve")]
pub fn no_oanda_token() -> vike_oanda::HistoryTokenProvider {
    Arc::new(|| Err(vike_oanda::HistoryTokenError::NotConfigured))
}
