#![cfg(feature = "data")]
//! **R2 — event fidelity: every `LiveDataSink` verb crosses the hop BIT-EXACT and in order.**
//!
//! `LiveDataSink` has twelve methods, and each one is a `StreamDown` frame. This suite drives the
//! REAL `RemoteDataClient` against the fake peer: the peer pushes the frames a daemon would write, and
//! a `RecordingSink` captures what the sink on the client's side received. Every float is compared with
//! `to_bits` (so `-0.0`, subnormals and the largest normals are all in play), every string and integer
//! by equality, and the ORDER of the calls is the order of the frames.
//!
//! The strategy generates only FINITE floats: a non-finite float serializes to `null` and cannot cross
//! (the `proto` module doc says so, and `a_non_finite_float_cannot_cross` below holds it down).
//!
//! If a counterexample appears, commit proptest's failure-persistence seed BESIDE this file as
//! crates/vike-gatewayhub-client/tests/event_roundtrip.proptest-regressions (written without backticks: the file does not exist until a counterexample does) — not under a
//! `proptest-regressions/` directory, which proptest never creates for an integration test.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use proptest::prelude::*;
use proptest::test_runner::TestCaseError;
use vike_data::{LiveDataSink, RecordingSink, SinkCall};
use vike_gatewayhub_client::opener::GatewayOpener;
use vike_market_stream::market::RemoteDataClient;
use vike_market_stream::wire::{GapVerb, StreamDown, WireStream};
use vike_marketdata::{Bar, BookLevel, BookUpdate, BookUpdateKind, L2Book, QuoteTick, TradeTick};
use vike_model::{DataDelay, StreamStatus};

#[allow(dead_code, clippy::allow_attributes)]
#[path = "support/fake_peer.rs"]
mod fake_peer;

use fake_peer::{FakePeer, PeerConfig, StreamHandle};

// ---- The events -----------------------------------------------------------------------------------

/// One sink call, as the test generates it. One variant per `LiveDataSink` method.
#[derive(Debug, Clone)]
enum Ev {
    Seed(String, Vec<Bar>),
    BarClose(String, Bar),
    Forming(String, Bar),
    Mark(f64, i64),
    BarCloseTick(f64, i64),
    Depth(f64, Vec<BookLevel>, Vec<BookLevel>, i64),
    Quote(QuoteTick),
    Trade(TradeTick),
    Book(L2Book),
    BookDelta(BookUpdate),
    Status(String, StreamStatus),
    Delay(String, DataDelay),
}

/// Every finite `f64` class: normal, subnormal, zero (both signs), either sign.
fn finite() -> impl Strategy<Value = f64> {
    proptest::num::f64::POSITIVE
        | proptest::num::f64::NEGATIVE
        | proptest::num::f64::NORMAL
        | proptest::num::f64::SUBNORMAL
        | proptest::num::f64::ZERO
}

fn opt_f64() -> impl Strategy<Value = Option<f64>> {
    proptest::option::of(finite())
}

fn text() -> impl Strategy<Value = String> {
    ".{0,10}"
}

fn bar() -> impl Strategy<Value = Bar> {
    (
        any::<i64>(),
        (finite(), finite(), finite(), finite(), finite()),
        (opt_f64(), opt_f64(), opt_f64()),
        proptest::option::of(text()),
    )
        .prop_map(|(ts, (open, high, low, close, volume), (funding, bid, ask), symbol)| Bar {
            ts,
            open,
            high,
            low,
            close,
            volume,
            funding,
            bid,
            ask,
            symbol,
        })
}

fn level() -> impl Strategy<Value = BookLevel> {
    (finite(), finite()).prop_map(|(p, q)| BookLevel::new(p, q))
}

fn quote() -> impl Strategy<Value = QuoteTick> {
    (any::<i64>(), any::<i64>(), (finite(), finite(), finite(), finite()), text()).prop_map(
        |(ts, local_ts, (bid, ask, bid_size, ask_size), symbol)| QuoteTick {
            ts,
            local_ts,
            bid,
            ask,
            bid_size,
            ask_size,
            symbol,
        },
    )
}

