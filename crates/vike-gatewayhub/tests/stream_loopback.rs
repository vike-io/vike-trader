//! The `stream` class END TO END: this crate's real server, fake lanes behind it, and the client
//! crate's real `RemoteDataClient` as the peer.
//!
//! What it holds: every sink verb crosses and arrives bit for bit and in order; a subscription's
//! errors keep their variant and their words; a seed emitted from INSIDE `subscribe_*` is not lost
//! (the `SubOk`-before-any-frame rule seen from the consumer's side); the two-phase teardown reaches
//! the venue client; and a venue that is down is a failed subscribe, not a different `Welcome`.

#[path = "support/fake_lanes.rs"]
#[allow(dead_code, clippy::allow_attributes)]
mod fake_lanes;
#[path = "support/harness.rs"]
#[allow(dead_code, clippy::allow_attributes)]
mod harness;
#[path = "support/raw_peer.rs"]
#[allow(dead_code, clippy::allow_attributes)]
mod raw_peer;

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use vike_data::{DataClient, LiveDataError, LiveDataSink};
use vike_gatewayhub_client::opener::GatewayOpener;
use vike_market_stream::market::RemoteDataClient;
use vike_marketdata::{Bar, BookLevel, BookUpdate, BookUpdateKind, L2Book, QuoteTick, TradeTick};
use vike_model::{DataDelay, StreamStatus};

use fake_lanes::Refusal;
use harness::Harness;

/// One thing the sink saw.
#[derive(Debug, Clone, PartialEq)]
enum Ev {
    Seed(String, String, usize),
    Close(String, String, i64),
    Forming(String, String, i64),
    Mark(String, f64, i64),
    BarCloseTick(String, f64, i64),
    Depth(String, f64, usize, usize, i64),
    Quote(String, QuoteTick),
    Trade(String, TradeTick),
    Book(String, usize),
    BookDelta(String, u64),
    Status(String, String, StreamStatus),
    Delay(String, String, DataDelay),
}

#[derive(Default)]
struct Rec {
    events: Mutex<Vec<Ev>>,
}

impl Rec {
    fn push(&self, ev: Ev) {
        self.events.lock().unwrap_or_else(PoisonError::into_inner).push(ev);
    }

