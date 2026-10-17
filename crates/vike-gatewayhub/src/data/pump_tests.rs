//! The tick pump lane's unit tests: the book encoder, the message translator and the shared hub, over a
//! scripted pump (a `TickSender` the test holds) and the shared recording sink. No socket, no venue.

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use vike_data::{RecordingSink, SinkCall};
use vike_exec::{
    DataDelayUpdate, FlowUpdate, Ingest, QuoteUpdate, StreamStatusUpdate, TickSender, TradeUpdate,
};
use vike_marketdata::{BookLevel, BookUpdateKind, L2Book, QuoteTick, TradeTick};
use vike_model::{DataDelay, FeedStatus, FlowToxicity, StreamStatus};

use super::*;

fn lvl(price: f64, qty: f64) -> BookLevel {
    BookLevel::new(price, qty)
}

fn book(tick: f64, seq: u64, bids: &[(f64, f64)], asks: &[(f64, f64)]) -> L2Book {
    let mut b = L2Book::new(tick);
    let bids: Vec<_> = bids.iter().map(|&(p, q)| lvl(p, q)).collect();
    let asks: Vec<_> = asks.iter().map(|&(p, q)| lvl(p, q)).collect();
    b.apply_snapshot(seq, &bids, &asks);
    b
}

fn quote_tick(symbol: &str, x: f64) -> QuoteTick {
    QuoteTick {
        ts: 1,
        local_ts: 2,
        bid: x,
        ask: x + 0.5,
        bid_size: 1.0,
        ask_size: 2.0,
        symbol: symbol.into(),
    }
}

fn trade_tick(symbol: &str, x: f64) -> TradeTick {
    TradeTick {
        ts: 3,
        local_ts: 4,
        price: x,
        size: 0.25,
        is_buyer_maker: true,
        symbol: symbol.into(),
    }
}

fn t0() -> Instant {
    Instant::now()
}

// ---- diff_side ---------------------------------------------------------------------------------------

#[test]
fn a_diff_reports_changed_added_and_removed_levels_in_both_orders() {
    let old = [lvl(10.0, 1.0), lvl(9.0, 2.0), lvl(8.0, 3.0)];
    let new = [lvl(11.0, 1.0), lvl(10.0, 1.0), lvl(9.0, 5.0)];
    // Bids (descending): 11 is new, 10 is unchanged, 9 changed, 8 gone.
    assert_eq!(diff_side(&old, &new, true), vec![lvl(11.0, 1.0), lvl(9.0, 5.0), lvl(8.0, 0.0)]);
    // The same numbers read as asks (ascending): the ORDER decides which side of the walk a level is on.
    let old = [lvl(8.0, 3.0), lvl(9.0, 2.0), lvl(10.0, 1.0)];
    let new = [lvl(9.0, 5.0), lvl(10.0, 1.0), lvl(11.0, 1.0)];
    assert_eq!(diff_side(&old, &new, false), vec![lvl(8.0, 0.0), lvl(9.0, 5.0), lvl(11.0, 1.0)]);
    assert!(diff_side(&old, &old, false).is_empty());
    assert_eq!(diff_side(&[], &new, false), new.to_vec());
    assert_eq!(
        diff_side(&old, &[], false),
        old.iter().map(|l| lvl(l.price, 0.0)).collect::<Vec<_>>()
    );
}

// ---- BookFeed ----------------------------------------------------------------------------------------

/// The first book of a feed is an anchor carrying the whole top of the book, and a book that did not
/// change what the subscriber can see sends nothing at all.
#[test]
fn the_first_book_is_an_anchor_and_an_unchanged_book_sends_nothing() {
    let mut feed = BookFeed::new();
    let b = book(0.5, 10, &[(100.0, 1.0), (99.5, 2.0)], &[(100.5, 1.5)]);
    let first = feed.next(&b, t0(), 7, "BTCUSDT").expect("an anchor");
    assert_eq!(first.kind, BookUpdateKind::Snapshot);
    assert_eq!((first.seq, first.tick_size, first.ts), (10, 0.5, 7));
    assert_eq!(first.symbol, "BTCUSDT");
    assert_eq!(first.bids, vec![lvl(100.0, 1.0), lvl(99.5, 2.0)]);
    assert_eq!(first.asks, vec![lvl(100.5, 1.5)]);
    // The pump sends the same standing book again (a bookTicker tick that moved nothing in the ladder).
    assert!(feed.next(&b, t0(), 8, "BTCUSDT").is_none());
}

#[test]
fn a_changed_book_is_a_delta_carrying_only_the_changed_levels() {
    let mut feed = BookFeed::new();
    let now = t0();
    feed.next(&book(0.5, 10, &[(100.0, 1.0), (99.5, 2.0)], &[(100.5, 1.5)]), now, 1, "S");
    let next = book(0.5, 11, &[(100.0, 4.0), (99.5, 2.0)], &[(100.5, 1.5), (101.0, 3.0)]);
    let delta = feed.next(&next, now, 2, "S").expect("a delta");
    assert_eq!(delta.kind, BookUpdateKind::Delta);
    assert_eq!(delta.seq, 11);
    assert_eq!(delta.bids, vec![lvl(100.0, 4.0)]);
    assert_eq!(delta.asks, vec![lvl(101.0, 3.0)]);
    // A level that left the book is a qty-0 level.
    let gone = book(0.5, 12, &[(100.0, 4.0)], &[(100.5, 1.5), (101.0, 3.0)]);
    let delta = feed.next(&gone, now, 3, "S").expect("a delta");
    assert_eq!(delta.bids, vec![lvl(99.5, 0.0)]);
    assert!(delta.asks.is_empty());
}

/// A small deterministic generator: the walk needs no crate.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self, bound: u64) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (self.0 >> 33) % bound
    }
}