fn trade() -> impl Strategy<Value = TradeTick> {
    (any::<i64>(), any::<i64>(), finite(), finite(), any::<bool>(), text()).prop_map(
        |(ts, local_ts, price, size, is_buyer_maker, symbol)| TradeTick {
            ts,
            local_ts,
            price,
            size,
            is_buyer_maker,
            symbol,
        },
    )
}

/// A folded book: a sane grid and levels, so `apply_snapshot` quantizes without overflowing a tick.
fn book() -> impl Strategy<Value = L2Book> {
    let px = 0.001f64..1.0e6;
    let qty = 0.001f64..1.0e9;
    (
        0.0001f64..1000.0,
        any::<u64>(),
        proptest::collection::vec((px.clone(), qty.clone()), 0..6),
        proptest::collection::vec((px, qty), 0..6),
    )
        .prop_map(|(tick, seq, bids, asks)| {
            let levels = |v: Vec<(f64, f64)>| -> Vec<BookLevel> {
                v.into_iter().map(|(p, q)| BookLevel::new(p, q)).collect()
            };
            let mut book = L2Book::new(tick);
            book.apply_snapshot(seq, &levels(bids), &levels(asks));
            book
        })
}

fn book_update() -> impl Strategy<Value = BookUpdate> {
    let kind = prop_oneof![
        Just(BookUpdateKind::Delta),
        Just(BookUpdateKind::Snapshot),
        Just(BookUpdateKind::GapStart),
        Just(BookUpdateKind::Stale),
        Just(BookUpdateKind::LiveResume),
    ];
    (
        (any::<i64>(), any::<i64>(), any::<u64>()),
        kind,
        finite(),
        proptest::collection::vec(level(), 0..4),
        proptest::collection::vec(level(), 0..4),
        text(),
    )
        .prop_map(|((ts, local_ts, seq), kind, tick_size, bids, asks, symbol)| BookUpdate {
            ts,
            local_ts,
            seq,
            kind,
            tick_size,
            bids,
            asks,
            symbol,
        })
}

/// A `stream` word: the interval spellings, the four tick labels, the pump's link word, and noise.
fn stream_word() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("1m".to_string()),
        Just("15s".to_string()),
        Just("quotes".to_string()),
        Just("trades".to_string()),
        Just("book".to_string()),
        Just("depth".to_string()),
        Just("ticks".to_string()),
        text(),
    ]
}

fn status() -> impl Strategy<Value = StreamStatus> {
    prop_oneof![
        any::<i64>().prop_map(|at_ts_ms| StreamStatus::GapStart { at_ts_ms }),
        proptest::option::of(any::<i64>())
            .prop_map(|gap_started_ts_ms| StreamStatus::Live { gap_started_ts_ms }),
        (any::<i64>(), any::<i64>()).prop_map(|(newest_data_ts_ms, now_ms)| StreamStatus::Stale {
            newest_data_ts_ms,
            now_ms
        }),
    ]
}

fn delay() -> impl Strategy<Value = DataDelay> {
    prop_oneof![
        Just(DataDelay::RealTime),
        proptest::option::of(any::<u64>()).prop_map(|lag_ms| DataDelay::Delayed { lag_ms }),
        Just(DataDelay::Frozen),
    ]
}

fn ev() -> impl Strategy<Value = Ev> {
    prop_oneof![
        (stream_word(), proptest::collection::vec(bar(), 0..4)).prop_map(|(i, b)| Ev::Seed(i, b)),
        (stream_word(), bar()).prop_map(|(i, b)| Ev::BarClose(i, b)),
        (stream_word(), bar()).prop_map(|(i, b)| Ev::Forming(i, b)),
        (finite(), any::<i64>()).prop_map(|(px, ts)| Ev::Mark(px, ts)),
        (finite(), any::<i64>()).prop_map(|(px, ts)| Ev::BarCloseTick(px, ts)),
        (
            finite(),
            proptest::collection::vec(level(), 0..4),
            proptest::collection::vec(level(), 0..4),
            any::<i64>()
        )
            .prop_map(|(t, b, a, ts)| Ev::Depth(t, b, a, ts)),
        quote().prop_map(Ev::Quote),
        trade().prop_map(Ev::Trade),
        book().prop_map(Ev::Book),
        book_update().prop_map(Ev::BookDelta),
        (stream_word(), status()).prop_map(|(w, s)| Ev::Status(w, s)),
        (stream_word(), delay()).prop_map(|(w, d)| Ev::Delay(w, d)),
    ]
}

