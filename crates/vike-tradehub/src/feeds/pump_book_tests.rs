//! The pump book fold's contracts, and the PARITY test that holds it equal to the book the old in-process
//! pump pushed (`feeds/pump_book.rs`'s module doc says why it exists).

use std::sync::Arc;
use std::time::{Duration, Instant};

use vike_data::{LiveDataSink, RecordingSink, SinkCall};
use vike_datahub_client::strat::LinkSink;
use vike_gatewayhub::data::pump::{ANCHOR_EVERY, ANCHOR_EVERY_UPDATES, BookFeed, PUMP_BOOK_LEVELS};
use vike_market_stream::wire::LINK_STREAM_WORD;
use vike_marketdata::{BookLevel, BookUpdate, BookUpdateKind, L2Book};
use vike_model::StreamStatus;
use vike_venues::link_deadman::STRAT_LINK_STREAM;

use super::PumpBookFold;

const VENUE: &str = "binance";
const SYMBOL: &str = "BTCUSDT";

fn fold() -> (PumpBookFold, Arc<RecordingSink>) {
    let inner = Arc::new(RecordingSink::default());
    (PumpBookFold::new(Arc::clone(&inner) as Arc<dyn LiveDataSink>), inner)
}

fn lv(price: f64, qty: f64) -> BookLevel {
    BookLevel::new(price, qty)
}

fn update(
    kind: BookUpdateKind,
    seq: u64,
    tick_size: f64,
    bids: &[BookLevel],
    asks: &[BookLevel],
) -> BookUpdate {
    BookUpdate {
        ts: 1,
        local_ts: 1,
        seq,
        kind,
        tick_size,
        bids: bids.to_vec(),
        asks: asks.to_vec(),
        symbol: SYMBOL.to_string(),
    }
}

fn anchor(seq: u64) -> BookUpdate {
    update(
        BookUpdateKind::Snapshot,
        seq,
        0.5,
        &[lv(100.0, 1.0), lv(99.5, 2.0)],
        &[lv(101.0, 1.0), lv(101.5, 2.0)],
    )
}

fn forwarded(inner: &RecordingSink) -> Vec<L2Book> {
    inner.books().into_iter().map(|(_, _, b)| b).collect()
}

/// **An anchor seeds the book and each delta is folded into it**: the engine is handed the WHOLE
/// folded book each time (what the in-process pump always pushed), a removed level (`qty == 0`) gone and
/// a new one in.
#[test]
fn an_anchor_then_deltas_rebuild_the_whole_book() {
    let (fold, inner) = fold();
    fold.book_update(VENUE, SYMBOL, anchor(1));
    fold.book_update(
        VENUE,
        SYMBOL,
        update(BookUpdateKind::Delta, 2, 0.5, &[lv(100.0, 0.0), lv(98.0, 5.0)], &[lv(101.0, 3.0)]),
    );
    let books = forwarded(&inner);
    assert_eq!(books.len(), 2, "one whole book per update");
    let last = &books[1];
    assert_eq!(last.best_bid(), Some(lv(99.5, 2.0)), "the removed best bid is gone");
    assert_eq!(last.best_ask(), Some(lv(101.0, 3.0)), "the changed ask is replaced");
    assert_eq!((last.bid_levels(), last.ask_levels()), (2, 2));
    assert_eq!(last.last_seq, 2);
    assert!((last.tick_size - 0.5).abs() < f64::EPSILON);
    // The first forwarded book is the anchor, untouched by the delta (a book is a value).
    assert_eq!(books[0].best_bid(), Some(lv(100.0, 1.0)));
}

/// A delta with no anchor held is the tail of a book this process no longer holds: dropped, and the
/// next anchor seeds it.
#[test]
fn a_delta_with_no_anchor_is_dropped_and_the_next_anchor_seeds() {
    let (fold, inner) = fold();
    fold.book_update(VENUE, SYMBOL, update(BookUpdateKind::Delta, 7, 0.5, &[lv(100.0, 4.0)], &[]));
    assert!(forwarded(&inner).is_empty());
    fold.book_update(VENUE, SYMBOL, anchor(8));
    assert_eq!(forwarded(&inner).len(), 1);
}

/// A duplicate or replayed delta (its sequence is not newer) changes nothing and loses nothing: the
/// book is kept and the next newer delta applies.
#[test]
fn a_stale_delta_is_ignored_and_the_book_kept() {
    let (fold, inner) = fold();
    fold.book_update(VENUE, SYMBOL, anchor(5));
    fold.book_update(VENUE, SYMBOL, update(BookUpdateKind::Delta, 5, 0.5, &[lv(100.0, 9.0)], &[]));
    assert_eq!(forwarded(&inner).len(), 1, "the duplicate said nothing");
    fold.book_update(VENUE, SYMBOL, update(BookUpdateKind::Delta, 6, 0.5, &[lv(100.0, 9.0)], &[]));
    let books = forwarded(&inner);
    assert_eq!(books.len(), 2);
    assert_eq!(books[1].best_bid(), Some(lv(100.0, 9.0)));
}

