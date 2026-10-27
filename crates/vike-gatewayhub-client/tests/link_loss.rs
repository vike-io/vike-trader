#![cfg(feature = "data")]
//! **A lost link is disclosed, then repaired.** Wire design B.5 and the module doc of `market`: when
//! the loss is noticed the client synthesizes a `GapStart` for EVERY held key - stamped where that
//! lane's last delivered frame left off, never with the detection clock - redials, replays the held set
//! under the original ids, and synthesizes `Live` for each key on the first frame back (its
//! re-acknowledgement). Recorded tapes therefore carry gap markers, and a consumer that reacts to
//! `GapStart` (a dead-man, a maker) hears about the hop going down.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use vike_data::{DataClient, LiveDataError, LiveDataSink, RecordingSink, SinkCall};
use vike_gatewayhub_client::client::GatewayLink;
use vike_gatewayhub_client::opener::GatewayOpener;
use vike_market_stream::market::RemoteDataClient;
use vike_market_stream::wire::{ByeReason, StreamDown, StreamUp, SubErrKind, SubOp};
use vike_marketdata::{QuoteTick, TradeTick};
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

/// `(symbol, stream word, status)` of every `stream_status` call so far.
fn statuses(sink: &RecordingSink) -> Vec<(String, String, StreamStatus)> {
    sink.recorded()
        .into_iter()
        .filter_map(|c| match c {
            SinkCall::StreamStatus { symbol, stream, status, .. } => Some((symbol, stream, status)),
            _ => None,
        })
        .collect()
}

