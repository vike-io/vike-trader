//! Drift register R3: **a venue pump never blocks on its consumer, and a loss is never silent.**
//!
//! The consumer here is a socket nobody reads (a laggard), and the producer is the thread a venue feed
//! would call the sink from. Each frame is BIG (a long symbol field), so the socket buffers fill after a
//! few megabytes and the session's writer blocks for real; from then on the bounded queue (a few
//! hundred KiB) is the only thing between the producer and the consumer.
//!
//! What must hold: every producer call returns promptly however long the consumer is parked; the loss
//! is announced by `Gap` frames whose numbers chain exactly with the frames around them; frames
//! delivered plus frames the gaps report lost equal frames produced; one session's laggard never
//! costs another session a frame; and a reader that keeps losing frames is cut off with `TooSlow`.

#[path = "support/fake_lanes.rs"]
#[allow(dead_code, clippy::allow_attributes)]
mod fake_lanes;
#[path = "support/harness.rs"]
#[allow(dead_code, clippy::allow_attributes)]
mod harness;
#[path = "support/raw_peer.rs"]
#[allow(dead_code, clippy::allow_attributes)]
mod raw_peer;

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use vike_data::{DataClient, LiveDataSink};
use vike_gatewayhub::server::limits::ServerConfig;
use vike_gatewayhub_client::opener::GatewayOpener;
use vike_hub_proto::auth::Scope;
use vike_market_stream::market::RemoteDataClient;
use vike_market_stream::wire::{ByeReason, StreamDown, StreamUp, SubOp, WireLane, WireStream};
use vike_marketdata::QuoteTick;
use vike_model::StreamStatus;

use harness::{Harness, test_keys};
use raw_peer::Raw;

/// A symbol this long makes every frame ~8 KiB, so a flood fills the socket buffers quickly.
const FAT: usize = 8 * 1024;

fn config() -> ServerConfig {
    let mut c = ServerConfig::PRODUCTION;
    c.limits.queue_bytes = 128 * 1024;
    c.limits.queue_frames = 256;
    // A tight producer loop outruns even a healthy writer, so these tests would trip the production
    // lapse budget (8 gaps a minute) on their own pace: the budget is raised out of the way here and
    // the one test about the budget lowers it.
    c.limits.lapse_budget = 1_000_000;
    c
}

fn fat_quote(ts: i64, symbol: &str) -> QuoteTick {
    QuoteTick {
        ts,
        local_ts: 0,
        bid: 1.0,
        ask: 2.0,
        bid_size: 1.0,
        ask_size: 1.0,
        symbol: format!("{symbol}{}", "x".repeat(FAT)),
    }
}

fn open_quotes(h: &Harness, symbol: &str) -> (Raw, u32) {
    let mut raw = Raw::open(h.addr, &test_keys(), Scope::Read);
    raw.open_stream("binance");
    raw.up(&StreamUp::Sub { sid: 1, op: SubOp::Quotes { symbol: symbol.into() }, class: None });
    let k = match raw.down() {
        StreamDown::SubOk { k, .. } => k,
        other => panic!("{other:?}"),
    };
    (raw, k)
}

/// Push `n` fat quotes through `sink`, returning the worst single-call latency and the total time.
fn flood(sink: &Arc<dyn LiveDataSink>, symbol: &str, n: i64) -> (Duration, Duration) {
    let started = Instant::now();
    let mut worst = Duration::ZERO;
    for ts in 0..n {
        let q = fat_quote(ts, symbol);
        let t0 = Instant::now();
        sink.quote("binance", symbol, q);
        worst = worst.max(t0.elapsed());
    }
    (worst, started.elapsed())
}