/// A delta on another price grid cannot be applied: the book is DROPPED (a wait for the next anchor), not
/// guessed at, and not re-served from the old grid.
#[test]
fn a_delta_on_another_grid_drops_the_book_until_an_anchor() {
    let (fold, inner) = fold();
    fold.book_update(VENUE, SYMBOL, anchor(1));
    fold.book_update(VENUE, SYMBOL, update(BookUpdateKind::Delta, 2, 0.25, &[lv(100.0, 4.0)], &[]));
    fold.book_update(VENUE, SYMBOL, update(BookUpdateKind::Delta, 3, 0.5, &[lv(100.0, 4.0)], &[]));
    assert_eq!(forwarded(&inner).len(), 1, "nothing was served after the grid changed");
    fold.book_update(VENUE, SYMBOL, anchor(4));
    assert_eq!(forwarded(&inner).len(), 2, "the anchor re-seeds");
}

/// A link `GapStart` (the pump's own disconnect word, or this process's strat-link word) drops the held
/// book and is itself forwarded; a bar lane's `GapStart` does not touch the book.
#[test]
fn a_link_gap_drops_the_book_and_a_bar_gap_does_not() {
    let gap = StreamStatus::GapStart { at_ts_ms: 5 };
    for word in [LINK_STREAM_WORD, STRAT_LINK_STREAM] {
        let (fold, inner) = fold();
        fold.book_update(VENUE, SYMBOL, anchor(1));
        fold.stream_status(VENUE, SYMBOL, word, gap);
        fold.book_update(
            VENUE,
            SYMBOL,
            update(BookUpdateKind::Delta, 2, 0.5, &[lv(100.0, 4.0)], &[]),
        );
        assert_eq!(forwarded(&inner).len(), 1, "{word}: the delta after a gap waits for an anchor");
        assert_eq!(inner.calls().iter().filter(|c| c.starts_with("stream_status")).count(), 1);
    }
    let (fold, inner) = fold();
    fold.book_update(VENUE, SYMBOL, anchor(1));
    fold.stream_status(VENUE, SYMBOL, "1m", gap);
    fold.book_update(VENUE, SYMBOL, update(BookUpdateKind::Delta, 2, 0.5, &[lv(100.0, 4.0)], &[]));
    assert_eq!(forwarded(&inner).len(), 2, "a bar lane's gap is not the book's");
}

/// What the engine was told, in order: `(stream, status)` of every `stream_status` call, and `"book"` markers
/// where a whole book was handed over (so a test reads the ORDER of the two).
fn told(inner: &RecordingSink) -> Vec<String> {
    inner
        .recorded()
        .into_iter()
        .filter_map(|c| match c {
            SinkCall::StreamStatus { stream, status, .. } => Some(match status {
                StreamStatus::GapStart { .. } => format!("{stream}:gap"),
                StreamStatus::Live { gap_started_ts_ms: Some(at) } => format!("{stream}:live@{at}"),
                StreamStatus::Live { gap_started_ts_ms: None } => format!("{stream}:live"),
                StreamStatus::Stale { newest_data_ts_ms, .. } => {
                    format!("{stream}:stale@{newest_data_ts_ms}")
                }
            }),
            SinkCall::Book { .. } => Some("BOOK".to_string()),
            _ => None,
        })
        .collect()
}

fn hole(sink: &dyn LiveDataSink, at: i64) {
    // What the tube's engine says for a lost frame: the pair, back to back (`disclose_hole`).
    sink.stream_status(VENUE, SYMBOL, "book", StreamStatus::GapStart { at_ts_ms: at });
    sink.stream_status(VENUE, SYMBOL, "book", StreamStatus::Live { gap_started_ts_ms: Some(at) });
}

/// **`Live` for the book means a consistent book.** A lost frame on the book lane is told as the pair
/// `GapStart`/`Live`; the engine must NOT be told the lane is healthy while the book it holds is missing a
/// delta. Until the next anchor lands the lane reads `Stale` (the dead-man ignores a `Stale`, a strategy
/// hears a non-`Live` status), no whole book is handed over, and the `Live` follows the anchor's book.
#[test]
fn a_book_hole_reads_stale_until_the_anchor_lands() {
    let (fold, inner) = fold();
    fold.book_update(VENUE, SYMBOL, anchor(1));
    hole(&fold, 42);
    assert_eq!(
        told(&inner),
        ["BOOK", "book:stale@42"],
        "the hole is Stale, and the paired Live is withheld: nothing says the lane is healthy"
    );
    // Deltas after the hole cannot be applied to what the engine holds: no book, no status.
    fold.book_update(VENUE, SYMBOL, update(BookUpdateKind::Delta, 3, 0.5, &[lv(100.0, 9.0)], &[]));
    assert_eq!(told(&inner), ["BOOK", "book:stale@42"], "no whole book after the hole");
    // The anchor re-seeds; the book comes first, then the Live that echoes the gap's stamp.
    fold.book_update(VENUE, SYMBOL, anchor(10));
    assert_eq!(told(&inner), ["BOOK", "book:stale@42", "BOOK", "book:live@42"]);
    // And a later delta flows again.
    fold.book_update(VENUE, SYMBOL, update(BookUpdateKind::Delta, 11, 0.5, &[lv(100.0, 3.0)], &[]));
    assert_eq!(forwarded(&inner).len(), 3);
}

