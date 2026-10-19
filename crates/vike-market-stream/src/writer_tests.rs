use std::net::{TcpListener, TcpStream};

use std::sync::Arc;
use vike_data::LiveDataSink;

use vike_marketdata::{L2Book, QuoteTick, TradeTick};

use super::*;
use crate::frame::{is_timeout, read_capped};
use crate::queue::QueueLimits;
use crate::sink::{OpenKind, SinkLimits};

const CEILING: u32 = 1 << 20;

fn cfg(budget: u64, episode: Duration) -> WriterConfig {
    WriterConfig {
        heartbeat: Duration::from_secs(600),
        lapse_budget: budget,
        lapse_window: Duration::from_secs(600),
        lapse_episode: episode,
        down_frame_ceiling_bytes: CEILING,
    }
}

// ---- The counter, on a synthetic clock ----------------------------------------------------------

#[test]
fn disclosures_inside_one_episode_count_once_and_a_later_one_counts_again() {
    let cfg = cfg(8, Duration::from_secs(1));
    let t0 = Instant::now();
    let mut lapses = Lapses::new(t0);
    // Nine disclosures inside one second of the first (three symbols times three verbs): ONE lapse.
    for i in 0..9 {
        assert_eq!(lapses.note(t0 + Duration::from_millis(i * 100), &cfg), 1, "disclosure {i}");
    }
    // The window is ANCHORED at the first disclosure: the next one past it opens episode two...
    assert_eq!(lapses.note(t0 + Duration::from_millis(1_000), &cfg), 2);
    // ... and a laggard that discloses without a pause still opens an episode every `lapse_episode`.
    assert_eq!(lapses.note(t0 + Duration::from_millis(1_900), &cfg), 2);
    assert_eq!(lapses.note(t0 + Duration::from_millis(2_000), &cfg), 3);
}

#[test]
fn the_budget_window_still_forgets_old_episodes() {
    let mut cfg = cfg(8, Duration::from_secs(1));
    cfg.lapse_window = Duration::from_secs(10);
    let t0 = Instant::now();
    let mut lapses = Lapses::new(t0);
    assert_eq!(lapses.note(t0, &cfg), 1);
    assert_eq!(lapses.note(t0 + Duration::from_secs(2), &cfg), 2);
    assert_eq!(lapses.note(t0 + Duration::from_secs(10), &cfg), 1, "a new window starts at zero");
}

// ---- The disclosures a real queue makes, over a real socket -------------------------------------

fn limits() -> SinkLimits {
    // Twelve data frames: with three symbols filled round-robin each key keeps its newest four.
    SinkLimits {
        queue: QueueLimits { bytes: 1 << 22, frames: 12, ctrl_frames: 64 },
        max_keys: 8,
        max_unsolicited: 2,
    }
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

/// A sink with three bound, acknowledged symbols (so their frames are not held).
fn bound_sink() -> WireSink {
    let sink = WireSink::new("binance", limits());
    for (sid, sym) in [(1, "AAA"), (2, "BBB"), (3, "CCC")] {
        let opened = sink.open_sub(sym).expect("open");
        assert_eq!(opened.kind, OpenKind::Fresh);
        sink.sub_ok(sid, sym, opened);
    }
    sink
}

/// One overflow: the three symbols print quotes and trades round-robin into a twelve-frame queue, so
/// every `(key, verb)` ends with a hole in front of its survivors. Nothing drains while it runs.
fn overflow(sink: &WireSink, from_ts: i64) {
    for ts in from_ts..from_ts + 40 {
        for sym in ["AAA", "BBB", "CCC"] {
            sink.quote("binance", sym, quote(ts));
            sink.trade("binance", sym, trade(ts));
        }
    }
}

/// The loopback pair: (writer end, reader end).
fn pair() -> (TcpStream, TcpStream) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let reader = TcpStream::connect(listener.local_addr().expect("addr")).expect("connect");
    let (server, _) = listener.accept().expect("accept");
    reader.set_read_timeout(Some(Duration::from_millis(400))).expect("timeout");
    (server, reader)
}

