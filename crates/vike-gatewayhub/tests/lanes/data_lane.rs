//! The REAL data lane (`DataLaneBridge`) behind the REAL server, with the client crate's real
//! `DataClientStream` as the peer: what the datahub sees, since it builds its clients over the hop.
//!
//! The scripted tests put a double where a bridge would be (`DataLaneBridge::over` a table that hands out
//! `fake_lanes`' recording client), so what is under test is the lane, the server and the wire: every
//! `DataClientSink` verb arrives bit for bit and in order per key; a seed a venue client emits from
//! INSIDE `subscribe_*` comes after its `SubscribeOk`; one stream socket is one venue client whatever the
//! venue (the sharing column stays the datahub's); an unsubscribe of an id nobody issued is a no-op.
//! The REAL-arm tests use the production table and touch no network: aster's `subscribe_quotes`
//! refuses before any thread starts, and a session that subscribes to nothing dials nothing.
//!
//! Needs the `data` feature (it links every bridge); the default build compiles this file to nothing.
#![cfg(feature = "data")]

use crate::{fake_lanes, harness};
use vike_gatewayhub::testkit::events::{bar, delta, quote, trade};
use vike_gatewayhub::testkit::gateway::{ServedThread, fast_timings};
use vike_gatewayhub::testkit::raw_peer;

use std::collections::BTreeSet;
use std::net::{SocketAddr, TcpListener};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use vike_data::{
    DataClient, DataClientError, DataClientSink, DataClientSubscriptionId, require_live_verb,
};
use vike_gatewayhub::data::venues::DataLaneBridge;
use vike_gatewayhub::data::{DataLane, DataLaneHolder};
use vike_gatewayhub::registry::{RosterLane, owned, roster};
use vike_gatewayhub::server::accept::serve;
use vike_gatewayhub::server::lanes::ServerLanes;
use vike_gatewayhub::server::limits::ServerConfig;
use vike_gatewayhub_client::client::GatewayLink;
use vike_gatewayhub_client::opener::StreamOpenerGateway;
use vike_hub_protocol::auth::Scope;
use vike_hub_protocol::features::advertised_market_data_venues;
use vike_market_stream::market::DataClientStream;
use vike_market_stream::wire::{StreamByeReason, StreamDown, StreamUp, SubscriptionOperation};
use vike_marketdata::{Bar, L2Book, L2BookLevel, L2BookUpdate, TickQuote, TickTrade};
use vike_model::{StreamDelay, StreamStatus};
use vike_venues::DataClientVerb;

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

impl DataClientSink for Lines {
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
        bids: Vec<L2BookLevel>,
        asks: Vec<L2BookLevel>,
        ts: i64,
    ) {
        self.push(symbol, format!("l2_snapshot {venue} {tick_size:?} {bids:?} {asks:?} {ts}"));
    }
    fn quote(&self, venue: &str, symbol: &str, quote: TickQuote) {
        self.push(symbol, format!("quote {venue} {quote:?}"));
    }
    fn trade(&self, venue: &str, symbol: &str, trade: TickTrade) {
        self.push(symbol, format!("trade {venue} {trade:?}"));
    }
    fn book(&self, venue: &str, symbol: &str, book: Arc<L2Book>) {
        self.push(symbol, format!("book {venue} {book:?}"));
    }
    fn book_update(&self, venue: &str, symbol: &str, update: L2BookUpdate) {
        self.push(symbol, format!("book_update {venue} {update:?}"));
    }
    fn stream_status(&self, venue: &str, symbol: &str, stream: &str, status: StreamStatus) {
        self.push(symbol, format!("stream_status {venue} {stream} {status:?}"));
    }
    fn data_delay(&self, venue: &str, symbol: &str, stream: &str, delay: StreamDelay) {
        self.push(symbol, format!("data_delay {venue} {stream} {delay:?}"));
    }
}