/// A second hole while the first is still waiting says nothing new and keeps the first stamp; one anchor
/// ends both.
#[test]
fn a_second_hole_before_the_anchor_keeps_the_first_stamp() {
    let (fold, inner) = fold();
    fold.book_update(VENUE, SYMBOL, anchor(1));
    hole(&fold, 10);
    hole(&fold, 20);
    assert_eq!(told(&inner), ["BOOK", "book:stale@10"]);
    fold.book_update(VENUE, SYMBOL, anchor(5));
    assert_eq!(told(&inner), ["BOOK", "book:stale@10", "BOOK", "book:live@10"]);
}

/// A `Live` on the book word with no hole open is somebody else's and passes (a venue's own status on a
/// lane it forwards), and a hole on another word never starts a wait.
#[test]
fn only_the_book_lanes_own_hole_starts_a_wait() {
    let (fold, inner) = fold();
    fold.book_update(VENUE, SYMBOL, anchor(1));
    fold.stream_status(VENUE, SYMBOL, "book", StreamStatus::Live { gap_started_ts_ms: None });
    fold.stream_status(VENUE, SYMBOL, "quotes", StreamStatus::GapStart { at_ts_ms: 3 });
    fold.stream_status(VENUE, SYMBOL, "quotes", StreamStatus::Live { gap_started_ts_ms: Some(3) });
    assert_eq!(told(&inner), ["BOOK", "book:live", "quotes:gap", "quotes:live@3"]);
    fold.book_update(VENUE, SYMBOL, update(BookUpdateKind::Delta, 2, 0.5, &[lv(100.0, 4.0)], &[]));
    assert_eq!(forwarded(&inner).len(), 2, "the book was never dropped");
}

/// A delta on another grid drops the book (above) and the engine still holds the old one, so that is a
/// stale book too: told, and ended by the anchor.
#[test]
fn a_grid_change_reads_stale_until_the_anchor() {
    let (fold, inner) = fold();
    fold.book_update(VENUE, SYMBOL, anchor(1));
    fold.book_update(VENUE, SYMBOL, update(BookUpdateKind::Delta, 2, 0.25, &[lv(100.0, 4.0)], &[]));
    let said = told(&inner);
    assert_eq!(said.len(), 2, "{said:?}");
    assert!(said[1].starts_with("book:stale@"), "{said:?}");
    fold.book_update(VENUE, SYMBOL, anchor(3));
    let said = told(&inner);
    assert_eq!(&said[2..], ["BOOK".to_string(), said[1].replace("stale", "live")]);
}

/// **Through the REAL `LinkSink`** (the datahub-client's receiving sink, which withholds a holed symbol's
/// deltas until a snapshot) and the REAL sender-side encoder: a delta the hop lost, the hole disclosed, the
/// encoder's next outputs. The engine's status is never `Live` between the hole and the anchor that repairs
/// it, and the book it ends with equals the sender's.
#[test]
fn a_hole_through_the_link_sink_is_stale_until_the_senders_next_anchor() {
    let inner = Arc::new(RecordingSink::default());
    let fold = Arc::new(PumpBookFold::new(Arc::clone(&inner) as Arc<dyn LiveDataSink>));
    let link = LinkSink::new(Arc::clone(&fold) as Arc<dyn LiveDataSink>);
    let mut encoder = BookFeed::new();
    let mut truth = L2Book::new(0.5);
    truth.apply_snapshot(1, &[lv(100.0, 1.0), lv(99.5, 2.0)], &[lv(101.0, 1.0), lv(101.5, 2.0)]);
    let start = Instant::now();
    let mut send = |truth: &L2Book, at: Instant, lose: bool, link: &LinkSink| {
        let u = encoder.next(truth, at, 0, SYMBOL);
        if let Some(u) = u
            && !lose
        {
            link.book_update(VENUE, SYMBOL, u);
        }
    };
    send(&truth, start, false, &link); // the anchor
    // The sender folds a change and the hop LOSES the delta; the tube discloses the hole.
    assert!(truth.apply_delta(2, &[lv(100.0, 0.0)], &[]));
    send(&truth, start + Duration::from_millis(10), true, &link);
    hole(&link, 50);
    // The sender's next delta reaches the link sink, which withholds it (the book is holed).
    assert!(truth.apply_delta(3, &[lv(98.0, 4.0)], &[]));
    send(&truth, start + Duration::from_millis(20), false, &link);
    let said = told(&inner);
    assert_eq!(said, ["BOOK", "book:stale@50"], "never Live, no book, while the hole is open");
    // The sender's periodic anchor (the timer is due and the book changed) repairs it.
    assert!(truth.apply_delta(4, &[lv(97.5, 1.0)], &[]));
    send(&truth, start + ANCHOR_EVERY + Duration::from_millis(30), false, &link);
    let said = told(&inner);
    assert_eq!(said, ["BOOK", "book:stale@50", "BOOK", "book:live@50"]);
    let last = forwarded(&inner).pop().expect("a book");
    assert_eq!(whole(&last), whole(&truth), "the repaired book is the sender's");
}

