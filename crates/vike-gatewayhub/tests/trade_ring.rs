//! **The event ring over the wire** (B.8 of the wave-2 plan): what a client that comes back with its
//! cursor is owed, and what it is told when the ring cannot pay it. Raw peers, so the exact frames and
//! their `replayed` flags are read, not interpreted by a client library.
#![cfg(feature = "trade")]

use std::time::{Duration, Instant};

use vike_gatewayhub::testkit::gateway::{TradeKit, trade_keys};
use vike_gatewayhub::testkit::trade::ScriptedTrade;
use vike_gatewayhub_client::proto::{
    Request, Response, TradeDown, TradeOpened, TradeUp, WireCursor,
};
use vike_hub_proto::auth::Scope;
use vike_model::AccountTier;
use vike_model::events::{Event, OrderAccepted, OrderRejected};

#[allow(dead_code, clippy::allow_attributes)]
#[path = "support/raw_peer.rs"]
mod raw_peer;
#[allow(dead_code, clippy::allow_attributes)]
#[path = "support/trade_rig.rs"]
mod trade_rig;

use raw_peer::Raw;
use trade_rig::{Rig, accepted, quick_config, rig, rig_with, start_args};

/// Opens a raw peer for `client_id`. A `LeaseHeld` answer is RETRIED (a fresh peer each try, 20 ms apart, for
/// at most 5 s): a test that drops its previous peer and opens as a DIFFERENT client id races the daemon's
/// noticing of the closed connection, which frees the lease on the daemon's own thread (load makes it late).
/// Every other answer, and a lease still held after the budget, is the panic it always was.
fn open_raw(rig: &Rig, client_id: &str, resume: Option<WireCursor>) -> (Raw, TradeOpened) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let mut raw = Raw::open(rig.gw.addr(), &trade_keys(), Scope::Trade);
        raw.send(&Request::TradeOpen {
            client_id: client_id.to_string(),
            venue: "binance".to_string(),
            account: None,
            tier: AccountTier::Demo,
            resume,
            takeover: false,
        });
        match raw.recv::<Response>() {
            Response::TradeOpened(opened) => return (raw, opened),
            Response::LeaseHeld { .. } if Instant::now() < deadline => {
                drop(raw);
                std::thread::sleep(Duration::from_millis(20));
            }
            other => panic!("{other:?}"),
        }
    }
}

fn start_adapter(raw: &mut Raw) {
    let mut start = start_args();
    start.rid = 1;
    start.route_key = "binance".into();
    raw.send(&TradeUp::AdapterStart(start));
    loop {
        match raw.recv::<TradeDown>() {
            TradeDown::AdapterReply { .. } => return,
            TradeDown::Heartbeat => {}
            other => panic!("{other:?}"),
        }
    }
}

/// The next event frame as `(seq, replayed, coid)`; control frames are skipped, a `ReplayUnavailable` is
/// returned as `Err(floor)`.
fn next_event(raw: &mut Raw) -> Result<(u64, bool, String), u64> {
    loop {
        match raw.recv_within::<TradeDown>(Duration::from_secs(5)).expect("a frame") {
            TradeDown::Event { seq, replayed, event: Event::OrderAccepted(a), .. } => {
                return Ok((seq, replayed, a.client_order_id));
            }
            TradeDown::ReplayUnavailable { floor } => return Err(floor),
            TradeDown::Heartbeat | TradeDown::CmdAck { .. } => {}
            other => panic!("{other:?}"),
        }
    }
}

#[test]
fn a_client_that_comes_back_with_its_cursor_is_replayed_what_it_missed_in_order() {
    let rig = rig();
    let (mut first, opened) = open_raw(&rig, "tradehub-1", None);
    start_adapter(&mut first);
    let adapter = rig.lane.adapter("binance", None).expect("adapter");
    for n in 1..=3 {
        adapter.emit(accepted(&format!("c{n}")));
    }
    for n in 4..=6 {
        adapter.emit(accepted(&format!("c{n}")));
    }
    for n in 1..=6 {
        assert_eq!(next_event(&mut first), Ok((n, false, format!("c{n}"))), "live, in order");
    }
    // The client folded only the first three before the link dropped (no final detach): the adapter
    // keeps running and the ring keeps what it stamped.
    drop(first);

    let cursor = WireCursor { gw_epoch: opened.gw_epoch, seq: 3, ts_ms: 0 };
    let (mut second, reopened) = open_raw(&rig, "tradehub-1", Some(cursor));
    assert_eq!(reopened.gw_epoch, opened.gw_epoch, "the same process, the same epoch");
    assert_eq!(reopened.event_ring_floor, Some(1));
    for n in 4..=6 {
        assert_eq!(next_event(&mut second), Ok((n, true, format!("c{n}"))), "replayed, in order");
    }
    adapter.emit(accepted("c7"));
    assert_eq!(next_event(&mut second), Ok((7, false, "c7".to_string())), "then live again");
    assert_eq!(rig.lane.starts(), 1, "and the adapter was never restarted");
}

#[test]
fn a_fresh_open_replays_nothing_and_names_the_floor() {
    let rig = rig();
    let (mut first, _opened) = open_raw(&rig, "tradehub-1", None);
    start_adapter(&mut first);
    let adapter = rig.lane.adapter("binance", None).expect("adapter");
    adapter.emit(accepted("old"));
    assert_eq!(next_event(&mut first), Ok((1, false, "old".to_string())));
    drop(first);
    let (mut second, reopened) = open_raw(&rig, "tradehub-2", None);
    assert_eq!(reopened.event_ring_floor, Some(1), "the ring holds seq 1 and says so");
    adapter.emit(accepted("new"));
    assert_eq!(
        next_event(&mut second),
        Ok((2, false, "new".to_string())),
        "no cursor: the history is not forwarded, the live stream is"
    );
}