/// **The subscriber's rebuild equals the pump's book, step by step**: anchor with `apply_snapshot`, every
/// delta with `apply_delta`, over a random walk that adds, moves and removes levels on both sides and
/// crosses the periodic anchor many times.
#[test]
fn a_delta_stream_rebuilds_the_pumps_book_through_apply_snapshot_and_apply_delta() {
    let mut rng = Lcg(7);
    let mut pump_book = L2Book::new(0.5);
    let mut seq = 100_u64;
    pump_book.apply_snapshot(
        seq,
        &(0..40).map(|i| lvl(1000.0 - i as f64 * 0.5, 1.0 + i as f64)).collect::<Vec<_>>(),
        &(0..40).map(|i| lvl(1000.5 + i as f64 * 0.5, 2.0 + i as f64)).collect::<Vec<_>>(),
    );
    let mut feed = BookFeed::new();
    let mut mine: Option<L2Book> = None;
    let (mut anchors, mut deltas) = (0, 0);
    let mut now = t0();
    for step in 0..600 {
        seq += 1 + rng.next(3);
        let mut bids = Vec::new();
        let mut asks = Vec::new();
        for _ in 0..(1 + rng.next(5)) {
            let off = rng.next(60) as f64 * 0.5;
            let qty = if rng.next(3) == 0 { 0.0 } else { 0.5 + rng.next(9) as f64 };
            if rng.next(2) == 0 {
                bids.push(lvl(1000.0 - off, qty));
            } else {
                asks.push(lvl(1000.5 + off, qty));
            }
        }
        assert!(pump_book.apply_delta(seq, &bids, &asks));
        // Time moves too, so the anchor timer is crossed as well as the delta counter.
        now += Duration::from_millis(37);
        let Some(update) = feed.next(&pump_book, now, step, "S") else { continue };
        match update.kind {
            BookUpdateKind::Snapshot => {
                anchors += 1;
                let mut b = L2Book::new(update.tick_size);
                b.apply_snapshot(update.seq, &update.bids, &update.asks);
                mine = Some(b);
            }
            BookUpdateKind::Delta => {
                deltas += 1;
                let b = mine.as_mut().expect("a delta never comes before an anchor");
                assert!(b.apply_delta(update.seq, &update.bids, &update.asks), "step {step}");
            }
            other => panic!("a pump book is an anchor or a delta, got {other:?}"),
        }
        let want = pump_book.top_n(PUMP_BOOK_LEVELS);
        let got = mine.as_ref().expect("anchored").top_n(PUMP_BOOK_LEVELS);
        assert_eq!(got, want, "the rebuilt book differs at step {step}");
    }
    assert!(anchors > 5 && deltas > 100, "anchors {anchors}, deltas {deltas}");
}

#[test]
fn a_fresh_anchor_follows_every_n_deltas_and_after_the_anchor_period() {
    let mut feed = BookFeed::new();
    let now = t0();
    let mut seq = 1_u64;
    let mut b = book(1.0, seq, &[(100.0, 1.0)], &[(101.0, 1.0)]);
    assert_eq!(feed.next(&b, now, 0, "S").expect("anchor").kind, BookUpdateKind::Snapshot);
    for i in 0..ANCHOR_EVERY_UPDATES {
        seq += 1;
        assert!(b.apply_delta(seq, &[lvl(100.0, 2.0 + f64::from(i))], &[]));
        let kinds = feed.next(&b, now, 0, "S").expect("update").kind;
        assert_eq!(kinds, BookUpdateKind::Delta, "delta number {i}");
    }
    seq += 1;
    assert!(b.apply_delta(seq, &[lvl(100.0, 500.0)], &[]));
    assert_eq!(
        feed.next(&b, now, 0, "S").expect("update").kind,
        BookUpdateKind::Snapshot,
        "the {ANCHOR_EVERY_UPDATES}-delta period ends in an anchor"
    );
    // And by the clock: one change after the period is an anchor even with few deltas behind it.
    seq += 1;
    assert!(b.apply_delta(seq, &[lvl(100.0, 600.0)], &[]));
    let later = now + ANCHOR_EVERY + Duration::from_millis(1);
    assert_eq!(feed.next(&b, later, 0, "S").expect("update").kind, BookUpdateKind::Snapshot);
}

#[test]
fn a_sequence_that_goes_backwards_or_a_new_grid_is_an_anchor_and_a_zero_sequence_is_not() {
    let now = t0();
    let mut feed = BookFeed::new();
    feed.next(&book(1.0, 50, &[(100.0, 1.0)], &[(101.0, 1.0)]), now, 0, "S");
    // The same content change under an OLDER sequence cannot be a delta the subscriber would accept.
    let back = book(1.0, 40, &[(100.0, 2.0)], &[(101.0, 1.0)]);
    assert_eq!(feed.next(&back, now, 0, "S").expect("update").kind, BookUpdateKind::Snapshot);
    // A different price grid is a different book.
    let regrid = book(0.5, 41, &[(100.0, 2.0)], &[(101.0, 1.0)]);
    assert_eq!(feed.next(&regrid, now, 0, "S").expect("update").kind, BookUpdateKind::Snapshot);
    // `seq == 0` is "the venue gave none": `apply_delta` always takes it, and so does the feed.
    let none = book(0.5, 0, &[(100.0, 3.0)], &[(101.0, 1.0)]);
    assert_eq!(feed.next(&none, now, 0, "S").expect("update").kind, BookUpdateKind::Delta);
}

#[test]
fn a_reset_makes_the_next_book_an_anchor() {
    let now = t0();
    let mut feed = BookFeed::new();
    feed.next(&book(1.0, 5, &[(100.0, 1.0)], &[(101.0, 1.0)]), now, 0, "S");
    feed.reset();
    let again = feed.next(&book(1.0, 5, &[(100.0, 1.0)], &[(101.0, 1.0)]), now, 0, "S");
    assert_eq!(again.expect("an anchor even for the same book").kind, BookUpdateKind::Snapshot);
}

#[test]
fn an_anchor_is_bounded_to_the_top_levels_a_side() {
    let bids: Vec<_> = (0..1500).map(|i| (100_000.0 - f64::from(i), 1.0)).collect();
    let asks: Vec<_> = (0..1500).map(|i| (100_001.0 + f64::from(i), 1.0)).collect();
    let big = book(1.0, 9, &bids, &asks);
    let update = BookFeed::new().next(&big, t0(), 0, "S").expect("anchor");
    assert_eq!((update.bids.len(), update.asks.len()), (PUMP_BOOK_LEVELS, PUMP_BOOK_LEVELS));
    assert_eq!(update.bids[0], lvl(100_000.0, 1.0), "best bid first");
    assert_eq!(update.asks[0], lvl(100_001.0, 1.0), "best ask first");
}

// ---- Translator --------------------------------------------------------------------------------------

fn quote_msg(symbol: &str, x: f64) -> Ingest {
    Ingest::Quote(Box::new(QuoteUpdate {
        venue: "binance".into(),
        symbol: symbol.into(),
        quote: quote_tick(symbol, x),
    }))
}

fn status_msg(status: FeedStatus) -> Ingest {
    Ingest::StreamStatus(Box::new(StreamStatusUpdate {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        stream: "ticks".into(),
        status,
    }))
}

fn book_msg(b: L2Book) -> Ingest {
    Ingest::Book(Box::new(vike_exec::BookUpdate {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        book: Arc::new(b),
    }))
}

/// Every frame is stamped with the symbol the SUBSCRIPTION named, whatever the pump labelled the tick
/// with, and the tick itself goes through untouched.
#[test]
fn quotes_and_trades_pass_through_under_the_subscribed_symbol() {
    let sink = RecordingSink::default();
    let mut tr = Translator::new("aster", "BTCUSDT.P");
    tr.deliver(&sink, &quote_msg("btcusdt", 60_000.0), t0());
    tr.deliver(
        &sink,
        &Ingest::Trade(Box::new(TradeUpdate {
            venue: "aster".into(),
            symbol: "btcusdt".into(),
            trade: trade_tick("btcusdt", 60_000.5),
        })),
        t0(),
    );
    let calls = sink.recorded();
    assert_eq!(calls.len(), 2, "{calls:?}");
    match &calls[0] {
        SinkCall::Quote { venue, symbol, quote } => {
            assert_eq!((venue.as_str(), symbol.as_str()), ("aster", "BTCUSDT.P"));
            assert_eq!(*quote, quote_tick("btcusdt", 60_000.0), "the tick is the pump's");
        }
        other => panic!("{other:?}"),
    }
    match &calls[1] {
        SinkCall::Trade { symbol, trade, .. } => {
            assert_eq!(symbol, "BTCUSDT.P");
            assert_eq!(trade.price, 60_000.5);
        }
        other => panic!("{other:?}"),
    }
}