/// Every one of the twelve `DataClientSink` verbs, for both symbols, the lossless ones several times,
/// each conflating verb once per key (a second emission of the same slot could legitimately be
/// superseded before it is sent, which is not what this test is about). Run once against a recording
/// sink and once against the lane's: the consumer must see what the first recorded.
fn script(sink: &dyn DataClientSink) {
    let venue = "binance";
    for (n, symbol) in SYMBOLS.into_iter().enumerate() {
        let salt = n as f64 * 1e-7;
        sink.seed_bars(
            venue,
            symbol,
            "1m",
            vec![bar(0, 0.1 + 0.2 + salt), bar(60_000, 1.0 / 3.0 + salt)],
        );
    }
    for i in 0..3_i64 {
        for (n, symbol) in SYMBOLS.into_iter().enumerate() {
            let x = 0.1 * (i as f64 + 1.0) + n as f64 * 1e-7;
            sink.quote(venue, symbol, quote(symbol, i, x));
            sink.trade(venue, symbol, trade(symbol, i, x));
            sink.close_bar(venue, symbol, "1m", bar(60_000 * (i + 1), x));
            sink.book_update(venue, symbol, delta(symbol, i as u64 + 1, x));
        }
    }
    for (n, symbol) in SYMBOLS.into_iter().enumerate() {
        let x = 100.0 + n as f64 * 1e-7;
        sink.forming_bar(venue, symbol, "1m", bar(240_000, x));
        sink.mark_tick(venue, symbol, x + 0.5, 7);
        sink.bar_close_tick(venue, symbol, f64::MIN_POSITIVE + x, 8);
        sink.l2_snapshot(
            venue,
            symbol,
            0.5,
            vec![L2BookLevel::new(x, 1.0), L2BookLevel::new(x - 0.5, 2.0)],
            vec![L2BookLevel::new(x + 0.5, 1.5)],
            9,
        );
        let mut book = L2Book::new(0.5);
        book.apply_snapshot(5, &[L2BookLevel::new(x, 1.0)], &[L2BookLevel::new(x + 0.5, 1.0)]);
        sink.book(venue, symbol, Arc::new(book));
        sink.stream_status(venue, symbol, "1m", StreamStatus::GapStart { at_timestamp_ms: 14 });
        sink.stream_status(
            venue,
            symbol,
            "1m",
            StreamStatus::Live { gap_started_timestamp_ms: Some(14) },
        );
        sink.data_delay(venue, symbol, "quotes", StreamDelay::Delayed { lag_ms: Some(900_000) });
    }
}

/// A server on an ephemeral loopback port whose data lane is `lane`.
struct Daemon {
    _served: ServedThread,
    addr: SocketAddr,
}

impl Daemon {
    fn start(lane: Arc<DataLaneBridge>) -> Daemon {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().expect("the assigned port");
        let lanes = ServerLanes { data: lane, rest: FakeRest::new(harness::registry_facts()) };
        let served = ServedThread::spawn("test-gwh-data-serve", move |stop| {
            serve(listener, harness::test_keys(), lanes, ServerConfig::PRODUCTION, stop)
        });
        Daemon { _served: served, addr }
    }

    /// The REAL lane over the production table (no settings rows of its own, so the release's
    /// addresses, as a root with no store hands them): every arm of `build_client`.
    fn real() -> Daemon {
        let settings = vike_secrets::venue_setting::seeded_venue_settings();
        Daemon::start(Arc::new(DataLaneBridge::new(settings)))
    }

    /// The real lane over a table that hands out the recording client of `fake_lanes` where a
    /// bridge would be: `(daemon, the double's sessions)`.
    fn scripted() -> (Daemon, Arc<FakeData>) {
        let fake = FakeData::new(roster(RosterLane::Stream));
        let table = Arc::clone(&fake);
        let lane = DataLaneBridge::over(Box::new(
            move |venue: &str,
                  sink: Arc<dyn DataClientSink>|
                  -> Result<Box<dyn DataClient + Send>, String> {
                let holder =
                    DataLaneHolder { session: 0, peer: None, scope: Scope::Read, key_id: None };
                table.open(venue, &holder, sink)
            },
        ));
        (Daemon::start(Arc::new(lane)), fake)
    }

    fn link(&self) -> GatewayLink {
        GatewayLink::new(self.addr.to_string(), harness::test_keys()).with_timings(fast_timings())
    }

    fn raw(&self) -> Raw {
        Raw::open(self.addr, &harness::test_keys(), Scope::Read)
    }

    /// A consumer: the client crate's real `DataClientStream` for `venue`, and what it received.
    fn consumer(&self, venue: &str) -> (DataClientStream, Arc<Lines>) {
        let lines = Arc::new(Lines::default());
        let client =
            DataClientStream::new(StreamOpenerGateway::new(self.link()), venue, lines.clone())
                .expect("a reader");
        (client, lines)
    }
}