/// Read until the writer goes quiet (a read timeout) or the stream ends.
fn read_quiet(reader: &mut TcpStream) -> Vec<StreamDown> {
    let mut out = Vec::new();
    loop {
        match read_capped::<_, StreamDown>(reader, CEILING) {
            Ok(frame) => {
                let bye = matches!(frame, StreamDown::Bye(_));
                out.push(frame);
                if bye {
                    return out;
                }
            }
            Err(e) if is_timeout(&e) => return out,
            Err(_) => return out,
        }
    }
}

fn gaps(frames: &[StreamDown]) -> usize {
    frames.iter().filter(|f| matches!(f, StreamDown::Gap { .. })).count()
}

fn too_slow(frames: &[StreamDown]) -> Option<u64> {
    frames.iter().find_map(|f| match f {
        StreamDown::Bye(ByeReason::TooSlow { lapses }) => Some(*lapses),
        _ => None,
    })
}

/// The disclosures the writer would make for what is queued now: each `Taken`'s gaps, and the tail's.
fn disclosures(sink: &WireSink) -> Vec<Vec<StreamDown>> {
    let mut out = Vec::new();
    loop {
        match sink.next(Duration::from_millis(1)) {
            Next::Frame(taken) => {
                if !taken.gaps.is_empty() {
                    out.push(taken.gaps);
                }
            }
            Next::Gaps(gaps) => out.push(gaps),
            Next::Heartbeat => {}
            Next::End(_) | Next::Idle => return out,
        }
    }
}

/// **One overflow across three hot symbols is one lapse.** Several `(key, verb)` holes are disclosed
/// by several `Gap`s, all of them written, and a budget of two is not crossed: the session lives. The
/// whole `run_writer` over a socket, the queue filled before the writer starts.
#[test]
fn one_overflow_across_three_symbols_discloses_every_hole_and_closes_nothing() {
    let sink = bound_sink();
    overflow(&sink, 0);
    let (server, mut reader) = pair();
    let writer_sink = sink.clone();
    let writer = std::thread::spawn(move || {
        run_writer(server, &writer_sink, cfg(2, Duration::from_secs(30)), 1);
    });
    let frames = read_quiet(&mut reader);
    assert!(
        gaps(&frames) > 3,
        "the overflow did not hole enough lanes to prove the count: {} gaps",
        gaps(&frames)
    );
    assert_eq!(too_slow(&frames), None, "one overflow closed the session: {frames:?}");
    // The session is still served: a later frame arrives on the same socket.
    sink.quote("binance", "AAA", quote(1_000));
    let later = read_quiet(&mut reader);
    assert!(matches!(later.last(), Some(StreamDown::Quote { .. })), "{later:?}");
    sink.finish(None);
    writer.join().expect("writer");
}

/// **A second, separate overflow is a second lapse.** With a budget of one the first episode is
/// within it and the second crosses it: the `Gap`s of the second are still written, then `TooSlow`
/// names two lapses. Driven through [`lapse`] with the disclosures a real queue made.
#[test]
fn two_separate_overflows_count_twice_and_cross_a_budget_of_one() {
    let cfg = cfg(1, Duration::from_millis(150));
    let sink = bound_sink();
    let (server, mut reader) = pair();
    let mut out = Batch::new(server, CEILING);
    let mut lapses = Lapses::new(Instant::now());

    overflow(&sink, 0);
    let first = disclosures(&sink);
    assert!(first.len() > 1, "the first overflow made {} disclosures", first.len());
    for batch in &first {
        assert!(lapse(&mut out, batch, &mut lapses, &cfg), "the first episode crossed the budget");
    }
    out.flush().expect("flush");
    let written = read_quiet(&mut reader);
    assert_eq!(gaps(&written), first.iter().map(Vec::len).sum::<usize>(), "every Gap is written");
    assert_eq!(too_slow(&written), None);

    // The episode passed; the second overflow is a new one.
    std::thread::sleep(Duration::from_millis(300));
    overflow(&sink, 1_000);
    let second = disclosures(&sink);
    assert!(!second.is_empty());
    assert!(!lapse(&mut out, &second[0], &mut lapses, &cfg), "the second episode must cut");
    let written = read_quiet(&mut reader);
    assert_eq!(gaps(&written), second[0].len(), "the Gaps of the crossing disclosure are written");
    assert_eq!(too_slow(&written), Some(2), "{written:?}");
}

// ---- Batching: one write per drained batch, the same bytes ---------------------------------------