/// The link words: a `Disconnected` opens a gap now, the `Live` after it echoes that stamp, both ride the
/// `"ticks"` word, and a `Disconnected` makes the next book an anchor again.
#[test]
fn the_pumps_link_words_cross_as_stream_status_on_the_ticks_word() {
    let sink = RecordingSink::default();
    let mut tr = Translator::new("binance", "BTCUSDT");
    tr.deliver(&sink, &book_msg(book(0.5, 1, &[(100.0, 1.0)], &[(100.5, 1.0)])), t0());
    tr.deliver(&sink, &status_msg(FeedStatus::Disconnected), t0());
    tr.deliver(&sink, &status_msg(FeedStatus::Live), t0());
    // The very same standing book after the outage: an anchor, not "nothing changed".
    tr.deliver(&sink, &book_msg(book(0.5, 1, &[(100.0, 1.0)], &[(100.5, 1.0)])), t0());
    let calls = sink.recorded();
    assert_eq!(calls.len(), 4, "{calls:?}");
    let SinkCall::StreamStatus { stream, status: StreamStatus::GapStart { at_ts_ms }, .. } =
        &calls[1]
    else {
        panic!("{:?}", calls[1])
    };
    assert_eq!(stream, "ticks");
    let SinkCall::StreamStatus { status: StreamStatus::Live { gap_started_ts_ms }, .. } = &calls[2]
    else {
        panic!("{:?}", calls[2])
    };
    assert_eq!(*gap_started_ts_ms, Some(*at_ts_ms), "the Live echoes the gap it closes");
    let SinkCall::BookUpdate { update, .. } = &calls[3] else { panic!("{:?}", calls[3]) };
    assert_eq!(update.kind, BookUpdateKind::Snapshot);
    // The map is the inverse of `StreamStatus::feed_status`: what the tradehub's dead-man hears.
    for (feed, wire) in [
        (FeedStatus::Disconnected, StreamStatus::GapStart { at_ts_ms: 1 }),
        (FeedStatus::Live, StreamStatus::Live { gap_started_ts_ms: None }),
        (FeedStatus::Stale, StreamStatus::Stale { newest_data_ts_ms: 1, now_ms: 2 }),
    ] {
        assert_eq!(wire.feed_status(), feed);
        assert_eq!(stream_status_of(feed, &mut None, 5).feed_status(), feed);
    }
}

#[test]
fn a_delay_label_crosses_and_a_message_a_pump_does_not_send_is_dropped() {
    let sink = RecordingSink::default();
    let mut tr = Translator::new("binance", "BTCUSDT");
    tr.deliver(
        &sink,
        &Ingest::DataDelay(Box::new(DataDelayUpdate {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            stream: "quotes".into(),
            delay: DataDelay::Delayed { lag_ms: Some(900) },
        })),
        t0(),
    );
    tr.deliver(
        &sink,
        &Ingest::Flow(Box::new(FlowUpdate {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            flow: FlowToxicity { bid: 0.1, ask: 0.2, ts: 1 },
        })),
        t0(),
    );
    tr.deliver(&sink, &Ingest::Watchdog, t0());
    let calls = sink.recorded();
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert!(matches!(&calls[0], SinkCall::DataDelay { stream, .. } if stream == "quotes"));
}

// ---- The hub over a scripted pump ----------------------------------------------------------------------

/// What a scripted spawner remembers.
#[derive(Default)]
struct Script {
    /// One entry per spawned pump: its key and the sender the test plays the pump through.
    pumps: Mutex<Vec<(String, String, f64, TickSender)>>,
    /// Stop flags, one per pump, raised by `PumpHandle::signal`.
    stops: Mutex<Vec<Arc<AtomicBool>>>,
    /// How many handles were dropped (joined, in a real pump).
    dropped: AtomicUsize,
    /// How long a handle takes to drop.
    drop_takes: Mutex<Duration>,
    /// The next spawn fails with this text.
    fail: Mutex<Option<String>>,
}

struct ScriptedStop {
    flag: Arc<AtomicBool>,
    script: Arc<Script>,
}

impl ScriptedStop {
    fn stop(&self) {
        self.flag.store(true, Ordering::SeqCst);
    }
}

impl Drop for ScriptedStop {
    fn drop(&mut self) {
        let takes = *self.script.drop_takes.lock().unwrap_or_else(PoisonError::into_inner);
        std::thread::sleep(takes);
        self.script.dropped.fetch_add(1, Ordering::SeqCst);
    }
}

fn scripted_hub(max: usize) -> (Arc<PumpHub>, Arc<Script>) {
    let script = Arc::new(Script::default());
    let for_spawner = Arc::clone(&script);
    let spawner: PumpSpawner = Box::new(move |venue, symbol, tick_size, ticks| {
        if let Some(why) = for_spawner.fail.lock().unwrap_or_else(PoisonError::into_inner).take() {
            return Err(why);
        }
        let flag = Arc::new(AtomicBool::new(false));
        for_spawner.stops.lock().unwrap_or_else(PoisonError::into_inner).push(Arc::clone(&flag));
        for_spawner.pumps.lock().unwrap_or_else(PoisonError::into_inner).push((
            venue.into(),
            symbol.into(),
            tick_size,
            ticks,
        ));
        Ok(PumpHandle::new(
            ScriptedStop { flag, script: Arc::clone(&for_spawner) },
            ScriptedStop::stop,
        ))
    });
    (PumpHub::with_max(spawner, max), script)
}

fn sink() -> Arc<RecordingSink> {
    Arc::new(RecordingSink::default())
}