#[test]
fn a_cursor_of_another_epoch_is_answered_as_no_cursor() {
    // A gatewayhub that restarted is a new process: a new random epoch, an empty ring.
    let first = rig();
    let (_raw, opened) = open_raw(&first, "tradehub-1", None);
    let second = rig();
    let stale = WireCursor { gw_epoch: opened.gw_epoch, seq: 3, ts_ms: 0 };
    let (mut raw, reopened) = open_raw(&second, "tradehub-1", Some(stale));
    assert_ne!(reopened.gw_epoch, opened.gw_epoch);
    match raw.recv_within::<TradeDown>(Duration::from_secs(5)).expect("a frame") {
        TradeDown::ReplayUnavailable { floor } => assert_eq!(floor, 0, "an empty ring"),
        other => panic!("a foreign epoch must be ReplayUnavailable, got {other:?}"),
    }
}

#[test]
fn a_cursor_older_than_the_ring_floor_is_unavailable_and_one_inside_it_replays() {
    let mut config = quick_config();
    config.limits.trade_ring_events = 4;
    let rig = rig_with(ScriptedTrade::new(&["binance"]), config, TradeKit::default());
    let (mut first, opened) = open_raw(&rig, "tradehub-1", None);
    start_adapter(&mut first);
    let adapter = rig.lane.adapter("binance", None).expect("adapter");
    // One at a time: a lease that reads as they come is never behind a ring of four.
    for n in 1..=10 {
        adapter.emit(accepted(&format!("c{n}")));
        assert_eq!(next_event(&mut first), Ok((n, false, format!("c{n}"))));
    }
    drop(first);
    // The ring holds 7..=10. A cursor at 2 needs seq 3: gone.
    let old = WireCursor { gw_epoch: opened.gw_epoch, seq: 2, ts_ms: 0 };
    let (mut raw, reopened) = open_raw(&rig, "tradehub-1", Some(old));
    assert_eq!(reopened.event_ring_floor, Some(7));
    assert_eq!(next_event(&mut raw), Err(7), "the floor, not a silent skip");
    drop(raw);
    // A cursor at 6 needs 7: held.
    let near = WireCursor { gw_epoch: opened.gw_epoch, seq: 6, ts_ms: 0 };
    let (mut raw, _) = open_raw(&rig, "tradehub-1", Some(near));
    for n in 7..=10 {
        assert_eq!(next_event(&mut raw), Ok((n, true, format!("c{n}"))));
    }
}

/// The next frame that is an event or a `ReplayUnavailable`, skipping heartbeats and acks.
fn next_event_frame(raw: &mut Raw) -> TradeDown {
    loop {
        match raw.recv_within::<TradeDown>(Duration::from_secs(5)).expect("a frame") {
            TradeDown::Heartbeat | TradeDown::CmdAck { .. } => {}
            other => return other,
        }
    }
}

#[test]
fn an_event_over_the_frame_ceiling_is_a_long_reject_reason_shrunk_and_still_delivered() {
    // W8b finding 5. A venue's error body can be any size; the reject carrying it is the terminal event of
    // an order. It used to be skipped with a log line, `next_seq` already past it and the client unaware.
    let rig = rig();
    let (mut raw, _opened) = open_raw(&rig, "tradehub-1", None);
    start_adapter(&mut raw);
    let adapter = rig.lane.adapter("binance", None).expect("adapter");
    adapter.emit(Event::OrderRejected(OrderRejected {
        client_order_id: "big".into(),
        reason: "x".repeat(200_000).into(),
        ts: 1,
    }));
    match next_event_frame(&mut raw) {
        TradeDown::Event { seq: 1, event: Event::OrderRejected(r), .. } => {
            assert_eq!(r.client_order_id, "big");
            assert!(r.reason.len() < 8 * 1024, "shrunk to a bounded reason: {}", r.reason.len());
            assert!(r.reason.starts_with("xxx"), "the head of the reason is kept");
        }
        other => panic!("the terminal event must be delivered, got {other:?}"),
    }
}

#[test]
fn an_event_that_cannot_be_shrunk_is_disclosed_as_replay_unavailable_never_skipped_in_silence() {
    let rig = rig();
    let (mut raw, _opened) = open_raw(&rig, "tradehub-1", None);
    start_adapter(&mut raw);
    let adapter = rig.lane.adapter("binance", None).expect("adapter");
    adapter.emit(Event::OrderAccepted(OrderAccepted {
        client_order_id: "huge".into(),
        venue_order_id: Some("v".repeat(100_000).into()),
        ts: 1,
    }));
    adapter.emit(accepted("next"));
    match next_event_frame(&mut raw) {
        TradeDown::ReplayUnavailable { floor } => {
            assert!(
                floor >= 2,
                "the client is told history through seq 1 is not deliverable: {floor}"
            )
        }
        other => panic!("a skipped event must be disclosed, got {other:?}"),
    }
    match next_event_frame(&mut raw) {
        TradeDown::Event { seq: 2, event: Event::OrderAccepted(a), .. } => {
            assert_eq!(a.client_order_id, "next");
        }
        other => panic!("{other:?}"),
    }
}