#[test]
fn all_twelve_verbs_arrive_bit_exact_and_in_order_per_key_through_the_lane() {
    let (daemon, fake) = Daemon::scripted();
    let (mut client, got) = daemon.consumer("binance");
    for symbol in SYMBOLS {
        client.subscribe_bars(symbol, "1m").expect("bars");
        client.subscribe_quotes(symbol).expect("quotes");
        client.subscribe_trades(symbol).expect("trades");
        client.subscribe_book(symbol).expect("book");
        client.subscribe_depth(symbol).expect("depth");
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
    assert_eq!(verbs.len(), 12, "the script must exercise every DataClientSink verb: {verbs:?}");

    script(session.sink.as_ref());
    got.wait_for_total(expected.total());
    // A frame the daemon invented (a gap marker, a duplicate) would arrive after the last real one.
    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(got.total(), expected.total(), "extra events arrived: {:?}", got.all());
    for symbol in SYMBOLS {
        let (want, have) = (expected.of(symbol), got.of(symbol));
        assert_eq!(have.len(), want.len(), "{symbol}: {have:?}");
        for (i, (w, h)) in want.iter().zip(&have).enumerate() {
            assert_eq!(h, w, "{symbol}: event {i} differs from what the venue client emitted");
        }
    }
}

#[test]
fn a_seed_a_venue_client_emits_inside_subscribe_arrives_after_its_sub_ok() {
    let (daemon, fake) = Daemon::scripted();
    fake.set_default_hook(Arc::new(|sink: &dyn DataClientSink, verb: &str, symbol: &str| {
        if verb == "bars[1m]" {
            sink.seed_bars("binance", symbol, "1m", vec![bar(0, 1.0), bar(60_000, 2.0)]);
            sink.stream_status(
                "binance",
                symbol,
                "1m",
                StreamStatus::Live { gap_started_timestamp_ms: None },
            );
        }
    }));

    // On the wire: the SubscribeOk is the first frame, and the seed that was emitted before the venue
    // call returned follows it under the same key.
    let mut raw = daemon.raw();
    raw.open_stream("binance");
    raw.up(&StreamUp::Subscribe {
        sid: 1,
        op: SubscriptionOperation::Bars { symbol: "BTCUSDT".into(), interval: "1m".into() },
        class: None,
    });
    let k = match raw.down() {
        StreamDown::SubscribeOk { sid: 1, k } => k,
        other => panic!("the first frame must be the SubscribeOk, not {other:?}"),
    };
    match raw.down() {
        StreamDown::Seed { k: seed_k, sequence: 1, bars, .. } => {
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
    let (mut polymarket, polymarket_got) = daemon.consumer("polymarket");
    polymarket.subscribe_book("TOKEN").expect("polymarket");

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
    assert_eq!(polymarket_got.total(), 0, "{:?}", polymarket_got.all());
    assert_ne!(first_got.of("BTCUSDT"), second_got.of("BTCUSDT"), "the two feeds were one");
}

#[test]
fn an_unsupported_lane_answers_unsupported_with_the_text_require_live_verb_gives() {
    // The REAL aster arm: its `subscribe_quotes` and `subscribe_book` refuse through
    // `require_live_verb` before any thread starts, so nothing is dialled.
    let daemon = Daemon::real();
    let (mut client, _got) = daemon.consumer("aster");
    for (verb, result) in [
        (DataClientVerb::Quotes, client.subscribe_quotes("BTCUSDT")),
        (DataClientVerb::Book, client.subscribe_book("BTCUSDT")),
    ] {
        let Err(DataClientError::Unsupported(want)) = require_live_verb("aster", verb) else {
            panic!("aster serves {verb:?}: this test needs a venue that does not");
        };
        match result {
            Err(DataClientError::Unsupported(text)) => assert_eq!(text, want),
            other => panic!("{verb:?}: expected Unsupported({want:?}), got {other:?}"),
        }
    }
    // The refusal leaves the session usable, and an id nobody issued is a no-op for the venue.
    client.unsubscribe(DataClientSubscriptionId(12_345));
    client.shutdown();
}

#[test]
fn an_unsubscribe_of_an_id_nobody_issued_is_a_no_op_and_a_known_one_reaches_the_venue_once() {
    let (daemon, fake) = Daemon::scripted();
    let (mut client, _got) = daemon.consumer("binance");
    let id = client.subscribe_quotes("BTCUSDT").expect("quotes");
    let session = fake.session(0);

    client.unsubscribe(DataClientSubscriptionId(9_999)); // never issued
    client.unsubscribe(id);
    client.unsubscribe(id); // already stopped

    // And the server's side of the same rule, on the wire: an `Unsubscribe` of a `sid` the session does
    // not hold changes nothing, and the session answers a ping afterwards.
    let mut raw = daemon.raw();
    raw.open_stream("binance");
    raw.up(&StreamUp::Unsubscribe { sid: 42 });
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
    assert_eq!(advertised_market_data_venues(&raw.features), owned(RosterLane::Stream));
}

#[test]
fn the_real_lane_opens_and_closes_a_session_for_every_rostered_venue_dialling_nothing() {
    let daemon = Daemon::real();
    for venue in roster(RosterLane::Stream) {
        let mut raw = daemon.raw();
        raw.open_stream(venue);
        raw.up(&StreamUp::Close);
        let frames = raw.down_until(|f| matches!(f, StreamDown::Bye(_)));
        assert!(
            matches!(frames.last(), Some(StreamDown::Bye(StreamByeReason::ClientClosed))),
            "{venue}: {frames:?}"
        );
    }
}