// ---- The frames ------------------------------------------------------------------------------------

/// The frame a daemon writes for `ev` under key `k`, numbering each lossless verb +1 from 1.
fn frame(ev: &Ev, k: u32, seqs: &mut HashMap<GapVerb, u64>) -> StreamDown {
    let mut next = |verb: GapVerb| {
        let s = seqs.entry(verb).or_insert(0);
        *s += 1;
        *s
    };
    match ev.clone() {
        Ev::Seed(interval, bars) => {
            StreamDown::Seed { k, seq: next(GapVerb::Seed), interval, bars }
        }
        Ev::BarClose(interval, bar) => {
            StreamDown::BarClose { k, seq: next(GapVerb::BarClose), interval, bar }
        }
        // The conflatable verbs carry an arbitrary `seq`: a jump on them is not a loss.
        Ev::Forming(interval, bar) => StreamDown::Forming { k, seq: 7_000_000, interval, bar },
        Ev::Mark(px, ts) => StreamDown::Mark { k, seq: 3, px, ts },
        Ev::BarCloseTick(px, ts) => StreamDown::BarCloseTick { k, seq: 900, px, ts },
        Ev::Depth(tick_size, bids, asks, ts) => {
            StreamDown::Depth { k, seq: 11, tick_size, bids, asks, ts }
        }
        Ev::Quote(quote) => StreamDown::Quote { k, seq: next(GapVerb::Quote), quote },
        Ev::Trade(trade) => StreamDown::Trade { k, seq: next(GapVerb::Trade), trade },
        Ev::Book(book) => StreamDown::Book { k, seq: next(GapVerb::Book), book: Box::new(book) },
        Ev::BookDelta(update) => {
            StreamDown::BookDelta { k, seq: next(GapVerb::BookDelta), update: Box::new(update) }
        }
        Ev::Status(word, status) => {
            StreamDown::Status { k, stream: WireStream::from_word(&word), status }
        }
        Ev::Delay(word, delay) => {
            StreamDown::Delay { k, stream: WireStream::from_word(&word), delay }
        }
    }
}

// ---- Bit-exact comparison ---------------------------------------------------------------------------

fn same(a: f64, b: f64) -> bool {
    a.to_bits() == b.to_bits()
}

fn same_opt(a: Option<f64>, b: Option<f64>) -> bool {
    a.map(f64::to_bits) == b.map(f64::to_bits)
}

fn same_bar(a: &Bar, b: &Bar) -> bool {
    a.ts == b.ts
        && same(a.open, b.open)
        && same(a.high, b.high)
        && same(a.low, b.low)
        && same(a.close, b.close)
        && same(a.volume, b.volume)
        && same_opt(a.funding, b.funding)
        && same_opt(a.bid, b.bid)
        && same_opt(a.ask, b.ask)
        && a.symbol == b.symbol
}

fn same_levels(a: &[BookLevel], b: &[BookLevel]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| same(x.price, y.price) && same(x.qty, y.qty))
}

fn same_quote(a: &QuoteTick, b: &QuoteTick) -> bool {
    a.ts == b.ts
        && a.local_ts == b.local_ts
        && same(a.bid, b.bid)
        && same(a.ask, b.ask)
        && same(a.bid_size, b.bid_size)
        && same(a.ask_size, b.ask_size)
        && a.symbol == b.symbol
}

fn same_trade(a: &TradeTick, b: &TradeTick) -> bool {
    a.ts == b.ts
        && a.local_ts == b.local_ts
        && same(a.price, b.price)
        && same(a.size, b.size)
        && a.is_buyer_maker == b.is_buyer_maker
        && a.symbol == b.symbol
}