/// Every other verb passes straight through, with the venue and symbol it was given.
#[test]
fn every_other_verb_passes_through() {
    let (fold, inner) = fold();
    fold.quote(
        VENUE,
        SYMBOL,
        vike_marketdata::QuoteTick {
            ts: 1,
            local_ts: 1,
            bid: 1.0,
            ask: 2.0,
            bid_size: 1.0,
            ask_size: 1.0,
            symbol: String::new(),
        },
    );
    fold.trade(
        VENUE,
        SYMBOL,
        vike_marketdata::TradeTick {
            ts: 1,
            local_ts: 1,
            price: 1.5,
            size: 1.0,
            is_buyer_maker: false,
            symbol: String::new(),
        },
    );
    fold.stream_status(
        VENUE,
        SYMBOL,
        LINK_STREAM_WORD,
        StreamStatus::Live { gap_started_ts_ms: None },
    );
    assert_eq!(inner.quotes().len(), 1);
    assert_eq!(inner.trades().len(), 1);
    assert_eq!(inner.calls().iter().filter(|c| c.starts_with("stream_status")).count(), 1);
}

// ---- parity -----------------------------------------------------------------------------------------

/// A sink that keeps only the LAST whole book and how many it was handed (the parity run is 2,000 steps;
/// a recording sink would clone every book it ever saw).
#[derive(Default)]
struct LastBook {
    held: std::sync::Mutex<(usize, Option<Arc<L2Book>>)>,
}

impl LastBook {
    fn served(&self) -> usize {
        self.held.lock().unwrap().0
    }
    fn last(&self) -> Option<Arc<L2Book>> {
        self.held.lock().unwrap().1.clone()
    }
}

impl LiveDataSink for LastBook {
    fn seed_bars(&self, _v: &str, _s: &str, _i: &str, _b: Vec<vike_marketdata::Bar>) {}
    fn close_bar(&self, _v: &str, _s: &str, _i: &str, _b: vike_marketdata::Bar) {}
    fn forming_bar(&self, _v: &str, _s: &str, _i: &str, _b: vike_marketdata::Bar) {}
    fn mark_tick(&self, _v: &str, _s: &str, _p: f64, _t: i64) {}
    fn quote(&self, _v: &str, _s: &str, _q: vike_marketdata::QuoteTick) {}
    fn trade(&self, _v: &str, _s: &str, _t: vike_marketdata::TradeTick) {}
    fn book(&self, _v: &str, _s: &str, book: Arc<L2Book>) {
        let mut held = self.held.lock().unwrap();
        held.0 += 1;
        held.1 = Some(book);
    }
}

/// A deterministic generator (a linear congruential step), so the test needs no RNG dependency and a
/// failure replays.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 =
            self.0.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

fn whole(book: &L2Book) -> (Vec<BookLevel>, Vec<BookLevel>) {
    book.top_n(usize::MAX)
}

/// **PARITY with the old direct feed.** The in-process pump pushed `Ingest::Book(Arc<L2Book>)`, the
/// whole folded book, for every change. Here the SAME sequence of books goes through the REAL encoder the
/// gatewayhub runs (`BookFeed`: anchors every 64 updates or 2 s, deltas in between, a reset on a link
/// loss) and this fold, and every book the engine would be handed must equal the book the pump held at
/// that step, level for level, with the same sequence and grid. A step the encoder says nothing about
/// (nothing visible changed) must leave the engine's book equal to the pump's.
#[test]
fn the_folded_book_equals_the_book_the_old_direct_feed_pushed() {
    const { assert!(PUMP_BOOK_LEVELS >= 100) }; // the test books fit inside the encoder's depth
    let inner = Arc::new(LastBook::default());
    let fold = PumpBookFold::new(Arc::clone(&inner) as Arc<dyn LiveDataSink>);
    let mut encoder = BookFeed::new();
    let mut rng = Lcg(0x5eed);
    let tick = 0.5;
    let mut truth = L2Book::new(tick);
    let mut seq = 1_u64;
    let (bids, asks): (Vec<_>, Vec<_>) = (
        (0..60).map(|i| lv(1000.0 - f64::from(i) * tick, 1.0 + f64::from(i % 7))).collect(),
        (0..60).map(|i| lv(1000.5 + f64::from(i) * tick, 1.0 + f64::from(i % 5))).collect(),
    );
    truth.apply_snapshot(seq, &bids, &asks);

    let start = Instant::now();
    let mut now = start;
    let mut anchors = 0_u32;
    let mut deltas = 0_u32;
    let mut served = 0_usize;
    let mut checked = 0_u32;
    for step in 0..2_000_u32 {
        // One change to the pump's own book: a level upserted or removed on a side.
        if step > 0 {
            let side_bid = rng.below(2) == 0;
            let offset = f64::from(u32::try_from(rng.below(80)).unwrap()) * tick;
            let price = if side_bid { 1000.0 - offset } else { 1000.5 + offset };
            let qty = if rng.below(3) == 0 {
                0.0
            } else {
                1.0 + f64::from(u32::try_from(rng.below(9)).unwrap())
            };
            seq += 1;
            let level = [lv(price, qty)];
            if side_bid {
                assert!(truth.apply_delta(seq, &level, &[]));
            } else {
                assert!(truth.apply_delta(seq, &[], &level));
            }
        }
        // Time passes: normally a few milliseconds, sometimes past the anchor timer.
        now += if step % 450 == 449 {
            ANCHOR_EVERY + Duration::from_millis(1)
        } else {
            Duration::from_millis(7)
        };
        // A link loss now and then: the encoder forgets what it sent, the fold is told.
        if step % 600 == 599 {
            encoder.reset();
            fold.stream_status(
                VENUE,
                SYMBOL,
                LINK_STREAM_WORD,
                StreamStatus::GapStart { at_ts_ms: 1 },
            );
        }
        match encoder.next(&truth, now, i64::from(step), SYMBOL) {
            Some(u) => {
                match u.kind {
                    BookUpdateKind::Snapshot => anchors += 1,
                    BookUpdateKind::Delta => deltas += 1,
                    _ => panic!("the encoder sends anchors and deltas only"),
                }
                fold.book_update(VENUE, SYMBOL, u);
                assert_eq!(inner.served(), served + 1, "step {step}: one whole book per update");
                served += 1;
                let got = inner.last().expect("a book was served");
                assert_eq!(whole(&got), whole(&truth), "step {step}: the levels differ");
                assert_eq!(got.last_seq, truth.last_seq, "step {step}: the sequence differs");
                assert!((got.tick_size - truth.tick_size).abs() < f64::EPSILON);
                checked += 1;
            }
            None => {
                // Nothing the subscriber can see changed: what it holds IS the pump's book.
                if let Some(held) = inner.last() {
                    assert_eq!(whole(&held), whole(&truth), "step {step}: a silent step");
                }
            }
        }
    }
    assert!(
        anchors >= 6,
        "the run covered the anchor timer, the update count and the link loss ({anchors})"
    );
    assert!(deltas > ANCHOR_EVERY_UPDATES, "…and long delta runs ({deltas})");
    assert!(checked > 1_500, "…and most steps said something ({checked})");
}