fn wait_statuses(sink: &RecordingSink, n: usize) -> Vec<(String, String, StreamStatus)> {
    let deadline = Instant::now() + WAIT;
    loop {
        let st = statuses(sink);
        if st.len() >= n {
            return st;
        }
        assert!(Instant::now() < deadline, "only {} of {n} status calls: {st:?}", st.len());
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn quote(ts: i64) -> QuoteTick {
    QuoteTick {
        ts,
        local_ts: ts,
        bid: 1.0,
        ask: 2.0,
        bid_size: 1.0,
        ask_size: 1.0,
        symbol: String::new(),
    }
}

fn trade(ts: i64) -> TradeTick {
    TradeTick {
        ts,
        local_ts: ts,
        price: 1.5,
        size: 1.0,
        is_buyer_maker: false,
        symbol: String::new(),
    }
}

#[test]
fn loss_discloses_gapstart_for_every_held_key_and_replay_closes_each_gap() {
    let peer = FakePeer::start(PeerConfig::new());
    let (mut c, sink) = client(peer.link());
    let opened = vike_model::now_ms();
    c.subscribe_quotes("A").unwrap();
    c.subscribe_trades("A").unwrap();
    c.subscribe_quotes("B").unwrap();
    c.subscribe_book("B").unwrap();
    let first = peer.wait_stream(0, WAIT);

    let before = vike_model::now_ms();
    std::thread::sleep(Duration::from_millis(5)); // a detection stamped "now" would land after `before`
    first.kill();
    let st = wait_statuses(&sink, 4);

    let keys: BTreeSet<(String, String)> =
        st[..4].iter().map(|(s, w, _)| (s.clone(), w.clone())).collect();
    let want: BTreeSet<(String, String)> =
        [("A", "quotes"), ("A", "trades"), ("B", "quotes"), ("B", "book")]
            .iter()
            .map(|(s, w)| ((*s).to_string(), (*w).to_string()))
            .collect();
    assert_eq!(keys, want, "one GapStart per held (symbol, lane), none doubled");
    // No lane delivered a frame, so each gap opens where its lane was acknowledged: after the client
    // started and before the loss - NEVER at the instant of detection (`a_loss_is_stamped_...` below
    // holds the stricter case with frames delivered).
    let mut at_of = std::collections::BTreeMap::new();
    for (symbol, word, status) in &st[..4] {
        let StreamStatus::GapStart { at_ts_ms } = status else {
            panic!("{status:?} before every GapStart")
        };
        assert!(
            *at_ts_ms >= opened && *at_ts_ms <= before,
            "{symbol}/{word}: {at_ts_ms} outside the lane's life [{opened}, {before}]"
        );
        at_of.insert((symbol.clone(), word.clone()), *at_ts_ms);
    }

    // The client redials and replays the held set under the ORIGINAL ids, in id order.
    let second = peer.wait_stream(1, WAIT);
    assert!(FakePeer::wait_until(WAIT, || peer.ups().len() >= 8));
    let replayed: Vec<u64> = peer
        .ups()
        .into_iter()
        .skip(4)
        .filter_map(|u| match u {
            StreamUp::Sub { sid, .. } => Some(sid),
            _ => None,
        })
        .collect();
    assert_eq!(replayed, [1, 2, 3, 4], "replayed under the original sids");

    // …and each key's gap is closed on its re-acknowledgement, echoing the stamp ITS GapStart carried.
    let st = wait_statuses(&sink, 8);
    for (symbol, word, status) in &st[4..8] {
        assert_eq!(
            *status,
            StreamStatus::Live { gap_started_ts_ms: Some(at_of[&(symbol.clone(), word.clone())]) }
        );
    }
    assert_eq!(c.held(), 4);

    // Data flows on the new session under the new session's key.
    let k = second.key_of("A").expect("A was re-acknowledged");
    second.send(&StreamDown::Quote { k, seq: 1, quote: quote(77) });
    let deadline = Instant::now() + WAIT;
    while !sink.recorded().iter().any(
        |c| matches!(c, SinkCall::Quote { symbol, quote, .. } if symbol == "A" && quote.ts == 77),
    ) {
        assert!(Instant::now() < deadline, "no quote after the reconnect");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// **A link loss opens each lane's gap where that lane's last delivered frame left off - not when the
/// loss was noticed.** Rows the daemon produced after the last frame this client read (its queue tail
/// when it ends a too-slow reader's session, or the silence a heartbeat deadline waits out) lie between
/// the two instants; a marker stamped at detection would leave them outside the gap it opens, and
/// replay-by-stamp would read a tape that is continuous right up to the detection. A lane that
/// delivered nothing falls back to where it was acknowledged. The `Live` that closes each gap echoes the
/// stamp its own `GapStart` carried.
#[test]
fn a_loss_is_stamped_with_each_lanes_last_delivered_frame_and_not_the_detection_clock() {
    let peer = FakePeer::start(PeerConfig::new());
    let (mut c, sink) = client(peer.link());
    let opened = vike_model::now_ms();
    c.subscribe_quotes("A").unwrap();
    c.subscribe_trades("A").unwrap();
    c.subscribe_quotes("B").unwrap(); // acknowledged, and never delivers a frame
    let first = peer.wait_stream(0, WAIT);
    let ka = first.key_of("A").expect("A was acknowledged");

    // Ten minutes ago on the clock the frames themselves carry.
    let quoted = opened - 600_000;
    let traded = quoted + 1_500;
    first.send(&StreamDown::Quote { k: ka, seq: 1, quote: quote(quoted) });
    first.send(&StreamDown::Trade { k: ka, seq: 1, trade: trade(traded) });
    assert!(
        FakePeer::wait_until(WAIT, || {
            let calls = sink.recorded();
            calls.iter().any(|c| matches!(c, SinkCall::Quote { .. }))
                && calls.iter().any(|c| matches!(c, SinkCall::Trade { .. }))
        }),
        "the frames were never delivered: {:?}",
        sink.recorded()
    );

    // The daemon ends a reader that fell behind, abandoning its queue tail.
    let before = vike_model::now_ms();
    first.bye(ByeReason::TooSlow { lapses: 4 });
    let st = wait_statuses(&sink, 3);
    let mut gaps = std::collections::BTreeMap::new();
    for (symbol, word, status) in &st[..3] {
        let StreamStatus::GapStart { at_ts_ms } = status else { panic!("{status:?}") };
        gaps.insert((symbol.as_str(), word.as_str()), *at_ts_ms);
    }
    assert_eq!(gaps[&("A", "quotes")], quoted, "stamped with A's last quote, not the detection");
    assert_eq!(gaps[&("A", "trades")], traded, "…and A's last trade, lane by lane");
    let silent = gaps[&("B", "quotes")];
    assert!(
        silent >= opened && silent <= before,
        "B never delivered: its gap opens where it was acknowledged ({silent} not in [{opened}, {before}])"
    );

    // Each lane's `Live` echoes its own stamp when the replay is acknowledged.
    let st = wait_statuses(&sink, 6);
    for (symbol, word, status) in &st[3..6] {
        let want = gaps[&(symbol.as_str(), word.as_str())];
        assert_eq!(
            *status,
            StreamStatus::Live { gap_started_ts_ms: Some(want) },
            "{symbol}/{word}"
        );
    }
}

#[test]
fn an_unsubscribed_key_is_not_replayed() {
    let peer = FakePeer::start(PeerConfig::new());
    let (mut c, _sink) = client(peer.link());
    let a = c.subscribe_quotes("A").unwrap();
    c.subscribe_quotes("B").unwrap();
    c.unsubscribe(a);
    peer.wait_stream(0, WAIT).kill();
    peer.wait_stream(1, WAIT);
    assert!(FakePeer::wait_until(WAIT, || {
        peer.ups().iter().filter(|u| matches!(u, StreamUp::Sub { .. })).count() >= 3
    }));
    let subs: Vec<(u64, SubOp)> = peer
        .ups()
        .into_iter()
        .filter_map(|u| match u {
            StreamUp::Sub { sid, op, .. } => Some((sid, op)),
            _ => None,
        })
        .collect();
    assert_eq!(subs.len(), 3, "A and B, then B alone: {subs:?}");
    assert_eq!(subs[2], (2, SubOp::Quotes { symbol: "B".into() }));
}

#[test]
fn a_deliberate_shutdown_synthesizes_nothing() {
    let peer = FakePeer::start(PeerConfig::new());
    let (mut c, sink) = client(peer.link());
    c.subscribe_quotes("A").unwrap();
    c.shutdown();
    std::thread::sleep(Duration::from_millis(150));
    assert!(
        statuses(&sink).is_empty(),
        "closing a session on purpose is not a gap: {:?}",
        statuses(&sink)
    );
}

/// The deadline is `max(floor, 3 × heartbeat)`: with the daemon stating a one-second heartbeat, a link
/// that says nothing for three seconds is a lost link.
#[test]
fn a_silent_link_is_lost_after_three_stated_heartbeats() {
    let peer = FakePeer::start(PeerConfig::new()); // states heartbeat_ms = 1000 and then says nothing
    let (mut c, sink) = client(peer.link());
    c.subscribe_quotes("A").unwrap();
    let started = Instant::now();
    let st = wait_statuses(&sink, 1);
    let took = started.elapsed();
    assert!(matches!(st[0].2, StreamStatus::GapStart { .. }), "{st:?}");
    assert!(
        took >= Duration::from_millis(2_700) && took < Duration::from_secs(7),
        "the deadline is three stated heartbeats, not the floor (300 ms here): {took:?}"
    );
}

#[test]
fn heartbeats_keep_a_quiet_link_alive_past_the_deadline() {
    let peer = FakePeer::start(PeerConfig::new());
    let (mut c, sink) = client(peer.link());
    c.subscribe_quotes("A").unwrap();
    let s = peer.wait_stream(0, WAIT);
    let started = Instant::now();
    while started.elapsed() < Duration::from_millis(4_200) {
        s.send(&StreamDown::Heartbeat);
        std::thread::sleep(Duration::from_millis(800));
    }
    assert!(statuses(&sink).is_empty(), "heartbeats proved the link alive: {:?}", statuses(&sink));
    assert!(c.is_up());
}

#[test]
fn a_gateway_that_stays_down_leaves_the_gap_open_and_subscribe_a_fast_transient_error() {
    let peer = FakePeer::start(PeerConfig::new());
    let (mut c, sink) = client(peer.link());
    let held = c.subscribe_quotes("A").unwrap();
    let addr = peer.addr_string();
    drop(peer); // the daemon goes away and does not come back
    wait_statuses(&sink, 1);

    let started = Instant::now();
    let Err(LiveDataError::Subscribe(text)) = c.subscribe_trades("Z") else { panic!() };
    assert!(started.elapsed() < Duration::from_millis(300), "{:?}", started.elapsed());
    assert!(text.contains("gatewayhub") && text.contains(&addr), "{text}");
    assert_eq!(c.held(), 1, "the held key waits for a daemon to replay it to");
    c.unsubscribe(held);
    assert_eq!(c.held(), 0);
    let t = Instant::now();
    c.shutdown();
    assert!(t.elapsed() < Duration::from_secs(3));
}

#[test]
fn an_idle_bye_with_nothing_held_parks_the_reader_until_the_next_subscribe() {
    let peer = FakePeer::start(PeerConfig::new());
    let (mut c, sink) = client(peer.link());
    let first = peer.wait_stream(0, WAIT);
    first.bye(ByeReason::SessionIdle);
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(peer.connections(), 1, "no reconnect while nothing wants the link");

    c.subscribe_quotes("A").expect("a subscribe wakes the reader and waits for the dial");
    assert_eq!(peer.connections(), 2);
    assert!(statuses(&sink).is_empty(), "an idle close with nothing held is no gap");
}

/// A peer that answers `A` with `refusal` for every `Sub` it sees once `armed` is raised (the first
/// subscription, before it, is acknowledged) and counts those answers in `refused`.
fn refusing_after_arming(
    armed: &Arc<AtomicBool>,
    refused: &Arc<AtomicUsize>,
    limit: usize,
    refusal: SubErrKind,
) -> FakePeer {
    let (armed, refused) = (Arc::clone(armed), Arc::clone(refused));
    FakePeer::start(PeerConfig::new().with_sub(move |_, op| {
        if armed.load(Ordering::SeqCst) && op.symbol() == "A" {
            let n = refused.fetch_add(1, Ordering::SeqCst);
            if n < limit {
                return SubAnswer::Err(refusal.clone());
            }
        }
        SubAnswer::Ok
    }))
}

/// **A replay the daemon refuses for good leaves that gap open.** `Unsupported` and `SymbolRejected`
/// can never succeed on a retry, so the key is not asked for again, no `Live` is invented for it, and
/// the client carries on with everything else.
#[test]
fn a_replay_refused_for_good_leaves_that_gap_open_and_is_not_asked_again() {
    for refusal in [
        SubErrKind::SymbolRejected("the daemon will not carry that spelling".into()),
        SubErrKind::Unsupported(
            "venue's declared VenueCaps.live_data serves no live quotes".into(),
        ),
    ] {
        let armed = Arc::new(AtomicBool::new(false));
        let refused = Arc::new(AtomicUsize::new(0));
        let peer = refusing_after_arming(&armed, &refused, usize::MAX, refusal.clone());
        let (mut c, sink) = client(peer.link());
        c.subscribe_quotes("A").unwrap();
        let first = peer.wait_stream(0, WAIT);
        armed.store(true, Ordering::SeqCst);
        first.kill();
        wait_statuses(&sink, 1);
        // The replay is refused…
        assert!(FakePeer::wait_until(WAIT, || refused.load(Ordering::SeqCst) >= 1), "{refusal:?}");
        // …and NEVER asked again, however long the backoff has had.
        std::thread::sleep(Duration::from_millis(400));
        assert_eq!(
            refused.load(Ordering::SeqCst),
            1,
            "a permanent refusal was retried: {refusal:?}"
        );
        let st = statuses(&sink);
        assert_eq!(st.len(), 1, "no Live for a key the daemon refused for good: {st:?}");
        assert!(matches!(st[0].2, StreamStatus::GapStart { .. }));
        // …and the client SAYS it holds a subscription that is not streaming (the broker believes it
        // live and never asks again, so this is what a diagnostic reads).
        assert_eq!(c.dead(), 1, "the refused key is reported dead: {refusal:?}");
        assert_eq!(c.held(), 1, "…and still held, so its gap marker stays open");
        // …and the client carries on with everything else.
        assert!(FakePeer::wait_until(WAIT, || c.is_up()), "the rebuilt session comes up");
        c.subscribe_quotes("B").expect("a fresh subscription on the rebuilt session");
    }
}

/// **A replay refused TRANSIENTLY stays held and is retried until the daemon acknowledges it.** The
/// broker believes the subscription is live and will not ask again, so a venue start failure, a full
/// budget or a draining session during the replay must not strand the key: it is re-sent at the
/// client's reconnect backoff on the SAME session (nothing is flapped), and the `SubOk` closes the gap
/// with a `Live` and the stream carries data again.
#[test]
fn a_replay_refused_transiently_is_retried_until_acknowledged_and_the_gap_closes() {
    for refusal in [
        SubErrKind::Subscribe("the venue was briefly unreachable".into()),
        SubErrKind::Budget { scope: "subscriptions per session".into(), held: 2048, cap: 2048 },
        SubErrKind::Draining,
    ] {
        let armed = Arc::new(AtomicBool::new(false));
        let refused = Arc::new(AtomicUsize::new(0));
        // The replay is refused, then its first retry is refused, then the second retry is acknowledged.
        let peer = refusing_after_arming(&armed, &refused, 2, refusal.clone());
        let (mut c, sink) = client(peer.link());
        c.subscribe_quotes("A").unwrap();
        c.subscribe_quotes("B").unwrap();
        let first = peer.wait_stream(0, WAIT);
        armed.store(true, Ordering::SeqCst);
        first.kill();
        // Both keys are gapped at the loss…
        wait_statuses(&sink, 2);

        // …and BOTH are live again: B on the first replay, A once its refusals are over.
        let st = wait_statuses(&sink, 4);
        let lives: Vec<&String> = st
            .iter()
            .filter(|(_, _, s)| matches!(s, StreamStatus::Live { gap_started_ts_ms: Some(_) }))
            .map(|(symbol, _, _)| symbol)
            .collect();
        assert_eq!(lives.len(), 2, "{refusal:?}: {st:?}");
        assert!(lives.iter().any(|s| *s == "A"), "the refused key never went live: {st:?}");
        assert_eq!(
            refused.load(Ordering::SeqCst),
            3,
            "refused twice, then acknowledged ({refusal:?})"
        );
        assert_eq!(peer.connections(), 2, "the retries ran on the rebuilt session: {refusal:?}");
        assert!(c.is_up());
        assert_eq!(c.held(), 2);

        // Data flows on the key whose replay needed its retries.
        let second = peer.wait_stream(1, WAIT);
        let k = second.key_of("A").expect("A was acknowledged on the second session");
        second.send(&StreamDown::Quote { k, seq: 1, quote: quote(4242) });
        let deadline = Instant::now() + WAIT;
        while !sink.recorded().iter().any(
            |c| matches!(c, SinkCall::Quote { symbol, quote, .. } if symbol == "A" && quote.ts == 4242),
        ) {
            assert!(Instant::now() < deadline, "no quote after the retried replay: {refusal:?}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

/// A key unsubscribed while its replay is waiting on a retry is dropped from the retry: nothing is sent
/// for a subscription the broker has given up.
#[test]
fn an_unsubscribed_key_is_not_retried() {
    let armed = Arc::new(AtomicBool::new(false));
    let refused = Arc::new(AtomicUsize::new(0));
    let peer = refusing_after_arming(&armed, &refused, usize::MAX, SubErrKind::Draining);
    let (mut c, sink) = client(peer.link());
    let a = c.subscribe_quotes("A").unwrap();
    let first = peer.wait_stream(0, WAIT);
    armed.store(true, Ordering::SeqCst);
    first.kill();
    wait_statuses(&sink, 1);
    assert!(FakePeer::wait_until(WAIT, || refused.load(Ordering::SeqCst) >= 1));
    c.unsubscribe(a);
    let after = refused.load(Ordering::SeqCst);
    std::thread::sleep(Duration::from_millis(400));
    assert!(
        refused.load(Ordering::SeqCst) <= after + 1,
        "a retry kept coming for an unsubscribed key: {} then {}",
        after,
        refused.load(Ordering::SeqCst)
    );
}
