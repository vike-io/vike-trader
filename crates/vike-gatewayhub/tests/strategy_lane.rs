//! The STRATEGY LANE's additions to the data lane (wave 2, W10), behind the REAL server with the client
//! crate's real opener and the tube's real `RemoteDataClient` as the peer: the hosted CEX tick pump (the
//! pump twin, R14, over scripted frames), the network a session names, and the credentialed streams.
//!
//! The venue is a double everywhere: the pump is a `TickSender` the test plays frames through (the shape
//! of `spawn_*_market_data`), the network and credentialed builders hand out `fake_lanes`' recording
//! client, so nothing dials. What is under test is the lane, the server and the wire.
//!
//! Needs the `data` feature; the default build compiles this file to nothing.
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

use std::collections::BTreeMap;
use std::io;
use std::net::{SocketAddr, TcpListener};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use vike_data::{DataClient, LiveDataSink, RecordingSink, SinkCall};
use vike_exec::{BookUpdate as PumpBook, QuoteUpdate, StreamStatusUpdate, TickSender, TradeUpdate};
use vike_gatewayhub::data::accounts::AccountSource;
use vike_gatewayhub::data::credentialed::CredentialedLane;
use vike_gatewayhub::data::pump::{PumpHandle, PumpHub, PumpSpawner};
use vike_gatewayhub::data::venues::BridgeData;
use vike_gatewayhub::data::{DataLane, Holder, PumpLease, PumpSink};
use vike_gatewayhub::registry::{Lane, owned};
use vike_gatewayhub::server::accept::serve;
use vike_gatewayhub::server::lanes::Lanes;
use vike_gatewayhub::server::limits::ServerConfig;
use vike_gatewayhub_client::client::GatewayLink;
use vike_gatewayhub_client::opener::GatewayOpener;
use vike_gatewayhub_client::proto::{Request, Response};
use vike_hub_proto::auth::Scope;
use vike_market_stream::market::RemoteDataClient;
use vike_market_stream::wire::{StreamDown, StreamUp, SubErrKind, SubOp};
use vike_marketdata::{BookLevel, BookUpdateKind, L2Book, QuoteTick, TradeTick};
use vike_model::{FeedStatus, StreamStatus};
use vike_secrets::account_directory::AccountDirectory;

use fake_lanes::{FakeData, FakeRest};
use raw_peer::Raw;

// ---- Fixtures --------------------------------------------------------------------------------------

/// A server on an ephemeral loopback port whose data lane is `lane`.
struct Daemon {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<io::Result<()>>>,
}

impl Daemon {
    fn start(lane: Arc<dyn DataLane>) -> Daemon {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().expect("the assigned port");
        let stop = Arc::new(AtomicBool::new(false));
        let lanes = Lanes { data: lane, rest: FakeRest::new(harness::registry_facts()) };
        let thread = {
            let stop = Arc::clone(&stop);
            std::thread::Builder::new()
                .name("test-gwh-strategy-serve".into())
                .spawn(move || {
                    serve(listener, harness::test_keys(), lanes, ServerConfig::PRODUCTION, stop)
                })
                .expect("spawn the server")
        };
        Daemon { addr, stop, thread: Some(thread) }
    }

    /// A link holding both keys.
    fn link(&self) -> GatewayLink {
        GatewayLink::new(self.addr.to_string(), harness::test_keys())
            .with_timings(harness::fast_timings())
    }

    /// A link holding the Read key only.
    fn read_link(&self) -> GatewayLink {
        GatewayLink::new(self.addr.to_string(), harness::read_only_keys())
            .with_timings(harness::fast_timings())
    }

    fn raw(&self, scope: Scope) -> Raw {
        Raw::open(self.addr, &harness::test_keys(), scope)
    }