    fn snapshot(&self) -> Vec<Ev> {
        self.events.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    fn wait_for(&self, what: &str, pred: impl Fn(&[Ev]) -> bool) -> Vec<Ev> {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let now = self.snapshot();
            if pred(&now) {
                return now;
            }
            assert!(Instant::now() < deadline, "timed out waiting for {what}; saw {now:?}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

impl LiveDataSink for Rec {
    fn seed_bars(&self, _v: &str, symbol: &str, interval: &str, bars: Vec<Bar>) {
        self.push(Ev::Seed(symbol.into(), interval.into(), bars.len()));
    }
    fn close_bar(&self, _v: &str, symbol: &str, interval: &str, bar: Bar) {
        self.push(Ev::Close(symbol.into(), interval.into(), bar.ts));
    }
    fn forming_bar(&self, _v: &str, symbol: &str, interval: &str, bar: Bar) {
        self.push(Ev::Forming(symbol.into(), interval.into(), bar.ts));
    }
    fn mark_tick(&self, _v: &str, symbol: &str, px: f64, ts: i64) {
        self.push(Ev::Mark(symbol.into(), px, ts));
    }
    fn bar_close_tick(&self, _v: &str, symbol: &str, px: f64, ts: i64) {
        self.push(Ev::BarCloseTick(symbol.into(), px, ts));
    }
    fn l2_snapshot(
        &self,
        _v: &str,
        symbol: &str,
        tick_size: f64,
        bids: Vec<BookLevel>,
        asks: Vec<BookLevel>,
        ts: i64,
    ) {
        self.push(Ev::Depth(symbol.into(), tick_size, bids.len(), asks.len(), ts));
    }
    fn quote(&self, _v: &str, symbol: &str, quote: QuoteTick) {
        self.push(Ev::Quote(symbol.into(), quote));
    }
    fn trade(&self, _v: &str, symbol: &str, trade: TradeTick) {
        self.push(Ev::Trade(symbol.into(), trade));
    }
    fn book(&self, _v: &str, symbol: &str, book: Arc<L2Book>) {
        self.push(Ev::Book(symbol.into(), book.bid_levels() + book.ask_levels()));
    }
    fn book_update(&self, _v: &str, symbol: &str, update: BookUpdate) {
        self.push(Ev::BookDelta(symbol.into(), update.seq));
    }
    fn stream_status(&self, _v: &str, symbol: &str, stream: &str, status: StreamStatus) {
        self.push(Ev::Status(symbol.into(), stream.into(), status));
    }
    fn data_delay(&self, _v: &str, symbol: &str, stream: &str, delay: DataDelay) {
        self.push(Ev::Delay(symbol.into(), stream.into(), delay));
    }
}

fn bar(ts: i64) -> Bar {
    Bar {
        ts,
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

fn quote(ts: i64, bid: f64) -> QuoteTick {
    QuoteTick {
        ts,
        local_ts: ts + 1,
        bid,
        ask: bid + 0.5,
        bid_size: 1.25,
        ask_size: 2.5,
        symbol: "BTCUSDT".into(),
    }
}

fn new_client(h: &Harness, venue: &str) -> (RemoteDataClient, Arc<Rec>) {
    let rec = Arc::new(Rec::default());
    let client = RemoteDataClient::new(GatewayOpener::new(h.link()), venue, rec.clone())
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
        evs.iter().filter(|e| matches!(e, Ev::Quote(..))).count() >= 200
    });
    let quotes: Vec<(&String, &QuoteTick)> = seen
        .iter()
        .filter_map(|e| match e {
            Ev::Quote(s, q) => Some((s, q)),
            _ => None,
        })
        .collect();
    assert_eq!(quotes.len(), 200);
    for (i, (symbol, q)) in quotes.iter().enumerate() {
        assert_eq!(symbol.as_str(), "BTCUSDT");
        assert_eq!(q.ts, i as i64, "out of order at {i}");
        assert_eq!(q.bid.to_bits(), (0.1 + (i as f64) * 0.1).to_bits(), "bid drifted at {i}");
        assert_eq!(q.ask.to_bits(), (0.1 + (i as f64) * 0.1 + 0.5).to_bits());
    }
    assert!(
        !seen.iter().any(|e| matches!(e, Ev::Status(..))),
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
    let sym = "BTCUSDT";

    sink.seed_bars(s, sym, "1m", vec![bar(0), bar(60_000)]);
    sink.close_bar(s, sym, "1m", bar(60_000));
    sink.forming_bar(s, sym, "1m", bar(120_000));
    sink.quote(s, sym, quote(5, 99.5));
    sink.mark_tick(s, sym, 100.5, 7);
    sink.bar_close_tick(s, sym, 100.25, 8);
    sink.l2_snapshot(
        s,
        sym,
        0.5,
        vec![BookLevel::new(100.0, 1.0), BookLevel::new(99.5, 2.0)],
        vec![BookLevel::new(100.5, 1.5)],
        9,
    );
    sink.trade(
        s,
        sym,
        TradeTick {
            ts: 10,
            local_ts: 11,
            price: 100.125,
            size: 0.3,
            is_buyer_maker: true,
            symbol: sym.into(),
        },
    );
    let mut book = L2Book::new(0.5);
    book.apply_snapshot(5, &[BookLevel::new(100.0, 1.0)], &[BookLevel::new(100.5, 1.0)]);
    sink.book(s, sym, Arc::new(book));
    sink.book_update(
        s,
        sym,
        BookUpdate {
            ts: 12,
            local_ts: 13,
            seq: 6,
            kind: BookUpdateKind::Delta,
            tick_size: 0.5,
            bids: vec![BookLevel::new(100.0, 0.0)],
            asks: vec![],
            symbol: sym.into(),
        },
    );
    sink.stream_status(s, sym, "1m", StreamStatus::GapStart { at_ts_ms: 14 });
    sink.data_delay(s, sym, "quotes", DataDelay::Delayed { lag_ms: Some(900_000) });

    let seen = rec.wait_for("all twelve verbs", |evs| {
        evs.iter().filter(|e| !matches!(e, Ev::Status(_, _, StreamStatus::Live { .. }))).count()
            >= 12
    });
    let has = |f: &dyn Fn(&Ev) -> bool| seen.iter().any(f);
    assert!(has(&|e| matches!(e, Ev::Seed(_, i, 2) if i == "1m")));
    assert!(has(
        &|e| matches!(e, Ev::Quote(_, q) if q.bid.to_bits() == 99.5f64.to_bits() && q.local_ts == 6)
    ));
    assert!(has(&|e| matches!(e, Ev::Close(_, _, 60_000))));
    assert!(has(&|e| matches!(e, Ev::Forming(_, _, 120_000))));
    assert!(has(&|e| matches!(e, Ev::Mark(_, px, 7) if px.to_bits() == 100.5f64.to_bits())));
    assert!(has(
        &|e| matches!(e, Ev::BarCloseTick(_, px, 8) if px.to_bits() == 100.25f64.to_bits())
    ));
    assert!(has(&|e| matches!(e, Ev::Depth(_, t, 2, 1, 9) if t.to_bits() == 0.5f64.to_bits())));
    assert!(has(
        &|e| matches!(e, Ev::Trade(_, t) if t.price.to_bits() == 100.125f64.to_bits() && t.is_buyer_maker)
    ));
    assert!(has(&|e| matches!(e, Ev::Book(_, 2))));
    assert!(has(&|e| matches!(e, Ev::BookDelta(_, 6))));
    assert!(has(
        &|e| matches!(e, Ev::Status(_, w, StreamStatus::GapStart { at_ts_ms: 14 }) if w == "1m")
    ));
    assert!(has(&|e| {
        matches!(e, Ev::Delay(_, w, DataDelay::Delayed { lag_ms: Some(900_000) }) if w == "quotes")
    }));
}

#[test]
fn a_seed_emitted_inside_subscribe_is_not_lost() {
    let h = Harness::start();
    h.data.set_default_hook(Arc::new(|sink: &dyn LiveDataSink, verb: &str, symbol: &str| {
        if verb == "bars[1m]" {
            sink.seed_bars("binance", symbol, "1m", vec![bar(0), bar(60_000), bar(120_000)]);
            sink.stream_status(
                "binance",
                symbol,
                "1m",
                StreamStatus::Live { gap_started_ts_ms: None },
            );
        }
    }));
    let (mut client, rec) = new_client(&h, "binance");
    client.subscribe_bars("ETHUSDT", "1m").expect("bars");
    let seen = rec.wait_for("the seed", |evs| evs.iter().any(|e| matches!(e, Ev::Seed(..))));
    assert!(
        matches!(seen.first(), Some(Ev::Seed(s, i, 3)) if s == "ETHUSDT" && i == "1m"),
        "the first thing the sink saw must be the seed: {seen:?}"
    );
    let seen = rec.wait_for("the status after it", |evs| evs.len() >= 2);
    assert!(matches!(seen[1], Ev::Status(_, _, StreamStatus::Live { .. })), "{seen:?}");
}

#[test]
fn start_failures_keep_their_variant_and_their_words() {
    let h = Harness::start();
    h.data.refuse_all("BADSYM", Refusal::Subscribe("the venue said no: unknown symbol".into()));
    h.data.refuse_all("NODEPTH", Refusal::Unsupported("no live depth feed"));
    let (mut client, _rec) = new_client(&h, "binance");
    match client.subscribe_quotes("BADSYM") {
        Err(LiveDataError::Subscribe(text)) => {
            assert_eq!(text, "the venue said no: unknown symbol");
        }
        other => panic!("{other:?}"),
    }
    match client.subscribe_depth("NODEPTH") {
        Err(LiveDataError::Unsupported(text)) => assert_eq!(text, "no live depth feed"),
        other => panic!("{other:?}"),
    }
    // A blank spelling is the daemon's refusal, in the daemon's words, before the venue is asked.
    match client.subscribe_quotes("   ") {
        Err(LiveDataError::Subscribe(text)) => assert!(text.contains("blank"), "{text}"),
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
        raw_peer::Raw::open(h.addr, &harness::test_keys(), vike_hub_proto::auth::Scope::Read)
            .features
            .clone();
    h.data.set_up(false);
    let (mut client, _rec) = new_client(&h, "binance");
    match client.subscribe_quotes("BTCUSDT") {
        Err(LiveDataError::Subscribe(text)) => {
            assert!(text.contains("the venue is down"), "the lane's words were lost: {text}");
        }
        other => panic!("{other:?}"),
    }
    let after =
        raw_peer::Raw::open(h.addr, &harness::test_keys(), vike_hub_proto::auth::Scope::Read)
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
        evs.iter().filter(|e| matches!(e, Ev::Quote(..))).count() >= 2
    });
    let symbols: Vec<&str> = seen
        .iter()
        .filter_map(|e| match e {
            Ev::Quote(s, _) => Some(s.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(symbols, vec!["btcusdt", "BTCUSDT"]);
}

#[test]
fn the_holder_a_lane_is_given_identifies_the_connection_that_opened_the_session() {
    let h = Harness::start();
    let mut raw =
        raw_peer::Raw::open(h.addr, &harness::test_keys(), vike_hub_proto::auth::Scope::Read);
    let (session, epoch, _heartbeat_ms) = raw.open_stream("binance");
    let holder = h.data.session(0).holder.clone();
    assert_eq!(holder.session, session, "the lane and the peer must name the session alike");
    assert_eq!(holder.scope, vike_hub_proto::auth::Scope::Read);
    assert!(holder.peer.is_some());
    let key_id = holder.key_id.expect("a keyed daemon names the key");
    assert!(key_id.starts_with("nk-") && key_id.len() > 8, "{key_id}");
    assert_ne!(epoch, 0);
    // The fingerprint is not the key.
    assert!(!key_id.contains("observe-key-of-the-tests"));
}
