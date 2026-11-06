//! `trade` -- vike-gatewayhub's trade-lane suites (feature `trade`): ONE test binary over what used to be 13.
//!
//! Cargo links a separate test BINARY per top-level `tests/*.rs` file, and every one of them links this
//! crate's whole dependency graph. Each former `tests/<name>.rs` member here is now
//! `tests/trade/<name>.rs`, included below as a plain module: its tests, bodies and module doc are
//! unchanged, and only the binary it links into moved (nextest names a test `trade <name>::<test>`).
//!
//! The grouping rule is `crates/vike-backtest/CLAUDE.md`'s "Test-binary consolidation". The two hop
//! harnesses carry `#[ignore]`d measuring runs; run one as
//! `cargo test -p vike-gatewayhub --release --test trade -- --ignored --nocapture --test-threads=1 order_hop::`.
//! The `#![cfg(feature = "trade")]` below is the sanctioned GROUP-level gate: every member carries that IDENTICAL
//! gate, so a build without it compiles an empty binary and reads no member, as each was before.
#![cfg(feature = "trade")]

// `#[path]` because this file is a test-target CRATE ROOT: a bare `mod x;` would resolve against
// `tests/`, not `tests/trade/`.
#[path = "trade/event_hop.rs"]
mod event_hop;
#[path = "trade/ibkr_reconnect.rs"]
mod ibkr_reconnect;
#[path = "trade/order_hop.rs"]
mod order_hop;
#[path = "trade/trade_boundary.rs"]
mod trade_boundary;
#[path = "trade/trade_hub.rs"]
mod trade_hub;
#[path = "trade/trade_lease.rs"]
mod trade_lease;
#[path = "trade/trade_once.rs"]
mod trade_once;
#[path = "trade/trade_orders.rs"]
mod trade_orders;
#[path = "trade/trade_recon.rs"]
mod trade_recon;
#[path = "trade/trade_ring.rs"]
mod trade_ring;
#[path = "trade/trade_seam.rs"]
mod trade_seam;
#[path = "trade/trade_wire.rs"]
mod trade_wire;
#[path = "trade/venue_link_status.rs"]
mod venue_link_status;

// ---- what several members ask of a lease, spelled once ---------------------------------------------------

use std::sync::Arc;
use std::time::Duration;

use vike_gatewayhub::testkit::raw_peer::Raw;
use vike_gatewayhub::testkit::trade_rig::start_args;
use vike_gatewayhub::trade::lease::{Flow, Lease, Next};
use vike_gatewayhub_client::proto::{AdapterReply, TradeDown, TradeUp};

/// The adapter start a client sends on `route_key` (request `rid`, reading the account's currency when
/// `reads`), and the `(rid, reply)` the lease answered within `within` (each member's own frame wait).
fn adapter_start(
    lease: &Arc<Lease>,
    route_key: &str,
    rid: u64,
    reads: bool,
    within: Duration,
) -> (u64, AdapterReply) {
    let mut s = start_args();
    s.rid = rid;
    s.route_key = route_key.into();
    s.read_account_ccy = reads;
    assert_eq!(lease.handle(TradeUp::AdapterStart(s)), Flow::Continue);
    match lease.next(within) {
        Next::Frame(TradeDown::AdapterReply { rid, reply }) => (rid, *reply),
        other => panic!("expected an adapter reply, got {other:?}"),
    }
}

/// The account currency a live start's reply carries.
fn reply_ccy(reply: AdapterReply) -> Option<String> {
    match reply {
        AdapterReply::Live { account_ccy, .. } => account_ccy,
        other => panic!("{other:?}"),
    }
}

/// The next frame a raw lease reads within `within`, past every frame `skip` passes over (heartbeats, and
/// for the ring's suite acks too).
fn next_down(raw: &mut Raw, within: Duration, skip: impl Fn(&TradeDown) -> bool) -> TradeDown {
    loop {
        let frame = raw.recv_within::<TradeDown>(within).expect("a frame");
        if !skip(&frame) {
            return frame;
        }
    }
}