/// Whether the sink's `call` is exactly what `ev` says, for `symbol` on `venue`.
fn matches_call(ev: &Ev, call: &SinkCall, venue: &str, symbol: &str) -> bool {
    match (ev, call) {
        (
            Ev::Seed(interval, bars),
            SinkCall::SeedBars { venue: v, symbol: s, interval: i, bars: got },
        ) => {
            v == venue
                && s == symbol
                && i == interval
                && bars.len() == got.len()
                && bars.iter().zip(got).all(|(a, b)| same_bar(a, b))
        }
        (
            Ev::BarClose(interval, bar),
            SinkCall::CloseBar { venue: v, symbol: s, interval: i, bar: got },
        ) => v == venue && s == symbol && i == interval && same_bar(bar, got),
        (
            Ev::Forming(interval, bar),
            SinkCall::FormingBar { venue: v, symbol: s, interval: i, bar: got },
        ) => v == venue && s == symbol && i == interval && same_bar(bar, got),
        (Ev::Mark(px, ts), SinkCall::MarkTick { venue: v, symbol: s, px: gp, ts: gt }) => {
            v == venue && s == symbol && same(*px, *gp) && ts == gt
        }
        (
            Ev::BarCloseTick(px, ts),
            SinkCall::BarCloseTick { venue: v, symbol: s, px: gp, ts: gt },
        ) => v == venue && s == symbol && same(*px, *gp) && ts == gt,
        (
            Ev::Depth(tick, bids, asks, ts),
            SinkCall::L2Snapshot { venue: v, symbol: s, tick_size, bids: gb, asks: ga, ts: gt },
        ) => {
            v == venue
                && s == symbol
                && same(*tick, *tick_size)
                && same_levels(bids, gb)
                && same_levels(asks, ga)
                && ts == gt
        }
        (Ev::Quote(q), SinkCall::Quote { venue: v, symbol: s, quote: got }) => {
            v == venue && s == symbol && same_quote(q, got)
        }
        (Ev::Trade(t), SinkCall::Trade { venue: v, symbol: s, trade: got }) => {
            v == venue && s == symbol && same_trade(t, got)
        }
        (Ev::Book(b), SinkCall::Book { venue: v, symbol: s, book: got }) => {
            let (want_b, want_a) = b.top_n(usize::MAX);
            let (got_b, got_a) = got.top_n(usize::MAX);
            v == venue
                && s == symbol
                && same(b.tick_size, got.tick_size)
                && b.last_seq == got.last_seq
                && same_levels(&want_b, &got_b)
                && same_levels(&want_a, &got_a)
        }
        (Ev::BookDelta(u), SinkCall::BookUpdate { venue: v, symbol: s, update: got }) => {
            v == venue
                && s == symbol
                && u.ts == got.ts
                && u.local_ts == got.local_ts
                && u.seq == got.seq
                && u.kind == got.kind
                && same(u.tick_size, got.tick_size)
                && same_levels(&u.bids, &got.bids)
                && same_levels(&u.asks, &got.asks)
                && u.symbol == got.symbol
        }
        (
            Ev::Status(word, status),
            SinkCall::StreamStatus { venue: v, symbol: s, stream, status: got },
        ) => v == venue && s == symbol && stream == word && status == got,
        (
            Ev::Delay(word, delay),
            SinkCall::DataDelay { venue: v, symbol: s, stream, delay: got },
        ) => v == venue && s == symbol && stream == word && delay == got,
        _ => false,
    }
}

// ---- The harness: ONE client and peer for the whole suite --------------------------------------------

struct Harness {
    _peer: FakePeer,
    _client: RemoteDataClient,
    sink: Arc<RecordingSink>,
    stream: StreamHandle,
    next_key: u32,
}

fn harness() -> &'static Mutex<Harness> {
    static H: OnceLock<Mutex<Harness>> = OnceLock::new();
    H.get_or_init(|| {
        let peer = FakePeer::start(PeerConfig::new());
        let sink = Arc::new(RecordingSink::default());
        let dyn_sink: Arc<dyn LiveDataSink> = sink.clone();
        let client = RemoteDataClient::new(GatewayOpener::new(peer.link()), "binance", dyn_sink)
            .expect("the reader thread");
        let stream = peer.wait_stream(0, Duration::from_secs(5));
        Mutex::new(Harness { _peer: peer, _client: client, sink, stream, next_key: 100 })
    })
}

