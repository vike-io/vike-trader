#![cfg(feature = "data")]
//! **The delivery class of a subscription over the hop** (decision 0167, verdict 3; T8b). A recorder that
//! writes `l2_snapshot` rows cannot take a superseded frame as "not a loss", so `RemoteDataClient`
//! implements `DataClient::set_delivery` and `is_dead`:
//!
//! * a class set BEFORE the subscribe is remembered per `(symbol, lane)` and rides the `Sub`;
//! * a class set AFTER is a `StreamUp::Delivery` frame keyed by the subscription's `sid`;
//! * a daemon that does not advertise `stream_class` is answered `Unsupported` (the broker warns and
//!   carries on) and is never sent the frame;
//! * a reconnect replays the class in the replayed `Sub`;
//! * once the daemon announces a raised depth lane in frame order (`StreamDown::Delivery`) the reader
//!   checks its `seq` like a quote's, and the conflation counter stops counting its skips;
//! * a held subscription the daemon refused for good on replay reads `is_dead`.
//!
//! The daemon's half (the queue honouring the floor) is `crates/vike-market-stream/src/queue_tests.rs`
//! and `crates/vike-gatewayhub/tests/delivery_class.rs`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use vike_data::delivery::Delivery;
use vike_data::{DataClient, LiveDataError, LiveDataSink, RecordingSink, SinkCall, StreamLabel};
use vike_gatewayhub_client::client::GatewayLink;
use vike_gatewayhub_client::opener::GatewayOpener;
use vike_gatewayhub_client::proto::FEATURE_GW_STREAM_CLASS;
use vike_market_stream::market::RemoteDataClient;
use vike_market_stream::wire::{StreamDown, StreamUp, SubErrKind, WireDelivery, WireLane};
use vike_marketdata::BookLevel;
use vike_model::StreamStatus;

#[allow(dead_code, clippy::allow_attributes)]
#[path = "support/fake_peer.rs"]
mod fake_peer;

use fake_peer::{FakePeer, PeerConfig, SubAnswer};

const WAIT: Duration = Duration::from_secs(8);

fn client(link: GatewayLink) -> (RemoteDataClient, Arc<RecordingSink>) {
    let sink = Arc::new(RecordingSink::default());
    let dyn_sink: Arc<dyn LiveDataSink> = sink.clone();
    (
        RemoteDataClient::new(GatewayOpener::new(link), "binance", dyn_sink)
            .expect("the reader thread"),
        sink,
    )
}

/// A peer that advertises everything the client vocabulary knows EXCEPT the delivery class: a daemon
/// that predates it.
fn peer_without_stream_class() -> FakePeer {
    let features: Vec<String> = fake_peer::features(&["binance"], &[])
        .into_iter()
        .filter(|f| f != "stream_class")
        .collect();
    assert!(!features.iter().any(|f| f == FEATURE_GW_STREAM_CLASS));
    FakePeer::start(PeerConfig::new().with_features(features))
}

/// The `(sid, class)` of every `Sub` the peer saw, in order.
fn subs(peer: &FakePeer) -> Vec<(u64, Option<WireDelivery>)> {
    peer.ups()
        .into_iter()
        .filter_map(|u| match u {
            StreamUp::Sub { sid, class, .. } => Some((sid, class)),
            _ => None,
        })
        .collect()
}

/// The `(sid, class)` of every `Delivery` frame the peer saw, in order.
fn deliveries(peer: &FakePeer) -> Vec<(u64, WireDelivery)> {
    peer.ups()
        .into_iter()
        .filter_map(|u| match u {
            StreamUp::Delivery { sid, class } => Some((sid, class)),
            _ => None,
        })
        .collect()
}

fn wait_for_deliveries(peer: &FakePeer, n: usize) -> Vec<(u64, WireDelivery)> {
    assert!(
        FakePeer::wait_until(WAIT, || deliveries(peer).len() >= n),
        "only {} of {n} Delivery frames: {:?}",
        deliveries(peer).len(),
        deliveries(peer)
    );
    deliveries(peer)
}

fn statuses(sink: &RecordingSink) -> Vec<(String, StreamStatus)> {
    sink.recorded()
        .into_iter()
        .filter_map(|c| match c {
            SinkCall::StreamStatus { stream, status, .. } => Some((stream, status)),
            _ => None,
        })
        .collect()
}

fn depths(sink: &RecordingSink) -> usize {
    sink.recorded().iter().filter(|c| matches!(c, SinkCall::L2Snapshot { .. })).count()
}

fn depth_frame(k: u32, seq: u64) -> StreamDown {
    StreamDown::Depth {
        k,
        seq,
        tick_size: 0.5,
        bids: vec![BookLevel::new(100.0, 1.0)],
        asks: vec![BookLevel::new(100.5, 1.0)],
        ts: i64::try_from(seq).expect("small") * 1_000,
    }
}

fn wait_depths(sink: &RecordingSink, n: usize) {
    assert!(
        FakePeer::wait_until(WAIT, || depths(sink) >= n),
        "only {} of {n} depth calls",
        depths(sink)
    );
}

