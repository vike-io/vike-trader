//! `market`'s reader, driven frame by frame with no socket: what a `Gap` and a sequence break stamp
//! their markers with, and what the reader keeps per key. The behaviour over a real link is
//! `crates/vike-gatewayhub-client/tests/link_loss.rs` and `crates/vike-gatewayhub-client/tests/dataclient_contract.rs`; these hold the decisions that need the
//! reader's private state.

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize};

use vike_data::{RecordingSink, SinkCall};
use vike_marketdata::{Bar, BookUpdate, BookUpdateKind, QuoteTick, TradeTick};

use super::*;

/// An opener that is never asked to dial: these tests drive the reader frame by frame with no socket.
/// Its numbers are the gatewayhub's own, so a decision that reads one reads what production reads.
struct NoDial;

impl Opener for NoDial {
    fn addr(&self) -> &str {
        "127.0.0.1:1"
    }

    fn limits(&self) -> crate::opener::StreamLimits {
        crate::opener::StreamLimits {
            up_frame_ceiling_bytes: 64 * 1024,
            down_frame_ceiling_bytes: 8 * 1024 * 1024,
            max_symbol_bytes: 1024,
        }
    }

    fn timings(&self) -> crate::opener::StreamTimings {
        crate::opener::StreamTimings {
            sub_reply: Duration::from_secs(10),
            stream_read_floor: Duration::from_secs(45),
            reconnect_base: Duration::from_millis(250),
            reconnect_max: Duration::from_secs(5),
        }
    }

    fn failure_text(&self, e: &dyn std::fmt::Display) -> String {
        format!("gatewayhub {}: {e}", self.addr())
    }

    fn open(&self, _venue: &str, _observe: &dyn Fn(&TcpStream)) -> io::Result<DataStream> {
        Err(io::Error::other("this opener never dials"))
    }
}

struct Rig {
    sh: Arc<Shared>,
    sink: Arc<RecordingSink>,
    state: Reader,
}

impl Rig {
    /// A reader state over `subs` (`sid`, op, phase), delivering into a recording sink. No socket: a
    /// `send` finds no writer and fails, which `deliver` ignores.
    fn new(subs: Vec<(u64, SubOp, Phase)>) -> Self {
        let sink = Arc::new(RecordingSink::default());
        let dyn_sink: Arc<dyn LiveDataSink> = sink.clone();
        let sh = Arc::new(Shared {
            opener: Box::new(NoDial),
            venue: "binance".to_string(),
            sink: dyn_sink,
            inner: Mutex::new(Inner {
                link: LinkState::Up,
                subs: subs.into_iter().map(|(sid, op, phase)| (sid, Entry { op, phase })).collect(),
                stopping: false,
                exited: false,
                parked: false,
                fresh_until: None,
                retired: Vec::new(),
                floors: HashMap::new(),
                class_support: Some(true),
            }),
            changed: Condvar::new(),
            stop: AtomicBool::new(false),
            writer: Mutex::new(None),
            connecting: Mutex::new(None),
            reader: OnceLock::new(),
            retire: AtomicBool::new(false),
            bound_keys: AtomicUsize::new(0),
            conflated: AtomicU64::new(0),
        });
        Self { sh, sink, state: Reader::default() }
    }

    fn deliver(&mut self, frame: StreamDown) -> Option<End> {
        deliver(&self.sh, &mut self.state, frame)
    }

    fn statuses(&self) -> Vec<(String, StreamStatus)> {
        self.sink
            .recorded()
            .into_iter()
            .filter_map(|c| match c {
                SinkCall::StreamStatus { stream, status, .. } => Some((stream, status)),
                _ => None,
            })
            .collect()
    }
}

fn quotes(symbol: &str) -> SubOp {
    SubOp::Quotes { symbol: symbol.to_string() }
}

