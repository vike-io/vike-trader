//! The `stream` class END TO END: this crate's real server, fake lanes behind it, and the client
//! crate's real `DataClientStream` as the peer.
//!
//! What it holds: every sink verb crosses and arrives bit for bit and in order; a subscription's
//! errors keep their variant and their words; a seed emitted from INSIDE `subscribe_*` is not lost
//! (the `SubscribeOk`-before-any-frame rule seen from the consumer's side); the two-phase teardown reaches
//! the venue client; and a venue that is down is a failed subscribe, not a different `Welcome`.

use crate::{fake_lanes, harness};
use vike_gatewayhub::testkit::raw_peer;

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use vike_data::{DataClient, DataClientError, DataClientSink};
use vike_gatewayhub_client::opener::StreamOpenerGateway;
use vike_market_stream::market::DataClientStream;
use vike_marketdata::{
    Bar, L2Book, L2BookLevel, L2BookUpdate, L2BookUpdateKind, TickQuote, TickTrade,
};
use vike_model::{StreamDelay, StreamStatus};

use fake_lanes::Refusal;
use harness::Harness;

/// One thing the sink saw.
#[derive(Debug, Clone, PartialEq)]
enum RecordedEvent {
    Seed(String, String, usize),
    Close(String, String, i64),
    Forming(String, String, i64),
    Mark(String, f64, i64),
    BarCloseTick(String, f64, i64),
    Depth(String, f64, usize, usize, i64),
    Quote(String, TickQuote),
    Trade(String, TickTrade),
    Book(String, usize),
    BookDelta(String, u64),
    Status(String, String, StreamStatus),
    Delay(String, String, StreamDelay),
    Funding(String, vike_marketdata::FundingRate),
}

#[derive(Default)]
struct Rec {
    events: Mutex<Vec<RecordedEvent>>,
}

impl Rec {
    fn push(&self, event: RecordedEvent) {
        self.events.lock().unwrap_or_else(PoisonError::into_inner).push(event);
    }

