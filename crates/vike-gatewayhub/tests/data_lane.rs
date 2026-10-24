//! The REAL data lane (`BridgeData`) behind the REAL server, with the client crate's real
//! `RemoteDataClient` as the peer: what the datahub will see once it is rewired onto the hop.
//!
//! The scripted tests put a double where a bridge would be (`BridgeData::over` a table that hands out
//! `fake_lanes`' recording client), so what is under test is the lane, the server and the wire: every
//! `LiveDataSink` verb arrives bit for bit and in order per key; a seed a venue client emits from
//! INSIDE `subscribe_*` comes after its `SubOk`; one stream socket is one venue client whatever the
//! venue (the sharing column stays the datahub's); an unsubscribe of an id nobody issued is a no-op.
//! The REAL-arm tests use the production table and touch no network: aster's `subscribe_quotes`
//! refuses before any thread starts, and a session that subscribes to nothing dials nothing.
//!
//! Needs the `data` feature (it links every bridge); the default build compiles this file to nothing.
#![cfg(feature = "data")]

#[path = "support/fake_lanes.rs"]
#[allow(dead_code, clippy::allow_attributes)]
mod fake_lanes;
#[path = "support/harness.rs"]
#[allow(dead_code, clippy::allow_attributes)]
mod harness;
#[path = "support/raw_peer.rs"]
#[allow(dead_code, clippy::allow_attributes)]
mod raw_peer;

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::net::{SocketAddr, TcpListener};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use vike_data::{DataClient, LiveDataError, LiveDataSink, SubscriptionId, require_live_verb};
use vike_gatewayhub::data::venues::BridgeData;
use vike_gatewayhub::data::{DataLane, Holder};
use vike_gatewayhub::registry::{Lane, owned, roster};
use vike_gatewayhub::server::accept::serve;
use vike_gatewayhub::server::lanes::Lanes;
use vike_gatewayhub::server::limits::ServerConfig;
use vike_gatewayhub_client::client::GatewayLink;
use vike_gatewayhub_client::opener::GatewayOpener;
use vike_gatewayhub_client::proto::advertised_md_venues;
use vike_hub_proto::auth::Scope;
use vike_market_stream::market::RemoteDataClient;
use vike_market_stream::wire::{ByeReason, StreamDown, StreamUp, SubOp};
use vike_marketdata::{Bar, BookLevel, BookUpdate, BookUpdateKind, L2Book, QuoteTick, TradeTick};
use vike_model::{DataDelay, StreamStatus};
use vike_venues::LiveVerb;

use fake_lanes::{FakeData, FakeRest};
use raw_peer::Raw;

const SYMBOLS: [&str; 2] = ["BTCUSDT", "ETHUSDT"];

/// A sink that writes each event as one line under its symbol. The SAME type records what the venue
/// client emitted and what the consumer received, so the two are compared in one spelling: `{:?}` of
/// an `f64` is the shortest text that parses back to the same bits, so equal lines are bit-equal
/// numbers (`-0.0` and the smallest normal included).
#[derive(Default)]
struct Lines {
    lines: Mutex<Vec<(String, String)>>,
}

impl Lines {
    fn push(&self, symbol: &str, line: String) {
        self.lines.lock().unwrap_or_else(PoisonError::into_inner).push((symbol.to_string(), line));
    }

    fn of(&self, symbol: &str) -> Vec<String> {
        let all = self.lines.lock().unwrap_or_else(PoisonError::into_inner);
        all.iter().filter(|(s, _)| s == symbol).map(|(_, l)| l.clone()).collect()
    }

    fn total(&self) -> usize {
        self.lines.lock().unwrap_or_else(PoisonError::into_inner).len()
    }