fn quote(ts: i64) -> QuoteTick {
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

fn trade(ts: i64) -> TradeTick {
    TradeTick {
        ts,
        local_ts: 0,
        price: 1.5,
        size: 1.0,
        is_buyer_maker: false,
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

fn delta(ts: i64, seq: u64) -> BookUpdate {
    BookUpdate {
        ts,
        local_ts: 0,
        seq,
        kind: BookUpdateKind::Delta,
        tick_size: 0.01,
        bids: Vec::new(),
        asks: Vec::new(),
        symbol: String::new(),
    }
}

/// Bind key `k` to `symbol` by acknowledging the one awaiting subscription.
fn bound(symbol: &str, k: u32) -> Rig {
    let mut rig = Rig::new(vec![(1, quotes(symbol), Phase::Awaiting)]);
    assert!(rig.deliver(StreamDown::SubOk { sid: 1, k }).is_none());
    rig
}

/// **A `Gap` frame stamps both markers with the hole's own time.** The daemon names when the hole
/// opens (the last frame delivered before it); the client's clock is seconds later behind a slow
/// reader, and replay orders by stamp.
#[test]
fn a_gap_frame_stamps_both_markers_with_its_own_time_and_not_the_clock() {
    let mut rig = bound("A", 7);
    let opened = 1_700_000_000_123_i64;
    assert!(now_ms() > opened + 1_000_000, "the test clock must be well past the stamp");
    rig.deliver(StreamDown::Gap {
        k: 7,
        verb: GapVerb::Quote,
        stream: WireStream::Lane(WireLane::Quotes),
        dropped: 3,
        from_seq: 1,
        to_seq: 5,
        from_ts_ms: opened,
    });
    assert_eq!(
        rig.statuses(),
        vec![
            ("quotes".to_string(), StreamStatus::GapStart { at_ts_ms: opened }),
            ("quotes".to_string(), StreamStatus::Live { gap_started_ts_ms: Some(opened) }),
        ]
    );
}

/// **A sequence break with no `Gap` opens at the last frame delivered for that `(key, verb)`**: each
/// verb its own stamp, each frame kind its own source of one (a quote's `ts`, a trade's, a bar's, the
/// newest bar of a seed, a book update's).
#[test]
fn a_silent_sequence_break_opens_at_the_last_delivered_frames_own_stamp() {
    let mut rig = bound("A", 7);
    // Quotes 1, 2 then a jump to 9: the hole opens at quote 2's stamp, not at the survivor's.
    rig.deliver(StreamDown::Quote { k: 7, seq: 1, quote: quote(1_000) });
    rig.deliver(StreamDown::Quote { k: 7, seq: 2, quote: quote(2_000) });
    rig.deliver(StreamDown::Quote { k: 7, seq: 9, quote: quote(9_000) });
    // A trade lane is its own verb: its stamp is not the quotes'.
    rig.deliver(StreamDown::Trade { k: 7, seq: 1, trade: trade(3_000) });
    rig.deliver(StreamDown::Trade { k: 7, seq: 5, trade: trade(8_000) });
    // A closed-bar lane, and a seed (stamped by its newest bar), and a book-update lane.
    rig.deliver(StreamDown::BarClose { k: 7, seq: 1, interval: "1m".into(), bar: bar(60_000) });
    rig.deliver(StreamDown::BarClose { k: 7, seq: 3, interval: "1m".into(), bar: bar(180_000) });
    rig.deliver(StreamDown::Seed {
        k: 7,
        seq: 1,
        interval: "5m".into(),
        bars: vec![bar(300_000), bar(600_000)],
    });
    rig.deliver(StreamDown::Seed { k: 7, seq: 4, interval: "5m".into(), bars: vec![bar(900_000)] });
    rig.deliver(StreamDown::BookDelta { k: 7, seq: 1, update: Box::new(delta(7_000, 1)) });
    rig.deliver(StreamDown::BookDelta { k: 7, seq: 6, update: Box::new(delta(7_900, 6)) });

    let opened: Vec<(String, i64)> = rig
        .statuses()
        .into_iter()
        .filter_map(|(stream, s)| match s {
            StreamStatus::GapStart { at_ts_ms } => Some((stream, at_ts_ms)),
            _ => None,
        })
        .collect();
    assert_eq!(
        opened,
        vec![
            ("quotes".to_string(), 2_000),
            ("trades".to_string(), 3_000),
            ("1m".to_string(), 60_000),
            ("5m".to_string(), 600_000),
            ("book".to_string(), 7_000),
        ],
        "each hole opens at the last frame delivered before it"
    );
}

/// A folded `book` frame carries no time at all, so the arrival clock is the only stamp a hole in
/// that lane can have; it is still disclosed.
#[test]
fn a_hole_in_the_stampless_book_lane_falls_back_to_the_arrival_clock() {
    let mut rig = bound("A", 7);
    let before = now_ms();
    rig.deliver(StreamDown::Book {
        k: 7,
        seq: 1,
        book: Box::new(vike_marketdata::L2Book::new(0.5)),
    });
    rig.deliver(StreamDown::Book {
        k: 7,
        seq: 3,
        book: Box::new(vike_marketdata::L2Book::new(0.5)),
    });
    let after = now_ms();
    let opened: Vec<i64> = rig
        .statuses()
        .into_iter()
        .filter_map(|(_, s)| match s {
            StreamStatus::GapStart { at_ts_ms } => Some(at_ts_ms),
            _ => None,
        })
        .collect();
    assert_eq!(opened.len(), 1);
    assert!(opened[0] >= before && opened[0] <= after, "{} not in [{before}, {after}]", opened[0]);
}

/// The `Live` dedup set (`live_sent`) only matters while a replay is outstanding: two replayed
/// subscriptions of one symbol and lane close that gap ONCE, and the set is emptied the moment none is
/// left replaying instead of keeping an entry per symbol the session ever replayed.
#[test]
fn the_live_dedup_set_is_emptied_once_no_replay_is_outstanding() {
    let replaying = || Phase::Replaying { gap_at: 5, tries: 0 };
    let mut rig = Rig::new(vec![(1, quotes("A"), replaying()), (2, quotes("A"), replaying())]);
    rig.deliver(StreamDown::SubOk { sid: 1, k: 3 });
    assert_eq!(rig.state.live_sent.len(), 1, "the second replay of the pair is still to come");
    rig.deliver(StreamDown::SubOk { sid: 2, k: 3 });
    assert_eq!(
        rig.statuses(),
        vec![("quotes".to_string(), StreamStatus::Live { gap_started_ts_ms: Some(5) })],
        "two replays of one symbol and lane close the gap once"
    );
    assert!(rig.state.live_sent.is_empty(), "nothing is replaying, so nothing is deduplicated");
}

/// Frames in flight for a key whose symbol was unsubscribed are dropped QUIETLY (not counted as strays),
/// the key itself is gone, and a symbol subscribed again before the reader looked keeps its key.
#[test]
fn an_unsubscribed_symbols_key_is_dropped_and_its_late_frames_are_not_strays() {
    let mut rig = Rig::new(vec![
        (1, quotes("A"), Phase::Awaiting),
        (2, quotes("B"), Phase::Awaiting),
        (3, quotes("C"), Phase::Awaiting),
    ]);
    rig.deliver(StreamDown::SubOk { sid: 1, k: 1 });
    rig.deliver(StreamDown::SubOk { sid: 2, k: 2 });
    rig.deliver(StreamDown::SubOk { sid: 3, k: 3 });
    assert_eq!(rig.state.keys.len(), 3);
    assert_eq!(rig.sh.bound_keys.load(Ordering::Relaxed), 3);

    // A and B are unsubscribed; B is subscribed AGAIN (a new sid) before the reader looks.
    {
        let mut inner = lock(&rig.sh.inner);
        inner.subs.remove(&1);
        inner.subs.remove(&2);
        inner.retired.push("A".to_string());
        inner.retired.push("B".to_string());
        inner.subs.insert(4, Entry { op: quotes("B"), phase: Phase::Awaiting });
    }
    rig.sh.retire.store(true, Ordering::Release);
    trim_retired(&rig.sh, &mut rig.state);

    assert!(!rig.state.keys.contains_key(&1), "A's key was dropped");
    assert!(rig.state.keys.contains_key(&2), "B was subscribed again: its key stays");
    assert!(rig.state.keys.contains_key(&3), "C was never unsubscribed");
    assert_eq!(rig.sh.bound_keys.load(Ordering::Relaxed), 2);
    assert!(!rig.sh.retire.load(Ordering::Acquire), "the flag is taken");

    // A frame still in flight for A's old key: dropped, and not counted as a stray.
    rig.deliver(StreamDown::Quote { k: 1, seq: 1, quote: quote(1) });
    assert!(rig.sink.recorded().is_empty(), "a frame for a dropped key reached the sink");
    assert_eq!(rig.state.unknown, 0, "a late frame for a retired key is not a stray");
    // A key the session never bound still is one.
    rig.deliver(StreamDown::Quote { k: 99, seq: 1, quote: quote(1) });
    assert_eq!(rig.state.unknown, 1);
}

/// The memory of dropped keys is bounded too.
#[test]
fn the_memory_of_dropped_keys_is_bounded() {
    let n = RETIRED_KEYS + 20;
    let mut rig = Rig::new(Vec::new());
    for i in 0..n {
        let sid = i as u64 + 1;
        lock(&rig.sh.inner)
            .subs
            .insert(sid, Entry { op: quotes(&format!("S{i}")), phase: Phase::Awaiting });
        rig.deliver(StreamDown::SubOk { sid, k: i as u32 + 1 });
    }
    assert_eq!(rig.state.keys.len(), n);
    {
        let mut inner = lock(&rig.sh.inner);
        inner.subs.clear();
        inner.retired = (0..n).map(|i| format!("S{i}")).collect();
    }
    trim_retired(&rig.sh, &mut rig.state);
    assert!(rig.state.keys.is_empty());
    assert_eq!(rig.state.retired.len(), RETIRED_KEYS, "the oldest tombstones fell off");
}

/// **A link loss opens each lane's gap at that lane's last delivered frame.** The loss is noticed late
/// (here by 30 ms; on a real link by a whole heartbeat deadline, or by a daemon queue tail abandoned
/// on `Bye(TooSlow)`), and the rows between the last delivered frame and the detection belong inside
/// the gap: a lane that delivered is stamped with its frame, a lane that delivered nothing with the
/// instant it was acknowledged, and a held entry never acknowledged in the session with the instant
/// the session opened. `Replaying` carries the same stamp the `GapStart` did, so the `Live` that closes
/// the gap echoes it.
#[test]
fn a_loss_opens_each_lane_at_its_last_delivered_frame_not_at_the_detection() {
    let trades = |symbol: &str| SubOp::Trades { symbol: symbol.to_string() };
    let mut rig = Rig::new(vec![
        (1, quotes("A"), Phase::Awaiting),
        (2, trades("A"), Phase::Awaiting),
        (3, quotes("B"), Phase::Awaiting),
        (4, trades("NEVER"), Phase::Awaiting), // held, and never acknowledged in this session
    ]);
    rig.state.opened_ms = 1_234;
    let acknowledged_after = now_ms();
    for (sid, k) in [(1, 1), (2, 1), (3, 2)] {
        rig.deliver(StreamDown::SubOk { sid, k });
    }
    let acknowledged_before = now_ms();
    rig.deliver(StreamDown::Quote { k: 1, seq: 1, quote: quote(1_000) });
    rig.deliver(StreamDown::Trade { k: 1, seq: 1, trade: trade(2_000) });
    lock(&rig.sh.inner).subs.get_mut(&4).unwrap().phase = Phase::Granted;

    std::thread::sleep(Duration::from_millis(30)); // the loss is noticed late
    let end = end_session(&rig.sh, &rig.state, End::Lost("test".to_string()));
    assert!(matches!(end, End::Lost(_)), "a loss is reconnected, not parked");

    let gaps: BTreeMap<(String, String), i64> = rig
        .sink
        .recorded()
        .into_iter()
        .filter_map(|c| match c {
            SinkCall::StreamStatus {
                symbol,
                stream,
                status: StreamStatus::GapStart { at_ts_ms },
                ..
            } => Some(((symbol, stream), at_ts_ms)),
            _ => None,
        })
        .collect();
    let at = |symbol: &str, word: &str| gaps[&(symbol.to_string(), word.to_string())];
    assert_eq!(gaps.len(), 4, "one GapStart per held (symbol, lane): {gaps:?}");
    assert_eq!(at("A", "quotes"), 1_000, "A's last delivered quote");
    assert_eq!(at("A", "trades"), 2_000, "A's last delivered trade");
    let silent = at("B", "quotes");
    assert!(
        silent >= acknowledged_after && silent <= acknowledged_before,
        "B delivered nothing: it opens where it was acknowledged, not at the detection ({silent})"
    );
    assert_eq!(at("NEVER", "trades"), 1_234, "a lane with no key falls back to the session's open");

    let inner = lock(&rig.sh.inner);
    for (sid, want) in [(1, 1_000), (2, 2_000), (4, 1_234)] {
        assert!(
            matches!(inner.subs[&sid].phase, Phase::Replaying { gap_at, .. } if gap_at == want),
            "sid {sid} replays under the stamp its GapStart carried"
        );
    }
}

/// A held subscription the daemon refused for good is said again (at `error`) once `DEAD_REMINDER` has
/// passed since it was last said, and not before; with none held there is nothing to say. (The log line
/// itself is the caller-visible half; this holds WHEN it is due.)
#[test]
fn a_dead_subscription_is_reminded_once_the_interval_passes_and_not_before() {
    let mut rig = Rig::new(vec![(1, quotes("A"), Phase::Replaying { gap_at: 5, tries: 0 })]);
    assert_eq!(rig.state.reminded, None, "nothing is dead yet");
    rig.deliver(StreamDown::SubErr {
        sid: 1,
        err: SubErrKind::Unsupported("the daemon dropped that lane".to_string()),
    });
    let said = rig.state.reminded.expect("the refusal starts the reminder clock");
    assert!(matches!(lock(&rig.sh.inner).subs[&1].phase, Phase::Dead));

    let almost = said + DEAD_REMINDER - Duration::from_secs(1);
    assert_eq!(remind_dead(&rig.sh, &mut rig.state, almost), 0, "not due yet");
    let due = said + DEAD_REMINDER;
    assert_eq!(remind_dead(&rig.sh, &mut rig.state, due), 1, "due: the one dead subscription");
    assert_eq!(
        remind_dead(&rig.sh, &mut rig.state, due + Duration::from_secs(1)),
        0,
        "said a second ago"
    );

    lock(&rig.sh.inner).subs.clear(); // the broker gave up on it
    assert_eq!(remind_dead(&rig.sh, &mut rig.state, due + 2 * DEAD_REMINDER), 0, "nothing held");
}

/// **The daemon superseding a conflatable frame is COUNTED and said, and is never a gap.** The daemon
/// numbers every frame before it decides what to drop, so a jump in a conflatable verb's `seq` is frames
/// it replaced (latest-wins) because this reader was behind. A live view needs no `GapStart` for that,
/// but a recorded `kind=depth` tape is thinner for it, so it is counted (`RemoteDataClient::conflated`)
/// and reported at `warn` at most once per `CONFLATION_REPORT`.
#[test]
fn a_skipped_conflatable_seq_is_counted_and_said_and_is_not_a_gap() {
    let depth = |seq: u64, ts: i64| StreamDown::Depth {
        k: 7,
        seq,
        tick_size: 0.5,
        bids: Vec::new(),
        asks: Vec::new(),
        ts,
    };
    let mut rig = bound("A", 7);
    for frame in [
        depth(1, 10),
        depth(5, 14), // 2, 3 and 4 were superseded
        StreamDown::Forming { k: 7, seq: 2, interval: "1m".to_string(), bar: bar(1) }, // 1 was
        StreamDown::Forming { k: 7, seq: 3, interval: "5m".to_string(), bar: bar(2) },
        StreamDown::Mark { k: 7, seq: 1, px: 1.0, ts: 1 },
        StreamDown::Mark { k: 7, seq: 2, px: 1.0, ts: 2 },
    ] {
        assert!(rig.deliver(frame).is_none());
    }
    assert_eq!(rig.sh.conflated.load(Ordering::Relaxed), 4, "3 depth + 1 forming superseded");
    let delivered = rig.sink.recorded();
    assert_eq!(delivered.len(), 6, "every frame that arrived reached the sink: {delivered:?}");
    assert!(rig.statuses().is_empty(), "a latest-wins skip is not a gap: {:?}", rig.statuses());

    // Said aloud: the new count, then nothing for CONFLATION_REPORT, then the next batch.
    let now = Instant::now();
    assert_eq!(report_conflated(&rig.sh, &mut rig.state, now), 4);
    assert_eq!(report_conflated(&rig.sh, &mut rig.state, now), 0, "nothing new");
    assert!(rig.deliver(depth(9, 18)).is_none()); // 6, 7 and 8 superseded
    assert_eq!(rig.sh.conflated.load(Ordering::Relaxed), 7);
    let soon = now + Duration::from_secs(1);
    assert_eq!(report_conflated(&rig.sh, &mut rig.state, soon), 0, "said a second ago");
    let later = now + CONFLATION_REPORT;
    assert_eq!(report_conflated(&rig.sh, &mut rig.state, later), 3);
}

// ---- The delivery class of a subscription (T8b) ------------------------------------------------------------

fn depth_op(symbol: &str) -> SubOp {
    SubOp::Depth { symbol: symbol.to_string() }
}

fn depth(k: u32, seq: u64, ts: i64) -> StreamDown {
    StreamDown::Depth { k, seq, tick_size: 0.5, bids: Vec::new(), asks: Vec::new(), ts }
}

fn class(k: u32, class: WireDelivery) -> StreamDown {
    StreamDown::Delivery { k, lane: WireLane::Depth, class }
}

/// **A raised depth lane is checked for continuity from the daemon's marker, and its skips are no longer
/// conflation.** Before the marker a jump is the daemon superseding (counted, never a gap); after it a
/// jump with no `Gap` is a silent loss, disclosed with the stamp of the last frame delivered, and the
/// conflation counter does not move.
#[test]
fn a_raised_depth_lane_is_continuity_checked_from_the_marker_and_is_not_conflation() {
    let mut rig = Rig::new(vec![(1, depth_op("A"), Phase::Awaiting)]);
    assert!(rig.deliver(StreamDown::SubOk { sid: 1, k: 7 }).is_none());
    for frame in [depth(7, 1, 10), depth(7, 4, 40)] {
        assert!(rig.deliver(frame).is_none());
    }
    assert_eq!(rig.sh.conflated.load(Ordering::Relaxed), 2, "2 and 3 were superseded");

    // The marker: the 4 -> 10 jump before the first lossless frame is not held against the new rule.
    assert!(rig.deliver(class(7, WireDelivery::Lossless)).is_none());
    for frame in [depth(7, 10, 100), depth(7, 11, 110)] {
        assert!(rig.deliver(frame).is_none());
    }
    assert!(rig.statuses().is_empty(), "{:?}", rig.statuses());
    assert_eq!(rig.sh.conflated.load(Ordering::Relaxed), 2, "a lossless lane is not conflation");

    // A silent jump is a hole opening at the last delivered snapshot's stamp.
    assert!(rig.deliver(depth(7, 14, 140)).is_none());
    assert_eq!(
        rig.statuses(),
        vec![
            ("depth".to_string(), StreamStatus::GapStart { at_ts_ms: 110 }),
            ("depth".to_string(), StreamStatus::Live { gap_started_ts_ms: Some(110) }),
        ]
    );
    assert_eq!(rig.sh.conflated.load(Ordering::Relaxed), 2);
}

/// The daemon's own `Gap` over the depth verb is a disclosed hole, and the frame after it chains.
#[test]
fn a_depth_gap_is_a_disclosed_hole_and_the_next_snapshot_chains_from_it() {
    let mut rig = Rig::new(vec![(1, depth_op("A"), Phase::Awaiting)]);
    rig.deliver(StreamDown::SubOk { sid: 1, k: 7 });
    rig.deliver(class(7, WireDelivery::Lossless));
    rig.deliver(depth(7, 1, 10));
    rig.deliver(StreamDown::Gap {
        k: 7,
        verb: GapVerb::Depth,
        stream: WireStream::Lane(WireLane::Depth),
        dropped: 3,
        from_seq: 1,
        to_seq: 5,
        from_ts_ms: 10,
    });
    rig.deliver(depth(7, 5, 50));
    assert_eq!(
        rig.statuses(),
        vec![
            ("depth".to_string(), StreamStatus::GapStart { at_ts_ms: 10 }),
            ("depth".to_string(), StreamStatus::Live { gap_started_ts_ms: Some(10) }),
        ],
        "one disclosure for the Gap; seq 5 followed it with no second one"
    );
}

/// A relax hands the numbering back: the frames the daemon supersedes from then on are counted from
/// exactly the frame after the last lossless one, not from the start of the lane's life.
#[test]
fn a_relax_counts_conflation_from_the_last_lossless_frame_and_not_from_the_lanes_start() {
    let mut rig = Rig::new(vec![(1, depth_op("A"), Phase::Awaiting)]);
    rig.deliver(StreamDown::SubOk { sid: 1, k: 7 });
    rig.deliver(class(7, WireDelivery::Lossless));
    for seq in 1..=30 {
        rig.deliver(depth(7, seq, i64::try_from(seq).unwrap() * 10));
    }
    rig.deliver(class(7, WireDelivery::Conflatable));
    rig.deliver(depth(7, 31, 310));
    assert_eq!(rig.sh.conflated.load(Ordering::Relaxed), 0, "31 follows 30");
    rig.deliver(depth(7, 34, 340));
    assert_eq!(rig.sh.conflated.load(Ordering::Relaxed), 2, "32 and 33 were superseded");
    assert!(rig.statuses().is_empty());
}

/// A marker for a lane whose verbs are lossless already, or for a key this session never bound, does no
/// harm.
#[test]
fn a_marker_for_an_already_lossless_lane_or_an_unknown_key_is_harmless() {
    let mut rig = bound("A", 7);
    assert!(
        rig.deliver(StreamDown::Delivery {
            k: 7,
            lane: WireLane::Quotes,
            class: WireDelivery::Control
        })
        .is_none()
    );
    rig.deliver(quote_frame(7, 1));
    rig.deliver(quote_frame(7, 2));
    assert!(
        rig.deliver(class(99, WireDelivery::Lossless)).is_none(),
        "a stray key is counted, dropped"
    );
    assert!(rig.statuses().is_empty());
}

fn quote_frame(k: u32, seq: u64) -> StreamDown {
    StreamDown::Quote { k, seq, quote: quote(i64::try_from(seq).unwrap()) }
}

/// `set_delivery` remembers the floor per `(symbol, lane)`; the `Sub` builder reads it, and honours the
/// daemon's answer about the feature.
#[test]
fn the_sub_frame_carries_the_remembered_floor_only_when_the_daemon_serves_it() {
    let rig = Rig::new(Vec::new());
    lock(&rig.sh.inner).floors.insert(("A".to_string(), WireLane::Depth), WireDelivery::Lossless);
    let sub = |symbol: &str, op: SubOp| match rig.sh.sub_frame(1, op) {
        StreamUp::Sub { class, .. } => (symbol.to_string(), class),
        other => panic!("{other:?}"),
    };
    assert_eq!(sub("A", depth_op("A")), ("A".to_string(), Some(WireDelivery::Lossless)));
    assert_eq!(sub("B", depth_op("B")).1, None, "another symbol");
    assert_eq!(sub("A", quotes("A")).1, None, "another lane");
    lock(&rig.sh.inner).class_support = Some(false);
    assert_eq!(sub("A", depth_op("A")).1, None, "a daemon without the feature is sent none");
}