fn wait_for(what: &str, mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ready() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn spawned(script: &Script) -> usize {
    script.pumps.lock().unwrap_or_else(PoisonError::into_inner).len()
}

fn sender(script: &Script, n: usize) -> TickSender {
    script.pumps.lock().unwrap_or_else(PoisonError::into_inner)[n].3.clone()
}

/// **Two sessions on one grid share ONE venue pump**; a different grid or symbol is another pump.
#[test]
fn sessions_naming_the_same_key_share_one_pump_and_others_start_their_own() {
    let (hub, script) = scripted_hub(8);
    let (a, b) = (sink(), sink());
    let _la = hub.join("binance", "BTCUSDT", 0.1, a.clone()).expect("first");
    let _lb = hub.join("binance", "BTCUSDT", 0.1, b.clone()).expect("second");
    assert_eq!(spawned(&script), 1, "the second session joined the running pump");
    assert_eq!(hub.running(), 1);

    sender(&script, 0)
        .quote(QuoteUpdate {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            quote: quote_tick("BTCUSDT", 1.0),
        })
        .expect("the pump's channel is open");
    wait_for("both sessions to hear the quote", || {
        a.recorded().len() == 2 && b.recorded().len() == 2
    });

    let _other_grid = hub.join("binance", "BTCUSDT", 0.5, sink()).expect("grid");
    let _other_symbol = hub.join("binance", "ETHUSDT", 0.1, sink()).expect("symbol");
    assert_eq!(spawned(&script), 3);
    assert_eq!(hub.running(), 3);
}

#[test]
fn the_last_session_out_stops_the_pump_and_the_first_one_out_does_not() {
    let (hub, script) = scripted_hub(8);
    let la = hub.join("binance", "BTCUSDT", 0.1, sink()).expect("first");
    let lb = hub.join("binance", "BTCUSDT", 0.1, sink()).expect("second");
    drop(la);
    assert_eq!(hub.running(), 1);
    assert!(!script.stops.lock().unwrap()[0].load(Ordering::SeqCst), "one holder is left");
    drop(lb);
    assert_eq!(hub.running(), 0, "the entry goes at once");
    assert!(script.stops.lock().unwrap()[0].load(Ordering::SeqCst), "the stop flag is up");
    // The join happens off the caller's thread, and happens.
    wait_for("the pump's handle to be dropped", || script.dropped.load(Ordering::SeqCst) == 1);
    // A subscriber after that starts a fresh pump.
    let _again = hub.join("binance", "BTCUSDT", 0.1, sink()).expect("restart");
    assert_eq!(spawned(&script), 2);
}

/// A pump thread can take seconds to join; the session reader drops a lease on an `Unsub`, so the drop
/// must return at once.
#[test]
fn dropping_the_last_lease_never_waits_for_the_pump_to_join() {
    let (hub, script) = scripted_hub(8);
    *script.drop_takes.lock().unwrap() = Duration::from_millis(600);
    let lease = hub.join("binance", "BTCUSDT", 0.1, sink()).expect("join");
    let started = Instant::now();
    drop(lease);
    assert!(
        started.elapsed() < Duration::from_millis(300),
        "the drop blocked {:?}",
        started.elapsed()
    );
    wait_for("the slow join to finish on its own thread", || {
        script.dropped.load(Ordering::SeqCst) == 1
    });
}

#[test]
fn the_pump_cap_and_a_failed_spawn_refuse_the_join_and_leave_nothing_running() {
    let (hub, script) = scripted_hub(1);
    let _held = hub.join("binance", "BTCUSDT", 0.1, sink()).expect("within the cap");
    let err = hub.join("binance", "ETHUSDT", 0.1, sink()).expect_err("past the cap");
    assert!(err.contains("already hosting 1 tick pumps"), "{err}");
    assert!(
        !vike_market_stream::market::is_transient_refusal(&err) && err.contains(CAP_MARK),
        "a hub full of RUNNING pumps is a cap, not a transient: {err}"
    );
    // The same key is not a new pump, so the cap does not apply to it.
    hub.join("binance", "BTCUSDT", 0.1, sink()).expect("a joiner of a running pump");

    let (hub, script2) = scripted_hub(4);
    *script2.fail.lock().unwrap() = Some("the venue refused".into());
    let err = hub.join("okx", "BTC-USDT-SWAP", 0.1, sink()).expect_err("spawn failed");
    assert_eq!(err, "the venue refused");
    assert_eq!(hub.running(), 0);
    assert_eq!(spawned(&script), 1);
}

/// A subscriber that joins late is told where the pump stands: the current book as an anchor, or that
/// the link is down. (A pump that has not spoken has nothing to restate.)
#[test]
fn a_late_joiner_is_told_the_current_book_and_a_standing_disconnect() {
    let (hub, script) = scripted_hub(4);
    let first = sink();
    let _l1 = hub.join("binance", "BTCUSDT", 0.5, first.clone()).expect("first");
    let ticks = sender(&script, 0);
    ticks
        .book(vike_exec::BookUpdate {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            book: Arc::new(book(0.5, 9, &[(100.0, 1.0)], &[(100.5, 2.0)])),
        })
        .expect("send");
    wait_for("the first session's anchor", || first.recorded().len() == 2);

    let late = sink();
    let _l2 = hub.join("binance", "BTCUSDT", 0.5, late.clone()).expect("late");
    let calls = late.recorded();
    assert_eq!(calls.len(), 2, "the link word, then the anchor: {calls:?}");
    assert!(
        matches!(
            &calls[0],
            SinkCall::StreamStatus { status: StreamStatus::Live { gap_started_ts_ms: None }, stream, .. }
                if stream == "ticks"
        ),
        "a joiner onto a pump that has spoken is told the link is up first: {calls:?}"
    );
    let SinkCall::BookUpdate { update, .. } = &calls[1] else { panic!("{:?}", calls[1]) };
    assert_eq!(update.kind, BookUpdateKind::Snapshot, "a joiner starts from an anchor");
    assert_eq!((update.bids.len(), update.asks.len()), (1, 1));

    ticks
        .stream_status(StreamStatusUpdate {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            stream: "ticks".into(),
            status: FeedStatus::Disconnected,
        })
        .expect("send");
    wait_for("the disconnect to reach both", || first.recorded().len() == 3);
    let later = sink();
    let _l3 = hub.join("binance", "BTCUSDT", 0.5, later.clone()).expect("later");
    let calls = later.recorded();
    assert!(
        matches!(
            &calls[..],
            [SinkCall::StreamStatus { status: StreamStatus::GapStart { .. }, .. }]
        ),
        "a joiner onto a disconnected pump hears the disconnect, not a stale book: {calls:?}"
    );
}

/// A pump that ends WITHOUT being stopped is a crash: its subscribers are told the link is down (the
/// signal that arms the dead-man), and the next subscriber starts a fresh pump.
#[test]
fn a_pump_that_dies_on_its_own_is_disclosed_as_a_disconnect_and_replaced_on_the_next_join() {
    let (hub, script) = scripted_hub(4);
    let heard = sink();
    let _lease = hub.join("binance", "BTCUSDT", 0.1, heard.clone()).expect("join");
    // The pump thread ends: every sender is dropped (the test holds the only one).
    script.pumps.lock().unwrap().clear();
    wait_for("the disconnect", || !heard.recorded().is_empty());
    assert!(matches!(
        &heard.recorded()[0],
        SinkCall::StreamStatus { status: StreamStatus::GapStart { .. }, stream, .. } if stream == "ticks"
    ));
    wait_for("the dead pump's entry to go", || hub.running() == 0);
    let _again = hub.join("binance", "BTCUSDT", 0.1, sink()).expect("a fresh pump");
    assert_eq!(spawned(&script), 1, "the replacement was spawned");
}

#[test]
fn the_real_table_has_one_arm_per_registry_pump_venue() {
    // A built pump shows nothing of how it was configured, so the pin reads the table's own text: one arm
    // per roster venue and no other.
    let src = include_str!("pump.rs");
    let body = &src[src.find("pub fn real_pump_table").expect("the fn")..];
    let body =
        &body[..body.find("// ---- The book: anchor plus deltas").expect("the next section")];
    let mut arms: Vec<&str> =
        body.lines().filter_map(|l| l.strip_prefix("        \"")?.split('"').next()).collect();
    arms.sort_unstable();
    let mut want = crate::registry::roster(Lane::Pump).to_vec();
    want.sort_unstable();
    assert_eq!(arms, want, "the arms and the registry's pump roster must be the same set");
    let table = real_pump_table();
    let (events, _rx) = vike_exec::event_channel(1);
    let ticks = TickSender { ingest: events.ingest.clone() };
    let err = table("polymarket", "X", 0.1, ticks).err().expect("polymarket has no pump");
    assert!(err.contains("no tick pump") && err.contains("binance, bybit, okx, aster"), "{err}");
}

// ---- A joiner onto an up pump, and a lost book frame --------------------------------------------------

fn quote_update(x: f64) -> QuoteUpdate {
    QuoteUpdate {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        quote: quote_tick("BTCUSDT", x),
    }
}

fn link_update(status: FeedStatus) -> StreamStatusUpdate {
    StreamStatusUpdate {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        stream: "ticks".into(),
        status,
    }
}

/// **A joiner onto a pump that is UP hears `Live{None}`; one onto a pump that has not spoken hears
/// nothing.** The joiner may be a session that replaced one whose last word was this pump's `GapStart`
/// (the datahub restarted while the venue blipped and recovered), and without the `Live` the dead-man's
/// window for it opens at the blip and never closes (review finding 2).
#[test]
fn a_joiner_onto_an_up_pump_hears_live_and_one_onto_a_silent_pump_hears_nothing() {
    let (hub, script) = scripted_hub(4);
    let first = sink();
    let _l1 = hub.join("binance", "BTCUSDT", 0.5, first.clone()).expect("first");
    // The pump has not spoken yet: there is nothing to restate.
    assert!(first.recorded().is_empty(), "{:?}", first.recorded());
    let silent = sink();
    let _l2 = hub.join("binance", "BTCUSDT", 0.5, silent.clone()).expect("second");
    assert!(silent.recorded().is_empty(), "a pump that never spoke is not announced as live");

    let ticks = sender(&script, 0);
    ticks.quote(quote_update(1.0)).expect("send");
    wait_for("the quote", || first.recorded().len() == 2 && silent.recorded().len() == 2);
    let late = sink();
    let _l3 = hub.join("binance", "BTCUSDT", 0.5, late.clone()).expect("late");
    assert!(
        matches!(
            &late.recorded()[..],
            [SinkCall::StreamStatus {
                status: StreamStatus::Live { gap_started_ts_ms: None },
                stream,
                ..
            }] if stream == "ticks"
        ),
        "an up pump with no book is announced live and nothing else: {:?}",
        late.recorded()
    );

    // An outage and its recovery: a joiner DURING it is told the gap, never live; one after the
    // recovery is told live again.
    ticks.stream_status(link_update(FeedStatus::Disconnected)).expect("send");
    wait_for("the disconnect", || first.recorded().len() == 3);
    let during = sink();
    let _l4 = hub.join("binance", "BTCUSDT", 0.5, during.clone()).expect("during");
    assert!(
        matches!(
            &during.recorded()[..],
            [SinkCall::StreamStatus { status: StreamStatus::GapStart { .. }, .. }]
        ),
        "{:?}",
        during.recorded()
    );
    ticks.stream_status(link_update(FeedStatus::Live)).expect("send");
    wait_for("the recovery", || first.recorded().len() == 4);
    let after = sink();
    let _l5 = hub.join("binance", "BTCUSDT", 0.5, after.clone()).expect("after");
    assert!(
        matches!(
            &after.recorded()[..],
            [SinkCall::StreamStatus { status: StreamStatus::Live { gap_started_ts_ms: None }, .. }]
        ),
        "the outage's book is not restated after the recovery: {:?}",
        after.recorded()
    );
}

/// **A delta is sent only when it is smaller than the anchor.** A full reshuffle (every level replaced)
/// would be a delta of twice the book's levels: it goes as the anchor it amounts to, bounded to the top
/// levels a side, so no frame the lane sends is bigger than an anchor (review finding 3b).
#[test]
fn a_delta_as_big_as_the_anchor_is_sent_as_the_anchor() {
    let mut feed = BookFeed::new();
    let now = t0();
    let level = |i: i32, base: f64| (base + f64::from(i), 1.0);
    let bids: Vec<_> = (0..1500).map(|i| level(-i, 100_000.0)).collect();
    let asks: Vec<_> = (0..1500).map(|i| level(i, 100_001.0)).collect();
    let first = feed.next(&book(1.0, 1, &bids, &asks), now, 0, "S").expect("anchor");
    assert_eq!(first.kind, BookUpdateKind::Snapshot);
    // The market moved far: the whole top of the book is a different set of prices.
    let bids: Vec<_> = (0..1500).map(|i| level(-i, 90_000.0)).collect();
    let asks: Vec<_> = (0..1500).map(|i| level(i, 90_001.0)).collect();
    let update = feed.next(&book(1.0, 2, &bids, &asks), now, 0, "S").expect("an update");
    assert_eq!(update.kind, BookUpdateKind::Snapshot, "a reshuffle is an anchor, not 4000 levels");
    assert_eq!(update.bids.len() + update.asks.len(), 2 * PUMP_BOOK_LEVELS);
    // A change that is smaller than the anchor stays a delta.
    let mut small = bids.clone();
    small[0].1 = 7.0;
    let update = feed.next(&book(1.0, 3, &small, &asks), now, 0, "S").expect("an update");
    assert_eq!(update.kind, BookUpdateKind::Delta);
    assert_eq!(update.bids.len() + update.asks.len(), 1);
}

/// A hub subscriber whose session loses a book frame: the counter the test moves stands for the wire
/// sink's `book_losses`.
fn tracked(hub: &Arc<PumpHub>, losses: &Arc<AtomicU64>) -> (Arc<RecordingSink>, PumpLease) {
    let heard = sink();
    let figure = Arc::clone(losses);
    let lease = hub
        .join_with(
            "binance",
            "BTCUSDT",
            0.5,
            PumpSink::tracked(heard.clone(), Arc::new(move || figure.load(Ordering::SeqCst))),
        )
        .expect("join");
    (heard, lease)
}

fn kinds(sink: &RecordingSink) -> Vec<BookUpdateKind> {
    sink.recorded()
        .iter()
        .filter_map(|c| match c {
            SinkCall::BookUpdate { update, .. } => Some(update.kind),
            _ => None,
        })
        .collect()
}

/// **A lost book frame is repaired with an anchor at once** (review finding 3a): the session's loss figure
/// moves, and the next pump message of ANY kind makes the standing book go out as an anchor, so a book
/// that is quiet does not stay diverged until it next changes. A loss seen when the next message is a
/// changed book makes THAT book the anchor. A feed whose figure never moves stays on deltas.
#[test]
fn a_lost_book_frame_is_repaired_with_an_anchor_at_once() {
    let (hub, script) = scripted_hub(4);
    let losses = Arc::new(AtomicU64::new(0));
    let (heard, _lease) = tracked(&hub, &losses);
    let ticks = sender(&script, 0);
    let send_book = |seq: u64, qty: f64| {
        ticks
            .book(vike_exec::BookUpdate {
                venue: "binance".into(),
                symbol: "BTCUSDT".into(),
                book: Arc::new(book(0.5, seq, &[(100.0, qty), (99.5, 2.0)], &[(100.5, 2.0)])),
            })
            .expect("send");
    };
    send_book(1, 1.0);
    send_book(2, 3.0);
    wait_for("an anchor and a delta", || kinds(&heard).len() == 2);
    assert_eq!(kinds(&heard), [BookUpdateKind::Snapshot, BookUpdateKind::Delta]);

    // The session loses a frame; the book is QUIET, and a quote arrives.
    losses.fetch_add(1, Ordering::SeqCst);
    ticks.quote(quote_update(1.0)).expect("send");
    wait_for("the repair", || kinds(&heard).len() == 3);
    let calls = heard.recorded();
    let Some(SinkCall::BookUpdate { update, .. }) =
        calls.iter().rev().find(|c| matches!(c, SinkCall::BookUpdate { .. }))
    else {
        panic!("{calls:?}")
    };
    assert_eq!(update.kind, BookUpdateKind::Snapshot, "the standing book, as an anchor");
    assert_eq!(update.bids[0], lvl(100.0, 3.0), "the CURRENT book, not the one before the loss");

    // The anchor re-based the feed: the next change is a delta again, and it does not loop.
    send_book(3, 4.0);
    wait_for("a delta", || kinds(&heard).len() == 4);
    assert_eq!(kinds(&heard)[3], BookUpdateKind::Delta);

    // A loss seen when the next message is a changed book: that book is the anchor, once.
    losses.fetch_add(1, Ordering::SeqCst);
    send_book(4, 5.0);
    wait_for("the repair by the book itself", || kinds(&heard).len() == 5);
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(kinds(&heard).len(), 5, "one anchor, not two: {:?}", kinds(&heard));
    assert_eq!(kinds(&heard)[4], BookUpdateKind::Snapshot);

    // Another session on the same pump whose figure never moved is untouched by the first one's loss.
    let steady = Arc::new(AtomicU64::new(0));
    let (other, _lease2) = tracked(&hub, &steady);
    send_book(5, 6.0);
    wait_for("both hear the change", || kinds(&other).len() == 2);
    assert_eq!(kinds(&other), [BookUpdateKind::Snapshot, BookUpdateKind::Delta]);
}

/// **A repeated disconnect restates the gap it opened and does not move it** (review finding 8): a pump
/// that crashes after the bridge's own `Disconnected` is disclosed as a second disconnect, and the `Live`
/// after the outage must echo when the outage BEGAN.
#[test]
fn a_repeated_disconnect_keeps_the_stamp_the_outage_opened_at() {
    let mut gap = None;
    assert_eq!(
        stream_status_of(FeedStatus::Disconnected, &mut gap, 100),
        StreamStatus::GapStart { at_ts_ms: 100 }
    );
    assert_eq!(
        stream_status_of(FeedStatus::Disconnected, &mut gap, 250),
        StreamStatus::GapStart { at_ts_ms: 100 },
        "the second disconnect restates the first one's stamp"
    );
    assert_eq!(
        stream_status_of(FeedStatus::Live, &mut gap, 300),
        StreamStatus::Live { gap_started_ts_ms: Some(100) }
    );
    // A new outage opens a new gap.
    assert_eq!(
        stream_status_of(FeedStatus::Disconnected, &mut gap, 400),
        StreamStatus::GapStart { at_ts_ms: 400 }
    );
}

/// **Pumps being reaped count against the cap** (review finding 6): a pump whose last holder left is a
/// socket and two threads until its reaper has joined it, so `Unsub`/`Sub` churn of distinct symbols
/// cannot run past `MAX_PUMPS` by the pumps it is stopping.
#[test]
fn a_pump_being_reaped_still_holds_its_slot_until_it_is_joined() {
    let (hub, script) = scripted_hub(1);
    *script.drop_takes.lock().unwrap() = Duration::from_millis(500);
    let lease = hub.join("binance", "BTCUSDT", 0.1, sink()).expect("within the cap");
    drop(lease);
    assert_eq!(hub.running(), 0, "the entry is gone at once");
    assert_eq!(hub.reaping(), 1, "...but the pump is still being stopped");
    let err = hub.join("binance", "ETHUSDT", 0.1, sink()).expect_err("the slot is not free yet");
    assert!(err.contains("already hosting 1 tick pumps") && err.contains("being stopped"), "{err}");
    assert!(
        vike_market_stream::market::is_transient_refusal(&err),
        "full only of pumps being stopped: they leave within seconds, a retry may succeed: {err}"
    );
    assert_eq!(spawned(&script), 1, "nothing was spawned past the cap");
    // The scripted pump's channel closes when the test lets go of its sender (a real pump thread ends it).
    script.pumps.lock().unwrap().clear();
    wait_for("the reaper to finish", || hub.reaping() == 0);
    let _again = hub.join("binance", "ETHUSDT", 0.1, sink()).expect("the slot is free again");
    assert_eq!(spawned(&script), 1, "a new pump after the clear");
}

/// **R31: the reconcile health gate's evidence.** A venue is degraded while one of its pumps last
/// disclosed a disconnect and has not recovered; another venue's pump, and a venue with no pump, read
/// healthy (the fail-soft direction), and the venue clears the moment its pump says `Live`.
#[test]
fn a_venue_with_a_pump_mid_gap_is_degraded_and_clears_on_live() {
    use crate::health::VenueHealth;

    let (hub, script) = scripted_hub(4);
    let first = sink();
    let _l1 = hub.join("binance", "BTCUSDT", 0.5, first.clone()).expect("join");
    let ticks = sender(&script, 0);
    let status = |s| StreamStatusUpdate {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        stream: "ticks".into(),
        status: s,
    };
    assert_eq!(hub.degraded("binance"), None, "a pump that has said nothing is healthy");
    ticks.stream_status(status(FeedStatus::Disconnected)).expect("send");
    wait_for("the disconnect to reach the session", || !first.recorded().is_empty());
    let reason = hub.degraded("binance").expect("mid-gap is degraded");
    assert!(
        reason.starts_with(crate::health::DEGRADED_PREFIX) && reason.contains("binance"),
        "{reason}"
    );
    assert_eq!(hub.degraded("bybit"), None, "per venue");
    ticks.stream_status(status(FeedStatus::Live)).expect("send");
    wait_for("the recovery", || hub.degraded("binance").is_none());
}

// ---- W12b: the periodic anchor of a quiet book, and a fresh pump's first word ------------------------

/// **The periodic anchor is due on a clock, whether or not the book moved.** `next` anchors on the timer
/// only when the top levels changed; `anchor_due` is the question a quiet book needs asked.
#[test]
fn the_periodic_anchor_is_due_on_the_clock_even_for_a_book_that_did_not_move() {
    let mut feed = BookFeed::new();
    let start = t0();
    assert!(!feed.anchor_due(start + ANCHOR_EVERY * 2), "nothing sent yet: nothing to repeat");
    let b = book(0.5, 1, &[(100.0, 1.0)], &[(100.5, 1.0)]);
    feed.next(&b, start, 0, "S").expect("anchor");
    assert!(!feed.anchor_due(start + ANCHOR_EVERY - Duration::from_millis(1)));
    assert!(feed.anchor_due(start + ANCHOR_EVERY), "due exactly at the period");
    assert!(
        feed.next(&b, start + ANCHOR_EVERY, 1, "S").is_none(),
        "`next` alone still sends nothing"
    );
    // A reset (a link loss) forgets what was sent: the next book is an anchor anyway, so nothing is due.
    feed.reset();
    assert!(!feed.anchor_due(start + ANCHOR_EVERY * 3));
}

/// **A QUIET book is re-anchored by any pump message once the period is due.** A subscriber that lost a
/// delta at the datahub's hop (the sender sees no loss there) holds a wrong book, and a book that never moves
/// would never correct it: the quote that arrives after the period brings the standing book as an anchor.
#[test]
fn a_quiet_book_is_re_anchored_by_the_next_pump_message_after_the_period() {
    let (hub, script) = scripted_hub(4);
    let heard = sink();
    let _lease = hub.join("binance", "BTCUSDT", 0.5, heard.clone()).expect("join");
    let ticks = sender(&script, 0);
    let send_book = || {
        ticks
            .book(vike_exec::BookUpdate {
                venue: "binance".into(),
                symbol: "BTCUSDT".into(),
                book: Arc::new(book(0.5, 1, &[(100.0, 1.0)], &[(100.5, 2.0)])),
            })
            .expect("send");
    };
    send_book();
    wait_for("the first anchor", || kinds(&heard).len() == 1);
    // Inside the period nothing is repeated, however much traffic there is.
    ticks.quote(quote_update(1.0)).expect("send");
    send_book();
    wait_for("the quote", || heard.quotes().len() == 1);
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(kinds(&heard).len(), 1, "no anchor inside the period: {:?}", kinds(&heard));
    // Past the period, the next message of ANY kind brings the standing book, once.
    std::thread::sleep(ANCHOR_EVERY);
    ticks.quote(quote_update(2.0)).expect("send");
    wait_for("the periodic anchor", || kinds(&heard).len() == 2);
    assert_eq!(kinds(&heard), [BookUpdateKind::Snapshot, BookUpdateKind::Snapshot]);
    ticks.quote(quote_update(3.0)).expect("send");
    wait_for("the third quote", || heard.quotes().len() == 3);
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(kinds(&heard).len(), 2, "one anchor per period, not one per message");
}

/// **A fresh pump says it is up with its first data.** A pump that starts after a datahub restart has no
/// `Disconnected`/`Live` of its own, and a subscriber whose old session ended on a `GapStart` of this pump
/// would keep its dead-man window open for a link the new pump has up. The first data message is preceded
/// by `Live{None}` on the ticks word, once; a pump that recovers from an outage says `Live` itself and is
/// not announced twice.
#[test]
fn a_fresh_pump_announces_live_once_with_its_first_data() {
    let (hub, script) = scripted_hub(4);
    let (first, second) = (sink(), sink());
    let _l1 = hub.join("binance", "BTCUSDT", 0.5, first.clone()).expect("first");
    let _l2 = hub.join("binance", "BTCUSDT", 0.5, second.clone()).expect("second");
    assert!(first.recorded().is_empty() && second.recorded().is_empty(), "nothing before data");
    let ticks = sender(&script, 0);
    ticks.quote(quote_update(1.0)).expect("send");
    wait_for("both hear the first quote", || {
        first.recorded().len() == 2 && second.recorded().len() == 2
    });
    for heard in [&first, &second] {
        assert!(
            matches!(
                &heard.recorded()[..],
                [
                    SinkCall::StreamStatus { status: StreamStatus::Live { gap_started_ts_ms: None }, stream, .. },
                    SinkCall::Quote { .. },
                ] if stream == "ticks"
            ),
            "{:?}",
            heard.recorded()
        );
    }
    ticks.quote(quote_update(2.0)).expect("send");
    wait_for("the second quote", || first.quotes().len() == 2);
    assert_eq!(first.recorded().len(), 3, "announced once: {:?}", first.recorded());

    // An outage and its own `Live`: the recovery is the pump's word, not announced a second time.
    ticks.stream_status(link_update(FeedStatus::Disconnected)).expect("send");
    ticks.stream_status(link_update(FeedStatus::Live)).expect("send");
    ticks.quote(quote_update(3.0)).expect("send");
    wait_for("the quote after the recovery", || first.quotes().len() == 3);
    let lives = first
        .recorded()
        .iter()
        .filter(|c| matches!(c, SinkCall::StreamStatus { status: StreamStatus::Live { .. }, .. }))
        .count();
    assert_eq!(
        lives,
        2,
        "the first announcement and the recovery's own Live: {:?}",
        first.recorded()
    );
}

/// **A quiet book is re-anchored ON THE CLOCK, with no pump message at all** (W12c, review 17 finding 4): the
/// anchor timer sends the standing book again once the period is old, so a subscriber that lost a delta at the
/// datahub's hop, and a pump that went silent without a `Disconnected`, cannot leave it stale for as long as the
/// symbol stays quiet. After the first anchor the pump says NOTHING; a second anchor still arrives, and then a
/// third, one period apart. (The test before it pins that a message past the period also brings one, and that
/// there is not one per message.)
#[test]
fn a_quiet_book_is_re_anchored_by_the_timer_with_no_pump_message() {
    let (hub, script) = scripted_hub(4);
    let heard = sink();
    let _lease = hub.join("binance", "BTCUSDT", 0.5, heard.clone()).expect("join");
    let ticks = sender(&script, 0);
    ticks
        .book(vike_exec::BookUpdate {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            book: Arc::new(book(0.5, 1, &[(100.0, 1.0)], &[(100.5, 2.0)])),
        })
        .expect("send");
    wait_for("the first anchor", || kinds(&heard).len() == 1);
    // Silence from here: the timer alone brings the next two.
    wait_for("the timer's anchor", || kinds(&heard).len() == 2);
    wait_for("the timer's second anchor", || kinds(&heard).len() == 3);
    assert!(
        kinds(&heard).iter().all(|k| *k == BookUpdateKind::Snapshot),
        "every one is an ANCHOR (the standing book whole): {:?}",
        kinds(&heard)
    );
    assert_eq!(heard.quotes().len(), 0, "the pump said nothing in between");
}

/// The timer sends nothing while the pump's link is down (a stale book is never handed out) and leaves with the
/// pump: a pump that disconnected is silent until it recovers, and its anchors resume with the recovery.
#[test]
fn the_anchor_timer_is_silent_while_the_link_is_down() {
    let (hub, script) = scripted_hub(4);
    let heard = sink();
    let _lease = hub.join("binance", "BTCUSDT", 0.5, heard.clone()).expect("join");
    let ticks = sender(&script, 0);
    ticks
        .book(vike_exec::BookUpdate {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            book: Arc::new(book(0.5, 1, &[(100.0, 1.0)], &[(100.5, 2.0)])),
        })
        .expect("send");
    wait_for("the first anchor", || kinds(&heard).len() == 1);
    ticks.stream_status(link_update(FeedStatus::Disconnected)).expect("send");
    std::thread::sleep(ANCHOR_EVERY + ANCHOR_EVERY / 2);
    assert_eq!(kinds(&heard).len(), 1, "no anchor of a book the outage ended: {:?}", kinds(&heard));
}

// ---- the timer does not launder a standing book that the pump itself has stopped vouching for (W12d) -------------

/// The only pump of `hub`, as the timer sees it.
fn the_pump(hub: &PumpHub) -> Arc<Shared> {
    lock(&hub.live).values().next().cloned().expect("a running pump")
}

fn send_whole(ticks: &TickSender, seq: u64) {
    ticks
        .book(vike_exec::BookUpdate {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            book: Arc::new(book(0.5, seq, &[(100.0, 1.0)], &[(100.5, 2.0)])),
        })
        .expect("send");
}

/// **The anchor timer stops restating a standing book once it is older than [`STANDING_BOOK_MAX_AGE`].** A pump
/// that went silent without a `Disconnected` (a half-open socket) used to look like a quiet, healthy book: every
/// ~2 s the old book went out again as a fresh anchor, which the tradehub's strategy prices off and the opt-in
/// silence dead-man counts as liveness. The timer's step is driven with explicit clock readings, so nothing
/// here waits for a period.
#[test]
fn the_timer_stops_re_anchoring_a_book_older_than_the_staleness_bound() {
    let (hub, script) = scripted_hub(4);
    let heard = sink();
    let _lease = hub.join("binance", "BTCUSDT", 0.5, heard.clone()).expect("join");
    send_whole(&sender(&script, 0), 1);
    wait_for("the first anchor", || kinds(&heard).len() == 1);
    let pump = the_pump(&hub);
    let base = Instant::now();

    // Young: the quiet book is re-anchored, as before.
    pump.anchor_quiet(base + ANCHOR_EVERY + Duration::from_millis(10));
    assert_eq!(kinds(&heard).len(), 2, "a quiet book inside the bound is anchored by the timer");

    // Past the bound, with the pump silent: nothing on the clock alone, however many periods pass.
    let late = base + STANDING_BOOK_MAX_AGE + ANCHOR_EVERY * 3;
    pump.anchor_quiet(late);
    pump.anchor_quiet(late + ANCHOR_EVERY * 2);
    assert_eq!(
        kinds(&heard).len(),
        2,
        "a book the pump has not refreshed is not restated as fresh"
    );

    // The pump speaks again (a new book): the standing book is young, and the timer resumes.
    send_whole(&sender(&script, 0), 2);
    wait_for("the pump's new book", || {
        pump.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .book_at
            .is_some_and(|at| at > base)
    });
    let after = kinds(&heard).len();
    pump.anchor_quiet(Instant::now() + ANCHOR_EVERY * 2 + Duration::from_millis(10));
    assert_eq!(kinds(&heard).len(), after + 1, "the timer is back with a fresh book");
}

/// **A pump that says `Stale` is never anchored from**, however young its standing book: the link is not down,
/// but the pump itself calls its data old. The first message of any kind after it ends the state.
#[test]
fn the_timer_is_silent_while_the_pump_says_stale() {
    let (hub, script) = scripted_hub(4);
    let heard = sink();
    let _lease = hub.join("binance", "BTCUSDT", 0.5, heard.clone()).expect("join");
    let ticks = sender(&script, 0);
    send_whole(&ticks, 1);
    wait_for("the first anchor", || kinds(&heard).len() == 1);
    let pump = the_pump(&hub);

    // The real timer thread is not what is under test (and would anchor the backdated member itself): stop it, and
    // give an iteration already in flight one tick to end.
    pump.stopping.store(true, Ordering::SeqCst);
    std::thread::sleep(ANCHOR_TICK * 2);
    // The member's periodic anchor is DUE when the `Stale` arrives (its last anchor is backdated past the period):
    // the message path must not restate the old book as a fresh anchor either.
    for member in &mut lock(&pump.inner).members {
        member.translator.book.anchored = Instant::now().checked_sub(ANCHOR_EVERY * 2);
    }
    ticks.stream_status(link_update(FeedStatus::Stale)).expect("send");
    wait_for("the pump's Stale", || {
        pump.inner.lock().unwrap_or_else(PoisonError::into_inner).stale
    });
    assert_eq!(kinds(&heard).len(), 1, "the Stale message itself restates nothing");
    let due = Instant::now() + ANCHOR_EVERY + Duration::from_millis(10);
    pump.anchor_quiet(due);
    assert_eq!(
        kinds(&heard).len(),
        1,
        "no anchor of a book the pump calls stale: {:?}",
        kinds(&heard)
    );

    ticks.stream_status(link_update(FeedStatus::Live)).expect("send");
    wait_for("the pump's Live", || {
        !pump.inner.lock().unwrap_or_else(PoisonError::into_inner).stale
    });
    // The Live message itself may repair a member whose period is due; the timer then resumes on its own clock.
    let after_live = kinds(&heard).len();
    pump.anchor_quiet(Instant::now() + ANCHOR_EVERY * 2 + Duration::from_millis(10));
    assert_eq!(kinds(&heard).len(), after_live + 1, "the timer resumes once the pump speaks again");
}

/// **A member whose session lost a book frame is repaired even from an old book**: that is the timer's purpose
/// (a quiet healthy symbol with a hole at the next hop), and without it the tradehub's fold would wait out its
/// deadline for an anchor that the staleness bound refused to send. A member that lost nothing gets none.
#[test]
fn a_lost_frame_is_still_repaired_from_a_book_past_the_bound() {
    let (hub, script) = scripted_hub(4);
    let losses = Arc::new(AtomicU64::new(0));
    let (heard, _lease) = tracked(&hub, &losses);
    send_whole(&sender(&script, 0), 1);
    wait_for("the first anchor", || kinds(&heard).len() == 1);
    let pump = the_pump(&hub);
    let late = Instant::now() + STANDING_BOOK_MAX_AGE + ANCHOR_EVERY * 3;

    pump.anchor_quiet(late);
    assert_eq!(kinds(&heard).len(), 1, "an old book and no loss: nothing");
    losses.fetch_add(1, Ordering::SeqCst);
    pump.anchor_quiet(late + Duration::from_millis(250));
    assert_eq!(kinds(&heard).len(), 2, "a lost frame is repaired with the standing book");
    assert_eq!(kinds(&heard)[1], BookUpdateKind::Snapshot);
    pump.anchor_quiet(late + Duration::from_millis(500));
    assert_eq!(kinds(&heard).len(), 2, "one repair per loss, not one per tick");
}
