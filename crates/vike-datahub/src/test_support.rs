//! The ONE spelling of the test scaffolding two or more of this crate's integration binaries
//! carried byte-for-byte: the loopback, key and frame helpers. (The inert half of a `HistStore`
//! double moved to `vike_data::hist_store_stubs!`, the workspace-wide spelling.)
//!
//! Behind `test-support`, which only DEV edges turn on (this crate's own, and vike-datahub-client's for
//! `spawn_server` and `eventually`): it lived in
//! `tests/common/mod.rs`, where every one of the sixteen binaries that declared `mod common;`
//! compiled its own copy; here the library compiles it ONCE and each binary imports
//! `vike_datahub::test_support::…`.
//!
//! Only a helper whose body was identical in every file it came from lives here. A same-named
//! helper whose numbers or wiring DIFFER per file (`spawn`, `planted`, `bar`, `trade`, `quote`,
//! `book`, …) stays local: each pins its own values.

use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;
use std::time::Duration;

use crate::serve;
use vike_data::{HistStore, TsRange};
use vike_datahub_client::proto::{Request, Response, write_frame};
use vike_datahub_client::{PROTO_VERSION, proto::read_frame};
use vike_hub_proto::auth::{self, DATAHUB_DOMAIN, HubKeys, Scope};

// ------------------------------------------------------------------------------------------------
// Keys
// ------------------------------------------------------------------------------------------------

/// The observe key of the keyed servers in `auth_roundtrip.rs`, `backfill_cancel.rs` and
/// `oanda_history_lane.rs` (a file whose keys differ, `delete_gate.rs`, keeps its own).
pub const OBSERVE_KEY: &[u8] = b"datahub-observe-key";
/// The control key beside [`OBSERVE_KEY`].
pub const CONTROL_KEY: &[u8] = b"datahub-control-key";

pub fn keys() -> HubKeys {
    HubKeys::new(OBSERVE_KEY.to_vec(), CONTROL_KEY.to_vec())
}

/// Open a socket and complete the handshake as `scope` with `keys`, returning the authenticated
/// stream (every caller today passes [`keys`]).
pub fn authed_stream(addr: SocketAddr, keys: &HubKeys, scope: Scope) -> TcpStream {
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
    // The serve loop's Result is only Err on an impossible listener close, not asserted here.
    vike_log::loopback::spawn_on_loopback("dh-test-serve", move |listener| {
        let _ = serve(listener, store);
    })
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

/// Drain a session's mailbox into decoded frames, in delivery order, synthesizing the writer's `TapeGap`
/// exactly where `run_market_writer` would (`md_hub.rs`, `gateway_twin.rs`).
pub fn drain_md(
    mailbox: &crate::md::mailbox::Mailbox,
) -> Vec<vike_datahub_client::market::MdFrame> {
    use crate::md::mailbox::Recv;
    use vike_datahub_client::market::MdFrame;
    let mut out = Vec::new();
    loop {
        match mailbox.recv_timeout(std::time::Duration::from_millis(1)) {
            Recv::Frame { bytes, owed_gap } => {
                if let Some((key, dropped, from_seq, to_seq)) = owed_gap {
                    out.push(MdFrame::TapeGap {
                        venue: key.venue.clone(),
                        symbol: key.symbol.clone(),
                        dropped,
                        from_seq,
                        to_seq,
                    });
                }
                // The publisher writes through the wire's own `write_frame`: a length prefix rides in front.
                assert!(bytes.len() > 4, "a framed payload carries a 4-byte length prefix");
                let response: Response =
                    serde_json::from_slice(&bytes[4..]).expect("decode Response");
                match response {
                    Response::Md(frame) => out.push(*frame),
                    other => panic!("a stream mailbox carries only Response::Md, got {other:?}"),
                }
            }
            _ => return out,
        }
    }
}

/// Subscribe `symbol` on `client`'s tick lane `lane` (the four verbs the feed broker carries), as
/// `feed_broker.rs` and `gateway_twin.rs` both drive a client by label.
pub fn subscribe_lane(
    client: &mut dyn vike_data::DataClient,
    lane: vike_data::StreamLabel,
    symbol: &str,
) -> Result<vike_data::SubscriptionId, vike_data::LiveDataError> {
    match lane {
        vike_data::StreamLabel::Quotes => client.subscribe_quotes(symbol),
        vike_data::StreamLabel::Trades => client.subscribe_trades(symbol),
        vike_data::StreamLabel::Book => client.subscribe_book(symbol),
        vike_data::StreamLabel::Depth => client.subscribe_depth(symbol),
    }
}

/// The ranges every limited request in `load_bars_bounded.rs` and `scan_ceilings.rs` is asked
/// over: the whole series, one starting INSIDE the three-copy group at `2_000`'s neighbour, and one
/// bounded on both sides.
pub fn ranges() -> [TsRange; 3] {
    [TsRange::all(), TsRange { start: Some(1_500), end: None }, TsRange::of(4_000, 16_000)]
}

// ------------------------------------------------------------------------------------------------
// The gatewayhub
// ------------------------------------------------------------------------------------------------

/// The link every REAL backfill table a test here builds and never calls is handed: an address
/// nothing listens on and EMPTY hub keys, which is what the production link carries until the node
/// keys are provisioned. Nothing can be dialled through it, so the real table's rows — built, never
/// called — could not reach a venue even if a test did call one: the call would fail naming the
/// missing key. A test that needs the daemon to ANSWER dials `vike_gatewayhub::testkit`'s loopback
/// gateway instead.
pub fn no_gateway() -> vike_gatewayhub_client::client::GatewayLink {
    vike_gatewayhub_client::client::GatewayLink::new(
        "127.0.0.1:1",
        HubKeys::new(Vec::new(), Vec::new()),
    )
}

// ------------------------------------------------------------------------------------------------
// Waiting
// ------------------------------------------------------------------------------------------------

/// Poll `cond` (through `vike_log::poll::wait_until`) for up to `within`, and panic naming `what` if it never held: the strategy
/// lane's suites (this crate's `tests/default_build/strat_lane.rs`, vike-datahub-client's `tests/strat_feed.rs`)
/// shared it as two copies until 2026-10-31.
pub fn eventually(within: Duration, what: &str, cond: impl Fn() -> bool) {
    assert!(vike_log::poll::wait_until(within, cond), "timed out waiting for: {what}");
}

// ------------------------------------------------------------------------------------------------
// The start history
// ------------------------------------------------------------------------------------------------

/// The minute series the start-history suites count in: `src/strat/history_tests.rs` (no socket) and
/// `tests/default_build/start_history.rs` (the lane over a real socket) pin the same bars.
pub mod start_history {
    use vike_marketdata::Bar;

    /// One minute: the bar step of every series here.
    pub const STEP: i64 = 60_000;
    /// A minute boundary: `BASE / STEP` is whole.
    pub const BASE: i64 = 1_760_000_040_000;

    /// The bar that opens `i` minutes after [`BASE`]; its values follow `i`, so two different bars
    /// differ.
    pub fn bar_at(i: i64) -> Bar {
        let f = i as f64 / 1000.0;
        Bar::ohlcv(BASE + i * STEP, 1.0 + f, 1.1 + f, 0.9 + f, 1.05 + f, 10.0 + i as f64)
    }
}