/// Push `events` under a fresh key bound to `symbol`, and compare what the sink received.
fn roundtrip(symbol: &str, events: &[Ev]) -> Result<(), TestCaseError> {
    let mut h = harness().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let before = h.sink.recorded().len();
    let k = h.next_key;
    h.next_key += 1;
    h.stream.send(&StreamDown::Key { k, symbol: symbol.to_string() });
    let mut seqs = HashMap::new();
    for e in events {
        h.stream.send(&frame(e, k, &mut seqs));
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while h.sink.recorded().len() < before + events.len() {
        prop_assert!(
            Instant::now() < deadline,
            "the sink saw only {} of {} calls",
            h.sink.recorded().len() - before,
            events.len()
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    let calls = h.sink.recorded();
    let got = &calls[before..];
    prop_assert_eq!(got.len(), events.len(), "an extra call reached the sink: {:?}", got);
    for (i, (e, call)) in events.iter().zip(got).enumerate() {
        prop_assert!(
            matches_call(e, call, "binance", symbol),
            "call {} differs after the hop:\n  sent {:?}\n  got  {:?}",
            i,
            e,
            call
        );
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(96))]

    /// R2: any sequence of the twelve verbs crosses bit-exact, in order, under any symbol spelling.
    #[test]
    fn every_sink_verb_survives_the_hop_bit_exact_and_in_order(
        symbol in ".{1,20}",
        events in proptest::collection::vec(ev(), 1..10),
    ) {
        roundtrip(&symbol, &events)?;
    }
}

/// One of each, deterministically: the twelve verbs are all carried (a regression here names the
/// verb, where the proptest names a shrunk counterexample).
#[test]
fn all_twelve_verbs_are_carried() {
    let bar = Bar {
        ts: 1,
        open: 1.5,
        high: 2.5,
        low: 0.5,
        close: 2.0,
        volume: 10.0,
        funding: Some(0.0001),
        bid: None,
        ask: Some(2.1),
        symbol: Some("BTCUSDT".into()),
    };
    let mut book = L2Book::new(0.5);
    book.apply_snapshot(9, &[BookLevel::new(100.0, 2.0)], &[BookLevel::new(100.5, 3.0)]);
    let events = vec![
        Ev::Seed("1m".into(), vec![bar.clone(), bar.clone()]),
        Ev::BarClose("1m".into(), bar.clone()),
        Ev::Forming("1m".into(), bar),
        Ev::Mark(101.25, 5),
        Ev::BarCloseTick(101.5, 6),
        Ev::Depth(0.5, vec![BookLevel::new(100.0, 1.0)], vec![BookLevel::new(100.5, 1.0)], 7),
        Ev::Quote(QuoteTick {
            ts: 8,
            local_ts: 9,
            bid: 100.0,
            ask: 100.5,
            bid_size: 1.0,
            ask_size: 2.0,
            symbol: "BTCUSDT".into(),
        }),
        Ev::Trade(TradeTick {
            ts: 10,
            local_ts: 11,
            price: 100.25,
            size: 0.125,
            is_buyer_maker: true,
            symbol: String::new(),
        }),
        Ev::Book(book),
        Ev::BookDelta(BookUpdate {
            ts: 12,
            local_ts: 13,
            seq: 14,
            kind: BookUpdateKind::Delta,
            tick_size: 0.5,
            bids: vec![BookLevel::new(100.0, 0.0)],
            asks: vec![],
            symbol: String::new(),
        }),
        Ev::Status("book".into(), StreamStatus::GapStart { at_ts_ms: 15 }),
        Ev::Delay("quotes".into(), DataDelay::Delayed { lag_ms: Some(900_000) }),
    ];
    assert_eq!(events.len(), 12, "the sink has twelve methods");
    roundtrip("BTCUSDT", &events).expect("all twelve verbs cross the hop unchanged");
}

/// A non-finite float serializes to `null` and cannot cross: the codec refuses it loudly rather than
/// delivering a different number. (A daemon drops such a row; it never reaches the wire.)
#[test]
fn a_non_finite_float_cannot_cross() {
    for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let frame = StreamDown::Mark { k: 1, seq: 1, px: bad, ts: 0 };
        let json = serde_json::to_string(&frame).expect("serde_json writes null for it");
        assert!(json.contains("null"), "{json}");
        assert!(
            serde_json::from_str::<StreamDown>(&json).is_err(),
            "null must not decode as a price"
        );
    }
}