#[test]
fn a_class_set_before_the_subscribe_rides_the_sub() {
    let peer = FakePeer::start(PeerConfig::new());
    let (mut c, _sink) = client(peer.link());
    c.set_delivery("A", StreamLabel::Depth, Delivery::Lossless).expect("a daemon that serves it");
    let a = c.subscribe_depth("A").unwrap();
    let b = c.subscribe_depth("B").unwrap();
    // A quotes subscription on the raised symbol is another lane: nothing to carry.
    let q = c.subscribe_quotes("A").unwrap();
    assert_eq!(
        subs(&peer),
        vec![(a.0, Some(WireDelivery::Lossless)), (b.0, None), (q.0, None)],
        "only the (symbol, lane) the broker named carries a class"
    );
    assert!(deliveries(&peer).is_empty(), "set before the subscribe is NOT a separate frame");
}

#[test]
fn a_class_changed_on_a_live_subscription_is_a_delivery_frame_keyed_by_its_sid() {
    let peer = FakePeer::start(PeerConfig::new());
    let (mut c, _sink) = client(peer.link());
    let a = c.subscribe_depth("A").unwrap();
    let b = c.subscribe_depth("B").unwrap();
    let q = c.subscribe_quotes("A").unwrap();
    assert_eq!(subs(&peer), vec![(a.0, None), (b.0, None), (q.0, None)]);

    c.set_delivery("A", StreamLabel::Depth, Delivery::Lossless).unwrap();
    assert_eq!(wait_for_deliveries(&peer, 1), vec![(a.0, WireDelivery::Lossless)]);
    // The recorder leaves: the floor is cleared on the same subscription.
    c.set_delivery("A", StreamLabel::Depth, Delivery::Conflatable).unwrap();
    assert_eq!(
        wait_for_deliveries(&peer, 2),
        vec![(a.0, WireDelivery::Lossless), (a.0, WireDelivery::Conflatable)],
        "B's depth and A's quotes were never touched"
    );
}

#[test]
fn a_daemon_without_stream_class_declines_and_is_never_sent_the_frame() {
    // After a session has opened the answer is known at once.
    let peer = peer_without_stream_class();
    let (mut c, _sink) = client(peer.link());
    let a = c.subscribe_depth("A").unwrap();
    let Err(LiveDataError::Unsupported(why)) =
        c.set_delivery("A", StreamLabel::Depth, Delivery::Lossless)
    else {
        panic!("a daemon without the feature must decline");
    };
    assert!(why.contains("stream_class"), "{why}");
    assert!(deliveries(&peer).is_empty(), "nothing was sent to a daemon that ignores it");
    // Clearing a floor that was never raised never fails, and the subscription is untouched.
    c.set_delivery("A", StreamLabel::Depth, Delivery::Conflatable).expect("nothing to relax");
    assert_eq!(subs(&peer), vec![(a.0, None)]);
}

#[test]
fn a_class_set_before_the_first_session_waits_for_the_dial_and_is_declined_the_same_way() {
    let peer = peer_without_stream_class();
    let (mut c, _sink) = client(peer.link());
    // A fresh client: no session has opened yet, and the answer depends on the daemon.
    let started = Instant::now();
    let Err(LiveDataError::Unsupported(_)) =
        c.set_delivery("A", StreamLabel::Depth, Delivery::Lossless)
    else {
        panic!("the dial already in flight answers it");
    };
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "bounded by the first dial: {:?}",
        started.elapsed()
    );
    // The declined floor was NOT remembered: the next subscribe carries nothing.
    let a = c.subscribe_depth("A").unwrap();
    assert_eq!(subs(&peer), vec![(a.0, None)]);
}

#[test]
fn a_reconnect_replays_the_class_and_a_cleared_class_is_not_replayed() {
    let peer = FakePeer::start(PeerConfig::new());
    let (mut c, _sink) = client(peer.link());
    c.set_delivery("A", StreamLabel::Depth, Delivery::Lossless).unwrap();
    let a = c.subscribe_depth("A").unwrap();
    let first = peer.wait_stream(0, WAIT);
    first.kill();
    peer.wait_stream(1, WAIT);
    assert!(FakePeer::wait_until(WAIT, || subs(&peer).len() >= 2));
    assert_eq!(
        subs(&peer),
        vec![(a.0, Some(WireDelivery::Lossless)), (a.0, Some(WireDelivery::Lossless))],
        "the replayed Sub restores the class under the original sid"
    );

    // Relaxed while up, then lost again: the replay carries none.
    assert!(FakePeer::wait_until(WAIT, || c.is_up()));
    c.set_delivery("A", StreamLabel::Depth, Delivery::Conflatable).unwrap();
    wait_for_deliveries(&peer, 1);
    peer.wait_stream(1, WAIT).kill();
    peer.wait_stream(2, WAIT);
    assert!(FakePeer::wait_until(WAIT, || subs(&peer).len() >= 3));
    assert_eq!(subs(&peer)[2], (a.0, None));
}

