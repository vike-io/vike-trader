use vike_marketdata::{Bar, BookLevel, QuoteTick, TradeTick};

use super::*;

fn limits(bytes: usize, frames: usize, ctrl: usize) -> QueueLimits {
    QueueLimits { bytes, frames, ctrl_frames: ctrl }
}

fn quote(ts: i64) -> Payload {
    Payload::Quote(QuoteTick {
        ts,
        local_ts: 0,
        bid: 1.0,
        ask: 2.0,
        bid_size: 1.0,
        ask_size: 1.0,
        symbol: String::new(),
    })
}

fn trade(ts: i64) -> Payload {
    Payload::Trade(TradeTick {
        ts,
        local_ts: 0,
        price: 1.5,
        size: 1.0,
        is_buyer_maker: false,
        symbol: String::new(),
    })
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

/// The cost of one `quote` frame, so a bound can be set to hold exactly `n` of them.
fn quote_cost() -> usize {
    quote(0).cost()
}

/// Drain the queue, returning every frame in wire order (gaps included).
fn drain(q: &mut Queue) -> Vec<StreamDown> {
    let mut out = Vec::new();
    loop {
        match q.take() {
            Some(t) => {
                out.extend(t.gaps);
                out.push(t.item.into_frame());
            }
            None => {
                let tail = q.tail_gaps();
                if tail.is_empty() {
                    return out;
                }
                out.extend(tail);
            }
        }
    }
}

fn quote_seqs(frames: &[StreamDown]) -> Vec<u64> {
    frames
        .iter()
        .filter_map(|f| match f {
            StreamDown::Quote { seq, .. } => Some(*seq),
            _ => None,
        })
        .collect()
}

fn gaps(frames: &[StreamDown]) -> Vec<(u64, u64, u64)> {
    frames
        .iter()
        .filter_map(|f| match f {
            StreamDown::Gap { dropped, from_seq, to_seq, .. } => {
                Some((*dropped, *from_seq, *to_seq))
            }
            _ => None,
        })
        .collect()
}

#[test]
fn seq_is_per_key_and_verb_starts_at_one_and_rises_by_one() {
    let mut q = Queue::new(limits(1 << 20, 1000, 100));
    for ts in 0..3 {
        q.push_data(1, quote(ts));
    }
    q.push_data(1, trade(0));
    q.push_data(2, quote(0));
    let frames = drain(&mut q);
    let mut by: Vec<(char, u32, u64)> = Vec::new();
    for f in &frames {
        match f {
            StreamDown::Quote { k, seq, .. } => by.push(('q', *k, *seq)),
            StreamDown::Trade { k, seq, .. } => by.push(('t', *k, *seq)),
            other => panic!("unexpected {other:?}"),
        }
    }
    assert_eq!(
        by,
        vec![('q', 1, 1), ('q', 1, 2), ('q', 1, 3), ('t', 1, 1), ('q', 2, 1)],
        "each (key, verb) counts on its own from 1, in emission order"
    );
}

/// **The accounting law.** Whatever the overflow does, frames delivered plus frames the gaps report
/// lost equal the frames pushed, every hole is announced before the first survivor after it, and the
/// numbers the gaps name chain with the numbers of the frames around them.
#[test]
fn a_flood_over_a_small_bound_is_fully_accounted_for() {
    let cost = quote_cost();
    let mut q = Queue::new(limits(cost * 4, 1000, 100));
    let pushed = 50u64;
    for ts in 0..pushed as i64 {
        q.push_data(7, quote(ts));
        assert!(q.data_bytes() <= cost * 4, "the byte bound was exceeded");
    }
    assert_eq!(q.data_frames(), 4);
    let frames = drain(&mut q);
    let seqs = quote_seqs(&frames);
    let lost: u64 = gaps(&frames).iter().map(|g| g.0).sum();
    assert_eq!(seqs.len() as u64 + lost, pushed, "a frame is neither delivered nor disclosed");
    // The newest survive: drop-OLDEST for the key that is filling the queue.
    assert_eq!(seqs, vec![47, 48, 49, 50]);
    // One hole, announced before its first survivor, chained from "nothing delivered" (0) to 47.
    assert_eq!(gaps(&frames), vec![(46, 0, 47)]);
    let gap_at = frames.iter().position(|f| matches!(f, StreamDown::Gap { .. })).expect("a gap");
    let first_quote_at =
        frames.iter().position(|f| matches!(f, StreamDown::Quote { .. })).expect("a quote");
    assert!(gap_at < first_quote_at, "the Gap must precede the first post-hole frame");
    assert_eq!(q.stats().lossless_evicted, 46);
}

#[test]
fn a_gap_chains_from_the_last_delivered_frame() {
    let cost = quote_cost();
    let mut q = Queue::new(limits(cost * 2, 1000, 100));
    q.push_data(1, quote(0));
    q.push_data(1, quote(1));
    // Deliver 1 and 2.
    let first = drain(&mut q);
    assert_eq!(quote_seqs(&first), vec![1, 2]);
    // Overflow again: 3 is evicted when 5 arrives behind 4, with 6 then evicting 4.
    for ts in 2..6 {
        q.push_data(1, quote(ts));
    }
    let second = drain(&mut q);
    assert_eq!(quote_seqs(&second), vec![5, 6]);
    assert_eq!(
        gaps(&second),
        vec![(2, 2, 5)],
        "from the last delivered (2) to the first survivor (5)"
    );
}

#[test]
fn a_lost_tail_is_disclosed_when_the_queue_goes_idle_and_the_next_frame_chains() {
    let cost = quote_cost();
    // The bound holds three quotes; a frame bigger than the whole bound is refused alone.
    let mut q = Queue::new(limits(cost * 3, 1000, 100));
    q.push_data(3, quote(0));
    let big = Payload::Seed { interval: "1m".into(), bars: (0..64).map(bar).collect() };
    assert!(big.cost() > cost * 3);
    // A seed larger than the bound is refused at once.
    let outcome = q.push_data(3, big);
    assert_eq!(outcome, Pushed::Evicted(1));
    let frames = drain(&mut q);
    // The quote went out; the lost seed had no survivor after it, so the tail flush discloses it.
    assert_eq!(quote_seqs(&frames), vec![1]);
    let tail: Vec<_> = frames
        .iter()
        .filter_map(|f| match f {
            StreamDown::Gap { verb, stream, dropped, from_seq, to_seq, .. } => {
                Some((*verb, stream.clone(), *dropped, *from_seq, *to_seq))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        tail,
        vec![(GapVerb::Seed, WireStream::Bar("1m".into()), 1, 0, 2)],
        "the tail gap names the lane, the count and the number the NEXT seed will carry"
    );
    // The next seed carries exactly the number the gap promised, and is not preceded by a second gap.
    q.push_data(3, Payload::Seed { interval: "1m".into(), bars: vec![bar(1)] });
    let next = drain(&mut q);
    assert!(gaps(&next).is_empty());
    assert!(matches!(next.as_slice(), [StreamDown::Seed { seq: 2, .. }]), "{next:?}");
}

#[test]
fn the_key_that_fills_the_queue_pays_not_its_neighbour() {
    let cost = quote_cost();
    let mut q = Queue::new(limits(cost * 3, 1000, 100));
    q.push_data(2, quote(0)); // the quiet key's only frame
    for ts in 0..10 {
        q.push_data(1, quote(ts)); // the bursty key
    }
    let frames = drain(&mut q);
    let quiet: Vec<_> =
        frames.iter().filter(|f| matches!(f, StreamDown::Quote { k: 2, .. })).collect();
    assert_eq!(quiet.len(), 1, "the quiet key lost a frame to its neighbour's burst");
    assert!(gaps(&frames).iter().all(|g| g.0 > 0));
    let bursty: Vec<u64> = frames
        .iter()
        .filter_map(|f| match f {
            StreamDown::Quote { k: 1, seq, .. } => Some(*seq),
            _ => None,
        })
        .collect();
    assert_eq!(bursty, vec![9, 10], "the bursty key keeps its NEWEST frames");
}

#[test]
fn with_nothing_of_its_own_queued_the_pusher_evicts_the_globally_oldest() {
    let cost = quote_cost();
    let mut q = Queue::new(limits(cost * 2, 1000, 100));
    q.push_data(1, quote(0));
    q.push_data(2, quote(0));
    // Key 3 has nothing queued: the globally oldest lossless frame (key 1's) goes.
    q.push_data(3, quote(0));
    let frames = drain(&mut q);
    let ks: Vec<u32> = frames
        .iter()
        .filter_map(|f| match f {
            StreamDown::Quote { k, .. } => Some(*k),
            _ => None,
        })
        .collect();
    assert_eq!(ks, vec![2, 3]);
    // Key 1 lost its only frame and has no survivor, so the loss is disclosed at the tail, naming the
    // number its next frame will carry.
    assert_eq!(gaps(&frames), vec![(1, 0, 2)]);
}

#[test]
fn a_conflatable_frame_supersedes_its_slot_and_never_a_lossless_one() {
    let mut q = Queue::new(limits(1 << 20, 1000, 100));
    q.push_data(1, Payload::BarClose { interval: "1m".into(), bar: bar(0) });
    assert_eq!(
        q.push_data(1, Payload::Forming { interval: "1m".into(), bar: bar(1) }),
        Pushed::Queued
    );
    assert_eq!(
        q.push_data(1, Payload::Forming { interval: "1m".into(), bar: bar(2) }),
        Pushed::Superseded
    );
    assert_eq!(q.data_frames(), 2, "one close and ONE forming snapshot");
    let frames = drain(&mut q);
    match frames.as_slice() {
        [StreamDown::BarClose { seq: 1, .. }, StreamDown::Forming { seq: 2, bar, .. }] => {
            assert_eq!(bar.ts, 2, "latest-wins");
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(q.stats().superseded, 1);
    assert!(gaps(&frames).is_empty(), "a superseded conflatable frame is not a loss");
}

#[test]
fn a_superseding_frame_takes_the_position_of_its_own_emission() {
    // Forming(t), Close(t), Forming(t+1): the survivor of the forming slot must come AFTER the close.
    let mut q = Queue::new(limits(1 << 20, 1000, 100));
    q.push_data(1, Payload::Forming { interval: "1m".into(), bar: bar(10) });
    q.push_data(1, Payload::BarClose { interval: "1m".into(), bar: bar(10) });
    q.push_data(1, Payload::Forming { interval: "1m".into(), bar: bar(20) });
    let frames = drain(&mut q);
    assert!(
        matches!(
            frames.as_slice(),
            [StreamDown::BarClose { .. }, StreamDown::Forming { bar, .. }] if bar.ts == 20
        ),
        "{frames:?}"
    );
}

#[test]
fn control_frames_are_never_evicted_and_a_full_lane_says_so() {
    let cost = quote_cost();
    let mut q = Queue::new(limits(cost * 2, 1000, 3));
    assert!(q.push_ctrl(Control::Key { k: 1, symbol: "A".into() }));
    assert!(q.push_ctrl(Control::Status {
        k: 1,
        stream: WireStream::Lane(WireLane::Quotes),
        status: StreamStatus::GapStart { at_ts_ms: 5 },
    }));
    for ts in 0..20 {
        q.push_data(1, quote(ts)); // a flood of data
    }
    assert_eq!(q.ctrl_frames(), 2, "an overflow touched the control lane");
    assert!(q.push_ctrl(Control::SubOk { sid: 1, k: 1 }));
    assert!(!q.push_ctrl(Control::SubOk { sid: 2, k: 1 }), "a full control lane must refuse");
    let frames = drain(&mut q);
    assert!(frames.iter().any(|f| matches!(f, StreamDown::Status { .. })), "the status was lost");
}

#[test]
fn a_status_is_delivered_in_emission_order_with_the_data_around_it() {
    let mut q = Queue::new(limits(1 << 20, 1000, 100));
    q.push_data(1, quote(0));
    q.push_ctrl(Control::Status {
        k: 1,
        stream: WireStream::Lane(WireLane::Quotes),
        status: StreamStatus::GapStart { at_ts_ms: 1 },
    });
    q.push_data(1, quote(2));
    let frames = drain(&mut q);
    assert!(
        matches!(
            frames.as_slice(),
            [
                StreamDown::Quote { seq: 1, .. },
                StreamDown::Status { .. },
                StreamDown::Quote { seq: 2, .. }
            ]
        ),
        "{frames:?}"
    );
}

#[test]
fn a_held_key_is_delivered_only_after_its_sub_ok() {
    let mut q = Queue::new(limits(1 << 20, 1000, 100));
    q.hold(5);
    // A seed emitted from INSIDE subscribe_*, before the SubOk exists, and another key's frame.
    q.push_data(5, Payload::Seed { interval: "1m".into(), bars: vec![bar(0)] });
    q.push_data(6, quote(0));
    q.push_ctrl(Control::Status {
        k: 5,
        stream: WireStream::Bar("1m".into()),
        status: StreamStatus::Live { gap_started_ts_ms: None },
    });
    // The writer sees only the unheld key.
    let first = q.take().expect("the unheld key is deliverable");
    assert!(matches!(first.item, Item::Data { k: 6, .. }));
    assert!(q.take().is_none(), "a held key's frame was delivered before its SubOk");
    // The SubOk is queued behind the held frames by stamp, and still goes FIRST.
    q.push_ctrl(Control::SubOk { sid: 1, k: 5 });
    let ok = q.take().expect("SubOk");
    assert!(matches!(ok.item, Item::Ctrl(Control::SubOk { k: 5, .. })));
    assert!(!q.is_held(5), "dequeuing the SubOk releases the key");
    let rest = drain(&mut q);
    assert!(matches!(
        rest.as_slice(),
        [StreamDown::Seed { k: 5, seq: 1, .. }, StreamDown::Status { k: 5, .. }]
    ));
}

#[test]
fn a_refused_subscription_purges_its_held_frames_and_numbers() {
    let mut q = Queue::new(limits(1 << 20, 1000, 100));
    q.hold(9);
    q.push_data(9, quote(0));
    q.push_data(9, quote(1));
    q.purge(9);
    assert!(q.is_empty() && !q.is_held(9));
    assert_eq!(q.data_bytes(), 0);
    // A later binding of key 9 starts its numbers afresh.
    q.push_data(9, quote(2));
    let frames = drain(&mut q);
    assert_eq!(quote_seqs(&frames), vec![1]);
}

#[test]
fn a_non_finite_row_never_reaches_the_wire_and_a_lossless_one_is_disclosed() {
    let mut q = Queue::new(limits(1 << 20, 1000, 100));
    q.push_data(1, quote(0));
    let bad = Payload::Quote(QuoteTick {
        ts: 1,
        local_ts: 0,
        bid: f64::NAN,
        ask: 2.0,
        bid_size: 1.0,
        ask_size: 1.0,
        symbol: String::new(),
    });
    assert_eq!(q.push_data(1, bad), Pushed::Refused);
    // A conflatable row is refused too, but it owes nothing: latest-wins.
    assert_eq!(q.push_data(1, Payload::Mark { px: f64::INFINITY, ts: 0 }), Pushed::Refused);
    q.push_data(1, quote(2));
    let frames = drain(&mut q);
    assert_eq!(quote_seqs(&frames), vec![1, 3]);
    assert_eq!(gaps(&frames), vec![(1, 1, 3)], "the refused quote is a disclosed hole");
    assert_eq!(q.stats().nonfinite, 2);
}

/// The sink scans a payload for non-finite floats BEFORE it takes its lock and hands the verdict in
/// (`push_data_checked`): the queue acts on the verdict it is given.
#[test]
fn the_queue_acts_on_the_finite_verdict_the_caller_scanned() {
    let mut q = Queue::new(limits(1 << 20, 1000, 100));
    assert_eq!(q.push_data_checked(1, quote(0), true), Pushed::Queued);
    assert_eq!(q.push_data_checked(1, quote(1), false), Pushed::Refused);
    assert_eq!(q.push_data_checked(1, quote(2), true), Pushed::Queued);
    let frames = drain(&mut q);
    assert_eq!(quote_seqs(&frames), vec![1, 3]);
    assert_eq!(gaps(&frames), vec![(1, 1, 3)], "the refused quote is a disclosed hole");
    assert_eq!(q.stats().nonfinite, 1);
}

#[test]
fn a_frame_the_writer_cannot_send_becomes_a_hole() {
    let mut q = Queue::new(limits(1 << 20, 1000, 100));
    q.push_data(1, quote(0));
    q.push_data(1, quote(1));
    q.push_data(1, quote(2));
    let first = q.take().expect("first");
    assert!(first.undo.is_some());
    let second = q.take().expect("second");
    // The writer found the second frame over the ceiling.
    q.note_unsendable(second.undo.expect("a lossless frame carries its undo"));
    let rest = drain(&mut q);
    assert_eq!(quote_seqs(&rest), vec![3]);
    assert_eq!(gaps(&rest), vec![(1, 1, 3)], "from the last frame actually delivered");
    assert_eq!(q.stats().unsendable, 1);
}

#[test]
fn holes_in_two_streams_of_one_verb_are_disclosed_per_stream() {
    // Seeds of two intervals share one (k, Seed) sequence; the Gap must name the right interval each.
    let big = |interval: &str| Payload::Seed {
        interval: interval.to_string(),
        bars: (0..64).map(bar).collect(),
    };
    let one = Payload::Seed { interval: "1m".into(), bars: vec![bar(0)] }.cost();
    let mut q = Queue::new(limits(one * 3, 1000, 100));
    q.push_data(1, big("1m"));
    q.push_data(1, big("5m"));
    q.push_data(1, Payload::Seed { interval: "1h".into(), bars: vec![bar(0)] });
    let frames = drain(&mut q);
    let streams: Vec<WireStream> = frames
        .iter()
        .filter_map(|f| match f {
            StreamDown::Gap { stream, .. } => Some(stream.clone()),
            _ => None,
        })
        .collect();
    assert!(streams.contains(&WireStream::Bar("1m".into())), "{streams:?}");
    assert!(streams.contains(&WireStream::Bar("5m".into())), "{streams:?}");
}

#[test]
fn a_released_key_is_freed_once_it_has_drained() {
    let mut q = Queue::new(limits(1 << 20, 1000, 100));
    q.push_data(4, quote(0));
    q.release_key(4);
    // Still queued: the numbers survive until the frame is out.
    q.push_data(4, quote(1)); // (a stray write racing the release is still numbered in sequence)
    let frames = drain(&mut q);
    assert_eq!(quote_seqs(&frames), vec![1, 2]);
    q.push_data(4, quote(2));
    let after = drain(&mut q);
    assert_eq!(quote_seqs(&after), vec![1], "the freed key restarts its numbers");
}

#[test]
fn the_frame_bound_holds_for_tiny_frames() {
    let mut q = Queue::new(limits(1 << 30, 5, 100));
    for ts in 0..40 {
        q.push_data(1, trade(ts));
    }
    assert_eq!(q.data_frames(), 5);
    let frames = drain(&mut q);
    let delivered = frames.iter().filter(|f| matches!(f, StreamDown::Trade { .. })).count() as u64;
    let lost: u64 = gaps(&frames).iter().map(|g| g.0).sum();
    assert_eq!(delivered + lost, 40);
}

#[test]
fn a_depth_snapshot_costs_by_its_levels() {
    let small = Payload::Depth {
        tick_size: 0.5,
        bids: vec![BookLevel::new(1.0, 1.0)],
        asks: vec![],
        ts: 0,
    };
    let large = Payload::Depth {
        tick_size: 0.5,
        bids: (0..100).map(|i| BookLevel::new(f64::from(i), 1.0)).collect(),
        asks: (0..100).map(|i| BookLevel::new(f64::from(i), 1.0)).collect(),
        ts: 0,
    };
    assert!(large.cost() > small.cost() * 10);
}

// ---- A Gap carries WHEN the hole opens ---------------------------------------------------------

/// The `from_ts_ms` of every `Gap` in `frames`, in order.
fn gap_opens(frames: &[StreamDown]) -> Vec<i64> {
    frames
        .iter()
        .filter_map(|f| match f {
            StreamDown::Gap { from_ts_ms, .. } => Some(*from_ts_ms),
            _ => None,
        })
        .collect()
}

/// The stamps of the quotes in `frames`, in order.
fn quote_stamps(frames: &[StreamDown]) -> Vec<i64> {
    frames
        .iter()
        .filter_map(|f| match f {
            StreamDown::Quote { quote, .. } => Some(quote.ts),
            _ => None,
        })
        .collect()
}

/// **A hole opens at the last frame DELIVERED before it** - not at the first frame lost, not at the
/// first survivor, and not at any clock. This is the property the store relies on: replay orders rows
/// by `(ts, seq)` and a recorded `GapStart` is a row of `seq` 0 filed under this stamp
/// (`crates/vike-store/src/archive_store.rs`'s `scan_book_updates_impl`, the recorder's
/// `stream_status`), so it must sort AFTER every frame delivered before the hole and BEFORE the oldest
/// survivor - a stamp that is the survivors' own or a later one folds deltas onto a book that is
/// missing the lost frames and emits confidently wrong quotes until it lands.
#[test]
fn a_gap_opens_at_the_last_delivered_frames_stamp_and_sorts_before_the_oldest_survivor() {
    let cost = quote_cost();
    let mut q = Queue::new(limits(cost * 2, 1000, 100));
    q.push_data(1, quote(100));
    q.push_data(1, quote(110));
    let delivered = drain(&mut q);
    assert_eq!(quote_stamps(&delivered), vec![100, 110]);

    // A slow reader: the next six queue behind the bound; the oldest four are lost (stamps 200..=203).
    for ts in 200..206 {
        q.push_data(1, quote(ts));
    }
    let second = drain(&mut q);
    assert_eq!(quote_stamps(&second), vec![204, 205], "the survivors are the newest two");
    assert_eq!(gaps(&second), vec![(4, 2, 7)]);
    let opens = gap_opens(&second);
    assert_eq!(opens, vec![110], "the hole opens at the last frame delivered before it");

    // The store's order: rows by (ts, seq), the marker a row of seq 0 filed under `opens[0]`. Its stamp
    // is at or after every frame delivered before the hole and at or before the oldest survivor's;
    // with the marker's seq 0 it sorts before every survivor.
    let at = opens[0];
    assert!(quote_stamps(&delivered).iter().all(|ts| *ts <= at), "a delivered frame is after it");
    assert!(quote_stamps(&second).iter().all(|ts| at <= *ts), "a survivor is before it");
    let marker = (at, 0_u64);
    for survivor in [(204_i64, 7_u64), (205, 8)] {
        assert!(marker < survivor, "the marker {marker:?} sorts after the survivor {survivor:?}");
    }
}

/// With nothing delivered before the hole, it opens at the first frame LOST.
#[test]
fn with_nothing_delivered_a_hole_opens_at_the_first_lost_frames_stamp() {
    let cost = quote_cost();
    let mut q = Queue::new(limits(cost * 4, 1000, 100));
    for ts in 0..50 {
        q.push_data(7, quote(1_000 + ts));
    }
    let frames = drain(&mut q);
    assert_eq!(gaps(&frames), vec![(46, 0, 47)]);
    assert_eq!(gap_opens(&frames), vec![1_000], "the first lost frame's own stamp");
    assert!(gap_opens(&frames)[0] <= quote_stamps(&frames)[0]);
}

/// A tail hole (the newest frames were the lost ones, flushed when the queue goes idle) opens at the
/// last frame delivered for THAT verb, and a seed is stamped by its newest bar.
#[test]
fn a_tail_hole_opens_at_the_last_delivered_frame_of_its_own_verb() {
    let cost = quote_cost();
    let mut q = Queue::new(limits(cost * 6, 1000, 100));
    // A quote is delivered (stamp 500); a seed (newest bar 20) is delivered; then a seed too big for
    // the whole bound is lost with nothing behind it.
    q.push_data(3, quote(500));
    q.push_data(3, Payload::Seed { interval: "1m".into(), bars: vec![bar(10), bar(20)] });
    let delivered = drain(&mut q);
    assert_eq!(quote_stamps(&delivered), vec![500]);
    let big = Payload::Seed { interval: "1m".into(), bars: (100..164).map(bar).collect() };
    assert!(big.cost() > cost * 6);
    assert_eq!(q.push_data(3, big), Pushed::Evicted(1));
    let tail = drain(&mut q);
    assert_eq!(
        gap_opens(&tail),
        vec![20],
        "the SEED lane's last delivered frame (its newest bar), not the quote's 500 or the lost seed's 163"
    );
}

/// A frame the writer could not send is a hole too, and it hands the books back: the hole opens at the
/// frame delivered before it.
#[test]
fn an_unsendable_frame_restores_the_last_delivered_stamp() {
    let mut q = Queue::new(limits(1 << 20, 1000, 100));
    q.push_data(1, quote(500));
    q.push_data(1, quote(600));
    q.push_data(1, quote(700));
    let _first = q.take().expect("first");
    let second = q.take().expect("second");
    q.note_unsendable(second.undo.expect("a lossless frame carries its undo"));
    let rest = drain(&mut q);
    assert_eq!(quote_stamps(&rest), vec![700]);
    assert_eq!(gaps(&rest), vec![(1, 1, 3)]);
    assert_eq!(gap_opens(&rest), vec![500], "the last frame actually delivered");
}

/// `Book` (the folded state) carries no stamp: the push clock is all its hole has.
#[test]
fn a_book_hole_opens_at_the_push_clock() {
    let one = Payload::Book(Arc::new(L2Book::new(0.5))).cost();
    let mut q = Queue::new(limits(one * 2, 1000, 100));
    let before = vike_model::now_ms();
    for _ in 0..5 {
        q.push_data(1, Payload::Book(Arc::new(L2Book::new(0.5))));
    }
    let after = vike_model::now_ms();
    let frames = drain(&mut q);
    let opens = gap_opens(&frames);
    assert_eq!(opens.len(), 1, "{frames:?}");
    assert!(opens[0] >= before && opens[0] <= after, "{} outside [{before}, {after}]", opens[0]);
}

// ---- A forming bar is latest-wins per (symbol, INTERVAL) ---------------------------------------

fn forming(interval: &str, ts: i64) -> Payload {
    Payload::Forming { interval: interval.into(), bar: bar(ts) }
}

fn forming_stamps(frames: &[StreamDown]) -> Vec<(String, i64)> {
    frames
        .iter()
        .filter_map(|f| match f {
            StreamDown::Forming { interval, bar, .. } => Some((interval.clone(), bar.ts)),
            _ => None,
        })
        .collect()
}

/// One symbol key carries every interval it is subscribed at (the md hub subscribes BTCUSDT at 1m and
/// 5m on one Shared client), and a forming bar is latest-wins per `(symbol, interval)`: a 5m snapshot
/// must not replace an unsent 1m one, nor the reverse. Each interval keeps its own newest snapshot.
#[test]
fn forming_snapshots_of_two_intervals_do_not_supersede_each_other() {
    let mut q = Queue::new(limits(1 << 20, 1000, 100));
    assert_eq!(q.push_data(1, forming("1m", 10)), Pushed::Queued);
    assert_eq!(q.push_data(1, forming("5m", 11)), Pushed::Queued, "the 5m slot is its own");
    assert_eq!(q.push_data(1, forming("1m", 12)), Pushed::Superseded, "same interval: latest wins");
    assert_eq!(q.push_data(1, forming("5m", 13)), Pushed::Superseded);
    assert_eq!(q.data_frames(), 2);
    let frames = drain(&mut q);
    let mut got = forming_stamps(&frames);
    got.sort();
    assert_eq!(got, vec![("1m".to_string(), 12), ("5m".to_string(), 13)]);
    assert_eq!(q.stats().superseded, 2, "only same-interval replacements are supersessions");
}

/// Under a backlog (two updates of each interval land before the writer takes anything) neither
/// interval starves: both slots hold their newest snapshot every round and the writer, taking what
/// is there, serves each. With one slot per `(key, verb)` the queue held a single forming frame
/// (whichever interval pushed last), so the other interval was never sent at all.
#[test]
fn under_a_backlog_neither_interval_starves() {
    let mut q = Queue::new(limits(1 << 20, 1000, 100));
    let mut wire: Vec<(String, i64)> = Vec::new();
    for round in 0..40_i64 {
        q.push_data(1, forming("1m", round * 10));
        q.push_data(1, forming("5m", round * 10 + 1));
        q.push_data(1, forming("1m", round * 10 + 2));
        q.push_data(1, forming("5m", round * 10 + 3));
        while let Some(t) = q.take() {
            wire.extend(forming_stamps(&[t.item.into_frame()]));
        }
    }
    for (interval, offset) in [("1m", 2), ("5m", 3)] {
        let sent: Vec<i64> =
            wire.iter().filter(|(i, _)| i == interval).map(|(_, ts)| *ts).collect();
        let want: Vec<i64> = (0..40).map(|round| round * 10 + offset).collect();
        assert_eq!(sent, want, "the {interval} lane lost or starved a round's newest snapshot");
    }
}

/// Mark, bar-close-tick and depth carry no interval: their slot is still `(key, verb)`.
#[test]
fn verbs_without_an_interval_keep_their_one_slot_per_key() {
    let mut q = Queue::new(limits(1 << 20, 1000, 100));
    assert_eq!(q.push_data(1, Payload::Mark { px: 1.0, ts: 1 }), Pushed::Queued);
    assert_eq!(q.push_data(1, Payload::Mark { px: 2.0, ts: 2 }), Pushed::Superseded);
    assert_eq!(q.push_data(2, Payload::Mark { px: 3.0, ts: 3 }), Pushed::Queued, "another key");
    assert_eq!(q.push_data(1, Payload::BarCloseTick { px: 1.0, ts: 1 }), Pushed::Queued);
    assert_eq!(q.push_data(1, Payload::BarCloseTick { px: 2.0, ts: 2 }), Pushed::Superseded);
    let depth = |ts| Payload::Depth { tick_size: 0.5, bids: Vec::new(), asks: Vec::new(), ts };
    assert_eq!(q.push_data(1, depth(1)), Pushed::Queued);
    assert_eq!(q.push_data(1, depth(2)), Pushed::Superseded);
    assert_eq!(q.data_frames(), 4);
}

/// A forming slot is released with its frame: after the frame is taken, the next snapshot of that
/// interval queues fresh (nothing stale in the slot map supersedes a frame that is already gone).
#[test]
fn a_taken_forming_frame_releases_its_slot() {
    let mut q = Queue::new(limits(1 << 20, 1000, 100));
    q.push_data(1, forming("1m", 1));
    assert!(q.take().is_some());
    assert_eq!(q.push_data(1, forming("1m", 2)), Pushed::Queued);
    assert_eq!(q.data_frames(), 1);
}

// ---- A non-finite level inside a folded book -----------------------------------------------------

/// A resting quantity that is NaN or infinite serializes as `null`, which the client cannot decode (it
/// ends the session, reconnects, replays and meets the same book again). The book is refused and
/// DISCLOSED as a hole instead, like a non-finite lossless row, and the session goes on.
#[test]
fn a_book_with_a_non_finite_level_is_refused_and_disclosed_not_sent() {
    let level = |price: f64, qty: f64| vike_marketdata::BookLevel::new(price, qty);
    let mut good = L2Book::new(0.5);
    good.apply_snapshot(1, &[level(100.0, 3.0)], &[level(100.5, 4.0)]);
    let mut bad_bid = good.clone();
    bad_bid.apply_delta(2, &[level(99.5, f64::NAN)], &[]);
    let mut bad_ask = good.clone();
    bad_ask.apply_delta(2, &[], &[level(101.0, f64::INFINITY)]);

    let mut q = Queue::new(limits(1 << 20, 1000, 100));
    assert_eq!(q.push_data(1, Payload::Book(Arc::new(good.clone()))), Pushed::Queued);
    assert_eq!(q.push_data(1, Payload::Book(Arc::new(bad_bid))), Pushed::Refused);
    assert_eq!(q.push_data(1, Payload::Book(Arc::new(bad_ask))), Pushed::Refused);
    assert_eq!(q.push_data(1, Payload::Book(Arc::new(good))), Pushed::Queued);
    assert_eq!(q.stats().nonfinite, 2);
    let frames = drain(&mut q);
    let books: Vec<u64> = frames
        .iter()
        .filter_map(|f| match f {
            StreamDown::Book { seq, .. } => Some(*seq),
            _ => None,
        })
        .collect();
    assert_eq!(books, vec![1, 4], "the finite books cross");
    assert_eq!(
        gaps(&frames),
        vec![(2, 1, 4)],
        "and the two refused ones are disclosed as ONE hole"
    );
    // Every frame that reached the wire serializes with no `null` in it.
    for frame in &frames {
        let json = serde_json::to_string(frame).expect("encodes");
        assert!(!json.contains("null"), "{json}");
    }
}

// ---- A subscriber can RAISE a lane's class (Depth), and a raise is the lossless rule whole -------------

fn depth(ts: i64) -> Payload {
    Payload::Depth {
        tick_size: 0.5,
        bids: vec![BookLevel::new(100.0, 1.0)],
        asks: vec![BookLevel::new(100.5, 1.0)],
        ts,
    }
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

/// The frames of `frames` by name, so an order can be asserted without spelling every payload.
fn names(frames: &[StreamDown]) -> Vec<&'static str> {
    frames
        .iter()
        .map(|f| match f {
            StreamDown::Depth { .. } => "depth",
            StreamDown::Quote { .. } => "quote",
            StreamDown::Gap { verb: GapVerb::Depth, .. } => "gap-depth",
            StreamDown::Gap { .. } => "gap",
            StreamDown::Delivery { class: WireDelivery::Conflatable, .. } => "class-conflatable",
            StreamDown::Delivery { class: WireDelivery::Lossless, .. } => "class-lossless",
            StreamDown::Delivery { .. } => "class-control",
            _ => "other",
        })
        .collect()
}

#[test]
fn a_depth_lane_nobody_raised_still_supersedes() {
    let mut q = Queue::new(limits(1 << 20, 1000, 100));
    q.push_data(1, depth(1));
    assert_eq!(q.push_data(1, depth(2)), Pushed::Superseded);
    assert_eq!(depth_seqs(&drain(&mut q)), vec![2], "latest-wins, and the jump is not a loss");
    assert_eq!(q.stats().superseded, 1);
}

#[test]
fn a_raised_depth_lane_is_never_superseded() {
    let mut q = Queue::new(limits(1 << 20, 1000, 100));
    assert_eq!(q.set_floor(1, WireLane::Depth, Delivery::Lossless), Some(Delivery::Lossless));
    for ts in 1..=4 {
        assert_eq!(q.push_data(1, depth(ts)), Pushed::Queued, "frame {ts}");
    }
    assert_eq!(q.stats().superseded, 0);
    let frames = drain(&mut q);
    assert_eq!(depth_seqs(&frames), vec![1, 2, 3, 4], "every snapshot, in order, continuous");
    assert!(gaps(&frames).is_empty());
}

#[test]
fn a_floor_names_the_change_once_and_says_nothing_about_a_no_op() {
    let mut q = Queue::new(limits(1 << 20, 1000, 100));
    assert_eq!(q.floor_of(1, WireLane::Depth), Delivery::Conflatable);
    assert_eq!(q.set_floor(1, WireLane::Depth, Delivery::Lossless), Some(Delivery::Lossless));
    assert_eq!(q.floor_of(1, WireLane::Depth), Delivery::Lossless);
    assert_eq!(q.set_floor(1, WireLane::Depth, Delivery::Lossless), None, "already raised");
    // Another key is another floor.
    assert_eq!(q.floor_of(2, WireLane::Depth), Delivery::Conflatable);
    // A lane whose verbs are lossless already is unchanged by a floor of lossless.
    for lane in [WireLane::Quotes, WireLane::Trades, WireLane::Book] {
        assert_eq!(q.set_floor(1, lane, Delivery::Lossless), None, "{lane:?}");
        assert_eq!(q.floor_of(1, lane), Delivery::Conflatable, "{lane:?} keeps no floor");
    }
    assert_eq!(q.set_floor(1, WireLane::Depth, Delivery::Conflatable), Some(Delivery::Conflatable));
    assert_eq!(q.set_floor(1, WireLane::Depth, Delivery::Conflatable), None);
    assert_eq!(q.floor_of(1, WireLane::Depth), Delivery::Conflatable);
}

#[test]
fn a_raise_applies_to_frames_pushed_after_it_and_a_queued_one_keeps_its_rule() {
    let mut q = Queue::new(limits(1 << 20, 1000, 100));
    q.push_data(1, depth(1));
    assert_eq!(q.push_data(1, depth(2)), Pushed::Superseded, "conflatable until the raise");
    assert!(q.set_floor(1, WireLane::Depth, Delivery::Lossless).is_some());
    // The announcement rides the control lane in emission order: after depth 2, before depth 3.
    assert!(q.push_ctrl(Control::Delivery {
        k: 1,
        lane: WireLane::Depth,
        class: WireDelivery::Lossless
    }));
    assert_eq!(q.push_data(1, depth(3)), Pushed::Queued);
    assert_eq!(q.push_data(1, depth(4)), Pushed::Queued, "not superseded any more");
    let frames = drain(&mut q);
    assert_eq!(names(&frames), vec!["depth", "class-lossless", "depth", "depth"]);
    assert_eq!(depth_seqs(&frames), vec![2, 3, 4]);
    assert_eq!(q.stats().superseded, 1, "only the frame queued before the raise");
}

#[test]
fn a_relax_makes_later_frames_conflatable_and_leaves_the_queued_lossless_ones_evictable() {
    let mut q = Queue::new(limits(1 << 20, 1000, 100));
    q.set_floor(1, WireLane::Depth, Delivery::Lossless);
    q.push_data(1, depth(1));
    q.push_data(1, depth(2));
    assert_eq!(q.set_floor(1, WireLane::Depth, Delivery::Conflatable), Some(Delivery::Conflatable));
    assert_eq!(q.push_data(1, depth(3)), Pushed::Queued);
    assert_eq!(q.push_data(1, depth(4)), Pushed::Superseded, "latest-wins again");
    let frames = drain(&mut q);
    assert_eq!(depth_seqs(&frames), vec![1, 2, 4]);
    assert_eq!(q.stats().superseded, 1);
}

/// The room a flood of raised depth snapshots makes is made out of the DEPTH lane's own oldest frames,
/// each owed as a `Gap` naming `GapVerb::Depth` - never out of a neighbour's lossless quote. This is the
/// delivery class's trap: "stop superseding" alone would have let `enforce_bounds` evict key 2's quote.
#[test]
fn the_overflow_of_a_raised_depth_lane_is_a_disclosed_gap_and_costs_a_neighbour_nothing() {
    let depth_cost = depth(0).cost();
    // Room for 3 depth frames and the neighbour's one quote.
    let mut q = Queue::new(limits(3 * depth_cost + quote_cost(), 1000, 100));
    q.set_floor(1, WireLane::Depth, Delivery::Lossless);
    q.push_data(2, quote(1));
    for ts in 1..=10 {
        q.push_data(1, depth(ts));
    }
    assert_eq!(q.stats().superseded, 0, "a raised lane is never superseded");
    assert_eq!(q.stats().lossless_evicted, 7, "ten pushed, three kept");
    let frames = drain(&mut q);
    assert_eq!(quote_seqs(&frames), vec![1], "the neighbour's quote survived the burst");
    assert_eq!(depth_seqs(&frames), vec![8, 9, 10], "the newest survive, in order");
    let depth_gaps: Vec<_> = frames
        .iter()
        .filter_map(|f| match f {
            StreamDown::Gap { k: 1, verb: GapVerb::Depth, dropped, from_seq, to_seq, .. } => {
                Some((*dropped, *from_seq, *to_seq))
            }
            _ => None,
        })
        .collect();
    assert_eq!(depth_gaps, vec![(7, 0, 8)], "one marker for the seven, spanning seq 1..=7");
    let names = names(&frames);
    let at = names.iter().position(|n| *n == "gap-depth").expect("a depth gap");
    assert_eq!(names[at + 1], "depth", "the marker lands immediately before the first survivor");
}

#[test]
fn a_lost_tail_of_raised_depth_frames_is_disclosed_when_the_queue_goes_idle() {
    let depth_cost = depth(0).cost();
    // Room for exactly one frame: the second evicts the first (the key pays).
    let mut q = Queue::new(limits(depth_cost, 1000, 100));
    q.set_floor(1, WireLane::Depth, Delivery::Lossless);
    q.push_data(1, depth(1));
    q.push_data(1, depth(2));
    // The writer takes the survivor; the loss precedes it as a marker.
    let frames = drain(&mut q);
    assert_eq!(depth_seqs(&frames), vec![2]);
    assert_eq!(gaps(&frames), vec![(1, 0, 2)], "the one evicted frame precedes the survivor");
}

#[test]
fn a_non_finite_depth_row_is_a_hole_only_on_a_raised_lane() {
    let bad = |ts| Payload::Depth {
        tick_size: 0.5,
        bids: vec![BookLevel::new(f64::NAN, 1.0)],
        asks: vec![],
        ts,
    };
    let mut q = Queue::new(limits(1 << 20, 1000, 100));
    assert_eq!(q.push_data(1, bad(1)), Pushed::Refused);
    q.push_data(1, depth(2));
    assert!(gaps(&drain(&mut q)).is_empty(), "conflatable: latest-wins, no marker");

    let mut q = Queue::new(limits(1 << 20, 1000, 100));
    q.set_floor(1, WireLane::Depth, Delivery::Lossless);
    assert_eq!(q.push_data(1, bad(1)), Pushed::Refused);
    q.push_data(1, depth(2));
    let frames = drain(&mut q);
    assert_eq!(gaps(&frames), vec![(1, 0, 2)], "raised: the refused row is a disclosed hole");
}

#[test]
fn a_floor_is_freed_with_its_key() {
    let mut q = Queue::new(limits(1 << 20, 1000, 100));
    q.set_floor(1, WireLane::Depth, Delivery::Lossless);
    q.set_floor(2, WireLane::Depth, Delivery::Lossless);
    q.release_key(1);
    assert_eq!(q.floor_of(1, WireLane::Depth), Delivery::Conflatable, "released and drained");
    q.hold(2);
    q.purge(2);
    assert_eq!(q.floor_of(2, WireLane::Depth), Delivery::Conflatable, "a refused key is forgotten");
}

#[test]
fn only_a_verb_with_a_wire_name_can_be_raised() {
    for verb in [Verb::Seed, Verb::BarClose, Verb::Quote, Verb::Trade, Verb::Book, Verb::BookDelta]
    {
        assert_eq!(verb.base(), Delivery::Lossless, "{verb:?}");
        assert!(verb.gap_verb().is_some(), "{verb:?}");
    }
    assert_eq!(Verb::Depth.base(), Delivery::Conflatable);
    assert_eq!(Verb::Depth.gap_verb(), Some(GapVerb::Depth));
    for verb in [Verb::Forming, Verb::Mark, Verb::BarCloseTick] {
        assert_eq!(verb.base(), Delivery::Conflatable, "{verb:?}");
        assert!(verb.gap_verb().is_none(), "{verb:?} has no wire name, so no floor reaches it");
    }
    assert_eq!(Verb::GAPPABLE.len(), 7);
    assert!(Verb::GAPPABLE.iter().all(|v| v.gap_verb().is_some()));
}

/// The queue's verb table IS `vike_data::delivery`'s: every queue verb maps to a sink verb, back again,
/// and carries that verb's base class.
#[test]
fn the_queue_verbs_are_the_delivery_tables_sink_verbs() {
    let all = [
        Verb::Seed,
        Verb::BarClose,
        Verb::Forming,
        Verb::Mark,
        Verb::BarCloseTick,
        Verb::Depth,
        Verb::Quote,
        Verb::Trade,
        Verb::Book,
        Verb::BookDelta,
    ];
    for verb in all {
        assert_eq!(Verb::from_sink_verb(verb.sink_verb()), Some(verb), "{verb:?}");
        assert_eq!(verb.base(), verb.sink_verb().base(), "{verb:?}");
    }
    let mapped = SinkVerb::ALL.iter().filter(|v| Verb::from_sink_verb(**v).is_some()).count();
    assert_eq!(mapped, all.len(), "ten data verbs; the two control verbs are not queued data");
    assert!(Verb::from_sink_verb(SinkVerb::StreamStatus).is_none());
    assert!(Verb::from_sink_verb(SinkVerb::DataDelay).is_none());
}