// ---- the parity test's edges: the anchor ceiling, a deeper book, a venue with no sequence -----------------

/// A book of `n` levels a side on a 0.5 grid.
fn deep(n: usize, seq: u64) -> L2Book {
    let bids: Vec<_> =
        (0..n).map(|i| lv(100_000.0 - i as f64 * 0.5, 1.0 + (i % 9) as f64)).collect();
    let asks: Vec<_> =
        (0..n).map(|i| lv(100_000.5 + i as f64 * 0.5, 1.0 + (i % 7) as f64)).collect();
    let mut book = L2Book::new(0.5);
    book.apply_snapshot(seq, &bids, &asks);
    book
}

/// **An anchor of exactly the ceiling (1000 levels a side) folds to the sender's book, and a delta on it
/// flows.** The parity test's books are ~120 levels; this is the largest anchor the lane carries.
#[test]
fn an_anchor_of_a_thousand_levels_a_side_is_the_senders_book() {
    const { assert!(PUMP_BOOK_LEVELS == 1000) };
    let (fold, inner) = fold();
    let mut encoder = BookFeed::new();
    let mut truth = deep(PUMP_BOOK_LEVELS, 1);
    let first = encoder.next(&truth, Instant::now(), 0, SYMBOL).expect("an anchor");
    assert_eq!(
        (first.kind, first.bids.len(), first.asks.len()),
        (BookUpdateKind::Snapshot, 1000, 1000)
    );
    fold.book_update(VENUE, SYMBOL, first);
    assert_eq!(whole(&forwarded(&inner)[0]), whole(&truth));
    // One level changes deep in the book: a delta, and the fold follows.
    assert!(truth.apply_delta(2, &[lv(100_000.0 - 400.0, 99.0)], &[]));
    let delta = encoder.next(&truth, Instant::now(), 1, SYMBOL).expect("a delta");
    assert_eq!(delta.kind, BookUpdateKind::Delta);
    fold.book_update(VENUE, SYMBOL, delta);
    let books = forwarded(&inner);
    assert_eq!(whole(books.last().expect("a book")), whole(&truth));
}

/// **A book DEEPER than the ceiling reaches the engine as its best 1000 levels a side, and stays that way.**
/// The old in-process pump pushed its whole standing book (binance seeds 1000 over REST and folds diffs, so it
/// can exceed that); the engine's book is now the encoder's `top_n(1000)` plus deltas of that prefix. The
/// fold must equal the pump's top 1000 at every step, including when a level falls out of the prefix and
/// another comes in (a removed level at rank 1000 is `qty == 0`, the 1001st takes its place).
#[test]
fn a_book_deeper_than_the_ceiling_reaches_the_engine_as_its_top_levels() {
    let (fold, inner) = fold();
    let mut encoder = BookFeed::new();
    let mut truth = deep(PUMP_BOOK_LEVELS + 300, 1);
    let top = |book: &L2Book| book.top_n(PUMP_BOOK_LEVELS);
    let start = Instant::now();
    let first = encoder.next(&truth, start, 0, SYMBOL).expect("an anchor");
    assert_eq!((first.bids.len(), first.asks.len()), (1000, 1000), "the anchor is bounded");
    fold.book_update(VENUE, SYMBOL, first);
    let engine = |inner: &RecordingSink| forwarded(inner).pop().expect("a book");
    assert_eq!(whole(&engine(&inner)), top(&truth));
    // The best bid is removed: the 1001st bid enters the prefix.
    assert!(truth.apply_delta(2, &[lv(100_000.0, 0.0)], &[]));
    let u = encoder.next(&truth, start, 1, SYMBOL).expect("an update");
    fold.book_update(VENUE, SYMBOL, u);
    assert_eq!(whole(&engine(&inner)), top(&truth), "the level that entered the prefix is there");
    assert_eq!(engine(&inner).bid_levels(), 1000, "and the engine's book stays at the ceiling");
    // A change BELOW the prefix (rank 1100) is invisible to the sender, so nothing is sent.
    assert!(truth.apply_delta(3, &[lv(100_000.0 - 1100.0 * 0.5, 55.0)], &[]));
    assert!(encoder.next(&truth, start, 2, SYMBOL).is_none(), "below the prefix: nothing to say");
}

