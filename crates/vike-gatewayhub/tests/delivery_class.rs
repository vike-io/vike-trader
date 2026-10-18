//! **The delivery class of a subscription, at the daemon** (decision 0167, verdict 3). The consumer is a
//! raw socket the test reads at its own pace, and the producer is the venue client's sink, so what is
//! under test is the daemon's queue honouring the floor a subscriber states:
//!
//! * a depth subscription marked lossless is NEVER superseded, and an overflow is a `Gap` naming
//!   `GapVerb::Depth` whose numbers chain with the frames around it (delivered + lost = produced);
//! * the same flood on a depth subscription nobody raised is conflated (latest-wins, no `Gap`) as it
//!   always was;
//! * a class changed on a live session is announced in frame order (`StreamDown::Delivery`);
//! * a `Delivery` frame for an unknown `sid` or a bar lane is a no-op;
//! * a `Sub` the venue refuses gives a joined key's floor back.
//!
//! The queue's own rules are `crates/vike-market-stream/src/queue_tests.rs`; the client's half is
//! `crates/vike-gatewayhub-client/tests/delivery_class.rs`.

#[path = "support/fake_lanes.rs"]
#[allow(dead_code, clippy::allow_attributes)]
mod fake_lanes;
#[path = "support/harness.rs"]
#[allow(dead_code, clippy::allow_attributes)]
mod harness;
#[path = "support/raw_peer.rs"]
#[allow(dead_code, clippy::allow_attributes)]
mod raw_peer;

use std::sync::Arc;
use std::time::Duration;

use vike_data::LiveDataSink;
use vike_gatewayhub::server::limits::ServerConfig;
use vike_hub_proto::auth::Scope;
use vike_market_stream::wire::{
    GapVerb, StreamDown, StreamUp, SubErrKind, SubOp, WireDelivery, WireLane, WireStream,
};
use vike_marketdata::BookLevel;

use fake_lanes::Refusal;
use harness::{Harness, test_keys};
use raw_peer::Raw;

/// Levels per side: a snapshot of this many is ~30 KiB on the wire, so a flood fills the socket buffers
/// after a few hundred frames and the session's writer blocks for real.
const LEVELS: usize = 1_000;

fn config() -> ServerConfig {
    let mut c = ServerConfig::PRODUCTION;
    c.limits.queue_bytes = 128 * 1024;
    c.limits.queue_frames = 256;
    // A tight producer loop outruns even a healthy writer: the lapse budget is raised out of the way.
    c.limits.lapse_budget = 1_000_000;
    c
}

fn levels(base: f64) -> Vec<BookLevel> {
    (0..LEVELS).map(|i| BookLevel::new(base + i as f64, 1.0)).collect()
}

fn depth_in(sink: &Arc<dyn LiveDataSink>, symbol: &str, ts: i64) {
    sink.l2_snapshot("binance", symbol, 0.5, levels(100.0), levels(200.0), ts);
}

fn open(h: &Harness) -> Raw {
    let mut raw = Raw::open(h.addr, &test_keys(), Scope::Read);
    raw.open_stream("binance");
    raw
}

/// `Sub` `op` as `sid` and read until its answer, returning the frames read (the answer last).
fn sub(raw: &mut Raw, sid: u64, op: SubOp, class: Option<WireDelivery>) -> Vec<StreamDown> {
    raw.up(&StreamUp::Sub { sid, op, class });
    raw.down_until(|f| {
        matches!(f, StreamDown::SubOk { sid: s, .. } | StreamDown::SubErr { sid: s, .. } if *s == sid)
    })
}

fn depth_op(symbol: &str) -> SubOp {
    SubOp::Depth { symbol: symbol.into() }
}