/// Read `raw` until `produced` quote frames are accounted for (delivered or reported lost), checking
/// the chain rules on the way. Returns `(delivered, lost, gaps)`.
fn audit(raw: &mut Raw, k: u32, produced: u64, progress: Option<&AtomicU64>) -> (u64, u64, u64) {
    let (mut last, mut delivered, mut lost, mut gaps) = (0u64, 0u64, 0u64, 0u64);
    let mut promised: Option<u64> = None;
    while delivered + lost < produced {
        match raw.down() {
            StreamDown::Quote { k: fk, seq, .. } if fk == k => {
                assert!(seq > last, "seq {seq} after {last}");
                match promised.take() {
                    Some(to) => assert_eq!(seq, to, "the Gap promised {to} and the frame is {seq}"),
                    None => assert_eq!(seq, last + 1, "a SILENT hole before {seq} (last {last})"),
                }
                last = seq;
                delivered += 1;
                if let Some(p) = progress {
                    p.fetch_add(1, Ordering::SeqCst);
                }
            }
            StreamDown::Gap { k: gk, dropped, from_seq, to_seq, stream, .. } if gk == k => {
                assert_eq!(from_seq, last, "a Gap that does not start where delivery stopped");
                assert!(to_seq > from_seq && dropped > 0);
                assert_eq!(stream, WireStream::Lane(WireLane::Quotes), "the Gap names the lane");
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
fn a_parked_consumer_never_blocks_the_producer_and_every_loss_is_disclosed() {
    const N: i64 = 5_000;
    let h = Harness::start_config(config());
    let (mut raw, k) = open_quotes(&h, "BTCUSDT");
    let sink = h.data.session(0).sink.clone();

    // The consumer is parked: nothing reads `raw` while the producer floods.
    let (worst, total) = flood(&sink, "BTCUSDT", N);
    assert!(
        worst < Duration::from_millis(500),
        "one sink call took {worst:?}: the pump was blocked"
    );
    assert!(total < Duration::from_secs(20), "{N} sink calls took {total:?}: the pump was blocked");

    // Un-park: every frame is accounted for, and the chain rules held on the way.
    let (delivered, lost, gaps) = audit(&mut raw, k, N as u64, None);
    assert_eq!(delivered + lost, N as u64);
    assert!(
        lost > 0 && gaps > 0,
        "the flood never overflowed ({delivered} delivered): the test proved nothing"
    );
    assert!(delivered > 0);
}

#[test]
fn a_reader_that_keeps_losing_frames_is_cut_off_with_too_slow() {
    let mut cfg = config();
    cfg.limits.lapse_budget = 0;
    let h = Harness::start_config(cfg);
    let (mut raw, k) = open_quotes(&h, "BTCUSDT");
    let sink = h.data.session(0).sink.clone();
    let _ = flood(&sink, "BTCUSDT", 4_000);
    let mut saw_gap = false;
    loop {
        match raw.down() {
            StreamDown::Gap { k: gk, .. } if gk == k => saw_gap = true,
            StreamDown::Bye(reason) => {
                assert!(matches!(reason, ByeReason::TooSlow { lapses: 1 }), "{reason:?}");
                break;
            }
            StreamDown::Quote { .. } | StreamDown::Heartbeat => {}
            other => panic!("{other:?}"),
        }
    }
    assert!(saw_gap, "the disclosure that tripped the budget must be written before the Bye");
    assert!(raw.closed_within(Duration::from_secs(3)));
}

#[test]
fn one_sessions_laggard_costs_another_session_nothing() {
    const N: i64 = 1_500;
    let h = Harness::start_config(config());
    let (_parked, _) = open_quotes(&h, "AAA");
    let (mut live, k) = open_quotes(&h, "BBB");
    let parked_sink = h.data.session(0).sink.clone();
    let live_sink = h.data.session(1).sink.clone();

    let consumed = Arc::new(AtomicU64::new(0));
    let progress = Arc::clone(&consumed);
    let reader = std::thread::Builder::new()
        .name("test-live-reader".into())
        .spawn(move || audit(&mut live, k, N as u64, Some(&progress)))
        .expect("spawn");
    // Interleave: the live session is fed while the parked one is flooded, by the same producer
    // thread. The live consumer is kept close behind the producer (at most 8 frames in flight, far
    // inside its queue), so any frame it loses was lost to its neighbour and not to its own pace.
    let started = Instant::now();
    for ts in 0..N {
        parked_sink.quote("binance", "AAA", fat_quote(ts, "AAA"));
        live_sink.quote("binance", "BBB", fat_quote(ts, "BBB"));
        let deadline = Instant::now() + Duration::from_secs(10);
        while (ts as u64 + 1).saturating_sub(consumed.load(Ordering::SeqCst)) > 8 {
            assert!(Instant::now() < deadline, "the live reader stopped consuming");
            std::thread::sleep(Duration::from_micros(100));
        }
    }
    let (delivered, lost, gaps) = reader.join().expect("reader");
    assert!(started.elapsed() < Duration::from_secs(30));
    assert_eq!(
        (lost, gaps),
        (0, 0),
        "a healthy session lost frames because a neighbour was parked"
    );
    assert_eq!(delivered, N as u64);
}

/// A sink that parks its caller on the first quote until released, then records what it saw.
#[derive(Default)]
struct Parking {
    release: AtomicBool,
    events: Mutex<Vec<(String, String, StreamStatus)>>,
    quotes: Mutex<u64>,
}

impl LiveDataSink for Parking {
    fn seed_bars(&self, _v: &str, _s: &str, _i: &str, _b: Vec<vike_marketdata::Bar>) {}
    fn close_bar(&self, _v: &str, _s: &str, _i: &str, _b: vike_marketdata::Bar) {}
    fn forming_bar(&self, _v: &str, _s: &str, _i: &str, _b: vike_marketdata::Bar) {}
    fn mark_tick(&self, _v: &str, _s: &str, _p: f64, _t: i64) {}
    fn quote(&self, _v: &str, _s: &str, _q: QuoteTick) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while !self.release.load(Ordering::SeqCst) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        *self.quotes.lock().unwrap_or_else(PoisonError::into_inner) += 1;
    }
    fn trade(&self, _v: &str, _s: &str, _t: vike_marketdata::TradeTick) {}
    fn book(&self, _v: &str, _s: &str, _b: Arc<vike_marketdata::L2Book>) {}
    fn stream_status(&self, _v: &str, symbol: &str, stream: &str, status: StreamStatus) {
        self.events.lock().unwrap_or_else(PoisonError::into_inner).push((
            symbol.to_string(),
            stream.to_string(),
            status,
        ));
    }
}

#[test]
fn the_real_client_turns_a_gap_into_a_disclosed_hole_for_its_sink() {
    const N: i64 = 4_000;
    let h = Harness::start_config(config());
    let sink = Arc::new(Parking::default());
    let mut client = RemoteDataClient::new(GatewayOpener::new(h.link()), "binance", sink.clone())
        .expect("client");
    client.subscribe_quotes("BTCUSDT").expect("quotes");
    let session_sink = h.data.session(0).sink.clone();

    // The first quote parks the client's reader thread inside its sink: the consumer is parked.
    let (worst, _total) = flood(&session_sink, "BTCUSDT", N);
    assert!(worst < Duration::from_millis(500), "the pump was blocked for {worst:?}");
    sink.release.store(true, Ordering::SeqCst);

    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let (count, holes) = {
            let events = sink.events.lock().unwrap_or_else(PoisonError::into_inner);
            let holes = events
                .iter()
                .filter(|(s, w, st)| {
                    s == "BTCUSDT" && w == "quotes" && matches!(st, StreamStatus::GapStart { .. })
                })
                .count();
            (*sink.quotes.lock().unwrap_or_else(PoisonError::into_inner), holes)
        };
        if holes > 0 && count > 0 {
            // Delivered + lost = produced is the raw suites' check; here the point is the DISCLOSURE.
            assert!(count < N as u64, "nothing was lost: the flood proved nothing");
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the client never disclosed a hole (saw {count} quotes)"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    client.shutdown();
}

/// One thing the sink saw, in the order it saw it.
#[derive(Debug, Clone, PartialEq)]
enum Ev {
    Quote(i64),
    GapStart(i64),
    Live(Option<i64>),
}

/// A sink that parks its caller (the client's reader) on every quote until released, and keeps one
/// ordered log of the quotes and the stream-health markers it was given.
#[derive(Default)]
struct Ordered {
    release: AtomicBool,
    log: Mutex<Vec<Ev>>,
}

impl Ordered {
    fn log(&self) -> Vec<Ev> {
        self.log.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }
}

impl LiveDataSink for Ordered {
    fn seed_bars(&self, _v: &str, _s: &str, _i: &str, _b: Vec<vike_marketdata::Bar>) {}
    fn close_bar(&self, _v: &str, _s: &str, _i: &str, _b: vike_marketdata::Bar) {}
    fn forming_bar(&self, _v: &str, _s: &str, _i: &str, _b: vike_marketdata::Bar) {}
    fn mark_tick(&self, _v: &str, _s: &str, _p: f64, _t: i64) {}
    fn quote(&self, _v: &str, _s: &str, q: QuoteTick) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while !self.release.load(Ordering::SeqCst) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        self.log.lock().unwrap_or_else(PoisonError::into_inner).push(Ev::Quote(q.ts));
    }
    fn trade(&self, _v: &str, _s: &str, _t: vike_marketdata::TradeTick) {}
    fn book(&self, _v: &str, _s: &str, _b: Arc<vike_marketdata::L2Book>) {}
    fn stream_status(&self, _v: &str, _symbol: &str, _stream: &str, status: StreamStatus) {
        let ev = match status {
            StreamStatus::GapStart { at_ts_ms } => Ev::GapStart(at_ts_ms),
            StreamStatus::Live { gap_started_ts_ms } => Ev::Live(gap_started_ts_ms),
            StreamStatus::Stale { .. } => return,
        };
        self.log.lock().unwrap_or_else(PoisonError::into_inner).push(ev);
    }
}

/// **A queue-overflow hole is stamped with WHEN IT OPENED, so it sorts before the survivors.** The
/// recorder files a `GapStart` under its `at_ts_ms` and replay orders rows by `(ts, seq)`
/// (`crates/vike-store/src/archive_store.rs`'s `scan_book_updates_impl`). A reader that is seconds
/// behind used to stamp the marker with its OWN clock - after every survivor still queued behind it -
/// so replay folded the survivors onto a book missing the lost frames and emitted wrong quotes until
/// the marker landed. Here the frames carry stamps 0..N (nothing like the wall clock) and every marker
/// must sit, by stamp, at or after the last quote delivered before it and at or before the first one
/// delivered after it - and its `Live` must carry the same stamp.
#[test]
fn a_hole_after_overflow_is_stamped_between_the_frames_around_it_not_with_the_readers_clock() {
    const N: i64 = 4_000;
    let h = Harness::start_config(config());
    let sink = Arc::new(Ordered::default());
    let mut client = RemoteDataClient::new(GatewayOpener::new(h.link()), "binance", sink.clone())
        .expect("client");
    client.subscribe_quotes("BTCUSDT").expect("quotes");
    let session_sink = h.data.session(0).sink.clone();

    // The consumer is parked inside its first quote while the producer floods past the queue bound.
    let (worst, _total) = flood(&session_sink, "BTCUSDT", N);
    assert!(worst < Duration::from_millis(500), "the pump was blocked for {worst:?}");
    sink.release.store(true, Ordering::SeqCst);

    let deadline = Instant::now() + Duration::from_secs(30);
    while !sink.log().contains(&Ev::Quote(N - 1)) {
        assert!(Instant::now() < deadline, "the newest quote never arrived");
        std::thread::sleep(Duration::from_millis(20));
    }
    let log = sink.log();
    let holes: Vec<(usize, i64)> = log
        .iter()
        .enumerate()
        .filter_map(|(i, e)| match e {
            Ev::GapStart(at) => Some((i, *at)),
            _ => None,
        })
        .collect();
    assert!(!holes.is_empty(), "the flood never overflowed: the test proved nothing");
    for (i, at) in holes {
        assert!((0..N).contains(&at), "the marker carries {at}: a clock, not a frame's stamp");
        let before = log[..i].iter().rev().find_map(|e| match e {
            Ev::Quote(ts) => Some(*ts),
            _ => None,
        });
        let after = log[i..].iter().find_map(|e| match e {
            Ev::Quote(ts) => Some(*ts),
            _ => None,
        });
        if let Some(before) = before {
            assert!(
                before <= at,
                "the marker {at} sorts BEFORE the quote {before} delivered ahead of it"
            );
        }
        let after = after.expect("a hole is announced before a survivor");
        assert!(at <= after, "the marker {at} sorts AFTER the oldest survivor {after}");
        assert_eq!(
            log.get(i + 1),
            Some(&Ev::Live(Some(at))),
            "the Live that closes the hole carries the same stamp"
        );
    }
    client.shutdown();
}