    fn snapshot(&self) -> Vec<RecordedEvent> {
        self.events.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    fn wait_for(&self, what: &str, pred: impl Fn(&[RecordedEvent]) -> bool) -> Vec<RecordedEvent> {
        let mut now = Vec::new();
        let held = vike_log::poll::wait_until(Duration::from_secs(10), || {
            now = self.snapshot();
            pred(&now)
        });
        assert!(held, "timed out waiting for {what}; saw {now:?}");
        now
    }
}

impl DataClientSink for Rec {
    fn seed_bars(&self, _v: &str, symbol: &str, interval: &str, bars: Vec<Bar>) {
        self.push(RecordedEvent::Seed(symbol.into(), interval.into(), bars.len()));
    }
    fn close_bar(&self, _v: &str, symbol: &str, interval: &str, bar: Bar) {
        self.push(RecordedEvent::Close(symbol.into(), interval.into(), bar.timestamp));
    }
    fn forming_bar(&self, _v: &str, symbol: &str, interval: &str, bar: Bar) {
        self.push(RecordedEvent::Forming(symbol.into(), interval.into(), bar.timestamp));
    }
    fn mark_tick(&self, _v: &str, symbol: &str, px: f64, ts: i64) {
        self.push(RecordedEvent::Mark(symbol.into(), px, ts));
    }
    fn bar_close_tick(&self, _v: &str, symbol: &str, px: f64, ts: i64) {
        self.push(RecordedEvent::BarCloseTick(symbol.into(), px, ts));
    }
    fn l2_snapshot(
        &self,
        _v: &str,
        symbol: &str,
        tick_size: f64,
        bids: Vec<L2BookLevel>,
        asks: Vec<L2BookLevel>,
        ts: i64,
    ) {
        self.push(RecordedEvent::Depth(symbol.into(), tick_size, bids.len(), asks.len(), ts));
    }
    fn quote(&self, _v: &str, symbol: &str, quote: TickQuote) {
        self.push(RecordedEvent::Quote(symbol.into(), quote));
    }
    fn trade(&self, _v: &str, symbol: &str, trade: TickTrade) {
        self.push(RecordedEvent::Trade(symbol.into(), trade));
    }
    fn book(&self, _v: &str, symbol: &str, book: Arc<L2Book>) {
        self.push(RecordedEvent::Book(symbol.into(), book.bid_levels() + book.ask_levels()));
    }
    fn book_update(&self, _v: &str, symbol: &str, update: L2BookUpdate) {
        self.push(RecordedEvent::BookDelta(symbol.into(), update.sequence));
    }
    fn stream_status(&self, _v: &str, symbol: &str, stream: &str, status: StreamStatus) {
        self.push(RecordedEvent::Status(symbol.into(), stream.into(), status));
    }
    fn data_delay(&self, _v: &str, symbol: &str, stream: &str, delay: StreamDelay) {
        self.push(RecordedEvent::Delay(symbol.into(), stream.into(), delay));
    }
    fn funding(&self, _v: &str, symbol: &str, funding: vike_marketdata::FundingRate) {
        self.push(RecordedEvent::Funding(symbol.into(), funding));
    }
}

/// **A venue's funding rate crosses the gatewayhub's hop to a client that asked for it.** The daemon advertises
/// `stream_funding`, so the client's session asks for funding frames before its first `Sub`; the venue client's
/// funding observation then reaches the receiving sink unchanged, under the bound symbol.
#[test]
fn a_funding_rate_crosses_the_hop_to_a_client_that_asked_for_it() {
    let h = Harness::start();
    let (mut client, rec) = new_client(&h, "binance");
    client.subscribe_bars("BTCUSDT.P", "1m").expect("bars");
    let funding = vike_marketdata::FundingRate {
        timestamp: 7,
        symbol: "BTCUSDT.P".into(),
        rate: 0.0000342,
        interval_ms: None,
        next_timestamp: Some(1_794_153_600_000),
        predicted: None,
        price: None,
    };
    h.data.session(0).sink.funding("binance", "BTCUSDT.P", funding.clone());
    let seen = rec.wait_for("the funding frame", |evs| {
        evs.iter().any(|e| matches!(e, RecordedEvent::Funding(..)))
    });
    assert!(seen.contains(&RecordedEvent::Funding("BTCUSDT.P".into(), funding)), "{seen:?}");
}

fn bar(ts: i64) -> Bar {
    Bar {
        timestamp: ts,
        open: 0.1 + 0.2,
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

fn quote(ts: i64, bid: f64) -> TickQuote {
    TickQuote {
        timestamp: ts,
        local_timestamp: ts + 1,
        bid,
        ask: bid + 0.5,
        bid_size: 1.25,
        ask_size: 2.5,
        symbol: "BTCUSDT".into(),
    }
}

fn new_client(h: &Harness, venue: &str) -> (DataClientStream, Arc<Rec>) {
    let rec = Arc::new(Rec::default());
    let client = DataClientStream::new(StreamOpenerGateway::new(h.link()), venue, rec.clone())
        .expect("a reader thread");
    (client, rec)
}

#[test]
fn quotes_arrive_in_order_bit_exact_and_unsubscribe_reaches_the_venue_client() {
    let h = Harness::start();
    let (mut client, rec) = new_client(&h, "binance");
    let id = client.subscribe_quotes("BTCUSDT").expect("the stream starts");
    let session = h.data.session(0);
    assert_eq!(session.venue, "binance");
    assert!(session.calls().contains(&"subscribe_quotes BTCUSDT".to_string()));

    for i in 0..200 {
        // 0.1 + i * 0.1 is a value with no short decimal form: bit-exactness is the point.
        session.sink.quote("binance", "BTCUSDT", quote(i, 0.1 + (i as f64) * 0.1));
    }
    let seen = rec.wait_for("200 quotes", |evs| {
        evs.iter().filter(|e| matches!(e, RecordedEvent::Quote(..))).count() >= 200
    });
    let quotes: Vec<(&String, &TickQuote)> = seen
        .iter()
        .filter_map(|e| match e {
            RecordedEvent::Quote(s, q) => Some((s, q)),
            _ => None,
        })
        .collect();
    assert_eq!(quotes.len(), 200);
    for (i, (symbol, q)) in quotes.iter().enumerate() {
        assert_eq!(symbol.as_str(), "BTCUSDT");
        assert_eq!(q.timestamp, i as i64, "out of order at {i}");
        assert_eq!(q.bid.to_bits(), (0.1 + (i as f64) * 0.1).to_bits(), "bid drifted at {i}");
        assert_eq!(q.ask.to_bits(), (0.1 + (i as f64) * 0.1 + 0.5).to_bits());
    }
    assert!(
        !seen.iter().any(|e| matches!(e, RecordedEvent::Status(..))),
        "a healthy stream disclosed a gap: {seen:?}"
    );

    client.unsubscribe(id);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !session.calls().iter().any(|c| c.starts_with("unsubscribe")) {
        assert!(Instant::now() < deadline, "the unsubscribe never reached the venue client");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn every_sink_verb_crosses_the_hop() {
    let h = Harness::start();
    let (mut client, rec) = new_client(&h, "binance");
    client.subscribe_bars("BTCUSDT", "1m").expect("bars");
    client.subscribe_quotes("BTCUSDT").expect("quotes");
    client.subscribe_trades("BTCUSDT").expect("trades");
    client.subscribe_book("BTCUSDT").expect("book");
    client.subscribe_depth("BTCUSDT").expect("depth");
    let sink = h.data.session(0).sink.clone();
    let s = "binance";
    let symbol_name = "BTCUSDT";

    sink.seed_bars(s, symbol_name, "1m", vec![bar(0), bar(60_000)]);
    sink.close_bar(s, symbol_name, "1m", bar(60_000));
    sink.forming_bar(s, symbol_name, "1m", bar(120_000));
    sink.quote(s, symbol_name, quote(5, 99.5));
    sink.mark_tick(s, symbol_name, 100.5, 7);
    sink.bar_close_tick(s, symbol_name, 100.25, 8);
    sink.l2_snapshot(
        s,
        symbol_name,
        0.5,
        vec![L2BookLevel::new(100.0, 1.0), L2BookLevel::new(99.5, 2.0)],
        vec![L2BookLevel::new(100.5, 1.5)],
        9,
    );
    sink.trade(
        s,
        symbol_name,
        TickTrade {
            timestamp: 10,
            local_timestamp: 11,
            price: 100.125,
            size: 0.3,
            is_buyer_maker: true,
            symbol: symbol_name.into(),
        },
    );
    let mut book = L2Book::new(0.5);
    book.apply_snapshot(5, &[L2BookLevel::new(100.0, 1.0)], &[L2BookLevel::new(100.5, 1.0)]);
    sink.book(s, symbol_name, Arc::new(book));
    sink.book_update(
        s,
        symbol_name,
        L2BookUpdate {
            timestamp: 12,
            local_timestamp: 13,
            sequence: 6,
            kind: L2BookUpdateKind::Delta,
            tick_size: 0.5,
            bids: vec![L2BookLevel::new(100.0, 0.0)],
            asks: vec![],
            symbol: symbol_name.into(),
        },
    );
    sink.stream_status(s, symbol_name, "1m", StreamStatus::GapStart { at_timestamp_ms: 14 });
    sink.data_delay(s, symbol_name, "quotes", StreamDelay::Delayed { lag_ms: Some(900_000) });

    let seen = rec.wait_for("all twelve verbs", |evs| {
        evs.iter()
            .filter(|e| !matches!(e, RecordedEvent::Status(_, _, StreamStatus::Live { .. })))
            .count()
            >= 12
    });
    let has = |f: &dyn Fn(&RecordedEvent) -> bool| seen.iter().any(f);
    assert!(has(&|e| matches!(e, RecordedEvent::Seed(_, i, 2) if i == "1m")));
    assert!(has(
        &|e| matches!(e, RecordedEvent::Quote(_, q) if q.bid.to_bits() == 99.5f64.to_bits() && q.local_timestamp == 6)
    ));
    assert!(has(&|e| matches!(e, RecordedEvent::Close(_, _, 60_000))));
    assert!(has(&|e| matches!(e, RecordedEvent::Forming(_, _, 120_000))));
    assert!(has(
        &|e| matches!(e, RecordedEvent::Mark(_, px, 7) if px.to_bits() == 100.5f64.to_bits())
    ));
    assert!(has(
        &|e| matches!(e, RecordedEvent::BarCloseTick(_, px, 8) if px.to_bits() == 100.25f64.to_bits())
    ));
    assert!(has(
        &|e| matches!(e, RecordedEvent::Depth(_, t, 2, 1, 9) if t.to_bits() == 0.5f64.to_bits())
    ));
    assert!(has(
        &|e| matches!(e, RecordedEvent::Trade(_, t) if t.price.to_bits() == 100.125f64.to_bits() && t.is_buyer_maker)
    ));
    assert!(has(&|e| matches!(e, RecordedEvent::Book(_, 2))));
    assert!(has(&|e| matches!(e, RecordedEvent::BookDelta(_, 6))));
    assert!(has(
        &|e| matches!(e, RecordedEvent::Status(_, w, StreamStatus::GapStart { at_timestamp_ms: 14 }) if w == "1m")
    ));
    assert!(has(&|e| {
        matches!(e, RecordedEvent::Delay(_, w, StreamDelay::Delayed { lag_ms: Some(900_000) }) if w == "quotes")
    }));
}

#[test]
fn a_seed_emitted_inside_subscribe_is_not_lost() {
    let h = Harness::start();
    h.data.set_default_hook(Arc::new(|sink: &dyn DataClientSink, verb: &str, symbol: &str| {
        if verb == "bars[1m]" {
            sink.seed_bars("binance", symbol, "1m", vec![bar(0), bar(60_000), bar(120_000)]);
            sink.stream_status(
                "binance",
                symbol,
                "1m",
                StreamStatus::Live { gap_started_timestamp_ms: None },
            );
        }
    }));
    let (mut client, rec) = new_client(&h, "binance");
    client.subscribe_bars("ETHUSDT", "1m").expect("bars");
    let seen =
        rec.wait_for("the seed", |evs| evs.iter().any(|e| matches!(e, RecordedEvent::Seed(..))));
    assert!(
        matches!(seen.first(), Some(RecordedEvent::Seed(s, i, 3)) if s == "ETHUSDT" && i == "1m"),
        "the first thing the sink saw must be the seed: {seen:?}"
    );
    let seen = rec.wait_for("the status after it", |evs| evs.len() >= 2);
    assert!(matches!(seen[1], RecordedEvent::Status(_, _, StreamStatus::Live { .. })), "{seen:?}");
}

#[test]
fn start_failures_keep_their_variant_and_their_words() {
    let h = Harness::start();
    h.data.refuse_all("BADSYM", Refusal::Subscribe("the venue said no: unknown symbol".into()));
    h.data.refuse_all("NODEPTH", Refusal::Unsupported("no live depth feed"));
    let (mut client, _rec) = new_client(&h, "binance");
    match client.subscribe_quotes("BADSYM") {
        Err(DataClientError::Subscribe(text)) => {
            assert_eq!(text, "the venue said no: unknown symbol");
        }
        other => panic!("{other:?}"),
    }
    match client.subscribe_depth("NODEPTH") {
        Err(DataClientError::Unsupported(text)) => assert_eq!(text, "no live depth feed"),
        other => panic!("{other:?}"),
    }
    // A blank spelling is the daemon's refusal, in the daemon's words, before the venue is asked.
    match client.subscribe_quotes("   ") {
        Err(DataClientError::Subscribe(text)) => assert!(text.contains("blank"), "{text}"),
        other => panic!("{other:?}"),
    }
    // A refusal leaves the session usable.
    assert!(client.subscribe_quotes("BTCUSDT").is_ok());
    assert!(
        !h.data.session(0).calls().iter().any(|c| c.contains("   ")),
        "the venue client was asked for a blank symbol"
    );
}

#[test]
fn the_two_phase_teardown_reaches_the_venue_client_and_a_close_is_answered_at_once() {
    let h = Harness::start();
    let (mut client, _rec) = new_client(&h, "binance");
    client.subscribe_quotes("BTCUSDT").expect("quotes");
    let session = h.data.session(0);
    client.begin_shutdown();
    let deadline = Instant::now() + Duration::from_secs(5);
    while session.begin_shutdowns.load(Ordering::SeqCst) == 0 {
        assert!(Instant::now() < deadline, "begin_shutdown never reached the venue client");
        std::thread::sleep(Duration::from_millis(10));
    }
    // The session is still usable after begin_shutdown, as an in-process client is.
    assert!(client.subscribe_trades("BTCUSDT").is_ok());
    let started = Instant::now();
    client.shutdown();
    assert!(
        started.elapsed() < Duration::from_millis(1500),
        "shutdown waited {:?}: the Bye(ClientClosed) did not arrive promptly",
        started.elapsed()
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while session.shutdowns.load(Ordering::SeqCst) == 0 {
        assert!(Instant::now() < deadline, "shutdown never reached the venue client");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(session.begin_shutdowns.load(Ordering::SeqCst) >= 1);
}

#[test]
fn a_venue_that_is_down_fails_the_subscribe_and_does_not_change_the_welcome() {
    let h = Harness::start();
    let before =
        raw_peer::Raw::open(h.addr, &harness::test_keys(), vike_hub_protocol::auth::Scope::Read)
            .features
            .clone();
    h.data.set_up(false);
    let (mut client, _rec) = new_client(&h, "binance");
    match client.subscribe_quotes("BTCUSDT") {
        Err(DataClientError::NotNow(text)) => {
            assert!(text.contains("the venue is down"), "the lane's words were lost: {text}");
        }
        other => panic!("{other:?}"),
    }
    let after =
        raw_peer::Raw::open(h.addr, &harness::test_keys(), vike_hub_protocol::auth::Scope::Read)
            .features
            .clone();
    assert_eq!(before, after, "the advertisement followed a venue's liveness");
    // The venue comes back: the same client recovers on its own.
    h.data.set_up(true);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if client.subscribe_quotes("BTCUSDT").is_ok() {
            break;
        }
        assert!(Instant::now() < deadline, "the client never recovered");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn a_symbol_the_feed_emits_unasked_is_bound_by_a_key_frame_and_delivered() {
    let h = Harness::start();
    let (mut client, rec) = new_client(&h, "binance");
    client.subscribe_quotes("BTCUSDT").expect("quotes");
    let sink = h.data.session(0).sink.clone();
    // A feed emitting a spelling it was not asked for: the client must be told the key first.
    sink.quote("binance", "btcusdt", quote(1, 5.0));
    sink.quote("binance", "BTCUSDT", quote(2, 6.0));
    let seen = rec.wait_for("both quotes", |evs| {
        evs.iter().filter(|e| matches!(e, RecordedEvent::Quote(..))).count() >= 2
    });
    let symbols: Vec<&str> = seen
        .iter()
        .filter_map(|e| match e {
            RecordedEvent::Quote(s, _) => Some(s.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(symbols, vec!["btcusdt", "BTCUSDT"]);
}

#[test]
fn the_holder_a_lane_is_given_identifies_the_connection_that_opened_the_session() {
    let h = Harness::start();
    let mut raw =
        raw_peer::Raw::open(h.addr, &harness::test_keys(), vike_hub_protocol::auth::Scope::Read);
    let (session, epoch, _heartbeat_ms) = raw.open_stream("binance");
    let holder = h.data.session(0).holder.clone();
    assert_eq!(holder.session, session, "the lane and the peer must name the session alike");
    assert_eq!(holder.scope, vike_hub_protocol::auth::Scope::Read);
    assert!(holder.peer.is_some());
    let key_id = holder.key_id.expect("a keyed daemon names the key");
    assert!(key_id.starts_with("nk-") && key_id.len() > 8, "{key_id}");
    assert_ne!(epoch, 0);
    // The fingerprint is not the key.
    assert!(!key_id.contains("observe-key-of-the-tests"));
}