/// **A venue that supplies no sequence (`seq == 0`) folds like any other**: `apply_delta` takes a zero
/// sequence always, and the encoder sends the delta as one.
#[test]
fn a_sequence_less_venue_folds_like_any_other() {
    let (fold, inner) = fold();
    let mut encoder = BookFeed::new();
    let mut truth = deep(50, 0);
    fold.book_update(
        VENUE,
        SYMBOL,
        encoder.next(&truth, Instant::now(), 0, SYMBOL).expect("anchor"),
    );
    for (i, qty) in [(3_usize, 9.0), (7, 0.0), (11, 4.0)] {
        assert!(truth.apply_delta(0, &[lv(100_000.0 - i as f64 * 0.5, qty)], &[]));
        let u = encoder.next(&truth, Instant::now(), 1, SYMBOL).expect("a delta");
        assert_eq!(u.kind, BookUpdateKind::Delta);
        fold.book_update(VENUE, SYMBOL, u);
        assert_eq!(whole(&forwarded(&inner).pop().expect("a book")), whole(&truth));
    }
}

// ---- the wait for an anchor has a deadline (W12c) --------------------------------------------------------------

/// Poll `ready` (the escalation runs on its own thread) for up to 10 s.
fn eventually(what: &str, mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ready() {
        assert!(Instant::now() < deadline, "never happened: {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// **The deadline is the sender's anchor period times three**, and the sender's period is read from the sender's
/// own constant, not copied: the sender has had three chances to anchor when the fold gives up waiting.
#[test]
fn the_anchor_deadline_is_three_of_the_senders_anchor_periods() {
    assert_eq!(super::ANCHOR_DEADLINE, ANCHOR_EVERY * 3);
}

/// **A hole that gets NO anchor does not stay `Stale` for ever**: past the deadline the engine is told
/// `Disconnected` (a `GapStart` on the book stream, which the connection-state dead-man counts and a strategy's
/// `on_feed_status` hears), once, with the stamp the hole opened at; the anchor, when it does land, closes it with the
/// `Live` that echoes the same stamp. (A hole with no escalation was `Stale` indefinitely, and a quiet book's
/// anchor needed a pump message to be sent at all.)
#[test]
fn a_hole_with_no_anchor_escalates_to_disconnected_and_the_late_anchor_closes_it() {
    let inner = Arc::new(RecordingSink::default());
    let fold = PumpBookFold::with_deadline(
        Arc::clone(&inner) as Arc<dyn LiveDataSink>,
        Duration::from_millis(60),
    );
    fold.book_update(VENUE, SYMBOL, anchor(1));
    hole(&fold, 42);
    assert_eq!(told(&inner), ["BOOK", "book:stale@42"]);
    eventually("the escalation", || told(&inner).len() == 3);
    assert_eq!(told(&inner), ["BOOK", "book:stale@42", "book:gap"], "told Disconnected, once");
    let gap = inner
        .recorded()
        .into_iter()
        .find_map(|c| match c {
            SinkCall::StreamStatus { status: StreamStatus::GapStart { at_ts_ms }, .. } => {
                Some(at_ts_ms)
            }
            _ => None,
        })
        .expect("a GapStart");
    assert_eq!(gap, 42, "stamped with when the hole opened, not with the arrival clock");
    // Said once, however long it goes on.
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(told(&inner).len(), 3);
    // The anchor that finally lands ends it: the book, then the `Live` that closes the window.
    fold.book_update(VENUE, SYMBOL, anchor(9));
    assert_eq!(told(&inner), ["BOOK", "book:stale@42", "book:gap", "BOOK", "book:live@42"]);
}

/// **An anchor inside the deadline cancels it**: a healthy sender's anchor ends the wait and the deadline passes
/// over a hole that is no longer there; and a SECOND hole opens a new wait of its own.
#[test]
fn an_anchor_inside_the_deadline_cancels_the_escalation() {
    let inner = Arc::new(RecordingSink::default());
    let fold = PumpBookFold::with_deadline(
        Arc::clone(&inner) as Arc<dyn LiveDataSink>,
        Duration::from_millis(200),
    );
    fold.book_update(VENUE, SYMBOL, anchor(1));
    hole(&fold, 10);
    fold.book_update(VENUE, SYMBOL, anchor(2));
    assert_eq!(told(&inner), ["BOOK", "book:stale@10", "BOOK", "book:live@10"]);
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(told(&inner).len(), 4, "the deadline of a hole that was anchored says nothing");

    // A later hole is a new wait with its own deadline.
    hole(&fold, 30);
    eventually("the second hole's escalation", || told(&inner).len() == 6);
    assert_eq!(told(&inner)[4..], ["book:stale@30", "book:gap"]);
}

// ---- the escalation is ordered before a racing anchor (W12d) ------------------------------------------------------

/// A sink that PARKS inside the escalation's `GapStart` on the book stream: it says it is parked, then waits to be
/// released, and only then hands the status on. Everything else passes straight through. It stands in for the
/// instant the watcher is mid-send (an `error!` format, a channel send that has to wait for the core) with the
/// anchor arriving, which no sleep can place.
struct Parking {
    inner: Arc<dyn LiveDataSink>,
    parked: std::sync::mpsc::Sender<()>,
    release: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
}

impl LiveDataSink for Parking {
    fn seed_bars(&self, v: &str, s: &str, i: &str, b: Vec<vike_marketdata::Bar>) {
        self.inner.seed_bars(v, s, i, b);
    }
    fn close_bar(&self, v: &str, s: &str, i: &str, b: vike_marketdata::Bar) {
        self.inner.close_bar(v, s, i, b);
    }
    fn forming_bar(&self, v: &str, s: &str, i: &str, b: vike_marketdata::Bar) {
        self.inner.forming_bar(v, s, i, b);
    }
    fn mark_tick(&self, v: &str, s: &str, p: f64, t: i64) {
        self.inner.mark_tick(v, s, p, t);
    }
    fn quote(&self, v: &str, s: &str, q: vike_marketdata::QuoteTick) {
        self.inner.quote(v, s, q);
    }
    fn trade(&self, v: &str, s: &str, t: vike_marketdata::TradeTick) {
        self.inner.trade(v, s, t);
    }
    fn book(&self, v: &str, s: &str, book: Arc<L2Book>) {
        self.inner.book(v, s, book);
    }
    fn stream_status(&self, venue: &str, symbol: &str, stream: &str, status: StreamStatus) {
        if stream == "book" && matches!(status, StreamStatus::GapStart { .. }) {
            self.parked.send(()).expect("the test is listening");
            self.release.lock().unwrap().recv().expect("the test releases the park");
        }
        self.inner.stream_status(venue, symbol, stream, status);
    }
}

/// One race: a hole opens, its watcher decides to escalate and PARKS inside the send, and an anchor is folded by
/// the reader thread meanwhile. Returns once both have finished. A build that lets the anchor in finishes the
/// reader at once; a build that makes the watcher's decision and send one critical section keeps the reader
/// waiting until the park is released; either way the verdict is read from the ORDER the sink saw, never from
/// how long anything took.
fn race_an_anchor_against_a_parked_escalation(sink: Arc<dyn LiveDataSink>) {
    let (parked_tx, parked_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let parking = Arc::new(Parking {
        inner: sink,
        parked: parked_tx,
        release: std::sync::Mutex::new(release_rx),
    });
    let fold = Arc::new(PumpBookFold::with_deadline(
        Arc::clone(&parking) as Arc<dyn LiveDataSink>,
        Duration::from_millis(40),
    ));
    fold.book_update(VENUE, SYMBOL, anchor(1));
    hole(fold.as_ref(), 42);
    // The watcher has decided and is inside the sink's `GapStart`.
    parked_rx.recv_timeout(Duration::from_secs(10)).expect("the watcher escalated");

    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let reader = {
        let fold = Arc::clone(&fold);
        std::thread::spawn(move || {
            fold.book_update(VENUE, SYMBOL, anchor(9));
            let _ = done_tx.send(());
        })
    };
    // A bounded NEGATIVE wait: a correct build keeps the reader out until the park ends, and never fails this.
    let _ = done_rx.recv_timeout(Duration::from_millis(250));
    release_tx.send(()).expect("release the watcher");
    reader.join().expect("the reader finished");
}

/// **The escalation's `GapStart` is ordered BEFORE an anchor that lands while it is in flight.** The watcher
/// decided to escalate under the fold's lock and sent the `GapStart` after letting it go; the reader thread folds
/// an anchor under the same lock and sends the `Live` after. An anchor landing in that gap reached the engine as
/// `Live` THEN `GapStart`, which opens a connection-state dead-man window nothing closes. Here the watcher is
/// parked inside the send and the anchor races it; the engine must be told `GapStart` first, then the book, then
/// the `Live` that closes it.
#[test]
fn an_anchor_racing_a_parked_escalation_is_ordered_after_it() {
    let inner = Arc::new(RecordingSink::default());
    race_an_anchor_against_a_parked_escalation(Arc::clone(&inner) as Arc<dyn LiveDataSink>);
    assert_eq!(
        told(&inner),
        ["BOOK", "book:stale@42", "book:gap", "BOOK", "book:live@42"],
        "the Live must come after the GapStart it closes"
    );
}

/// A spawned core with the connection-state dead-man armed for [`VENUE`] (`CancelAllAndHalt`, grace `grace`).
fn armed_core(grace: Duration) -> vike_core::CoreHandle {
    use vike_core::{CoreConfig, DeadManAction, LinkDeadManConfig};
    use vike_exec::testing::RecordingClient;
    use vike_exec::{Account, BalanceMode, ExecutionEngine, RiskGate};
    use vike_model::RiskLimits;

    let engine = ExecutionEngine::new(
        Account::new(1.0, VENUE, None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        VENUE,
        SYMBOL,
    );
    vike_core::spawn_core(
        engine,
        CoreConfig {
            link_deadman: Some(LinkDeadManConfig {
                grace,
                action: DeadManAction::CancelAllAndHalt,
                venues: std::iter::once(VENUE.to_string()).collect(),
            }),
            ..CoreConfig::default()
        },
    )
}

fn core_sink(handle: &vike_core::CoreHandle) -> Arc<dyn LiveDataSink> {
    Arc::new(vike_tradehub_core::CoreLaneSink::new(
        handle.bar_sender(),
        handle.market_sender(),
        handle.tick_sender(),
    ))
}

/// **Through the REAL `CoreLaneSink`, a real core and its REAL `LinkDeadMan`: a `Live` that races a late
/// escalation leaves no window open.** The same race as above, delivered to a spawned core armed with the
/// connection-state dead-man. In the bad order the `Live` finds no window and the `GapStart` after it opens one
/// nothing closes, so the grace later halts a venue whose feed is healthy; in the right order the `Live` closes
/// the window the `GapStart` opened. The proof of absence waits out four graces and reads that the core is still
/// trading; its control is the next test.
#[test]
fn a_late_escalation_racing_the_anchor_leaves_no_dead_man_window_open() {
    let grace = Duration::from_millis(400);
    let handle = armed_core(grace);
    race_an_anchor_against_a_parked_escalation(core_sink(&handle));

    std::thread::sleep(grace * 4);
    assert_eq!(
        handle.snapshot().trading_state,
        vike_model::TradingState::Active,
        "a healthy feed's venue was halted"
    );
    handle.shutdown_and_join();
}

/// The control of the test above, on the same kind of core: a `Live` followed by the late `GapStart` it should
/// have followed (the order the race used to produce) DOES trip the dead-man, so the proof of absence above is a
/// proof and not a dead switch.
#[test]
fn the_dead_man_the_race_test_uses_trips_on_a_window_nothing_closes() {
    let handle = armed_core(Duration::from_millis(400));
    let sink = core_sink(&handle);
    sink.stream_status(VENUE, SYMBOL, "book", StreamStatus::Live { gap_started_ts_ms: Some(42) });
    sink.stream_status(VENUE, SYMBOL, "book", StreamStatus::GapStart { at_ts_ms: 42 });
    eventually("the dead-man trips on a window nothing closes", || {
        handle.snapshot().trading_state == vike_model::TradingState::Halted
    });
    handle.shutdown_and_join();
}

// ---- an identical anchor is not forwarded again; a dropped fold ends its waits (W12d) -----------------------------

/// **An anchor that restates the book the engine already holds is not handed over again**: the sender's periodic
/// anchor of a quiet book arrives every ~2 s, and `SpreadMaker::requote` re-amends its resting quotes on every
/// call (the refresh tolerance is off by default), so each identical book would be a venue amend. A CHANGED
/// anchor, and the anchor that ends a hole, are forwarded; the folded book still moves on (a later delta applies).
#[test]
fn an_identical_anchor_is_not_forwarded_twice() {
    let (fold, inner) = fold();
    fold.book_update(VENUE, SYMBOL, anchor(1));
    fold.book_update(VENUE, SYMBOL, anchor(2));
    fold.book_update(VENUE, SYMBOL, anchor(3));
    assert_eq!(forwarded(&inner).len(), 1, "the quiet book's re-anchors said nothing");
    // A change in a level IS forwarded.
    let mut moved = anchor(4);
    moved.bids[0] = lv(100.0, 7.0);
    fold.book_update(VENUE, SYMBOL, moved);
    assert_eq!(forwarded(&inner).len(), 2);
    // A delta after the silent anchors applies to the re-seeded book (its sequence moved on: 4 -> 5).
    fold.book_update(VENUE, SYMBOL, update(BookUpdateKind::Delta, 5, 0.5, &[lv(99.5, 9.0)], &[]));
    assert_eq!(forwarded(&inner).len(), 3);
    // The same levels on another GRID are not the same book.
    fold.book_update(
        VENUE,
        SYMBOL,
        update(
            BookUpdateKind::Snapshot,
            6,
            0.25,
            &[lv(100.0, 7.0), lv(99.5, 9.0)],
            &[lv(101.0, 1.0), lv(101.5, 2.0)],
        ),
    );
    assert_eq!(forwarded(&inner).len(), 4);
}

/// The anchor that ENDS a hole is forwarded even though it restates the book the hole dropped, and its `Live`
/// follows (the hole left the engine with a book it was told is stale).
#[test]
fn the_anchor_that_ends_a_hole_is_forwarded_even_if_identical() {
    let (fold, inner) = fold();
    fold.book_update(VENUE, SYMBOL, anchor(1));
    hole(&fold, 42);
    fold.book_update(VENUE, SYMBOL, anchor(2));
    assert_eq!(told(&inner), ["BOOK", "book:stale@42", "BOOK", "book:live@42"]);
}

/// **A fold dropped with a hole open lets its watcher go without a word**: a feed that is shut down (or a failed
/// start's teardown) must not have a detached thread tell the engine, seconds later, that a book nobody holds any
/// more is `Disconnected`.
#[test]
fn a_dropped_fold_ends_its_watcher_without_escalating() {
    let inner = Arc::new(RecordingSink::default());
    let fold = PumpBookFold::with_deadline(
        Arc::clone(&inner) as Arc<dyn LiveDataSink>,
        Duration::from_millis(150),
    );
    fold.book_update(VENUE, SYMBOL, anchor(1));
    hole(&fold, 42);
    drop(fold);
    // The watcher's deadline is 150 ms: well past it, nothing was said beyond the Stale.
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(told(&inner), ["BOOK", "book:stale@42"], "no escalation after the fold is gone");
}