/// A sink of bytes that counts its `write_all`s.
#[derive(Default)]
struct Counting {
    bytes: Vec<u8>,
    writes: usize,
}

impl Write for Counting {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.writes += 1;
        self.bytes.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Run the writer loop over a [`Counting`] until the queue is dry, then end the session.
fn run_to_idle(sink: &WireSink, cfg: WriterConfig) -> Counting {
    let mut counted = Counting::default();
    std::thread::scope(|s| {
        let writer = s.spawn(|| drive(&mut counted, sink, cfg, 1));
        let deadline = Instant::now() + Duration::from_secs(10);
        while sink.queued().0 > 0 {
            assert!(Instant::now() < deadline, "the writer never drained the queue");
            std::thread::sleep(Duration::from_millis(1));
        }
        // Everything the writer holds goes out on the way out of the loop.
        sink.finish(None);
        writer.join().expect("writer");
    });
    counted
}

/// What the wire carries for what `sink` holds, frame by frame: each frame encoded alone, as the
/// writer did before it batched.
fn reference_bytes(sink: &WireSink) -> (Vec<u8>, usize) {
    let mut bytes = Vec::new();
    let mut frames = 0;
    loop {
        let mut next = Vec::new();
        match sink.next(Duration::from_millis(1)) {
            Next::Frame(taken) => {
                next.extend(taken.gaps);
                next.push(taken.item.into_frame());
            }
            Next::Gaps(gaps) => next.extend(gaps),
            Next::Heartbeat => next.push(StreamDown::Heartbeat),
            Next::End(_) | Next::Idle => return (bytes, frames),
        }
        for frame in next {
            bytes.extend_from_slice(&crate::frame::encode_capped(&frame, CEILING).expect("encode"));
            frames += 1;
        }
    }
}

fn decode_all(mut bytes: &[u8]) -> Vec<StreamDown> {
    let mut out = Vec::new();
    while !bytes.is_empty() {
        out.push(read_capped::<_, StreamDown>(&mut bytes, CEILING).expect("a whole frame"));
    }
    out
}

fn a_book() -> L2Book {
    let level = |price: f64, qty: f64| vike_marketdata::BookLevel::new(price, qty);
    let mut book = L2Book::new(0.5);
    book.apply_snapshot(
        7,
        &[level(100.0, 3.0), level(99.5, 1.25), level(99.0, 8.0)],
        &[level(100.5, 4.0), level(101.0, 0.5)],
    );
    book
}

/// **A book serialized by reference is the `StreamDown::Book` frame, byte for byte.** The writer skips
/// `Item::into_frame`'s deep copy of the book; the bytes must not know.
#[test]
fn a_book_frame_serialized_by_reference_is_the_stream_down_book_byte_for_byte() {
    let book = a_book();
    let owned = crate::frame::encode_capped(
        &StreamDown::Book { k: 3, seq: 9, book: Box::new(book.clone()) },
        CEILING,
    )
    .expect("encode");
    let mut batch = Batch::new(Vec::new(), CEILING);
    batch.put_book(3, 9, &book).expect("put");
    assert_eq!(batch.buf, owned);
    let back: StreamDown = read_capped(&mut batch.buf.as_slice(), CEILING).expect("decode");
    assert!(matches!(back, StreamDown::Book { k: 3, seq: 9, .. }), "{back:?}");
}

/// **The batch is byte-for-byte the frames written one by one.** The same queue (an overflow across
/// three symbols, so `Gap`s and control frames ride in it too) is drained once the old way and once
/// by the batching writer: the bytes are equal, and they go out in a few writes.
#[test]
fn the_batched_bytes_equal_the_frames_written_one_by_one() {
    let (reference, frames) = {
        let sink = bound_sink();
        overflow(&sink, 0);
        sink.book("binance", "AAA", Arc::new(a_book()));
        reference_bytes(&sink)
    };
    assert!(frames > 12, "a trivial queue proves nothing: {frames} frames");
    let sink = bound_sink();
    overflow(&sink, 0);
    sink.book("binance", "AAA", Arc::new(a_book()));
    let batched = run_to_idle(&sink, cfg(1_000_000, Duration::from_secs(1)));
    assert_eq!(batched.bytes, reference, "the wire bytes changed");
    // One batch, unless the age bound split it on a stalled thread: never a write per frame.
    assert!(batched.writes * 2 <= frames, "{} writes for {frames} frames", batched.writes);
}

/// **A long queue is written in batches: far fewer writes than frames, and never a batch past its
/// bound.** 301 frames at 64 per batch is five writes; the age bound may split a batch on a slow box,
/// so the assertion is a ceiling well under one write per frame, plus the floor the bound implies.
#[test]
fn a_long_queue_is_written_in_a_few_batches_with_the_same_bytes() {
    let big = || {
        let sink = WireSink::new(
            "binance",
            SinkLimits {
                queue: QueueLimits { bytes: 1 << 24, frames: 1 << 14, ctrl_frames: 64 },
                max_keys: 8,
                max_unsolicited: 2,
            },
        );
        let opened = sink.open_sub("AAA").expect("open");
        sink.sub_ok(1, "AAA", opened);
        for ts in 0..300 {
            sink.quote("binance", "AAA", quote(ts));
        }
        sink
    };
    let (reference, frames) = reference_bytes(&big());
    assert_eq!(frames, 301);
    let sink = big();
    let batched = run_to_idle(&sink, cfg(1_000_000, Duration::from_secs(1)));
    assert_eq!(batched.bytes, reference);
    assert!(batched.writes >= 301_usize.div_ceil(BATCH_FRAMES), "{} writes", batched.writes);
    assert!(batched.writes <= 150, "{} writes for 301 frames: not batched", batched.writes);
}

/// **The last frame queued is written at once.** An idle writer with a ten-minute heartbeat gets one
/// frame and the peer reads it: the writer does not hold a batch open for company.
#[test]
fn the_last_frame_queued_is_written_without_waiting_for_more() {
    let sink = bound_sink();
    let (server, mut reader) = pair();
    reader.set_read_timeout(Some(Duration::from_secs(5))).expect("timeout");
    let writer_sink = sink.clone();
    let writer = std::thread::spawn(move || {
        run_writer(server, &writer_sink, cfg(8, Duration::from_secs(1)), 1);
    });
    // The three SubOks come first; then one quote on an otherwise idle session.
    for _ in 0..3 {
        let frame: StreamDown = read_capped(&mut reader, CEILING).expect("SubOk");
        assert!(matches!(frame, StreamDown::SubOk { .. }), "{frame:?}");
    }
    let started = Instant::now();
    sink.quote("binance", "AAA", quote(7));
    let frame: StreamDown = read_capped(&mut reader, CEILING).expect("the quote");
    assert!(matches!(frame, StreamDown::Quote { seq: 1, .. }), "{frame:?}");
    assert!(started.elapsed() < Duration::from_secs(2), "the quote waited {:?}", started.elapsed());
    sink.finish(None);
    writer.join().expect("writer");
}

/// **A frame over the ceiling inside a batch adds no byte and the stream stays in step.** The middle
/// quote carries a symbol longer than the ceiling: it is dropped before its first byte, the frames
/// around it decode, and the hole is disclosed by a `Gap` ahead of the next survivor.
#[test]
fn a_refused_frame_in_the_middle_of_a_batch_leaves_the_stream_in_step() {
    let sink = bound_sink();
    sink.quote("binance", "AAA", quote(0));
    let mut fat = quote(1);
    fat.symbol = "x".repeat(2_000);
    sink.quote("binance", "AAA", fat);
    sink.quote("binance", "AAA", quote(2));
    let mut small = cfg(1_000_000, Duration::from_secs(1));
    small.down_frame_ceiling_bytes = 1_000;
    let counted = run_to_idle(&sink, small);
    let frames = decode_all(&counted.bytes);
    let quotes: Vec<u64> = frames
        .iter()
        .filter_map(|f| match f {
            StreamDown::Quote { seq, .. } => Some(*seq),
            _ => None,
        })
        .collect();
    assert_eq!(quotes, [1, 3], "{frames:?}");
    assert_eq!(gaps(&frames), 1, "the refused quote is a disclosed hole: {frames:?}");
}

/// A batch is due at its frame count, its byte count or its age, and not before.
#[test]
fn a_batch_is_full_at_its_frame_count_its_byte_count_or_its_age() {
    let t0 = Instant::now();
    let mut b = Batch::new(Vec::new(), CEILING);
    assert!(!b.full(t0), "an empty batch is not due");
    b.put(&StreamDown::Heartbeat).expect("put");
    assert!(!b.full(t0), "one fresh frame is not due");
    assert!(b.full(Instant::now() + BATCH_AGE + Duration::from_millis(1)), "age");
    for _ in 1..BATCH_FRAMES - 1 {
        b.put(&StreamDown::Heartbeat).expect("put");
    }
    assert!(!b.full(t0), "{} frames are under the count", BATCH_FRAMES - 1);
    b.put(&StreamDown::Heartbeat).expect("put");
    assert!(b.full(t0), "the frame count");
    b.flush().expect("flush");
    assert!(b.is_empty() && !b.full(Instant::now() + BATCH_AGE * 2), "a flushed batch starts over");
    b.put(&StreamDown::Key { k: 1, symbol: "x".repeat(BATCH_BYTES) }).expect("put");
    assert!(b.full(t0), "the byte count");
}

/// A refused frame leaves the buffer as it was; a good one is appended after it.
#[test]
fn a_frame_over_the_ceiling_adds_nothing_to_the_batch() {
    let mut b = Batch::new(Vec::new(), 100);
    b.put(&StreamDown::Heartbeat).expect("put");
    let before = b.buf.clone();
    let err =
        b.put(&StreamDown::Key { k: 1, symbol: "x".repeat(500) }).expect_err("over the ceiling");
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    assert_eq!(b.buf, before);
    assert_eq!(b.frames, 1);
}

// ---- A seed over the ceiling is cut to its newest bars, not dropped ------------------------------

/// The strategy lane's `SL_FRAME_CEILING_BYTES` (`vike-datahub-client`'s `proto`, a layer above this crate).
const LANE_CEILING: u32 = 128 * 1024;

/// What a venue's REST warm-up hands the sink: `n` one-minute bars of a BTC perp, prices and volume decimal
/// strings parsed to `f64` the way `kline_to_bar` does, no symbol (`None` serializes as `null`).
fn kline_seed(n: usize) -> Vec<Bar> {
    let price = |i: usize, phase: f64| -> f64 {
        let p = 107_000.0 + (i as f64 * 0.37 + phase).sin() * 150.0;
        format!("{p:.1}").parse().expect("a price")
    };
    (0..n)
        .map(|i| {
            let volume = 40.0 + ((i * 7919) % 5000) as f64 / 31.0;
            Bar::ohlcv(
                1_760_000_000_000 + i as i64 * 60_000,
                price(i, 0.0),
                price(i, 0.3),
                price(i, -0.3),
                price(i, 0.1),
                format!("{volume:.3}").parse().expect("a volume"),
            )
        })
        .collect()
}

fn seed_frame(k: u32, seq: u64, bars: &[Bar]) -> StreamDown {
    StreamDown::Seed { k, seq, interval: "1m".to_string(), bars: bars.to_vec() }
}

/// **A seed serialized by reference is the `StreamDown::Seed` frame, byte for byte** when it fits.
#[test]
fn a_seed_frame_serialized_by_reference_is_the_stream_down_seed_byte_for_byte() {
    let bars = kline_seed(5);
    let owned = crate::frame::encode_capped(&seed_frame(3, 9, &bars), CEILING).expect("encode");
    let mut batch = Batch::new(Vec::new(), CEILING);
    assert_eq!(batch.put_seed(3, 9, "1m", &bars).expect("put"), 0, "nothing is left out");
    assert_eq!(batch.buf, owned);
}

/// **The cut is exact to the byte**: at every ceiling the frame carries the longest newest-suffix of the bars
/// whose JSON body fits it (the comma between bars counted), says how many it left out, and a ceiling under
/// one bar refuses the seed whole and adds no byte.
#[test]
fn a_seed_over_the_ceiling_is_cut_to_the_longest_newest_run_that_fits() {
    let bars = kline_seed(3);
    let body = |n: usize| {
        serde_json::to_vec(&seed_frame(3, 9, &bars[3 - n..])).expect("encode").len() as u32
    };
    for (ceiling, kept) in
        [(body(3), 3), (body(3) - 1, 2), (body(2), 2), (body(2) - 1, 1), (body(1), 1)]
    {
        let mut batch = Batch::new(Vec::new(), ceiling);
        let left_out = batch.put_seed(3, 9, "1m", &bars).expect("a cut seed is put");
        assert_eq!(left_out, 3 - kept, "ceiling {ceiling}");
        let sent: StreamDown = read_capped(&mut batch.buf.as_slice(), ceiling).expect("decode");
        let StreamDown::Seed { k: 3, seq: 9, bars: got, .. } = sent else { panic!("not a seed") };
        assert_eq!(got, bars[3 - kept..], "ceiling {ceiling}: the NEWEST bars, in order");
    }
    let mut batch = Batch::new(Vec::new(), body(1) - 1);
    let err = batch.put_seed(3, 9, "1m", &bars).expect_err("no bar fits");
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    assert!(batch.buf.is_empty(), "a refused seed adds no byte");
    assert_eq!(batch.frames, 0);
}

/// **The incident (2026-10-18, v0.1.72): bybit's 999-bar kline seed is about 150 KB and the strategy lane
/// reads 128 KiB.** The writer used to log `a frame body of 149869 bytes exceeds the 131072-byte ceiling`
/// and owe a hole, so the tradehub's maker started with no bar history. Now the seed is delivered, read
/// by a client under the lane's own ceiling, as its newest bars, with no hole.
#[test]
fn a_thousand_bar_kline_seed_over_the_lane_ceiling_reaches_the_client_as_its_newest_bars() {
    let bars = kline_seed(999);
    let whole = serde_json::to_vec(&seed_frame(1, 1, &bars)).expect("encode").len();
    assert!(
        whole > LANE_CEILING as usize + 10_000,
        "the seed must reproduce the incident's size: {whole} bytes against {LANE_CEILING}"
    );

    let sink = bound_sink();
    sink.seed_bars("bybit", "AAA", "1m", bars.clone());
    let mut lane = cfg(1_000_000, Duration::from_secs(1));
    lane.down_frame_ceiling_bytes = LANE_CEILING;
    let counted = run_to_idle(&sink, lane);

    // The client side: every frame read under the lane's ceiling, as the tradehub does.
    let mut rest = counted.bytes.as_slice();
    let mut frames = Vec::new();
    while !rest.is_empty() {
        frames.push(
            read_capped::<_, StreamDown>(&mut rest, LANE_CEILING).expect("a frame the lane reads"),
        );
    }
    assert_eq!(gaps(&frames), 0, "a cut seed is delivered, not a disclosed hole");
    let mut seeds = frames.iter().filter_map(|f| match f {
        StreamDown::Seed { k, seq, bars, .. } => Some((*k, *seq, bars)),
        _ => None,
    });
    let (k, seq, got) = seeds.next().expect("the seed arrived");
    assert!(seeds.next().is_none(), "one seed in, one seed out");
    assert_eq!(seq, 1);
    assert!(got.len() > 800 && got.len() < bars.len(), "{} of {} bars", got.len(), bars.len());
    assert_eq!(got.as_slice(), &bars[bars.len() - got.len()..], "the newest bars, in order");
    let one_more = seed_frame(k, seq, &bars[bars.len() - got.len() - 1..]);
    assert!(
        crate::frame::encode_capped(&one_more, LANE_CEILING).is_err(),
        "one more bar would not fit: the cut keeps all the ceiling allows"
    );
}

/// A seed that cannot be cut to anything is still what it was: dropped before its first byte and owed as a
/// disclosed hole, with the stream in step for the frames around it.
#[test]
fn a_seed_with_no_bar_that_fits_is_still_a_disclosed_hole() {
    let sink = bound_sink();
    sink.seed_bars("binance", "AAA", "1m", kline_seed(3));
    sink.quote("binance", "AAA", quote(2));
    let mut small = cfg(1_000_000, Duration::from_secs(1));
    small.down_frame_ceiling_bytes = 200;
    let counted = run_to_idle(&sink, small);
    let mut rest = counted.bytes.as_slice();
    let mut frames = Vec::new();
    while !rest.is_empty() {
        frames.push(read_capped::<_, StreamDown>(&mut rest, 200).expect("a whole frame"));
    }
    assert!(!frames.iter().any(|f| matches!(f, StreamDown::Seed { .. })), "{frames:?}");
    assert_eq!(gaps(&frames), 1, "the refused seed is a disclosed hole: {frames:?}");
    assert!(frames.iter().any(|f| matches!(f, StreamDown::Quote { .. })), "{frames:?}");
}