    fn all(&self) -> Vec<(String, String)> {
        self.lines.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    fn wait_for_total(&self, want: usize) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.total() < want {
            assert!(
                Instant::now() < deadline,
                "timed out: {} of {want} events arrived; saw {:?}",
                self.total(),
                self.all()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

impl LiveDataSink for Lines {
    fn seed_bars(&self, venue: &str, symbol: &str, interval: &str, bars: Vec<Bar>) {
        self.push(symbol, format!("seed_bars {venue} {interval} {bars:?}"));
    }
    fn close_bar(&self, venue: &str, symbol: &str, interval: &str, bar: Bar) {
        self.push(symbol, format!("close_bar {venue} {interval} {bar:?}"));
    }
    fn forming_bar(&self, venue: &str, symbol: &str, interval: &str, bar: Bar) {
        self.push(symbol, format!("forming_bar {venue} {interval} {bar:?}"));
    }
    fn mark_tick(&self, venue: &str, symbol: &str, px: f64, ts: i64) {
        self.push(symbol, format!("mark_tick {venue} {px:?} {ts}"));
    }
    fn bar_close_tick(&self, venue: &str, symbol: &str, px: f64, ts: i64) {
        self.push(symbol, format!("bar_close_tick {venue} {px:?} {ts}"));
    }
    fn l2_snapshot(
        &self,
        venue: &str,
        symbol: &str,
        tick_size: f64,
        bids: Vec<BookLevel>,
        asks: Vec<BookLevel>,
        ts: i64,
    ) {
        self.push(symbol, format!("l2_snapshot {venue} {tick_size:?} {bids:?} {asks:?} {ts}"));
    }
    fn quote(&self, venue: &str, symbol: &str, quote: QuoteTick) {
        self.push(symbol, format!("quote {venue} {quote:?}"));
    }
    fn trade(&self, venue: &str, symbol: &str, trade: TradeTick) {
        self.push(symbol, format!("trade {venue} {trade:?}"));
    }
    fn book(&self, venue: &str, symbol: &str, book: Arc<L2Book>) {
        self.push(symbol, format!("book {venue} {book:?}"));
    }
    fn book_update(&self, venue: &str, symbol: &str, update: BookUpdate) {
        self.push(symbol, format!("book_update {venue} {update:?}"));
    }
    fn stream_status(&self, venue: &str, symbol: &str, stream: &str, status: StreamStatus) {
        self.push(symbol, format!("stream_status {venue} {stream} {status:?}"));
    }
    fn data_delay(&self, venue: &str, symbol: &str, stream: &str, delay: DataDelay) {
        self.push(symbol, format!("data_delay {venue} {stream} {delay:?}"));
    }
}

/// A bar whose every float has a non-trivial bit pattern.
fn bar(ts: i64, x: f64) -> Bar {
    Bar {
        ts,
        open: x,
        high: x + 1.5,
        low: -0.0,
        close: x / 3.0,
        volume: f64::MIN_POSITIVE,
        funding: Some(1e-4),
        bid: Some(x - 0.25),
        ask: None,
        symbol: None,
    }
}

fn quote(symbol: &str, ts: i64, x: f64) -> QuoteTick {
    QuoteTick {
        ts,
        local_ts: ts + 1,
        bid: x,
        ask: x + 0.5,
        bid_size: 1.25,
        ask_size: 2.5,
        symbol: symbol.into(),
    }
}

fn trade(symbol: &str, ts: i64, x: f64) -> TradeTick {
    TradeTick {
        ts,
        local_ts: ts + 1,
        price: x,
        size: 0.3,
        is_buyer_maker: ts % 2 == 0,
        symbol: symbol.into(),
    }
}

fn delta(symbol: &str, seq: u64, x: f64) -> BookUpdate {
    BookUpdate {
        ts: seq as i64,
        local_ts: seq as i64 + 1,
        seq,
        kind: BookUpdateKind::Delta,
        tick_size: 0.5,
        bids: vec![BookLevel::new(x, 0.0)],
        asks: vec![BookLevel::new(x + 0.5, 1.5)],
        symbol: symbol.into(),
    }
}

/// Every one of the twelve `LiveDataSink` verbs, for both symbols, the lossless ones several times,
/// each conflating verb once per key (a second emission of the same slot could legitimately be
/// superseded before it is sent, which is not what this test is about). Run once against a recording
/// sink and once against the lane's: the consumer must see what the first recorded.
fn script(sink: &dyn LiveDataSink) {
    let venue = "binance";
    for (n, sym) in SYMBOLS.into_iter().enumerate() {
        let salt = n as f64 * 1e-7;
        sink.seed_bars(
            venue,
            sym,
            "1m",
            vec![bar(0, 0.1 + 0.2 + salt), bar(60_000, 1.0 / 3.0 + salt)],
        );
    }
    for i in 0..3_i64 {
        for (n, sym) in SYMBOLS.into_iter().enumerate() {
            let x = 0.1 * (i as f64 + 1.0) + n as f64 * 1e-7;
            sink.quote(venue, sym, quote(sym, i, x));
            sink.trade(venue, sym, trade(sym, i, x));
            sink.close_bar(venue, sym, "1m", bar(60_000 * (i + 1), x));
            sink.book_update(venue, sym, delta(sym, i as u64 + 1, x));
        }
    }
    for (n, sym) in SYMBOLS.into_iter().enumerate() {
        let x = 100.0 + n as f64 * 1e-7;
        sink.forming_bar(venue, sym, "1m", bar(240_000, x));
        sink.mark_tick(venue, sym, x + 0.5, 7);
        sink.bar_close_tick(venue, sym, f64::MIN_POSITIVE + x, 8);
        sink.l2_snapshot(
            venue,
            sym,
            0.5,
            vec![BookLevel::new(x, 1.0), BookLevel::new(x - 0.5, 2.0)],
            vec![BookLevel::new(x + 0.5, 1.5)],
            9,
        );
        let mut book = L2Book::new(0.5);
        book.apply_snapshot(5, &[BookLevel::new(x, 1.0)], &[BookLevel::new(x + 0.5, 1.0)]);
        sink.book(venue, sym, Arc::new(book));
        sink.stream_status(venue, sym, "1m", StreamStatus::GapStart { at_ts_ms: 14 });
        sink.stream_status(venue, sym, "1m", StreamStatus::Live { gap_started_ts_ms: Some(14) });
        sink.data_delay(venue, sym, "quotes", DataDelay::Delayed { lag_ms: Some(900_000) });
    }
}

/// A server on an ephemeral loopback port whose data lane is `lane`.
struct Daemon {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<io::Result<()>>>,
}

impl Daemon {
    fn start(lane: Arc<BridgeData>) -> Daemon {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().expect("the assigned port");
        let stop = Arc::new(AtomicBool::new(false));
        let lanes = Lanes { data: lane, rest: FakeRest::new(harness::registry_facts()) };
        let thread = {
            let stop = Arc::clone(&stop);
            std::thread::Builder::new()
                .name("test-gwh-data-serve".into())
                .spawn(move || {
                    serve(listener, harness::test_keys(), lanes, ServerConfig::PRODUCTION, stop)
                })
                .expect("spawn the server")
        };
        Daemon { addr, stop, thread: Some(thread) }
    }

    /// The REAL lane over the production table (no settings rows): every arm of `build_client`.
    fn real() -> Daemon {
        Daemon::start(Arc::new(BridgeData::new(BTreeMap::new())))
    }

    /// The real lane over a table that hands out the recording client of `fake_lanes` where a
    /// bridge would be: `(daemon, the double's sessions)`.
    fn scripted() -> (Daemon, Arc<FakeData>) {
        let fake = FakeData::new(roster(Lane::Stream));
        let table = Arc::clone(&fake);
        let lane = BridgeData::over(Box::new(
            move |venue: &str,
                  sink: Arc<dyn LiveDataSink>|
                  -> Result<Box<dyn DataClient + Send>, String> {
                let holder = Holder { session: 0, peer: None, scope: Scope::Read, key_id: None };
                table.open(venue, &holder, sink)
            },
        ));
        (Daemon::start(Arc::new(lane)), fake)
    }

    fn link(&self) -> GatewayLink {
        GatewayLink::new(self.addr.to_string(), harness::test_keys())
            .with_timings(harness::fast_timings())
    }

    fn raw(&self) -> Raw {
        Raw::open(self.addr, &harness::test_keys(), Scope::Read)
    }

    /// A consumer: the client crate's real `RemoteDataClient` for `venue`, and what it received.
    fn consumer(&self, venue: &str) -> (RemoteDataClient, Arc<Lines>) {
        let lines = Arc::new(Lines::default());
        let client = RemoteDataClient::new(GatewayOpener::new(self.link()), venue, lines.clone())
            .expect("a reader");
        (client, lines)
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[test]
fn all_twelve_verbs_arrive_bit_exact_and_in_order_per_key_through_the_lane() {
    let (daemon, fake) = Daemon::scripted();
    let (mut client, got) = daemon.consumer("binance");
    for sym in SYMBOLS {
        client.subscribe_bars(sym, "1m").expect("bars");
        client.subscribe_quotes(sym).expect("quotes");
        client.subscribe_trades(sym).expect("trades");
        client.subscribe_book(sym).expect("book");
        client.subscribe_depth(sym).expect("depth");
    }
    let session = fake.session(0);
    assert_eq!(session.venue, "binance");

    let expected = Lines::default();
    script(&expected);
    let verbs: BTreeSet<String> = expected
        .all()
        .into_iter()
        .filter_map(|(_, line)| line.split(' ').next().map(str::to_string))
        .collect();
    assert_eq!(verbs.len(), 12, "the script must exercise every LiveDataSink verb: {verbs:?}");

    script(session.sink.as_ref());
    got.wait_for_total(expected.total());
    // A frame the daemon invented (a gap marker, a duplicate) would arrive after the last real one.
    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(got.total(), expected.total(), "extra events arrived: {:?}", got.all());
    for sym in SYMBOLS {
        let (want, have) = (expected.of(sym), got.of(sym));
        assert_eq!(have.len(), want.len(), "{sym}: {have:?}");
        for (i, (w, h)) in want.iter().zip(&have).enumerate() {
            assert_eq!(h, w, "{sym}: event {i} differs from what the venue client emitted");
        }
    }
}

#[test]
fn a_seed_a_venue_client_emits_inside_subscribe_arrives_after_its_sub_ok() {
    let (daemon, fake) = Daemon::scripted();
    fake.set_default_hook(Arc::new(|sink: &dyn LiveDataSink, verb: &str, symbol: &str| {
        if verb == "bars[1m]" {
            sink.seed_bars("binance", symbol, "1m", vec![bar(0, 1.0), bar(60_000, 2.0)]);
            sink.stream_status(
                "binance",
                symbol,
                "1m",
                StreamStatus::Live { gap_started_ts_ms: None },
            );
        }
    }));

    // On the wire: the SubOk is the first frame, and the seed that was emitted before the venue
    // call returned follows it under the same key.
    let mut raw = daemon.raw();
    raw.open_stream("binance");
    raw.up(&StreamUp::Sub {
        sid: 1,
        op: SubOp::Bars { symbol: "BTCUSDT".into(), interval: "1m".into() },
        class: None,
    });
    let k = match raw.down() {
        StreamDown::SubOk { sid: 1, k } => k,
        other => panic!("the first frame must be the SubOk, not {other:?}"),
    };
    match raw.down() {
        StreamDown::Seed { k: seed_k, seq: 1, bars, .. } => {
            assert_eq!(seed_k, k);
            assert_eq!(bars.len(), 2);
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(raw.down(), StreamDown::Status { k: status_k, .. } if status_k == k));

    // Through the client: the consumer's first event is the seed, not nothing.
    let (mut client, got) = daemon.consumer("binance");
    client.subscribe_bars("ETHUSDT", "1m").expect("bars");
    got.wait_for_total(2);
    let lines = got.of("ETHUSDT");
    assert!(lines[0].starts_with("seed_bars binance 1m "), "{lines:?}");
    assert!(lines[1].starts_with("stream_status binance 1m "), "{lines:?}");
}

#[test]
fn one_stream_socket_is_one_venue_client_whatever_the_venue() {
    let (daemon, fake) = Daemon::scripted();
    // Two consumers of the same key on the same venue, and one of a venue the datahub declares
    // `PerHolder`: the lane never merges them. Sharing is the datahub's column, applied before the
    // hop; the daemon builds one client per socket.
    let (mut first, first_got) = daemon.consumer("binance");
    first.subscribe_quotes("BTCUSDT").expect("first");
    let (mut second, second_got) = daemon.consumer("binance");
    second.subscribe_quotes("BTCUSDT").expect("second");
    let (mut poly, poly_got) = daemon.consumer("polymarket");
    poly.subscribe_book("TOKEN").expect("polymarket");

    assert_eq!(fake.opened(), 3, "the lane built a different number of clients than sockets");
    let venues: Vec<String> = (0..3).map(|n| fake.session(n).venue.clone()).collect();
    assert_eq!(venues, ["binance", "binance", "polymarket"]);

    // A frame from one venue client reaches ITS consumer and no other.
    fake.session(0).sink.quote("binance", "BTCUSDT", quote("BTCUSDT", 1, 5.0));
    first_got.wait_for_total(1);
    fake.session(1).sink.quote("binance", "BTCUSDT", quote("BTCUSDT", 2, 6.0));
    second_got.wait_for_total(1);
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(first_got.total(), 1, "{:?}", first_got.all());
    assert_eq!(second_got.total(), 1, "{:?}", second_got.all());
    assert_eq!(poly_got.total(), 0, "{:?}", poly_got.all());
    assert_ne!(first_got.of("BTCUSDT"), second_got.of("BTCUSDT"), "the two feeds were one");
}

#[test]
fn an_unsupported_lane_answers_unsupported_with_the_text_require_live_verb_gives() {
    // The REAL aster arm: its `subscribe_quotes` and `subscribe_book` refuse through
    // `require_live_verb` before any thread starts, so nothing is dialled.
    let daemon = Daemon::real();
    let (mut client, _got) = daemon.consumer("aster");
    for (verb, result) in [
        (LiveVerb::Quotes, client.subscribe_quotes("BTCUSDT")),
        (LiveVerb::Book, client.subscribe_book("BTCUSDT")),
    ] {
        let Err(LiveDataError::Unsupported(want)) = require_live_verb("aster", verb) else {
            panic!("aster serves {verb:?}: this test needs a venue that does not");
        };
        match result {
            Err(LiveDataError::Unsupported(text)) => assert_eq!(text, want),
            other => panic!("{verb:?}: expected Unsupported({want:?}), got {other:?}"),
        }
    }
    // The refusal leaves the session usable, and an id nobody issued is a no-op for the venue.
    client.unsubscribe(SubscriptionId(12_345));
    client.shutdown();
}

#[test]
fn an_unsubscribe_of_an_id_nobody_issued_is_a_no_op_and_a_known_one_reaches_the_venue_once() {
    let (daemon, fake) = Daemon::scripted();
    let (mut client, _got) = daemon.consumer("binance");
    let id = client.subscribe_quotes("BTCUSDT").expect("quotes");
    let session = fake.session(0);

    client.unsubscribe(SubscriptionId(9_999)); // never issued
    client.unsubscribe(id);
    client.unsubscribe(id); // already stopped

    // And the server's side of the same rule, on the wire: an `Unsub` of a `sid` the session does
    // not hold changes nothing, and the session answers a ping afterwards.
    let mut raw = daemon.raw();
    raw.open_stream("binance");
    raw.up(&StreamUp::Unsub { sid: 42 });
    raw.up(&StreamUp::Ping);
    assert!(matches!(raw.down(), StreamDown::Heartbeat));

    let deadline = Instant::now() + Duration::from_secs(5);
    while !session.calls().iter().any(|c| c.starts_with("unsubscribe")) {
        assert!(Instant::now() < deadline, "the unsubscribe never reached the venue client");
        std::thread::sleep(Duration::from_millis(10));
    }
    std::thread::sleep(Duration::from_millis(150));
    let unsubscribes: Vec<String> =
        session.calls().into_iter().filter(|c| c.starts_with("unsubscribe")).collect();
    assert_eq!(unsubscribes.len(), 1, "{:?}", session.calls());
    assert!(
        !fake.session(1).calls().iter().any(|c| c.starts_with("unsubscribe")),
        "the wire-level Unsub of an unknown sid reached the venue client"
    );
}

#[test]
fn the_welcome_advertises_the_registrys_stream_roster_in_order() {
    let daemon = Daemon::real();
    let raw = daemon.raw();
    assert_eq!(advertised_md_venues(&raw.features), owned(Lane::Stream));
}

#[test]
fn the_real_lane_opens_and_closes_a_session_for_every_rostered_venue_dialling_nothing() {
    let daemon = Daemon::real();
    for venue in roster(Lane::Stream) {
        let mut raw = daemon.raw();
        raw.open_stream(venue);
        raw.up(&StreamUp::Close);
        let frames = raw.down_until(|f| matches!(f, StreamDown::Bye(_)));
        assert!(
            matches!(frames.last(), Some(StreamDown::Bye(ByeReason::ClientClosed))),
            "{venue}: {frames:?}"
        );
    }
}