    /// The `Welcome.features` a peer is told.
    fn features(&self) -> Vec<String> {
        let mut raw = Raw::connect(self.addr);
        raw.hello().1
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

fn wait_for(what: &str, mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ready() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn quote_tick(symbol: &str, x: f64) -> QuoteTick {
    QuoteTick {
        ts: 11,
        local_ts: 12,
        bid: x,
        ask: x + 0.5,
        bid_size: 1.5,
        ask_size: 2.5,
        symbol: symbol.into(),
    }
}

fn trade_tick(symbol: &str, x: f64) -> TradeTick {
    TradeTick {
        ts: 13,
        local_ts: 14,
        price: x,
        size: 0.75,
        is_buyer_maker: false,
        symbol: symbol.into(),
    }
}

fn book(seq: u64, bids: &[(f64, f64)], asks: &[(f64, f64)]) -> L2Book {
    let mut b = L2Book::new(0.5);
    let bids: Vec<_> = bids.iter().map(|&(p, q)| BookLevel::new(p, q)).collect();
    let asks: Vec<_> = asks.iter().map(|&(p, q)| BookLevel::new(p, q)).collect();
    b.apply_snapshot(seq, &bids, &asks);
    b
}

// ---- The scripted pump -------------------------------------------------------------------------------

/// What the scripted spawner remembers: every pump started and its stop flag.
#[derive(Default)]
struct Pumps {
    started: Mutex<Vec<Started>>,
}

/// One pump the lane started: its key, the sender the test plays it through and its stop flag.
struct Started {
    venue: String,
    symbol: String,
    tick_size: f64,
    ticks: TickSender,
    stop: Arc<AtomicBool>,
}

struct Stop(Arc<AtomicBool>);

impl Stop {
    fn stop(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

impl Pumps {
    fn spawner(self: &Arc<Self>) -> PumpSpawner {
        let me = Arc::clone(self);
        Box::new(move |venue, symbol, tick_size, ticks| {
            let flag = Arc::new(AtomicBool::new(false));
            me.started.lock().unwrap_or_else(PoisonError::into_inner).push(Started {
                venue: venue.into(),
                symbol: symbol.into(),
                tick_size,
                ticks,
                stop: Arc::clone(&flag),
            });
            Ok(PumpHandle::new(Stop(flag), Stop::stop))
        })
    }

    fn count(&self) -> usize {
        self.started.lock().unwrap_or_else(PoisonError::into_inner).len()
    }

    fn sender(&self, n: usize) -> TickSender {
        self.started.lock().unwrap_or_else(PoisonError::into_inner)[n].ticks.clone()
    }

    fn stopped(&self, n: usize) -> bool {
        self.started.lock().unwrap_or_else(PoisonError::into_inner)[n].stop.load(Ordering::SeqCst)
    }
}

/// The data lane: the recording client where a venue client would be (a session is a venue client AND its
/// pumps; a pump-only session subscribes nothing on the client), the scripted pump hub.
fn pump_lane(pumps: &Arc<Pumps>) -> Arc<BridgeData> {
    let fake = FakeData::new(&["binance", "bybit", "okx", "aster", "polymarket"]);
    Arc::new(
        BridgeData::over(Box::new(move |venue: &str, sink: Arc<dyn LiveDataSink>| {
            let holder = Holder { session: 0, peer: None, scope: Scope::Read, key_id: None };
            fake.open(venue, &holder, sink)
        }))
        .with_pumps(PumpHub::new(pumps.spawner())),
    )
}

fn pump_client(daemon: &Daemon, venue: &str) -> (RemoteDataClient, Arc<RecordingSink>) {
    let got = Arc::new(RecordingSink::default());
    let opener = GatewayOpener::strategy(daemon.link(), None, false, true);
    let client = RemoteDataClient::new(opener, venue, got.clone()).expect("a reader");
    (client, got)
}

// ---- The pump twin -------------------------------------------------------------------------------------

/// **The pump over the hop**: what a venue pump pushes onto its tick channel arrives at the strategy
/// lane's client as the sink verb of the same name, in order, under the subscribed symbol: the quote and
/// the trade as the pump built them, the book as an anchor and then a delta, the link words on `"ticks"`.
#[test]
fn a_pump_session_delivers_quote_trade_book_and_link_words_in_order() {
    let pumps = Arc::new(Pumps::default());
    let daemon = Daemon::start(pump_lane(&pumps));
    let (mut client, got) = pump_client(&daemon, "binance");
    client.subscribe_ticks("BTCUSDT", 0.5).expect("the pump subscription");
    assert_eq!(pumps.count(), 1);
    {
        let started = pumps.started.lock().unwrap();
        assert_eq!(
            (started[0].venue.as_str(), started[0].symbol.as_str(), started[0].tick_size),
            ("binance", "BTCUSDT", 0.5)
        );
    }

    let ticks = pumps.sender(0);
    // The pump labels its ticks with its own spelling; the wire is keyed by the subscription's.
    ticks
        .quote(QuoteUpdate {
            venue: "binance".into(),
            symbol: "btcusdt".into(),
            quote: quote_tick("btcusdt", 60_000.0),
        })
        .expect("send");
    ticks
        .trade(TradeUpdate {
            venue: "binance".into(),
            symbol: "btcusdt".into(),
            trade: trade_tick("btcusdt", 60_000.25),
        })
        .expect("send");
    let b1 = Arc::new(book(10, &[(100.0, 1.0), (99.5, 2.0)], &[(100.5, 1.5)]));
    let b2 = Arc::new(book(11, &[(100.0, 4.0), (99.5, 2.0)], &[(100.5, 1.5), (101.0, 3.0)]));
    for b in [&b1, &b2] {
        ticks
            .book(PumpBook {
                venue: "binance".into(),
                symbol: "BTCUSDT".into(),
                book: Arc::clone(b),
            })
            .expect("send");
    }
    for status in [FeedStatus::Disconnected, FeedStatus::Live] {
        ticks
            .stream_status(StreamStatusUpdate {
                venue: "binance".into(),
                symbol: "BTCUSDT".into(),
                stream: "ticks".into(),
                status,
            })
            .expect("send");
    }
    wait_for("the seven frames", || got.recorded().len() >= 7);

    let calls = got.recorded();
    // A pump that has just started has no `Disconnected`/`Live` of its own, so its first word is the
    // announcement that it is up (W12b): `Live` on the ticks word, ahead of the data it precedes.
    let SinkCall::StreamStatus {
        stream,
        status: StreamStatus::Live { gap_started_ts_ms: None },
        ..
    } = &calls[0]
    else {
        panic!("{:?}", calls[0])
    };
    assert_eq!(stream, "ticks");
    let SinkCall::Quote { venue, symbol, quote } = &calls[1] else { panic!("{:?}", calls[1]) };
    assert_eq!((venue.as_str(), symbol.as_str()), ("binance", "BTCUSDT"));
    assert_eq!(*quote, quote_tick("btcusdt", 60_000.0), "the tick is the pump's, bit for bit");
    let SinkCall::Trade { trade, .. } = &calls[2] else { panic!("{:?}", calls[2]) };
    assert_eq!(*trade, trade_tick("btcusdt", 60_000.25));

    // The book: an anchor, then only what changed, and the client-side fold equals the pump's book.
    let SinkCall::BookUpdate { update: anchor, .. } = &calls[3] else { panic!("{:?}", calls[3]) };
    let SinkCall::BookUpdate { update: delta, .. } = &calls[4] else { panic!("{:?}", calls[4]) };
    assert_eq!((anchor.kind, delta.kind), (BookUpdateKind::Snapshot, BookUpdateKind::Delta));
    assert_eq!(delta.bids, vec![BookLevel::new(100.0, 4.0)]);
    assert_eq!(delta.asks, vec![BookLevel::new(101.0, 3.0)]);
    let mut rebuilt = L2Book::new(anchor.tick_size);
    rebuilt.apply_snapshot(anchor.seq, &anchor.bids, &anchor.asks);
    assert!(rebuilt.apply_delta(delta.seq, &delta.bids, &delta.asks));
    assert_eq!(rebuilt.top_n(1000), b2.top_n(1000), "the subscriber's book is the pump's");

    // The link words, on the pump's own word, with the instants the lane stamped.
    let SinkCall::StreamStatus { stream, status: StreamStatus::GapStart { at_ts_ms }, .. } =
        &calls[5]
    else {
        panic!("{:?}", calls[5])
    };
    assert_eq!(stream, "ticks");
    let SinkCall::StreamStatus { status: StreamStatus::Live { gap_started_ts_ms }, .. } = &calls[6]
    else {
        panic!("{:?}", calls[6])
    };
    assert_eq!(*gap_started_ts_ms, Some(*at_ts_ms));
    client.shutdown();
}

/// **One venue pump per `(venue, symbol, grid)` across sessions**, and the last one out stops it.
#[test]
fn two_sessions_on_one_key_share_one_pump_and_the_last_to_leave_stops_it() {
    let pumps = Arc::new(Pumps::default());
    let daemon = Daemon::start(pump_lane(&pumps));
    let (mut a, got_a) = pump_client(&daemon, "binance");
    let (mut b, got_b) = pump_client(&daemon, "binance");
    let id_a = a.subscribe_ticks("BTCUSDT", 0.5).expect("a");
    let id_b = b.subscribe_ticks("BTCUSDT", 0.5).expect("b");
    assert_eq!(pumps.count(), 1, "two sessions, one venue connection");

    pumps
        .sender(0)
        .quote(QuoteUpdate {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            quote: quote_tick("BTCUSDT", 5.0),
        })
        .expect("send");
    wait_for("both to hear it", || !got_a.recorded().is_empty() && !got_b.recorded().is_empty());

    // Another grid is another pump.
    let mut c = pump_client(&daemon, "binance").0;
    c.subscribe_ticks("BTCUSDT", 0.1).expect("c");
    assert_eq!(pumps.count(), 2);

    a.unsubscribe(id_a);
    std::thread::sleep(Duration::from_millis(200));
    assert!(!pumps.stopped(0), "one holder is left");
    b.unsubscribe(id_b);
    wait_for("the pump to be stopped", || pumps.stopped(0));
    assert!(!pumps.stopped(1), "the other grid's pump is untouched");
    a.shutdown();
    b.shutdown();
    c.shutdown();
    wait_for("the session's end to release the last pump", || pumps.stopped(1));
}

/// A session that ends (its socket closes) releases its pump without an `Unsub`.
#[test]
fn a_session_that_closes_releases_its_pump() {
    let pumps = Arc::new(Pumps::default());
    let daemon = Daemon::start(pump_lane(&pumps));
    let (mut client, _got) = pump_client(&daemon, "okx");
    client.subscribe_ticks("BTC-USDT-SWAP", 0.1).expect("subscribe");
    assert!(!pumps.stopped(0));
    client.shutdown();
    wait_for("the pump to be released", || pumps.stopped(0));
}

/// The refusals, as a raw peer sees them: a grid the pump cannot be sized to, a venue with no pump, a
/// symbol the session already pumps, and a lane that hosts none. Each is permanent and names why.
#[test]
fn a_pump_subscription_is_refused_by_grid_by_venue_and_by_repeat() {
    let pumps = Arc::new(Pumps::default());
    let daemon = Daemon::start(pump_lane(&pumps));

    let mut raw = daemon.raw(Scope::Read);
    raw.open_stream("binance");
    let sub = |sid, symbol: &str, tick_size| StreamUp::Sub {
        sid,
        op: SubOp::Ticks { symbol: symbol.into(), tick_size },
        class: None,
    };
    let refusal = |raw: &mut Raw, sid: u64| -> SubErrKind {
        let frames = raw.down_until(|f| matches!(f, StreamDown::SubErr { sid: s, .. } | StreamDown::SubOk { sid: s, .. } if *s == sid));
        match frames.last() {
            Some(StreamDown::SubErr { err, .. }) => err.clone(),
            other => panic!("expected a refusal, got {other:?}"),
        }
    };
    raw.up(&sub(1, "BTCUSDT", 0.0));
    match refusal(&mut raw, 1) {
        SubErrKind::SymbolRejected(t) => assert!(t.contains("not positive"), "{t}"),
        other => panic!("{other:?}"),
    }
    raw.up(&sub(2, "BTCUSDT", 1.0e12));
    match refusal(&mut raw, 2) {
        SubErrKind::SymbolRejected(t) => assert!(t.contains("ceiling"), "{t}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(pumps.count(), 0, "a refused grid started nothing");

    raw.up(&sub(3, "BTCUSDT", 0.5));
    let acked = raw.down_until(|f| {
        matches!(f, StreamDown::SubOk { sid: 3, .. } | StreamDown::SubErr { sid: 3, .. })
    });
    assert!(matches!(acked.last(), Some(StreamDown::SubOk { .. })), "{acked:?}");
    raw.up(&sub(4, "BTCUSDT", 0.25));
    match refusal(&mut raw, 4) {
        SubErrKind::SymbolRejected(t) => assert!(t.contains("already pumps"), "{t}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(pumps.count(), 1);

    // A venue outside the pump roster: polymarket is a stream venue of the lane's fake roster here.
    let fake = FakeData::new(&["binance", "polymarket"]);
    let table = Arc::clone(&fake);
    let lane = BridgeData::over(Box::new(move |venue: &str, sink: Arc<dyn LiveDataSink>| {
        let holder = Holder { session: 0, peer: None, scope: Scope::Read, key_id: None };
        table.open(venue, &holder, sink)
    }))
    .with_pumps(PumpHub::new(pumps.spawner()));
    let daemon = Daemon::start(Arc::new(lane));
    let mut raw = daemon.raw(Scope::Read);
    raw.open_stream("polymarket");
    raw.up(&sub(1, "TOKEN", 0.5));
    match refusal(&mut raw, 1) {
        SubErrKind::Unsupported(t) => assert!(t.contains("no tick pump for this venue"), "{t}"),
        other => panic!("{other:?}"),
    }

    // A lane that hosts no pump at all refuses the op the same way (and advertises no `gw_pump`).
    let bare = Daemon::start(FakeData::new(&["binance"]));
    assert!(!bare.features().iter().any(|f| f == "gw_pump"));
    let mut raw = bare.raw(Scope::Read);
    raw.open_stream("binance");
    raw.up(&sub(1, "BTCUSDT", 0.5));
    assert!(matches!(refusal(&mut raw, 1), SubErrKind::Unsupported(_)));
}

// ---- The advertisement ------------------------------------------------------------------------------

#[test]
fn the_strategy_lane_strings_are_advertised_exactly_when_the_lane_is_mounted() {
    let pumps = Arc::new(Pumps::default());
    let lane = BridgeData::over(Box::new(|v, _s| Err(format!("none for {v}"))))
        .with_pumps(PumpHub::new(pumps.spawner()))
        .with_networks(Box::new(|v, n, _s| Err(format!("{v}/{n}"))))
        .with_credentialed(CredentialedLane::over(
            Arc::new(NoAccounts),
            Box::new(|v, _s, _d| Err(format!("none for {v}"))),
        ));
    let features = Daemon::start(Arc::new(lane)).features();
    for want in [
        "gw_pump",
        "stream_network",
        "cs_venue=alpaca",
        "cs_venue=ctrader",
        "cs_venue=ig",
        "cs_venue=oanda",
    ] {
        assert!(features.iter().any(|f| f == want), "{want} missing from {features:?}");
    }
    let bare = Daemon::start(FakeData::new(&["binance"])).features();
    for absent in ["gw_pump", "stream_network"] {
        assert!(
            !bare.iter().any(|f| f == absent),
            "{absent} advertised by a lane with none: {bare:?}"
        );
    }
    assert!(!bare.iter().any(|f| f.starts_with("cs_venue=")), "{bare:?}");
}

struct NoAccounts;

impl AccountSource for NoAccounts {
    fn directory(&self, _venue: &str) -> AccountDirectory {
        AccountDirectory::unread()
    }
}

// ---- The network ---------------------------------------------------------------------------------------

/// A lane whose network table records `(venue, network)` and hands out the recording client.
fn network_lane(seen: &Arc<Mutex<Vec<(String, String)>>>) -> (Arc<BridgeData>, Arc<FakeData>) {
    let fake = FakeData::new(&["binance", "hyperliquid"]);
    let plain = Arc::clone(&fake);
    let named = Arc::clone(&fake);
    let record = Arc::clone(seen);
    let lane = BridgeData::over(Box::new(move |venue: &str, sink: Arc<dyn LiveDataSink>| {
        let holder = Holder { session: 0, peer: None, scope: Scope::Read, key_id: None };
        plain.open(venue, &holder, sink)
    }))
    .with_networks(Box::new(
        move |venue: &str, network: &str, sink: Arc<dyn LiveDataSink>| {
            record.lock().unwrap().push((venue.to_string(), network.to_string()));
            let holder = Holder { session: 0, peer: None, scope: Scope::Read, key_id: None };
            named.open(venue, &holder, sink)
        },
    ));
    (Arc::new(lane), fake)
}

/// **A session's network reaches the lane**: the named one for the venue that takes it, none for a
/// session that names none; and the bytes are the old bytes when no network is named.
#[test]
fn a_session_on_a_named_network_builds_the_client_for_that_network() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (lane, fake) = network_lane(&seen);
    let daemon = Daemon::start(lane);
    let got = Arc::new(RecordingSink::default());
    let opener = GatewayOpener::strategy(daemon.link(), Some("testnet".into()), false, false);
    let mut client = RemoteDataClient::new(opener, "hyperliquid", got).expect("a reader");
    client.subscribe_quotes("BTC").expect("subscribe on the test network");
    assert_eq!(*seen.lock().unwrap(), vec![("hyperliquid".to_string(), "testnet".to_string())]);
    assert_eq!(fake.opened(), 1);
    client.shutdown();

    // No network named: the plain table, the network table is not asked.
    let mut plain = RemoteDataClient::new(
        GatewayOpener::new(daemon.link()),
        "hyperliquid",
        Arc::new(RecordingSink::default()),
    )
    .expect("a reader");
    plain.subscribe_quotes("BTC").expect("subscribe on the default network");
    assert_eq!(seen.lock().unwrap().len(), 1, "no network named, no network asked");
    plain.shutdown();

    // The wire: absent when none (every old peer's bytes), present when named.
    let none = serde_json::to_string(&Request::DataOpen { venue: "binance".into(), network: None })
        .unwrap();
    assert_eq!(none, r#"{"DataOpen":{"venue":"binance"}}"#);
    let named = serde_json::to_string(&Request::DataOpen {
        venue: "hyperliquid".into(),
        network: Some("testnet".into()),
    })
    .unwrap();
    assert_eq!(named, r#"{"DataOpen":{"venue":"hyperliquid","network":"testnet"}}"#);
}

/// **A network is never ignored**: a venue that takes none refuses it, an unknown word is the lane's
/// refusal, and a daemon that does not advertise `stream_network` is never sent one.
#[test]
fn a_network_is_refused_where_it_means_nothing_and_never_sent_to_a_daemon_that_ignores_it() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (lane, fake) = network_lane(&seen);
    let daemon = Daemon::start(lane);
    let mut raw = daemon.raw(Scope::Read);
    raw.send(&Request::DataOpen { venue: "binance".into(), network: Some("testnet".into()) });
    match raw.recv::<Response>() {
        Response::Error(t) => {
            assert!(t.contains("takes no network") && t.contains("testnet"), "{t}")
        }
        other => panic!("{other:?}"),
    }
    assert!(seen.lock().unwrap().is_empty(), "the lane was not asked");
    assert_eq!(fake.opened(), 0, "nothing was opened");

    // The real table refuses an unknown word without dialling anything.
    let real = BridgeData::new(BTreeMap::new());
    let daemon = Daemon::start(Arc::new(real));
    let mut raw = daemon.raw(Scope::Read);
    raw.send(&Request::DataOpen { venue: "hyperliquid".into(), network: Some("moonnet".into()) });
    match raw.recv::<Response>() {
        Response::Error(t) => {
            assert!(t.contains("unknown network") && t.contains("moonnet"), "{t}")
        }
        other => panic!("{other:?}"),
    }
    // Both real words open a client (network-free construction) and a session that names nothing else.
    for network in ["mainnet", "testnet"] {
        let opener = GatewayOpener::strategy(daemon.link(), Some(network.into()), false, false);
        let mut c =
            RemoteDataClient::new(opener, "hyperliquid", Arc::new(RecordingSink::default()))
                .expect("a reader");
        // No subscription: a session that subscribes to nothing dials nothing; the open itself must work.
        c.begin_shutdown();
        c.shutdown();
    }

    // A daemon without the string: the opener refuses locally and nothing reaches the lane.
    let fake = FakeData::new(&["hyperliquid"]);
    let old = Daemon::start(fake.clone());
    let opener = GatewayOpener::strategy(old.link(), Some("testnet".into()), false, false);
    let mut c = RemoteDataClient::new(opener, "hyperliquid", Arc::new(RecordingSink::default()))
        .expect("a reader");
    let err = c.subscribe_quotes("BTC").expect_err("no stream_network advertised");
    assert!(
        format!("{err:?}").contains("stream_network")
            || format!("{err}").contains("stream_network"),
        "{err:?}"
    );
    assert_eq!(fake.opened(), 0, "the open was never sent");
    c.shutdown();
}

// ---- The credentialed streams ---------------------------------------------------------------------------

/// A credentialed lane whose builder records the venue and hands out the recording client.
fn credentialed_lane(built: &Arc<Mutex<Vec<String>>>) -> (Arc<BridgeData>, Arc<FakeData>) {
    let fake = FakeData::new(&["alpaca", "ctrader", "ig", "oanda", "binance"]);
    let table = Arc::clone(&fake);
    let record = Arc::clone(built);
    let credentialed = CredentialedLane::over(
        Arc::new(NoAccounts),
        Box::new(move |venue, sink, _directory| {
            record.lock().unwrap().push(venue.to_string());
            let holder = Holder { session: 0, peer: None, scope: Scope::Write, key_id: None };
            table.open(venue, &holder, sink)
        }),
    );
    let plain = Arc::clone(&fake);
    let lane = BridgeData::over(Box::new(move |venue: &str, sink: Arc<dyn LiveDataSink>| {
        let holder = Holder { session: 0, peer: None, scope: Scope::Read, key_id: None };
        plain.open(venue, &holder, sink)
    }))
    .with_credentialed(credentialed);
    (Arc::new(lane), fake)
}

/// **Credentialed streams are `Scope::Write`'s**: the control key opens one and the lane builds it; the
/// observe key is refused by the daemon (a raw peer, since the client refuses a Read link locally) and by
/// the client before it dials; a keyless venue is not a credentialed one; and no network rides along.
#[test]
fn a_credentialed_stream_is_served_to_the_control_scope_only() {
    let built = Arc::new(Mutex::new(Vec::new()));
    let (lane, _fake) = credentialed_lane(&built);
    let daemon = Daemon::start(lane);

    let opener = GatewayOpener::strategy(daemon.link(), None, true, false);
    let mut client = RemoteDataClient::new(opener, "alpaca", Arc::new(RecordingSink::default()))
        .expect("a reader");
    client.subscribe_quotes("AAPL").expect("the control key opens the credentialed stream");
    assert_eq!(*built.lock().unwrap(), vec!["alpaca".to_string()]);
    client.shutdown();

    // The daemon's own enforcement, seen by a raw Read peer.
    let mut raw = daemon.raw(Scope::Read);
    raw.send(&Request::DataOpen { venue: "oanda".into(), network: None });
    match raw.recv::<Response>() {
        Response::Error(t) => assert!(t.contains("control-scope") && t.contains("oanda"), "{t}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(built.lock().unwrap().len(), 1, "the refused open built nothing");

    // A network on a credentialed venue is refused too.
    let mut raw = daemon.raw(Scope::Write);
    raw.send(&Request::DataOpen { venue: "ig".into(), network: Some("testnet".into()) });
    match raw.recv::<Response>() {
        Response::Error(t) => assert!(t.contains("takes no network"), "{t}"),
        other => panic!("{other:?}"),
    }

    // The client refuses a Read link before dialling.
    let opener = GatewayOpener::strategy(daemon.read_link(), None, true, false);
    let mut c = RemoteDataClient::new(opener, "alpaca", Arc::new(RecordingSink::default()))
        .expect("a reader");
    assert!(c.subscribe_quotes("AAPL").is_err(), "a Read link cannot open a credentialed stream");
    c.shutdown();

    // A keyless venue asked for as credentialed is not in `cs_venue=`.
    let opener = GatewayOpener::strategy(daemon.link(), None, true, false);
    let mut c = RemoteDataClient::new(opener, "binance", Arc::new(RecordingSink::default()))
        .expect("a reader");
    let err = c.subscribe_quotes("BTCUSDT").expect_err("binance is not credentialed");
    assert!(format!("{err:?}").contains("cs_venue=binance"), "{err:?}");
    c.shutdown();
    assert_eq!(built.lock().unwrap().len(), 1);
}

/// The lane's refusal text reaches the peer unchanged (the tradehub maps it onto its startup refusal).
#[test]
fn a_credentialed_refusal_reaches_the_peer_in_the_lanes_words() {
    let lane = BridgeData::over(Box::new(|v, _s| Err(format!("none for {v}")))).with_credentialed(
        CredentialedLane::over(
            Arc::new(NoAccounts),
            Box::new(|venue, _s, _d| {
                Err(format!("{venue}'s market data is CREDENTIALED: give the DEFAULT {venue} account its fields"))
            }),
        ),
    );
    let daemon = Daemon::start(Arc::new(lane));
    let mut raw = daemon.raw(Scope::Write);
    raw.send(&Request::DataOpen { venue: "oanda".into(), network: None });
    match raw.recv::<Response>() {
        Response::Error(t) => {
            assert!(t.contains("CREDENTIALED") && t.contains("DEFAULT oanda"), "{t}")
        }
        other => panic!("{other:?}"),
    }
    // The connection stays positional after a refused open.
    raw.send(&Request::Ping);
    assert!(matches!(raw.recv::<Response>(), Response::Pong));
    assert_eq!(owned(Lane::CredentialedStream), ["alpaca", "ctrader", "ig", "oanda"]);
}

// ---- An Unsub releases the pump BEFORE the key ----------------------------------------------------------

/// A lane whose pump lease, when it is dropped, lets one more frame of the symbol reach the session's
/// sink: the frame a real pump has in flight while its subscriber leaves. Everything else is `FakeData`.
struct InFlightOnLeave(Arc<FakeData>);

struct EmitOnDrop(Arc<dyn LiveDataSink>, String);

impl Drop for EmitOnDrop {
    fn drop(&mut self) {
        self.0.quote("binance", &self.1, quote_tick(&self.1, 1.0));
    }
}

impl DataLane for InFlightOnLeave {
    fn venues(&self) -> Vec<String> {
        self.0.venues()
    }

    fn open(
        &self,
        venue: &str,
        holder: &Holder,
        sink: Arc<dyn LiveDataSink>,
    ) -> Result<Box<dyn DataClient + Send>, String> {
        self.0.open(venue, holder, sink)
    }

    fn pump_venues(&self) -> Vec<String> {
        vec!["binance".to_string()]
    }

    fn open_pump(
        &self,
        _venue: &str,
        _holder: &Holder,
        symbol: &str,
        _tick_size: f64,
        sink: PumpSink,
    ) -> Result<PumpLease, String> {
        Ok(PumpLease::new(EmitOnDrop(sink.sink, symbol.to_string())))
    }
}

/// **An `Unsub` drops the pump lease BEFORE it gives the key back** (review finding 7): a frame the pump had
/// in flight when its subscriber left would otherwise find the symbol UNBOUND and bind an unsolicited key
/// for it, announced by a stray `Key` frame the subscriber never asked for (and one of the session's
/// unsolicited slots burned for good).
#[test]
fn an_unsub_leaves_no_stray_key_for_a_frame_the_pump_had_in_flight() {
    let daemon = Daemon::start(Arc::new(InFlightOnLeave(FakeData::new(&["binance"]))));
    let mut raw = daemon.raw(Scope::Read);
    raw.open_stream("binance");
    raw.up(&StreamUp::Sub {
        sid: 1,
        op: SubOp::Ticks { symbol: "BTCUSDT".into(), tick_size: 0.5 },
        class: None,
    });
    let acked = raw.down_until(|f| matches!(f, StreamDown::SubOk { sid: 1, .. }));
    assert!(matches!(acked.last(), Some(StreamDown::SubOk { .. })), "{acked:?}");

    raw.up(&StreamUp::Unsub { sid: 1 });
    // Two heartbeat round trips bracket everything the Unsub could have queued.
    let mut seen = Vec::new();
    for _ in 0..2 {
        raw.up(&StreamUp::Ping);
        loop {
            let frame = raw.down();
            if matches!(frame, StreamDown::Heartbeat) {
                break;
            }
            seen.push(frame);
        }
        std::thread::sleep(Duration::from_millis(150));
    }
    assert!(
        !seen.iter().any(|f| matches!(f, StreamDown::Key { .. })),
        "the in-flight frame of an unsubscribed symbol bound a stray key: {seen:?}"
    );
}