#[test]
fn an_unsubscribed_lane_keeps_no_floor() {
    let peer = FakePeer::start(PeerConfig::new());
    let (mut c, _sink) = client(peer.link());
    c.set_delivery("A", StreamLabel::Depth, Delivery::Lossless).unwrap();
    let a = c.subscribe_depth("A").unwrap();
    c.unsubscribe(a);
    let again = c.subscribe_depth("A").unwrap();
    assert_eq!(
        subs(&peer),
        vec![(a.0, Some(WireDelivery::Lossless)), (again.0, None)],
        "the floor was the old subscription's; the broker states it afresh"
    );
}

/// **A raised lane is checked like a quote's, from the daemon's marker on.** Before it, the lane is
/// conflatable and a `seq` jump is the daemon superseding (counted, never a gap); after it, a jump with
/// no `Gap` is a silent loss and is disclosed; and a relax restarts the conflation count so the whole
/// lossless era is not counted as superseded.
#[test]
fn a_raised_depth_lane_is_checked_for_continuity_from_the_daemons_marker_and_not_counted() {
    let peer = FakePeer::start(PeerConfig::new());
    let (mut c, sink) = client(peer.link());
    c.subscribe_depth("A").unwrap();
    let s = peer.wait_stream(0, WAIT);
    let k = s.key_of("A").expect("acknowledged");

    // Conflatable era: 2 and 3 were superseded.
    s.send(&depth_frame(k, 1));
    s.send(&depth_frame(k, 4));
    wait_depths(&sink, 2);
    assert_eq!(c.conflated(), 2);
    assert!(statuses(&sink).is_empty());

    // The marker, then a clean lossless era: the jump from 4 to 10 BEFORE it is not held against it.
    s.send(&StreamDown::Delivery { k, lane: WireLane::Depth, class: WireDelivery::Lossless });
    s.send(&depth_frame(k, 10));
    s.send(&depth_frame(k, 11));
    wait_depths(&sink, 4);
    assert_eq!(c.conflated(), 2, "a lossless lane's frames are not conflation");
    assert!(statuses(&sink).is_empty(), "{:?}", statuses(&sink));

    // A silent jump on the raised lane is a disclosed hole, exactly as a quote's would be.
    s.send(&depth_frame(k, 14));
    wait_depths(&sink, 5);
    let st = statuses(&sink);
    assert_eq!(st.len(), 2, "GapStart then Live: {st:?}");
    assert!(matches!(st[0], (ref w, StreamStatus::GapStart { .. }) if w == "depth"), "{st:?}");
    assert_eq!(c.conflated(), 2, "still not counted");

    // The daemon's own Gap names the verb and is disclosed too.
    s.send(&StreamDown::Gap {
        k,
        verb: vike_market_stream::wire::GapVerb::Depth,
        stream: vike_market_stream::wire::WireStream::Lane(WireLane::Depth),
        dropped: 3,
        from_seq: 14,
        to_seq: 18,
        from_ts_ms: 14_000,
    });
    s.send(&depth_frame(k, 18));
    wait_depths(&sink, 6);
    assert_eq!(
        statuses(&sink).len(),
        4,
        "the Gap was a hole; seq 18 followed it with no second one"
    );

    // Relaxed: latest-wins again, and the lossless era's numbers are not counted as superseded.
    s.send(&StreamDown::Delivery { k, lane: WireLane::Depth, class: WireDelivery::Conflatable });
    s.send(&depth_frame(k, 19));
    s.send(&depth_frame(k, 25));
    wait_depths(&sink, 8);
    assert_eq!(c.conflated(), 2 + 5, "19 starts afresh; 20..=24 were superseded after the relax");
}

/// **`is_dead` reads `Phase::Dead`.** The daemon refuses a replayed subscription for good, so the
/// subscription is not streaming and nothing would ever ask again: the broker's `is_dead` check is the
/// one thing that does.
#[test]
fn a_held_subscription_the_daemon_refused_for_good_on_replay_reads_dead() {
    let armed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let armed_in = Arc::clone(&armed);
    let peer = FakePeer::start(PeerConfig::new().with_sub(move |_, op| {
        if armed_in.load(std::sync::atomic::Ordering::SeqCst) && op.symbol() == "A" {
            return SubAnswer::Err(SubErrKind::Unsupported("no live depth feed".into()));
        }
        SubAnswer::Ok
    }));
    let (mut c, sink) = client(peer.link());
    let a = c.subscribe_depth("A").unwrap();
    let b = c.subscribe_depth("B").unwrap();
    assert!(!c.is_dead(a) && !c.is_dead(b), "healthy subscriptions");
    let first = peer.wait_stream(0, WAIT);
    armed.store(true, std::sync::atomic::Ordering::SeqCst);
    first.kill();
    assert!(FakePeer::wait_until(WAIT, || c.is_dead(a)), "the refused replay reads dead");
    assert!(FakePeer::wait_until(WAIT, || statuses(&sink).len() >= 3), "both gapped, B went live");
    assert!(!c.is_dead(b), "B was replayed fine");
    assert!(!c.is_dead(vike_data::SubscriptionId(99)), "an id the client never held is not dead");
    assert_eq!(c.dead(), 1);
    // Dropping the dead subscription (what the broker does) makes it a stranger.
    c.unsubscribe(a);
    assert!(!c.is_dead(a));
}