/// Read `raw` until `produced` depth snapshots are accounted for (delivered or reported lost),
/// checking the chain rules of a LOSSLESS lane on the way: no silent hole, every `Gap` names the depth
/// verb and chains with the frames on either side. Returns `(delivered, lost, gaps)`.
fn audit_lossless_depth(raw: &mut Raw, k: u32, produced: u64) -> (u64, u64, u64) {
    let (mut last, mut delivered, mut lost, mut gaps) = (0u64, 0u64, 0u64, 0u64);
    let mut promised: Option<u64> = None;
    while delivered + lost < produced {
        match raw.down() {
            StreamDown::Depth { k: fk, seq, .. } if fk == k => {
                assert!(seq > last, "seq {seq} after {last}");
                match promised.take() {
                    Some(to) => assert_eq!(seq, to, "the Gap promised {to} and the frame is {seq}"),
                    None => assert_eq!(seq, last + 1, "a SILENT hole before {seq} (last {last})"),
                }
                last = seq;
                delivered += 1;
            }
            StreamDown::Gap { k: gk, verb, dropped, from_seq, to_seq, stream, .. } if gk == k => {
                assert_eq!(verb, GapVerb::Depth, "the Gap names the verb it is about");
                assert_eq!(from_seq, last, "a Gap that does not start where delivery stopped");
                assert!(to_seq > from_seq && dropped > 0);
                assert_eq!(stream, WireStream::Lane(WireLane::Depth), "the Gap names the lane");
                lost += dropped;
                gaps += 1;
                promised = Some(to_seq);
            }
            StreamDown::Heartbeat => {}
            other => panic!("unexpected frame {other:?}"),
        }
    }
    (delivered, lost, gaps)
}

#[test]
fn a_lossless_depth_subscription_is_never_superseded_and_an_overflow_is_a_depth_gap() {
    const N: i64 = 1_500;
    let h = Harness::start_config(config());
    let mut raw = open(&h);
    let answer = sub(&mut raw, 1, depth_op("BTCUSDT"), Some(WireDelivery::Lossless));
    let k = match answer.as_slice() {
        [StreamDown::SubOk { k, .. }] => *k,
        other => panic!("{other:?}"),
    };
    // The change is announced right behind the SubOk, in frame order.
    match raw.down() {
        StreamDown::Delivery { k: dk, lane: WireLane::Depth, class: WireDelivery::Lossless } => {
            assert_eq!(dk, k);
        }
        other => panic!("the lane's change must be announced: {other:?}"),
    }
    let sink = h.data.session(0).sink.clone();

    // The consumer is parked while the producer floods.
    let started = std::time::Instant::now();
    for ts in 0..N {
        depth_in(&sink, "BTCUSDT", ts);
    }
    assert!(started.elapsed() < Duration::from_secs(30), "the pump was blocked");

    let (delivered, lost, gaps) = audit_lossless_depth(&mut raw, k, N as u64);
    assert_eq!(delivered + lost, N as u64, "every snapshot is delivered or owed as a Gap");
    assert!(
        lost > 0 && gaps > 0,
        "the flood never overflowed ({delivered} delivered): the test proved nothing"
    );
}

#[test]
fn the_same_flood_on_a_depth_subscription_nobody_raised_is_conflated_with_no_gap() {
    const N: i64 = 1_500;
    let h = Harness::start_config(config());
    let mut raw = open(&h);
    let answer = sub(&mut raw, 1, depth_op("BTCUSDT"), None);
    let k = match answer.as_slice() {
        [StreamDown::SubOk { k, .. }] => *k,
        other => panic!("{other:?}"),
    };
    let sink = h.data.session(0).sink.clone();
    for ts in 0..N {
        depth_in(&sink, "BTCUSDT", ts);
    }
    let (mut delivered, mut last) = (0u64, 0u64);
    while last < N as u64 {
        match raw.down() {
            StreamDown::Depth { k: fk, seq, .. } if fk == k => {
                assert!(seq > last);
                last = seq;
                delivered += 1;
            }
            StreamDown::Heartbeat => {}
            other => panic!("a conflatable lane owes no Gap and announces nothing: {other:?}"),
        }
    }
    assert!(delivered < N as u64, "latest-wins skipped frames ({delivered} of {N} delivered)");
}

