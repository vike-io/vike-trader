use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};

use super::*;

fn limits(bytes: usize, frames: usize) -> SinkLimits {
    SinkLimits {
        queue: QueueLimits { bytes, frames, ctrl_frames: 64 },
        max_keys: 8,
        max_unsolicited: 2,
    }
}

fn big() -> SinkLimits {
    limits(1 << 22, 1 << 15)
}

fn tick(ts: i64) -> QuoteTick {
    QuoteTick {
        ts,
        local_ts: 0,
        bid: 1.0,
        ask: 2.0,
        bid_size: 1.0,
        ask_size: 1.0,
        symbol: String::new(),
    }
}

fn bar(ts: i64) -> Bar {
    Bar {
        ts,
        open: 1.0,
        high: 2.0,
        low: 0.5,
        close: 1.5,
        volume: 3.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}

/// Everything the writer would write, in order, until it goes idle.
fn drain(sink: &WireSink) -> Vec<StreamDown> {
    let mut out = Vec::new();
    loop {
        match sink.next(Duration::from_millis(1)) {
            Next::Frame(taken) => {
                out.extend(taken.gaps);
                out.push(taken.item.into_frame());
            }
            Next::Gaps(gaps) => out.extend(gaps),
            Next::Heartbeat => out.push(StreamDown::Heartbeat),
            Next::End(reason) => {
                if let Some(r) = reason {
                    out.push(StreamDown::Bye(r));
                }
                return out;
            }
            Next::Idle => return out,
        }
    }
}

#[test]
fn a_seed_emitted_inside_subscribe_is_delivered_after_its_sub_ok() {
    let sink = WireSink::new("binance", big());
    let opened = sink.open_sub("BTCUSDT").expect("open");
    assert_eq!(opened.kind, OpenKind::Fresh);
    // The venue client emits from INSIDE subscribe_bars, before it returns...
    sink.seed_bars("binance", "BTCUSDT", "1m", vec![bar(0)]);
    sink.stream_status("binance", "BTCUSDT", "1m", StreamStatus::Live { gap_started_ts_ms: None });
    // ...so nothing may be delivered yet.
    assert!(drain(&sink).is_empty(), "a frame of a key reached the writer before its SubOk");
    sink.sub_ok(7, "BTCUSDT", opened);
    let frames = drain(&sink);
    match frames.as_slice() {
        [
            StreamDown::SubOk { sid: 7, k },
            StreamDown::Seed { k: sk, seq: 1, .. },
            StreamDown::Status { k: stk, .. },
        ] => {
            assert_eq!((k, sk, stk), (&opened.k, &opened.k, &opened.k));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_refused_subscription_forgets_its_key_and_its_frames() {
    let sink = WireSink::new("binance", big());
    let opened = sink.open_sub("BTCUSDT").expect("open");
    sink.quote("binance", "BTCUSDT", tick(0));
    sink.sub_failed(3, "BTCUSDT", opened, SubErrKind::Subscribe("no".into()));
    let frames = drain(&sink);
    assert!(
        matches!(frames.as_slice(), [StreamDown::SubErr { sid: 3, .. }]),
        "a refused key's frames reached the wire: {frames:?}"
    );
    // The symbol is free to bind again, under a NEW key (a key is never reused in a session).
    let again = sink.open_sub("BTCUSDT").expect("open again");
    assert_eq!(again.kind, OpenKind::Fresh);
    assert_ne!(again.k, opened.k);
}

#[test]
fn a_second_subscription_on_a_symbol_shares_its_key() {
    let sink = WireSink::new("binance", big());
    let first = sink.open_sub("ETHUSDT").expect("open");
    sink.sub_ok(1, "ETHUSDT", first);
    let second = sink.open_sub("ETHUSDT").expect("open");
    assert_eq!((second.k, second.kind), (first.k, OpenKind::Existing));
    sink.sub_ok(2, "ETHUSDT", second);
    let frames = drain(&sink);
    assert!(matches!(
        frames.as_slice(),
        [StreamDown::SubOk { sid: 1, .. }, StreamDown::SubOk { sid: 2, .. }]
    ));
}

#[test]
fn a_symbol_nobody_asked_for_is_announced_by_a_key_frame_before_its_first_frame() {
    let sink = WireSink::new("polymarket", big());
    sink.quote("polymarket", "TOKEN-A", tick(0));
    sink.quote("polymarket", "TOKEN-A", tick(1));
    let frames = drain(&sink);
    match frames.as_slice() {
        [
            StreamDown::Key { k, symbol },
            StreamDown::Quote { k: qk, seq: 1, .. },
            StreamDown::Quote { seq: 2, .. },
        ] => {
            assert_eq!(symbol, "TOKEN-A");
            assert_eq!(k, qk);
        }
        other => panic!("{other:?}"),
    }
    // A later subscription to it adopts the key rather than minting another.
    let opened = sink.open_sub("TOKEN-A").expect("open");
    assert_eq!(opened.kind, OpenKind::Adopted);
}

#[test]
fn unasked_symbols_past_the_cap_are_dropped_and_counted_not_bound() {
    let sink = WireSink::new("polymarket", big()); // max_unsolicited = 2
    for symbol in ["A", "B", "C", "D"] {
        sink.quote("polymarket", symbol, tick(0));
    }
    let frames = drain(&sink);
    let keys = frames.iter().filter(|f| matches!(f, StreamDown::Key { .. })).count();
    assert_eq!(keys, 2);
    assert_eq!(sink.summary().unbound_dropped, 2, "the dropped frames were not counted");
}

#[test]
fn the_symbol_cap_is_a_transient_budget_refusal() {
    let sink = WireSink::new("binance", limits(1 << 20, 1000)); // max_keys = 8
    for i in 0..8 {
        sink.open_sub(&format!("S{i}")).expect("under the cap");
    }
    match sink.open_sub("S8") {
        Err(SubErrKind::Budget { held: 8, cap: 8, .. }) => {}
        other => panic!("{other:?}"),
    }
}

#[test]
fn the_last_unsubscribe_releases_the_key() {
    let sink = WireSink::new("binance", big());
    let a = sink.open_sub("X").expect("open");
    sink.sub_ok(1, "X", a);
    let b = sink.open_sub("X").expect("open");
    sink.sub_ok(2, "X", b);
    drain(&sink);
    sink.sub_ended("X");
    // One subscription remains: still bound.
    assert_eq!(sink.open_sub("X").expect("still bound").kind, OpenKind::Existing);
    sink.sub_ended("X");
    sink.sub_ended("X");
    // None remain: a new binding is a fresh key.
    let fresh = sink.open_sub("X").expect("rebind");
    assert_eq!(fresh.kind, OpenKind::Fresh);
    assert_ne!(fresh.k, a.k);
}

#[test]
fn finish_ends_the_writer_with_the_reason_and_later_pushes_are_ignored() {
    let sink = WireSink::new("binance", big());
    sink.quote("binance", "A", tick(0));
    sink.finish(Some(ByeReason::ServerStopping));
    sink.finish(Some(ByeReason::ClientClosed)); // the first reason stands
    assert!(sink.is_ended());
    sink.quote("binance", "A", tick(1));
    let frames = drain(&sink);
    assert!(
        matches!(frames.as_slice(), [StreamDown::Bye(ByeReason::ServerStopping)]),
        "{frames:?}"
    );
    assert!(matches!(sink.open_sub("B"), Err(SubErrKind::Draining)));
}

#[test]
fn a_ping_asks_for_a_heartbeat_once() {
    let sink = WireSink::new("binance", big());
    sink.request_heartbeat();
    sink.request_heartbeat();
    let frames = drain(&sink);
    assert!(matches!(frames.as_slice(), [StreamDown::Heartbeat]), "{frames:?}");
}

#[test]
fn a_full_control_lane_ends_the_session_with_the_named_reason() {
    let mut l = big();
    l.queue.ctrl_frames = 3;
    l.max_unsolicited = 100;
    let sink = WireSink::new("binance", l);
    for i in 0..10 {
        sink.stream_status(
            "binance",
            &format!("S{i}"),
            "1m",
            StreamStatus::GapStart { at_ts_ms: i },
        );
    }
    assert!(sink.is_ended());
    let frames = drain(&sink);
    assert!(
        matches!(frames.last(), Some(StreamDown::Bye(ByeReason::ControlLaneOverflow))),
        "{frames:?}"
    );
}

/// **Never blocks the producer (the R3 property, at the sink).** A writer that never runs is a
/// consumer parked forever: the producer's calls must all return, the queue must stay inside its
/// bounds, and what was lost must be announced.
#[test]
fn a_parked_consumer_never_blocks_the_producer_and_the_bounds_hold() {
    let sink = WireSink::new("binance", limits(64 * 1024, 512));
    let started = Instant::now();
    let mut worst = Duration::ZERO;
    for ts in 0..50_000 {
        let t0 = Instant::now();
        sink.quote("binance", "BTCUSDT", tick(ts));
        worst = worst.max(t0.elapsed());
    }
    let total = started.elapsed();
    let (frames, bytes) = sink.queued();
    assert!(
        frames <= 512 && bytes <= 64 * 1024,
        "the bounds were exceeded: {frames} frames / {bytes} bytes"
    );
    assert!(
        total < Duration::from_secs(5),
        "50k pushes took {total:?}: the producer is being blocked"
    );
    assert!(worst < Duration::from_millis(250), "one push took {worst:?}");
    // Un-park: everything pushed is accounted for.
    let out = drain(&sink);
    let delivered = out.iter().filter(|f| matches!(f, StreamDown::Quote { .. })).count() as u64;
    let lost: u64 = out
        .iter()
        .map(|f| match f {
            StreamDown::Gap { dropped, .. } => *dropped,
            _ => 0,
        })
        .sum();
    assert_eq!(
        delivered + lost,
        50_000,
        "{delivered} delivered + {lost} disclosed != 50000 pushed"
    );
    assert!(lost > 0, "the test never overflowed");
}

/// Many producers and one consumer that is deliberately slow: whatever interleaving the scheduler
/// picks, each key's delivered frames and disclosed holes add up to what it pushed, its delivered
/// `seq`s only rise, and a hole is always announced before the frame after it.
#[test]
fn concurrent_producers_and_a_slow_consumer_lose_nothing_silently() {
    const PRODUCERS: usize = 4;
    const PER: i64 = 4_000;
    let mut l = limits(24 * 1024, 64);
    l.max_unsolicited = PRODUCERS;
    let sink = WireSink::new("binance", l);
    let done = Arc::new(AtomicBool::new(false));

    let consumer = {
        let sink = sink.clone();
        let done = Arc::clone(&done);
        std::thread::Builder::new()
            .name("sink-test-wr".into())
            .spawn(move || {
                let mut out: Vec<StreamDown> = Vec::new();
                loop {
                    match sink.next(Duration::from_millis(5)) {
                        Next::Frame(t) => {
                            out.extend(t.gaps);
                            out.push(t.item.into_frame());
                            if out.len().is_multiple_of(16) {
                                std::thread::sleep(Duration::from_micros(200));
                            }
                        }
                        Next::Gaps(g) => out.extend(g),
                        Next::Idle if done.load(Ordering::SeqCst) => return out,
                        Next::Idle | Next::Heartbeat => {}
                        Next::End(_) => return out,
                    }
                }
            })
            .expect("spawn consumer")
    };
    let producers: Vec<_> = (0..PRODUCERS)
        .map(|p| {
            let sink = sink.clone();
            std::thread::Builder::new()
                .name(format!("sink-test-p{p}"))
                .spawn(move || {
                    let symbol = format!("SYM{p}");
                    for ts in 0..PER {
                        sink.quote("binance", &symbol, tick(ts));
                    }
                })
                .expect("spawn producer")
        })
        .collect();
    for p in producers {
        p.join().expect("producer");
    }
    done.store(true, Ordering::SeqCst);
    let out = consumer.join().expect("consumer");

    // Resolve key -> symbol from the Key frames, then audit each key.
    let mut symbols: HashMap<u32, String> = HashMap::new();
    for f in &out {
        if let StreamDown::Key { k, symbol } = f {
            symbols.insert(*k, symbol.clone());
        }
    }
    assert_eq!(symbols.len(), PRODUCERS, "every producer's symbol is bound once");
    for (k, symbol) in &symbols {
        let mut last = 0u64;
        let mut delivered = 0u64;
        let mut lost = 0u64;
        let mut pending_gap_to: Option<u64> = None;
        for f in &out {
            match f {
                StreamDown::Quote { k: fk, seq, .. } if fk == k => {
                    assert!(*seq > last, "{symbol}: seq {seq} after {last}");
                    match pending_gap_to.take() {
                        Some(to) => assert_eq!(
                            *seq, to,
                            "{symbol}: the gap promised {to}, the frame is {seq}"
                        ),
                        None => assert_eq!(*seq, last + 1, "{symbol}: a silent hole before {seq}"),
                    }
                    last = *seq;
                    delivered += 1;
                }
                StreamDown::Gap { k: gk, dropped, from_seq, to_seq, .. } if gk == k => {
                    assert_eq!(
                        *from_seq, last,
                        "{symbol}: a gap that does not start where delivery stopped"
                    );
                    assert!(to_seq > from_seq);
                    lost += dropped;
                    pending_gap_to = Some(*to_seq);
                }
                _ => {}
            }
        }
        // A tail gap names the number the NEXT frame would carry; every other gap was followed by it.
        let total = u64::try_from(PER).expect("fits");
        assert_eq!(
            delivered + lost,
            total,
            "{symbol}: {delivered} delivered + {lost} disclosed != {total}"
        );
    }
}

// ---- The class of a subscription ---------------------------------------------------------------------------

fn depth_in(sink: &WireSink, ts: i64) {
    sink.l2_snapshot(
        "binance",
        "BTCUSDT",
        0.5,
        vec![BookLevel::new(100.0, 1.0)],
        vec![BookLevel::new(100.5, 1.0)],
        ts,
    );
}

fn depth_seqs(frames: &[StreamDown]) -> Vec<u64> {
    frames
        .iter()
        .filter_map(|f| match f {
            StreamDown::Depth { seq, .. } => Some(*seq),
            _ => None,
        })
        .collect()
}

#[test]
fn a_floor_set_before_the_venue_is_asked_covers_a_frame_emitted_inside_subscribe() {
    let sink = WireSink::new("binance", big());
    let opened = sink.open_sub("BTCUSDT").expect("open");
    // The class goes in BEFORE `subscribe_depth` is called...
    assert_eq!(
        sink.set_delivery(opened.k, WireLane::Depth, Delivery::Lossless),
        Delivery::Conflatable,
        "the previous floor"
    );
    // ...so two snapshots the venue emits from inside it are both kept.
    depth_in(&sink, 1);
    depth_in(&sink, 2);
    sink.sub_ok(1, "BTCUSDT", opened);
    let frames = drain(&sink);
    match frames.as_slice() {
        [
            StreamDown::SubOk { sid: 1, .. },
            StreamDown::Delivery { lane: WireLane::Depth, class: WireDelivery::Lossless, .. },
            StreamDown::Depth { seq: 1, .. },
            StreamDown::Depth { seq: 2, .. },
        ] => {}
        other => panic!("{other:?}"),
    }
    assert_eq!(sink.summary().queue.superseded, 0);
}

#[test]
fn a_change_of_class_is_announced_in_frame_order_and_a_no_op_says_nothing() {
    let sink = WireSink::new("binance", big());
    let opened = sink.open_sub("BTCUSDT").expect("open");
    sink.sub_ok(1, "BTCUSDT", opened);
    assert!(drain(&sink).len() == 1, "just the SubOk");

    depth_in(&sink, 1);
    depth_in(&sink, 2); // supersedes 1: conflatable
    assert_eq!(
        sink.set_delivery(opened.k, WireLane::Depth, Delivery::Lossless),
        Delivery::Conflatable
    );
    depth_in(&sink, 3);
    depth_in(&sink, 4);
    // Setting the same floor again, and a floor on a lane that is lossless already, are no-ops.
    assert_eq!(
        sink.set_delivery(opened.k, WireLane::Depth, Delivery::Lossless),
        Delivery::Lossless
    );
    sink.set_delivery(opened.k, WireLane::Quotes, Delivery::Lossless);
    let frames = drain(&sink);
    match frames.as_slice() {
        [
            StreamDown::Depth { seq: 2, .. },
            StreamDown::Delivery { k, lane: WireLane::Depth, class: WireDelivery::Lossless },
            StreamDown::Depth { seq: 3, .. },
            StreamDown::Depth { seq: 4, .. },
        ] => assert_eq!(*k, opened.k),
        other => panic!("{other:?}"),
    }

    // Relaxing is announced too, and the frames after it are latest-wins again.
    assert_eq!(
        sink.set_delivery(opened.k, WireLane::Depth, Delivery::Conflatable),
        Delivery::Lossless
    );
    depth_in(&sink, 5);
    depth_in(&sink, 6);
    let frames = drain(&sink);
    match frames.as_slice() {
        [
            StreamDown::Delivery { class: WireDelivery::Conflatable, .. },
            StreamDown::Depth { seq: 6, .. },
        ] => {}
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_raised_depth_flood_is_disclosed_through_the_gap_ledger_never_dropped_silently() {
    let depth_cost = Payload::Depth {
        tick_size: 0.5,
        bids: vec![BookLevel::new(100.0, 1.0)],
        asks: vec![BookLevel::new(100.5, 1.0)],
        ts: 0,
    }
    .cost();
    let sink = WireSink::new("binance", limits(2 * depth_cost, 1 << 15));
    let opened = sink.open_sub("BTCUSDT").expect("open");
    sink.set_delivery(opened.k, WireLane::Depth, Delivery::Lossless);
    sink.sub_ok(1, "BTCUSDT", opened);
    for ts in 1..=9 {
        depth_in(&sink, ts);
    }
    let frames = drain(&sink);
    let kept = depth_seqs(&frames);
    let lost: u64 = frames
        .iter()
        .filter_map(|f| match f {
            StreamDown::Gap { verb: crate::wire::GapVerb::Depth, dropped, .. } => Some(*dropped),
            _ => None,
        })
        .sum();
    assert_eq!(kept, vec![8, 9]);
    assert_eq!(kept.len() as u64 + lost, 9, "every snapshot is delivered or owed as a Gap");
    assert_eq!(sink.summary().queue.superseded, 0);
    assert_eq!(sink.summary().queue.lossless_evicted, 7);
}