#[test]
fn a_class_changed_on_a_live_session_is_announced_in_frame_order() {
    let h = Harness::start_config(config());
    let mut raw = open(&h);
    let answer = sub(&mut raw, 1, depth_op("BTCUSDT"), None);
    let k = match answer.as_slice() {
        [StreamDown::SubOk { k, .. }] => *k,
        other => panic!("{other:?}"),
    };
    let sink = h.data.session(0).sink.clone();
    depth_in(&sink, "BTCUSDT", 1);
    assert!(matches!(raw.down(), StreamDown::Depth { seq: 1, .. }));

    raw.up(&StreamUp::Delivery { sid: 1, class: WireDelivery::Lossless });
    match raw.down() {
        StreamDown::Delivery { k: dk, lane: WireLane::Depth, class: WireDelivery::Lossless } => {
            assert_eq!(dk, k);
        }
        other => panic!("{other:?}"),
    }
    // Frames pushed after the announcement are lossless, continuous from where the lane stood.
    for ts in 2..=4 {
        depth_in(&sink, "BTCUSDT", ts);
    }
    for want in 2..=4 {
        match raw.down() {
            StreamDown::Depth { seq, .. } => assert_eq!(seq, want),
            other => panic!("{other:?}"),
        }
    }
    // The same class again says nothing; relaxing is announced.
    raw.up(&StreamUp::Delivery { sid: 1, class: WireDelivery::Lossless });
    raw.up(&StreamUp::Delivery { sid: 1, class: WireDelivery::Conflatable });
    match raw.down() {
        StreamDown::Delivery { class: WireDelivery::Conflatable, .. } => {}
        other => panic!("a repeat must be silent and the relax announced: {other:?}"),
    }
}

#[test]
fn a_delivery_frame_for_an_unknown_sid_or_a_bar_lane_is_a_no_op() {
    let h = Harness::start_config(config());
    let mut raw = open(&h);
    sub(&mut raw, 1, SubOp::Bars { symbol: "BTCUSDT".into(), interval: "1m".into() }, None);
    raw.up(&StreamUp::Delivery { sid: 99, class: WireDelivery::Lossless });
    raw.up(&StreamUp::Delivery { sid: 1, class: WireDelivery::Lossless });
    raw.up(&StreamUp::Ping);
    // The reader handled both frames in order and answered the Ping: nothing was announced or refused.
    assert!(matches!(raw.down(), StreamDown::Heartbeat), "a no-op owes no frame");
}

#[test]
fn a_sub_the_venue_refuses_gives_a_joined_keys_floor_back() {
    let h = Harness::start_config(config());
    let mut raw = open(&h);
    // The key exists (a live quotes subscription) when the depth Sub arrives asking for a raise.
    sub(&mut raw, 1, SubOp::Quotes { symbol: "BTCUSDT".into() }, None);
    h.data.session(0).refuse("BTCUSDT", Refusal::Subscribe("no depth here".into()));
    raw.up(&StreamUp::Sub { sid: 2, op: depth_op("BTCUSDT"), class: Some(WireDelivery::Lossless) });
    let frames = raw.down_until(|f| matches!(f, StreamDown::SubErr { sid: 2, .. }));
    assert!(
        matches!(frames.last(), Some(StreamDown::SubErr { err: SubErrKind::Subscribe(_), .. })),
        "{frames:?}"
    );
    // The raise was announced, and so was giving it back: the lane is conflatable again.
    let classes: Vec<WireDelivery> = frames
        .iter()
        .filter_map(|f| match f {
            StreamDown::Delivery { class, .. } => Some(*class),
            _ => None,
        })
        .collect();
    assert_eq!(classes, vec![WireDelivery::Lossless, WireDelivery::Conflatable], "{frames:?}");
}
