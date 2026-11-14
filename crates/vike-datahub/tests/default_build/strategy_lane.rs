//! The STRATEGY LANE over a real socket: the door (`StrategyOpen`), the scope that fences it, the tube's
//! frames behind it, the broker it subscribes through, the bounds and the drills of its slow consumer,
//! and the tick pump. The hub's own contracts with no socket are `src/strat/strategy_tests.rs`; the broker's
//! third holder is `tests/default_build/feed_broker.rs`.
//!
//! The venue side is a SCRIPTED `DataClient` behind the real `FeedBroker`; the tradehub side is a raw
//! socket speaking the tube (`StreamUp` out, `StreamDown` in), so what is asserted is the wire the real
//! `DataClientStrategyLane` will read. The pump's far end is a fake gatewayhub stream peer (`FakePump`).

use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use vike_data::DataClientBuilder;

use vike_data::{
    DataClient, DataClientError, DataClientSink, DataClientSubscriptionId, HistoryStore,
    HistoryStoreMemory,
};
use vike_datahub::feeds::{DataClientBrokerSharing, FeedBroker};
use vike_datahub::server::{ServerLanes, ServerTimings, serve_with_lanes};
use vike_datahub::strat::{StrategyLane, StrategyLaneLimits, StrategyLaneTicksBuilder};
use vike_datahub_client::proto::{
    FEATURE_FUNDING_HISTORY, FEATURE_INPUT_BARS, FEATURE_STRATEGY_LANE, FEATURE_STRATEGY_LANE_BARS,
    FEATURE_STRATEGY_LANE_BOOK, FEATURE_STRATEGY_LANE_QUOTES, FEATURE_STRATEGY_LANE_TICKS,
    FEATURE_STREAM_CLASS, Request, Response, STRATEGY_LANE_FRAME_CEILING_BYTES,
    advertised_strategy_venues, write_frame,
};
use vike_hub_protocol::auth::FEATURE_AUTH;
use vike_hub_protocol::auth::{DATAHUB_DOMAIN, HubKeys, Scope};
use vike_market_stream::frame::{read_capped, write_capped};
use vike_market_stream::market::DataClientStream;
// The opener that dials the fake pump directly (no handshake: the pump is a fake peer), shared with
// the other strategy-lane suite.
use vike_market_stream::test_support::DirectOpener;
use vike_market_stream::wire::{
    StreamByeReason, StreamDown, StreamGapVerb, StreamSubscriptionErrorKind, StreamUp,
    StreamWordWire, SubscriptionOperation,
};
use vike_marketdata::{Bar, L2Book, L2BookLevel, TickQuote, TickTrade};
use vike_model::{StreamLabel, StreamStatus};

use vike_data::delivery::DataClientSinkDelivery;
use vike_datahub::test_support::{authed_stream, eventually, exchange, keys};

const TRADE_KEY: &[u8] = b"datahub-strategy-lane-key";

fn trade_keys() -> HubKeys {
    keys().with_trade(TRADE_KEY.to_vec())
}

// ---- the venue side: a scripted client behind the real broker ------------------------------------------

type Log = Arc<Mutex<Vec<String>>>;
type Sinks = Arc<Mutex<Vec<Arc<dyn DataClientSink>>>>;

fn bar(ts: i64) -> Bar {
    vike_marketdata::test_support::distinct_ohlc_bar(ts, 3.0)
}

fn quote(ts: i64) -> TickQuote {
    vike_marketdata::test_support::quote(ts, 1.0, 2.0)
}

fn trade(ts: i64) -> TickTrade {
    vike_marketdata::test_support::trade(ts, 1.5, 1.0, false)
}

struct Scripted {
    venue: String,
    sink: Arc<dyn DataClientSink>,
    log: Log,
    next: u64,
}

impl Scripted {
    fn start(
        &mut self,
        verb: &str,
        symbol: &str,
    ) -> Result<DataClientSubscriptionId, DataClientError> {
        if symbol == "REFUSED" {
            return Err(DataClientError::Subscribe("scripted refusal".into()));
        }
        self.next += 1;
        self.log.lock().unwrap().push(format!("{verb} {symbol} -> {}", self.next));
        Ok(DataClientSubscriptionId(self.next))
    }
}

impl DataClient for Scripted {
    /// Emits a seed from INSIDE the subscribe, the way a fast venue can: it must arrive after the `SubscribeOk`.
    fn subscribe_bars(
        &mut self,
        s: &str,
        interval: &str,
    ) -> Result<DataClientSubscriptionId, DataClientError> {
        let id = self.start(&format!("bars:{interval}"), s)?;
        self.sink.seed_bars(&self.venue, s, interval, vec![bar(1)]);
        Ok(id)
    }
    fn subscribe_quotes(&mut self, s: &str) -> Result<DataClientSubscriptionId, DataClientError> {
        self.start("quotes", s)
    }
    fn subscribe_trades(&mut self, s: &str) -> Result<DataClientSubscriptionId, DataClientError> {
        self.start("trades", s)
    }
    fn subscribe_book(&mut self, s: &str) -> Result<DataClientSubscriptionId, DataClientError> {
        self.start("book", s)
    }
    fn subscribe_depth(&mut self, s: &str) -> Result<DataClientSubscriptionId, DataClientError> {
        self.start("depth", s)
    }
    fn unsubscribe(&mut self, id: DataClientSubscriptionId) {
        self.log.lock().unwrap().push(format!("unsubscribe {}", id.0));
    }
    fn set_delivery(
        &mut self,
        symbol: &str,
        lane: vike_model::StreamLabel,
        class: vike_data::delivery::DataClientSinkDelivery,
    ) -> Result<(), DataClientError> {
        self.log.lock().unwrap().push(format!("set_delivery {symbol} {} {class:?}", lane.as_str()));
        Ok(())
    }
    fn begin_shutdown(&mut self) {
        self.log.lock().unwrap().push("begin_shutdown".into());
    }
    fn shutdown(&mut self) {}
}

fn count(log: &Log, needle: &str) -> usize {
    log.lock().unwrap().iter().filter(|l| l.contains(needle)).count()
}

/// How long [`eventually`] polls a condition in this file.
const EVENTUALLY: Duration = Duration::from_secs(5);

// ---- the rig ---------------------------------------------------------------------------------------------

struct Rig {
    addr: SocketAddr,
    hub: Arc<StrategyLane>,
    broker: Arc<FeedBroker>,
    log: Log,
    sinks: Sinks,
    /// The name the broker built each venue client under, in build order (`hyperliquid`).
    built: Log,
}

impl Rig {
    /// The sink the venue's (shared) client was built with: emitting through it is a venue emission.
    fn venue_sink(&self) -> Arc<dyn DataClientSink> {
        Arc::clone(&self.sinks.lock().unwrap()[0])
    }
}

fn rig_with(limits: StrategyLaneLimits, ticks: Option<StrategyLaneTicksBuilder>) -> Rig {
    rig_venues(vec!["binance".to_string(), "hyperliquid".to_string()], limits, ticks)
}

/// [`rig_with`] over the venues the hub serves.
fn rig_venues(
    venues: Vec<String>,
    limits: StrategyLaneLimits,
    ticks: Option<StrategyLaneTicksBuilder>,
) -> Rig {
    let log: Log = Arc::default();
    let sinks: Sinks = Arc::default();
    let built: Log = Arc::default();
    let (l, s, b) = (Arc::clone(&log), Arc::clone(&sinks), Arc::clone(&built));
    let builder: DataClientBuilder = Box::new(
        move |venue: &str,
              sink: Arc<dyn DataClientSink>|
              -> Result<Box<dyn DataClient + Send>, String> {
            s.lock().unwrap().push(Arc::clone(&sink));
            b.lock().unwrap().push(venue.to_string());
            Ok(Box::new(Scripted { venue: venue.into(), sink, log: Arc::clone(&l), next: 0 }))
        },
    );
    let broker = FeedBroker::new(builder, |_| DataClientBrokerSharing::Shared);
    let hub = StrategyLane::new(Arc::clone(&broker), venues, ticks, limits);
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let store: Arc<dyn HistoryStore + Send + Sync> = Arc::new(HistoryStoreMemory::new());
    let lanes = ServerLanes { strat: Some(Arc::clone(&hub)), ..ServerLanes::default() };
    thread::spawn(move || {
        let _ = serve_with_lanes(
            listener,
            store,
            None,
            Some(trade_keys()),
            None,
            None,
            lanes,
            None,
            false,
            ServerTimings::PRODUCTION,
        );
    });
    Rig { addr, hub, broker, log, sinks, built }
}

fn rig() -> Rig {
    rig_with(StrategyLaneLimits::PRODUCTION, None)
}

// ---- the tradehub side: a raw socket speaking the tube -----------------------------------------------------

fn open_on(addr: SocketAddr, venue: &str, network: Option<&str>) -> (TcpStream, Response) {
    let mut s = authed_stream(addr, &trade_keys(), Scope::Trade);
    let reply = exchange(
        &mut s,
        &Request::StrategyOpen { venue: venue.to_string(), network: network.map(str::to_string) },
    );
    (s, reply)
}

fn lane(addr: SocketAddr, venue: &str) -> TcpStream {
    lane_on(addr, venue, None)
}

fn lane_on(addr: SocketAddr, venue: &str, network: Option<&str>) -> TcpStream {
    let (s, reply) = open_on(addr, venue, network);
    match reply {
        Response::StrategyOpened { heartbeat_ms, .. } => assert!(heartbeat_ms > 0),
        other => panic!("expected StrategyOpened, got {other:?}"),
    }
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    s
}

fn up(s: &mut TcpStream, frame: &StreamUp) {
    write_frame(s, frame).expect("a client frame writes");
}

fn down(s: &mut TcpStream) -> StreamDown {
    read_capped::<_, StreamDown>(s, STRATEGY_LANE_FRAME_CEILING_BYTES)
        .expect("a server frame reads")
}

/// The next frame that is not a heartbeat. BOUNDED: a lane that keeps heartbeating and never sends the
/// frame a test waits for must fail the test, not hang it (a heartbeat resets the socket's read timeout).
fn next(s: &mut TcpStream) -> StreamDown {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        match down(s) {
            StreamDown::Heartbeat => {
                assert!(
                    Instant::now() < deadline,
                    "only heartbeats for 20 s: the awaited frame never came"
                );
            }
            other => return other,
        }
    }
}

/// A `Sub` and its `SubscribeOk`, NOTHING more read. A session that joins a BROKER lane (quotes, trades, book, depth, a
/// bar interval; not the pump) is told where the lane stands right after the `SubscribeOk` (`Live{None}` when its venue
/// client has disclosed nothing), a frame this helper leaves on the socket: a test that holds the session and reads
/// on says which it expects, with [`sub_joined`] (and reads the restatement itself with [`joined`] where the
/// order of frames is the point). `next` does NOT skip it, so a spurious or duplicated `Live` fails the test that
/// meets it.
fn sub(
    s: &mut TcpStream,
    sid: u64,
    op: SubscriptionOperation,
    class: Option<DataClientSinkDelivery>,
) -> u32 {
    up(s, &StreamUp::Subscribe { sid, op, class });
    match next(s) {
        StreamDown::SubscribeOk { sid: got, k } => {
            assert_eq!(got, sid);
            k
        }
        StreamDown::SubscribeError { sid: got, err } => panic!("sid {got} refused: {err:?}"),
        other => panic!("expected SubscribeOk, got {other:?}"),
    }
}

/// The join restatement of the lane `op` subscribed, read and checked: the next frame is `Live{None}` for key
/// `k` on `op`'s own stream word. Nothing else is allowed in its place.
fn joined(s: &mut TcpStream, k: u32, op: &SubscriptionOperation) {
    match next(s) {
        StreamDown::Status { k: got, stream, status } => {
            assert_eq!(got, k, "the restatement is about the joined key");
            assert_eq!(stream, op.stream(), "…and about the lane that was joined");
            assert_eq!(
                status,
                StreamStatus::Live { gap_started_timestamp_ms: None },
                "a lane that has disclosed nothing is up"
            );
        }
        other => panic!("expected the join restatement, got {other:?}"),
    }
}

/// [`sub`] on a broker lane, then its join restatement ([`joined`]): the whole of what a first subscription to
/// a lane word of a symbol puts on the wire.
fn sub_joined(
    s: &mut TcpStream,
    sid: u64,
    op: SubscriptionOperation,
    class: Option<DataClientSinkDelivery>,
) -> u32 {
    let k = sub(s, sid, op.clone(), class);
    joined(s, k, &op);
    k
}

fn refused(s: &mut TcpStream, sid: u64, op: SubscriptionOperation) -> StreamSubscriptionErrorKind {
    up(s, &StreamUp::Subscribe { sid, op, class: None });
    match next(s) {
        StreamDown::SubscribeError { sid: got, err } => {
            assert_eq!(got, sid);
            err
        }
        other => panic!("expected SubscribeError, got {other:?}"),
    }
}

fn quotes_op(symbol: &str) -> SubscriptionOperation {
    SubscriptionOperation::Quotes { symbol: symbol.to_string() }
}

fn welcome_features(addr: SocketAddr) -> Vec<String> {
    let mut s = TcpStream::connect(addr).expect("connect");
    match exchange(&mut s, &Request::Hello { proto_version: vike_datahub_client::PROTO_VERSION }) {
        Response::Welcome { features, .. } => features,
        other => panic!("expected Welcome, got {other:?}"),
    }
}

// ---- advertisement and the door ------------------------------------------------------------------------------

/// **The lane is advertised by the MOUNTED hub on a KEYED server, family by family and venue by
/// venue**, the pump family only when the build has a pump, and beside the tube's class string. The
/// plane's own venues (`market_data_venue=`) are not the lane's.
#[test]
fn a_mounted_hub_on_a_keyed_server_advertises_the_lane_its_families_and_its_venues() {
    let features = welcome_features(rig().addr);
    for f in [
        FEATURE_STRATEGY_LANE,
        FEATURE_STRATEGY_LANE_QUOTES,
        FEATURE_STRATEGY_LANE_BARS,
        FEATURE_STRATEGY_LANE_BOOK,
        FEATURE_STREAM_CLASS,
        FEATURE_AUTH,
    ] {
        assert!(features.iter().any(|x| x == f), "missing {f}: {features:?}");
    }
    assert!(
        !features.iter().any(|x| x == FEATURE_STRATEGY_LANE_TICKS),
        "no pump table is mounted, so no pump family: {features:?}"
    );
    assert_eq!(advertised_strategy_venues(&features), vec!["binance", "hyperliquid"]);
    assert!(vike_datahub_client::advertised_market_data_venues(&features).is_empty());

    let with_pump = rig_with(StrategyLaneLimits::PRODUCTION, Some(never_dial()));
    assert!(welcome_features(with_pump.addr).iter().any(|x| x == FEATURE_STRATEGY_LANE_TICKS));
}

/// A pump table whose clients never dial: enough to be MOUNTED.
fn never_dial() -> StrategyLaneTicksBuilder {
    Box::new(|_venue: &str, _sink: Arc<dyn DataClientSink>| -> Result<DataClientStream, String> {
        Err("this test pump never dials".into())
    })
}

/// **A key-less server serves no lane whatever is mounted** - a lane fenced by a scope nothing enforces
/// is a firehose for whoever reaches the socket. The advertisement is withheld, the verb is refused by
/// name, and the connection stays positional.
#[test]
fn a_keyless_server_serves_no_lane_even_with_a_hub_mounted() {
    let log: Log = Arc::default();
    let builder: DataClientBuilder = Box::new(
        |_: &str, _: Arc<dyn DataClientSink>| -> Result<Box<dyn DataClient + Send>, String> {
            Err("no client".into())
        },
    );
    let broker = FeedBroker::new(builder, |_| DataClientBrokerSharing::Shared);
    let hub = StrategyLane::new(
        broker,
        vec!["binance".to_string()],
        None,
        StrategyLaneLimits::PRODUCTION,
    );
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let store: Arc<dyn HistoryStore + Send + Sync> = Arc::new(HistoryStoreMemory::new());
    let lanes = ServerLanes { strat: Some(hub), ..ServerLanes::default() };
    thread::spawn(move || {
        let _ = serve_with_lanes(
            listener,
            store,
            None,
            None,
            None,
            None,
            lanes,
            None,
            false,
            ServerTimings::PRODUCTION,
        );
    });
    let features = welcome_features(addr);
    assert!(
        !features.iter().any(|f| f.starts_with("strategy_lane") || f.starts_with("strategy_venue"))
    );
    let mut s = TcpStream::connect(addr).unwrap();
    match exchange(&mut s, &Request::StrategyOpen { venue: "binance".into(), network: None }) {
        Response::Error(why) => assert!(why.contains("no hub keys"), "{why}"),
        other => panic!("expected a refusal, got {other:?}"),
    }
    assert!(matches!(exchange(&mut s, &Request::Ping), Response::Pong), "still positional");
    drop(log);
}

/// **A keyed datahub with no lane mounted refuses `StrategyOpen` with the sentence that names the key**, on
/// a connection that stays positional; and a keyed server that holds no strategy-lane key at all refuses
/// the Trade handshake itself (no key, no scope).
#[test]
fn a_server_without_a_mounted_lane_names_the_key_that_arms_it() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let store: Arc<dyn HistoryStore + Send + Sync> = Arc::new(HistoryStoreMemory::new());
    thread::spawn(move || {
        let _ =
            vike_datahub::serve_authed(listener, store, None, Some(trade_keys()), None, None, None);
    });
    let mut s = authed_stream(addr, &trade_keys(), Scope::Trade);
    match exchange(&mut s, &Request::StrategyOpen { venue: "binance".into(), network: None }) {
        Response::Error(why) => {
            assert!(
                why.contains("VIKE_DATAHUB_TRADE_KEY") && why.contains("datahub setup"),
                "{why}"
            );
        }
        other => panic!("{other:?}"),
    }
    // ...and the connection is still the server's to use (a refusal, not a desync).
    assert!(matches!(exchange(&mut s, &Request::Ping), Response::Error(_)));

    // A server that holds the pair but no Trade key refuses a Trade handshake.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let store: Arc<dyn HistoryStore + Send + Sync> = Arc::new(HistoryStoreMemory::new());
    thread::spawn(move || {
        let _ = vike_datahub::serve_authed(listener, store, None, Some(keys()), None, None, None);
    });
    let mut raw = TcpStream::connect(addr).unwrap();
    let nonce = match exchange(
        &mut raw,
        &Request::Hello { proto_version: vike_datahub_client::PROTO_VERSION },
    ) {
        Response::Welcome { nonce, .. } => nonce.expect("keyed"),
        other => panic!("{other:?}"),
    };
    let mac = vike_hub_protocol::auth::sign(
        DATAHUB_DOMAIN,
        TRADE_KEY,
        &nonce,
        vike_datahub_client::PROTO_VERSION,
        Scope::Trade,
    );
    assert!(matches!(
        exchange(&mut raw, &Request::Auth { scope: Scope::Trade, mac }),
        Response::AuthDenied { .. }
    ));
}

/// **Only the strategy-lane key opens the lane, and it opens nothing else**: a read or a control
/// connection is refused `StrategyOpen` (and stays usable), and a Trade connection is refused every store
/// verb.
#[test]
fn the_trade_scope_opens_the_lane_and_nothing_else_and_no_other_scope_opens_it() {
    let rig = rig();
    for scope in [Scope::Read, Scope::Write] {
        let mut s = authed_stream(rig.addr, &keys(), scope);
        match exchange(&mut s, &Request::StrategyOpen { venue: "binance".into(), network: None }) {
            Response::Error(why) => assert!(why.contains("requires the Trade scope"), "{why}"),
            other => panic!("{scope:?}: {other:?}"),
        }
        assert!(matches!(exchange(&mut s, &Request::Ping), Response::Pong), "{scope:?} survives");
    }
    let mut t = authed_stream(rig.addr, &trade_keys(), Scope::Trade);
    for r in [Request::Ping, Request::ListSeries, Request::Coverage] {
        match exchange(&mut t, &r) {
            Response::Error(why) => assert!(why.contains("Trade scope"), "{r:?}: {why}"),
            other => panic!("{r:?}: {other:?}"),
        }
    }
    assert!(matches!(
        exchange(&mut t, &Request::StrategyOpen { venue: "binance".into(), network: None }),
        Response::StrategyOpened { .. }
    ));
}

/// **The lane's key reads the stored funding series and no other series** (decision 0262's amendment of
/// 2026-11-08): `StrategyFundingHistory`, advertised beside the lane, answers the `interval=funding` bars of its
/// window (the bars of another interval of the same symbol are not reachable through it), and a read or a
/// control connection is refused it by scope.
#[test]
fn the_trade_scope_reads_the_stored_funding_series_and_no_other_scope_does() {
    let store = HistoryStoreMemory::new();
    let point = |ts: i64, rate: f64| Bar { close: 60_000.0, funding: Some(rate), ..bar(ts) };
    store
        .append_bars(
            "binance",
            "BTCUSDT.P",
            vike_data::source::FUNDING_INTERVAL,
            &[point(1_000, 0.0001), point(2_000, 0.0002), point(3_000, 0.0003)],
            None,
        )
        .expect("the funding series");
    store.append_bars("binance", "BTCUSDT.P", "1h", &[bar(2_500)], None).expect("a price bar");
    let addr = serve_a_lane_over(store);
    assert!(welcome_features(addr).iter().any(|f| f == FEATURE_FUNDING_HISTORY));
    let read = Request::StrategyFundingHistory {
        venue: "binance".into(),
        symbol: "BTCUSDT.P".into(),
        start: 1_500,
        end: 3_000,
    };
    let mut t = authed_stream(addr, &trade_keys(), Scope::Trade);
    match exchange(&mut t, &read) {
        Response::Bars(bars) => assert_eq!(
            bars.iter().map(|b| (b.timestamp, b.funding, b.close)).collect::<Vec<_>>(),
            vec![(2_000, Some(0.0002), 60_000.0), (3_000, Some(0.0003), 60_000.0)],
            "the window's settlements, and no price bar"
        ),
        other => panic!("{other:?}"),
    }
    for scope in [Scope::Read, Scope::Write] {
        let mut s = authed_stream(addr, &keys(), scope);
        match exchange(&mut s, &read) {
            Response::Error(why) => assert!(why.contains("requires the Trade scope"), "{why}"),
            other => panic!("{scope:?}: {other:?}"),
        }
    }
}

/// A keyed server over `store` with a strategy lane mounted for `binance` (a broker whose venue client
/// never builds: the store reads need none).
fn serve_a_lane_over(store: HistoryStoreMemory) -> SocketAddr {
    let builder: DataClientBuilder = Box::new(
        |_: &str, _: Arc<dyn DataClientSink>| -> Result<Box<dyn DataClient + Send>, String> {
            Err("no client".into())
        },
    );
    let broker = FeedBroker::new(builder, |_| DataClientBrokerSharing::Shared);
    let hub = StrategyLane::new(
        broker,
        vec!["binance".to_string()],
        None,
        StrategyLaneLimits::PRODUCTION,
    );
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let store: Arc<dyn HistoryStore + Send + Sync> = Arc::new(store);
    let lanes = ServerLanes { strat: Some(hub), ..ServerLanes::default() };
    thread::spawn(move || {
        let _ = serve_with_lanes(
            listener,
            store,
            None,
            Some(trade_keys()),
            None,
            None,
            lanes,
            None,
            false,
            ServerTimings::PRODUCTION,
        );
    });
    addr
}

/// **The lane's key reads a strategy input's stored bar series** (decision 0271's live half):
/// `StrategyInputBars`, advertised beside the lane, answers the venue's own candles for no `source` and the
/// named `user-<label>` lane's bars for one, each only its window, over a store holding BOTH series of the
/// same symbol and interval; a read or a control connection is refused it by scope.
#[test]
fn the_trade_scope_reads_an_inputs_bars_and_its_source_lane_and_no_other_scope_does() {
    let store = HistoryStoreMemory::new();
    let venue_bar = |ts: i64| Bar { close: 1.0, ..bar(ts) };
    let lane_bar = |ts: i64| Bar { close: 2.0, ..bar(ts) };
    store
        .append_bars("binance", "USDT-USD", "1d", &[venue_bar(1_000), venue_bar(2_000)], None)
        .expect("the venue's series");
    store
        .append_bars_sourced(
            "binance",
            "user-coinbase",
            "USDT-USD",
            "1d",
            &[lane_bar(1_000), lane_bar(2_000), lane_bar(3_000)],
            None,
        )
        .expect("the user's lane");
    let addr = serve_a_lane_over(store);
    assert!(welcome_features(addr).iter().any(|f| f == FEATURE_INPUT_BARS));
    let read = |source: Option<&str>| Request::StrategyInputBars {
        venue: "binance".into(),
        symbol: "USDT-USD".into(),
        interval: "1d".into(),
        source: source.map(str::to_string),
        start: 1_500,
        end: 3_000,
    };
    let mut t = authed_stream(addr, &trade_keys(), Scope::Trade);
    for (source, want) in
        [(None, vec![(2_000, 1.0)]), (Some("user-coinbase"), vec![(2_000, 2.0), (3_000, 2.0)])]
    {
        match exchange(&mut t, &read(source)) {
            Response::Bars(bars) => assert_eq!(
                bars.iter().map(|b| (b.timestamp, b.close)).collect::<Vec<_>>(),
                want,
                "{source:?}: the window's bars of that series and no other"
            ),
            other => panic!("{source:?}: {other:?}"),
        }
    }
    for scope in [Scope::Read, Scope::Write] {
        let mut s = authed_stream(addr, &keys(), scope);
        match exchange(&mut s, &read(None)) {
            Response::Error(why) => assert!(why.contains("requires the Trade scope"), "{why}"),
            other => panic!("{scope:?}: {other:?}"),
        }
    }
}

/// **Refusals before the switch leave the connection positional**: an unserved venue (naming the served
/// set), a network on a venue that has none, an unknown network word, and a full lane.
#[test]
fn a_refused_open_leaves_the_connection_positional() {
    let rig = rig_with(
        StrategyLaneLimits { max_stream_connections: 1, ..StrategyLaneLimits::PRODUCTION },
        None,
    );
    let mut t = authed_stream(rig.addr, &trade_keys(), Scope::Trade);
    for (venue, network, needle) in [
        ("kalshi", None, "binance, hyperliquid"),
        ("binance", Some("mainnet"), "takes no network"),
        ("binance", Some("testnet"), "takes no network"),
        ("hyperliquid", Some("moonnet"), "unknown network"),
    ] {
        match exchange(
            &mut t,
            &Request::StrategyOpen { venue: venue.into(), network: network.map(str::to_string) },
        ) {
            Response::Error(why) => assert!(why.contains(needle), "{venue}: {why}"),
            other => panic!("{venue}: {other:?}"),
        }
    }
    // ...and the connection still answers a Trade-scope verb positionally (a refusal, not a desync).
    assert!(matches!(exchange(&mut t, &Request::Ping), Response::Error(_)));
    // Fill the lane's one slot from another connection; the next open is a CAP refusal (not a transient).
    let mut first = lane(rig.addr, "binance");
    match exchange(&mut t, &Request::StrategyOpen { venue: "binance".into(), network: None }) {
        Response::Error(why) => {
            assert!(why.contains(vike_market_stream::market::CAP_MARK), "{why}")
        }
        other => panic!("{other:?}"),
    }
    up(&mut first, &StreamUp::Close);
    assert!(matches!(next(&mut first), StreamDown::Bye(StreamByeReason::ClientClosed)));
    eventually(EVENTUALLY, "the slot is given back", || rig.hub.sessions() == 0);
    assert!(matches!(
        exchange(&mut t, &Request::StrategyOpen { venue: "binance".into(), network: None }),
        Response::StrategyOpened { .. }
    ));
}

/// **Hyperliquid's test network is a client of its own** (the broker keys the lane's clients by venue AND
/// network): its keys do not share the main network's real subscription, it is never handed the main
/// network's frames and the main network is never handed its, though both ask for the same symbol.
#[test]
fn a_test_network_session_has_its_own_client_and_hears_only_its_own_frames() {
    let rig = rig();
    let mut main = lane(rig.addr, "hyperliquid");
    let mut test = lane_on(rig.addr, "hyperliquid", Some("testnet"));
    let k_main = sub_joined(&mut main, 1, quotes_op("BTC"), None);
    let k_test = sub_joined(&mut test, 1, quotes_op("BTC"), None);
    assert_eq!(*rig.built.lock().unwrap(), vec!["hyperliquid", "hyperliquid@testnet"]);
    assert_eq!(count(&rig.log, "quotes BTC"), 2, "two clients, so two real subscriptions");

    // Each client's frames, under the name its client was built with.
    let (main_sink, test_sink) = {
        let sinks = rig.sinks.lock().unwrap();
        (Arc::clone(&sinks[0]), Arc::clone(&sinks[1]))
    };
    test_sink.quote("hyperliquid@testnet", "BTC", quote(7));
    main_sink.quote("hyperliquid", "BTC", quote(9));
    match next(&mut test) {
        StreamDown::Quote { k, quote, .. } => assert_eq!((k, quote.timestamp), (k_test, 7)),
        other => panic!("{other:?}"),
    }
    match next(&mut main) {
        StreamDown::Quote { k, quote, .. } => assert_eq!((k, quote.timestamp), (k_main, 9)),
        other => panic!("{other:?}"),
    }
    // Nothing crossed: the next frame each side sees is the one sent AFTER (a crossed frame would be first).
    test_sink.quote("hyperliquid@testnet", "BTC", quote(8));
    main_sink.quote("hyperliquid", "BTC", quote(10));
    match next(&mut test) {
        StreamDown::Quote { quote, .. } => assert_eq!(quote.timestamp, 8),
        other => panic!("{other:?}"),
    }
    match next(&mut main) {
        StreamDown::Quote { quote, .. } => assert_eq!(quote.timestamp, 10),
        other => panic!("{other:?}"),
    }
    // `mainnet` named in full is the main network's client, not a third one.
    let mut named = lane_on(rig.addr, "hyperliquid", Some("mainnet"));
    sub_joined(&mut named, 1, quotes_op("BTC"), None);
    assert_eq!(rig.built.lock().unwrap().len(), 2, "`mainnet` shares the main client");
}

// ---- the tube behind the door ---------------------------------------------------------------------------------

/// **`StrategyOpened` is the LAST positional frame**: after it a positional verb gets no answer, only the
/// tube's frames (a `Ping` is the tube's, answered by a `Heartbeat`), and a frame that is not the tube's
/// ends the session.
#[test]
fn stratopened_is_the_last_positional_frame_and_the_tube_speaks_from_then_on() {
    let rig = rig();
    let mut s = lane(rig.addr, "binance");
    up(&mut s, &StreamUp::Ping);
    assert!(matches!(down(&mut s), StreamDown::Heartbeat), "a tube Ping is answered in-band");
    // A positional verb is not the tube's grammar: undecodable, so the session ends (EOF).
    // (Not `Ping`: a unit variant is its bare name on the wire, and "Ping" is ALSO the tube's `StreamUp::Ping`.)
    write_frame(&mut s, &Request::Inventory).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut buffer = [0u8; 1];
    use std::io::Read;
    let n = s.read(&mut buffer).expect("a closed socket reads EOF, not a timeout");
    assert_eq!(n, 0, "the lane closes on a frame that is not the tube's");
    eventually(EVENTUALLY, "the session ends", || rig.hub.sessions() == 0);
}

/// **The whole round trip**: a `Sub` is acknowledged, the venue's frames come back keyed and numbered,
/// a seed emitted INSIDE the subscribe arrives after the `SubscribeOk`, and an `Unsubscribe` reaches the venue.
#[test]
fn a_subscription_is_acknowledged_streamed_and_released() {
    let rig = rig();
    let mut s = lane(rig.addr, "binance");
    let kq = sub_joined(&mut s, 1, quotes_op("BTCUSDT.P"), None);
    let bars = SubscriptionOperation::Bars { symbol: "ETHUSDT".into(), interval: "1m".into() };
    let kb = sub(&mut s, 2, bars.clone(), None);
    assert_ne!(kq, kb, "one key per symbol");
    // The seed the venue emitted inside `subscribe_bars` follows its SubscribeOk, and the join restatement follows
    // the seed (the restatement is the last thing a subscription puts on the wire).
    match next(&mut s) {
        StreamDown::Seed { k, sequence: 1, interval, bars } => {
            assert_eq!((k, interval.as_str(), bars.len()), (kb, "1m", 1));
        }
        other => panic!("expected the seed, got {other:?}"),
    }
    joined(&mut s, kb, &bars);
    let venue = rig.venue_sink();
    venue.quote("binance", "BTCUSDT.P", quote(10));
    venue.quote("binance", "BTCUSDT.P", quote(11));
    venue.trade("binance", "BTCUSDT.P", trade(12)); // not a lane this session holds
    for want in [1u64, 2] {
        match next(&mut s) {
            StreamDown::Quote { k, sequence: seq, quote } => {
                assert_eq!((k, seq, quote.timestamp as u64), (kq, want, 9 + want))
            }
            other => panic!("expected quote {want}, got {other:?}"),
        }
    }
    up(&mut s, &StreamUp::Unsubscribe { sid: 1 });
    eventually(EVENTUALLY, "the venue is told", || count(&rig.log, "unsubscribe") == 1);
    venue.quote("binance", "BTCUSDT.P", quote(13));
    up(&mut s, &StreamUp::Ping);
    assert!(
        matches!(down(&mut s), StreamDown::Heartbeat),
        "after the Unsub nothing but the heartbeat follows: the quote was not delivered"
    );
    // A repeat of an acknowledged `sid` is answered again, counting nothing.
    up(
        &mut s,
        &StreamUp::Subscribe {
            sid: 2,
            op: SubscriptionOperation::Bars { symbol: "ETHUSDT".into(), interval: "1m".into() },
            class: None,
        },
    );
    assert!(matches!(next(&mut s), StreamDown::SubscribeOk { sid: 2, k } if k == kb));
    assert_eq!(count(&rig.log, "bars:1m ETHUSDT"), 1, "the venue was asked once");
}

/// **Refusals are per-subscription and carry the tube's kinds**: a venue refusal is transient
/// (`Subscribe`), a bad spelling is permanent (`SymbolRejected`), the per-session cap is a `Budget`, and
/// the lane's key caps are `Budget`s that name which cap - none of them ends the session.
#[test]
fn refusals_are_per_subscription_and_name_what_is_full() {
    let rig = rig_with(
        StrategyLaneLimits {
            max_specs_per_session: 3,
            max_keys_per_venue: 2,
            max_keys_total: 8,
            ..StrategyLaneLimits::PRODUCTION
        },
        None,
    );
    let mut s = lane(rig.addr, "binance");
    assert!(matches!(
        refused(&mut s, 1, quotes_op("REFUSED")),
        StreamSubscriptionErrorKind::Subscribe(_)
    ));
    assert!(matches!(
        refused(&mut s, 2, quotes_op("  ")),
        StreamSubscriptionErrorKind::SymbolRejected(_)
    ));
    assert!(matches!(
        refused(
            &mut s,
            3,
            SubscriptionOperation::Bars { symbol: "X".into(), interval: "a\nb".into() }
        ),
        StreamSubscriptionErrorKind::SymbolRejected(_)
    ));
    assert_eq!(rig.hub.keys_held(), 0, "a refused subscription holds no key");
    sub_joined(&mut s, 4, quotes_op("A"), None);
    sub_joined(&mut s, 5, quotes_op("B"), None);
    match refused(&mut s, 6, quotes_op("C")) {
        StreamSubscriptionErrorKind::Budget { scope, held: 2, cap: 2 } => {
            assert!(scope.contains("on this venue"), "{scope}")
        }
        other => panic!("{other:?}"),
    }
    // The same KEY, a third subscription, and a NEW lane word of symbol A: it is restated too.
    sub_joined(&mut s, 7, SubscriptionOperation::Trades { symbol: "A".into() }, None);
    match refused(&mut s, 8, SubscriptionOperation::Trades { symbol: "B".into() }) {
        StreamSubscriptionErrorKind::Budget { scope, .. } => {
            assert!(scope.contains("subscriptions per session"), "{scope}")
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(rig.hub.keys_held(), 2);
    up(&mut s, &StreamUp::Ping);
    assert!(matches!(down(&mut s), StreamDown::Heartbeat), "the session survived every refusal");
}

/// **Two tradehub sessions on one key share ONE venue subscription and each gets the frames**; one
/// leaving leaves the other streaming, and the last leaving releases the venue.
#[test]
fn two_sessions_share_one_venue_subscription_and_one_leaving_does_not_disturb_the_other() {
    let rig = rig();
    let (mut a, mut b) = (lane(rig.addr, "binance"), lane(rig.addr, "binance"));
    let ka = sub_joined(&mut a, 1, quotes_op("BTCUSDT.P"), None);
    let kb = sub_joined(&mut b, 1, quotes_op("BTCUSDT.P"), None);
    assert_eq!(count(&rig.log, "quotes BTCUSDT.P"), 1, "ONE real subscription for two sessions");
    assert_eq!(rig.broker.real_subscriptions(), 1);
    assert_eq!(rig.hub.keys_held(), 1, "...and one key against the lane's caps");
    let venue = rig.venue_sink();
    venue.quote("binance", "BTCUSDT.P", quote(1));
    assert!(matches!(next(&mut a), StreamDown::Quote { k, .. } if k == ka));
    assert!(matches!(next(&mut b), StreamDown::Quote { k, .. } if k == kb));

    up(&mut a, &StreamUp::Close);
    assert!(matches!(next(&mut a), StreamDown::Bye(StreamByeReason::ClientClosed)));
    eventually(EVENTUALLY, "a's session ends", || rig.hub.sessions() == 1);
    assert_eq!(count(&rig.log, "unsubscribe"), 0, "b still holds the venue subscription");
    venue.quote("binance", "BTCUSDT.P", quote(2));
    assert!(matches!(next(&mut b), StreamDown::Quote { sequence: 2, .. }));
    up(&mut b, &StreamUp::Close);
    assert!(matches!(next(&mut b), StreamDown::Bye(StreamByeReason::ClientClosed)));
    eventually(EVENTUALLY, "the venue is released", || rig.broker.real_subscriptions() == 0);
    assert_eq!(count(&rig.log, "unsubscribe"), 1);
    eventually(EVENTUALLY, "every key comes back", || rig.hub.keys_held() == 0);
}

/// **A session that dies without a word gives back everything**: its venue subscription, its key, its
/// routes, its slot.
#[test]
fn a_session_that_vanishes_releases_its_keys_its_routes_and_its_slot() {
    let rig = rig();
    let mut s = lane(rig.addr, "binance");
    sub_joined(&mut s, 1, quotes_op("BTCUSDT.P"), None);
    assert_eq!(rig.broker.real_subscriptions(), 1);
    drop(s); // no Close, no Unsub
    eventually(EVENTUALLY, "the venue is released", || rig.broker.real_subscriptions() == 0);
    eventually(EVENTUALLY, "the key is given back", || rig.hub.keys_held() == 0);
    eventually(EVENTUALLY, "the slot is given back", || rig.hub.sessions() == 0);
}

/// **A depth lane a strategy wants every snapshot of is lossless on BOTH hops**: the tube's marker is
/// queued ahead of the ack for this hop, and the broker's key is raised BEFORE the venue is asked, so
/// the conflation cannot happen upstream of the floor. A `Delivery` frame after the subscribe moves both.
#[test]
fn a_depth_floor_is_stated_on_both_hops() {
    let rig = rig();
    let mut s = lane(rig.addr, "binance");
    up(
        &mut s,
        &StreamUp::Subscribe {
            sid: 1,
            op: SubscriptionOperation::Depth { symbol: "BTCUSDT.P".into() },
            class: Some(DataClientSinkDelivery::Lossless),
        },
    );
    let k = match next(&mut s) {
        StreamDown::SubscribeOk { sid: 1, k } => k,
        other => panic!("{other:?}"),
    };
    // ...and this hop announces the raised lane right behind the ack, in frame order.
    match next(&mut s) {
        StreamDown::Delivery {
            k: got,
            lane: StreamLabel::Depth,
            class: DataClientSinkDelivery::Lossless,
        } => {
            assert_eq!(got, k);
        }
        other => panic!("the raised lane must be announced: {other:?}"),
    }
    // …and the join restatement is behind both: the last thing a subscription puts on the wire.
    joined(&mut s, k, &SubscriptionOperation::Depth { symbol: "BTCUSDT.P".into() });
    let lines = rig.log.lock().unwrap().clone();
    let raised = lines.iter().position(|l| l == "set_delivery BTCUSDT.P depth Lossless");
    let asked = lines.iter().position(|l| l.starts_with("depth BTCUSDT.P ->"));
    assert!(
        raised.is_some_and(|r| asked.is_some_and(|a| r < a)),
        "the venue-side floor precedes the subscribe: {lines:?}"
    );
    // Relaxed after the subscribe: both hops move.
    up(&mut s, &StreamUp::Delivery { sid: 1, class: DataClientSinkDelivery::Conflatable });
    match next(&mut s) {
        StreamDown::Delivery {
            k: got,
            lane: StreamLabel::Depth,
            class: DataClientSinkDelivery::Conflatable,
        } => {
            assert_eq!(got, k);
        }
        other => panic!("{other:?}"),
    }
    eventually(EVENTUALLY, "the broker relaxes the key", || {
        count(&rig.log, "set_delivery BTCUSDT.P depth Conflatable") == 1
    });
}

// ---- the slow consumer: bounded, disclosed, never blocking the venue -----------------------------------------

/// **A tradehub that stops reading never blocks a venue pump, and nothing it misses is missed silently**
/// (drill M4). The reader is stalled while the venue floods; the longest single sink call is measured and
/// stays far under anything a pump would feel; and when the tradehub reads again every emitted quote is
/// accounted for - delivered, or counted in a `Gap` that names it - with the stream continuous across
/// each gap. The longest call is printed for the notes.
#[test]
fn a_stalled_tradehub_never_blocks_the_venue_pump_and_every_loss_is_a_counted_gap() {
    const FLOOD: u64 = 200_000;
    const SLOW_CALL: Duration = Duration::from_millis(100);
    const MAX_SLOW_CALLS: usize = 20;
    const MAX_CALL_CEILING: Duration = Duration::from_secs(2);
    let rig = rig_with(
        StrategyLaneLimits {
            queue_frames: 64,
            queue_bytes: 256 * 1024,
            lapse_budget: 1_000_000,
            write_timeout: Duration::from_secs(60),
            heartbeat: Duration::from_millis(500),
            ..StrategyLaneLimits::PRODUCTION
        },
        None,
    );
    let mut s = lane(rig.addr, "binance");
    let k = sub_joined(&mut s, 1, quotes_op("FLOOD"), None);
    let venue = rig.venue_sink();
    let mut longest = Duration::ZERO;
    let mut slow = 0usize;
    let started = Instant::now();
    for i in 0..FLOOD {
        let t = Instant::now();
        venue.quote("binance", "FLOOD", quote(i as i64));
        let took = t.elapsed();
        longest = longest.max(took);
        slow += usize::from(took > SLOW_CALL);
    }
    let flooded_in = started.elapsed();
    println!(
        "HOP-DRILL stalled-reader longest_sink_call_us={} calls_over_100ms={slow} flood_total_ms={}",
        longest.as_micros(),
        flooded_in.as_millis()
    );
    // Wall time on a loaded runner includes the scheduler taking the TEST thread off its core: one 176 ms
    // call failed a release CI run, and a bare "longest < 100 ms" fails 3 runs in 30 at 2x CPU
    // oversubscription (which also costs up to 9 slow calls, hence the allowance).
    // A pump that really waits on the stalled reader (a full queue, a held lock) does it REPEATEDLY, or for
    // the whole write timeout (60 s here), so the bound is on how many calls were slow plus a ceiling.
    // shortcut: up to MAX_SLOW_CALLS stalls of any length below the ceiling are indistinguishable from
    // preemption; upgrade to per-call thread CPU time plus a wait-state probe if that case must be caught.
    assert!(
        slow <= MAX_SLOW_CALLS,
        "{slow} of {FLOOD} sink calls took over {SLOW_CALL:?} under a stalled reader (longest {longest:?})"
    );
    assert!(
        longest < MAX_CALL_CEILING,
        "a venue pump's sink call took {longest:?} under a stalled reader"
    );

    // Now read everything: delivered + dropped must be exactly what the venue emitted.
    let (mut delivered, mut dropped_total, mut last_sequence, mut gaps) = (0u64, 0u64, 0u64, 0u32);
    while last_sequence < FLOOD {
        match next(&mut s) {
            StreamDown::Quote { k: got, sequence: seq, .. } => {
                assert_eq!(got, k);
                assert!(seq > last_sequence, "sequence runs forward: {seq} after {last_sequence}");
                delivered += 1;
                last_sequence = seq;
            }
            StreamDown::Gap {
                verb: StreamGapVerb::Quote,
                dropped,
                from_sequence: from_seq,
                to_sequence: to_seq,
                ..
            } => {
                assert_eq!(from_seq, last_sequence, "the gap starts where delivery stopped");
                assert_eq!(from_seq + dropped + 1, to_seq, "the gap names every frame it dropped");
                dropped_total += dropped;
                gaps += 1;
                last_sequence = to_seq - 1;
            }
            StreamDown::Status { .. } | StreamDown::Delay { .. } => {}
            other => panic!("unexpected frame: {other:?}"),
        }
    }
    assert!(
        gaps > 0 && dropped_total > 0,
        "a 64-frame queue under a {FLOOD}-quote flood must have lost frames"
    );
    assert_eq!(delivered + dropped_total, FLOOD, "every quote is delivered or counted");
}

/// **A reader that keeps lapsing is ended with `Bye(TooSlow)`** and its slot comes back: the client
/// reconnects and replays, which is the lane's repair.
#[test]
fn a_reader_that_keeps_losing_frames_is_ended_too_slow() {
    let rig = rig_with(
        StrategyLaneLimits {
            queue_frames: 16,
            queue_bytes: 64 * 1024,
            lapse_budget: 0,
            write_timeout: Duration::from_secs(60),
            ..StrategyLaneLimits::PRODUCTION
        },
        None,
    );
    let mut s = lane(rig.addr, "binance");
    sub_joined(&mut s, 1, quotes_op("FLOOD"), None);
    let venue = rig.venue_sink();
    for i in 0..60_000 {
        venue.quote("binance", "FLOOD", quote(i));
    }
    let mut bye = None;
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        if let StreamDown::Bye(reason) = down(&mut s) {
            bye = Some(reason);
            break;
        }
    }
    assert!(matches!(bye, Some(StreamByeReason::TooSlow { .. })), "{bye:?}");
    eventually(EVENTUALLY, "the slot comes back", || rig.hub.sessions() == 0);
}

// ---- the tick pump ------------------------------------------------------------------------------------------------

/// A fake gatewayhub stream peer: accepts one tube connection, acknowledges `Sub`s, streams a quote and
/// a trade per `Ticks` subscription, and records what it was asked.
struct FakePump {
    addr: SocketAddr,
    asked: Arc<Mutex<Vec<String>>>,
}

impl FakePump {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().unwrap();
        let asked: Arc<Mutex<Vec<String>>> = Arc::default();
        let log = Arc::clone(&asked);
        thread::spawn(move || {
            let Ok((mut s, _)) = listener.accept() else { return };
            while let Ok(frame) = read_capped::<_, StreamUp>(&mut s, 64 * 1024) {
                match frame {
                    StreamUp::Subscribe {
                        sid,
                        op: SubscriptionOperation::Ticks { symbol, tick_size },
                        ..
                    } => {
                        log.lock().unwrap().push(format!("ticks {symbol} {tick_size}"));
                        let _ =
                            write_capped(&mut s, &StreamDown::SubscribeOk { sid, k: 1 }, 1 << 20);
                        let _ = write_capped(
                            &mut s,
                            &StreamDown::Quote { k: 1, sequence: 1, quote: quote(100) },
                            1 << 20,
                        );
                        let _ = write_capped(
                            &mut s,
                            &StreamDown::Trade { k: 1, sequence: 1, trade: trade(101) },
                            1 << 20,
                        );
                    }
                    StreamUp::Unsubscribe { sid } => {
                        log.lock().unwrap().push(format!("unsub {sid}"))
                    }
                    StreamUp::Close => {
                        let _ = write_capped(
                            &mut s,
                            &StreamDown::Bye(StreamByeReason::ClientClosed),
                            1 << 20,
                        );
                        break;
                    }
                    _ => {}
                }
            }
        });
        Self { addr, asked }
    }
}

fn pump_table(pump: &FakePump) -> StrategyLaneTicksBuilder {
    let addr = pump.addr.to_string();
    Box::new(
        move |venue: &str, sink: Arc<dyn DataClientSink>| -> Result<DataClientStream, String> {
            DataClientStream::new(DirectOpener(addr.clone()), venue, sink)
                .map_err(|e| e.to_string())
        },
    )
}

/// **The pump lane**: a `Ticks` subscription is answered by the session's own pump client (never the
/// broker), the pump's quote and trade come back on the symbol's key, the grid reaches the pump, an
/// `Unsubscribe` reaches it, and a bad grid, a venue with no pump and a build with no pump are each refused
/// by name - the first two permanently.
#[test]
fn a_ticks_subscription_is_served_by_the_sessions_own_pump_and_refused_where_there_is_none() {
    let pump = FakePump::start();
    let rig = rig_with(StrategyLaneLimits::PRODUCTION, Some(pump_table(&pump)));
    let mut s = lane(rig.addr, "binance");
    let k = sub(
        &mut s,
        1,
        SubscriptionOperation::Ticks { symbol: "BTCUSDT.P".into(), tick_size: 0.1 },
        None,
    );
    assert_eq!(*pump.asked.lock().unwrap(), vec!["ticks BTCUSDT.P 0.1".to_string()]);
    let (mut got_quote, mut got_trade) = (false, false);
    while !(got_quote && got_trade) {
        match next(&mut s) {
            StreamDown::Quote { k: got, .. } => {
                assert_eq!(got, k);
                got_quote = true;
            }
            StreamDown::Trade { k: got, .. } => {
                assert_eq!(got, k);
                got_trade = true;
            }
            StreamDown::Status { .. } => {}
            other => panic!("{other:?}"),
        }
    }
    assert_eq!(rig.broker.real_subscriptions(), 0, "the pump never touched the broker");
    assert_eq!(count(&rig.log, "->"), 0, "...nor the broker's venue client");

    match refused(
        &mut s,
        2,
        SubscriptionOperation::Ticks { symbol: "BTCUSDT.P".into(), tick_size: 0.0 },
    ) {
        StreamSubscriptionErrorKind::SymbolRejected(why) => {
            assert!(why.contains("tick size"), "{why}")
        }
        other => panic!("{other:?}"),
    }
    up(&mut s, &StreamUp::Unsubscribe { sid: 1 });
    eventually(EVENTUALLY, "the pump is told", || {
        pump.asked.lock().unwrap().iter().any(|l| l.starts_with("unsub"))
    });

    // A venue the lane serves that has no pump (hyperliquid), and a build with no pump table at all.
    let mut hl = lane(rig.addr, "hyperliquid");
    match refused(&mut hl, 1, SubscriptionOperation::Ticks { symbol: "BTC".into(), tick_size: 0.5 })
    {
        StreamSubscriptionErrorKind::Unsupported(why) => {
            assert!(why.contains("no tick pump"), "{why}")
        }
        other => panic!("{other:?}"),
    }
    let bare = rig_with(StrategyLaneLimits::PRODUCTION, None);
    let mut s2 = lane(bare.addr, "binance");
    match refused(
        &mut s2,
        1,
        SubscriptionOperation::Ticks { symbol: "BTCUSDT.P".into(), tick_size: 0.1 },
    ) {
        StreamSubscriptionErrorKind::Unsupported(why) => {
            assert!(why.contains("serves no tick pump"), "{why}")
        }
        other => panic!("{other:?}"),
    }
    // A pump that cannot be built is a TRANSIENT refusal (the table's reason, verbatim).
    let broken = rig_with(StrategyLaneLimits::PRODUCTION, Some(never_dial()));
    let mut s3 = lane(broken.addr, "binance");
    match refused(
        &mut s3,
        1,
        SubscriptionOperation::Ticks { symbol: "BTCUSDT.P".into(), tick_size: 0.1 },
    ) {
        StreamSubscriptionErrorKind::Subscribe(why) => {
            assert!(why.contains("never dials"), "{why}")
        }
        other => panic!("{other:?}"),
    }
}

// ---- the book frame fits the ceiling the client reads under -------------------------------------------------------

/// A full thousand-level folded book rides the lane whole: it fits the ceiling the session writes
/// under, so the writer never turns a real book into a disclosed hole.
#[test]
fn a_full_book_is_delivered_whole_under_the_lane_ceiling() {
    let rig = rig();
    let mut s = lane(rig.addr, "binance");
    let k = sub_joined(&mut s, 1, SubscriptionOperation::Book { symbol: "BTCUSDT.P".into() }, None);
    let levels = |base: f64| -> Vec<L2BookLevel> {
        (0..1000)
            .map(|i| L2BookLevel::new(base + f64::from(i), 1.234_567_890_123_456_7e-3))
            .collect()
    };
    let mut book = L2Book::new(1.0);
    book.apply_snapshot(1, &levels(1.0e6), &levels(2.0e6));
    rig.venue_sink().book("binance", "BTCUSDT.P", Arc::new(book));
    match next(&mut s) {
        StreamDown::Book { k: got, sequence: 1, book } => {
            assert_eq!(got, k);
            assert_eq!(book.best_bid().map(|l| l.price), Some(1.0e6 + 999.0));
        }
        other => panic!("{other:?}"),
    }
}

/// **A tradehub that feeds the whole roster is served** (W12 DOUBT 2): it dials ONE connection per feed, so
/// it opens one per roster venue, the Hyperliquid testnet beside the mainnet, and one per Polymarket token
/// (ten more here), at once. With the first cap (four) its fifth open was refused. All of them are served
/// at the shipped limits, the lane is held to `STRATEGY_LANE_MAX_STREAM_CONNECTIONS` and not past it, and a slot given back
/// is a slot again.
#[test]
fn a_tradehub_opening_every_roster_venue_is_served_and_the_lane_stops_at_its_cap() {
    let roster: Vec<String> = vike_model::VENUES.iter().map(|v| (*v).to_string()).collect();
    assert_eq!(roster.len(), 14, "the roster the cap is argued for");
    let rig = rig_venues(roster.clone(), StrategyLaneLimits::PRODUCTION, None);
    let mut held: Vec<TcpStream> = Vec::new();
    for venue in &roster {
        held.push(lane(rig.addr, venue));
    }
    held.push(lane_on(rig.addr, "hyperliquid", Some("testnet")));
    for _ in 0..10 {
        held.push(lane(rig.addr, "polymarket"));
    }
    assert_eq!(held.len(), 25);
    eventually(EVENTUALLY, "every dial holds a slot", || rig.hub.sessions() == 25);

    // Up to the cap, and not past it: the next open is a CAP refusal.
    let cap = vike_datahub::strat::limits::STRATEGY_LANE_MAX_STREAM_CONNECTIONS;
    while held.len() < cap {
        held.push(lane(rig.addr, "binance"));
    }
    eventually(EVENTUALLY, "the lane is full", || rig.hub.sessions() == cap);
    let mut t = authed_stream(rig.addr, &trade_keys(), Scope::Trade);
    match exchange(&mut t, &Request::StrategyOpen { venue: "binance".into(), network: None }) {
        Response::Error(why) => {
            assert!(why.contains(vike_market_stream::market::CAP_MARK), "{why}")
        }
        other => panic!("past the cap: {other:?}"),
    }
    // One goes, one comes.
    let mut gone = held.pop().expect("a session");
    up(&mut gone, &StreamUp::Close);
    assert!(matches!(next(&mut gone), StreamDown::Bye(StreamByeReason::ClientClosed)));
    eventually(EVENTUALLY, "the slot is given back", || rig.hub.sessions() == cap - 1);
    assert!(matches!(
        exchange(&mut t, &Request::StrategyOpen { venue: "binance".into(), network: None }),
        Response::StrategyOpened { .. }
    ));
}

/// **One strategy session's shutdown never drains a venue client another session of the route is using**
/// (review finding 12): every strategy session is ONE holder to the broker, which counts holders and so
/// would drain the shared client on the first session's `BeginShutdown` or end (a reconnect overlap),
/// refusing the other's subscribes. Only the route's SOLE session may.
#[test]
fn a_session_shutting_down_does_not_drain_a_client_another_session_uses() {
    let rig = rig();
    let (mut a, mut b) = (lane(rig.addr, "binance"), lane(rig.addr, "binance"));
    sub_joined(&mut a, 1, quotes_op("BTCUSDT.P"), None);
    sub_joined(&mut b, 1, quotes_op("BTCUSDT.P"), None);
    up(&mut a, &StreamUp::BeginShutdown);
    up(&mut a, &StreamUp::Close);
    assert!(matches!(next(&mut a), StreamDown::Bye(StreamByeReason::ClientClosed)));
    eventually(EVENTUALLY, "a's session ends", || rig.hub.sessions() == 1);
    assert_eq!(count(&rig.log, "begin_shutdown"), 0, "b still uses the shared client");
    // b can still subscribe on it.
    sub_joined(&mut b, 2, quotes_op("ETHUSDT.P"), None);
    // The sole session's shutdown does reach the client.
    up(&mut b, &StreamUp::BeginShutdown);
    eventually(EVENTUALLY, "the sole session drains it", || count(&rig.log, "begin_shutdown") >= 1);
}

/// **A session that joins a broker lane is told where the lane stands.** The venue's client discloses
/// transitions only, so a tradehub back after a datahub restart (whose old session ended on a `GapStart`
/// whose `Live` fell into the dead socket) hears nothing about a lane that is up and keeps a dead-man
/// window open for it. Right after the `SubscribeOk`: `Live` when the lane has disclosed nothing, the STANDING
/// `GapStart` (with its stamp) while an outage is open, and the last `Live` after it.
#[test]
fn a_session_that_joins_a_lane_is_told_where_it_stands() {
    let rig = rig();
    let quotes = StreamWordWire::Lane(StreamLabel::Quotes);
    let joined = |s: &mut TcpStream, sid: u64| -> (u32, StreamStatus) {
        up(s, &StreamUp::Subscribe { sid, op: quotes_op("BTCUSDT.P"), class: None });
        let k = match next(s) {
            StreamDown::SubscribeOk { sid: got, k } => {
                assert_eq!(got, sid);
                k
            }
            other => panic!("expected SubscribeOk, got {other:?}"),
        };
        match next(s) {
            StreamDown::Status { k: got, stream, status } => {
                assert_eq!(
                    (got, stream),
                    (k, quotes.clone()),
                    "the restatement is about the joined lane"
                );
                (k, status)
            }
            other => panic!("expected the joiner's status, got {other:?}"),
        }
    };

    let mut a = lane(rig.addr, "binance");
    let (_, first) = joined(&mut a, 1);
    assert_eq!(
        first,
        StreamStatus::Live { gap_started_timestamp_ms: None },
        "nothing disclosed: up"
    );

    // An outage the first session hears; a session joining DURING it is told the gap and its stamp.
    let venue = rig.venue_sink();
    venue.stream_status(
        "binance",
        "BTCUSDT.P",
        "quotes",
        StreamStatus::GapStart { at_timestamp_ms: 7 },
    );
    assert!(matches!(
        next(&mut a),
        StreamDown::Status { status: StreamStatus::GapStart { at_timestamp_ms: 7 }, .. }
    ));
    let mut b = lane(rig.addr, "binance");
    let (_, during) = joined(&mut b, 1);
    assert_eq!(
        during,
        StreamStatus::GapStart { at_timestamp_ms: 7 },
        "a joiner during an outage hears it"
    );

    // After the recovery a joiner is told the recovery, which closes any window it holds.
    venue.stream_status(
        "binance",
        "BTCUSDT.P",
        "quotes",
        StreamStatus::Live { gap_started_timestamp_ms: Some(7) },
    );
    for s in [&mut a, &mut b] {
        assert!(matches!(
            next(s),
            StreamDown::Status {
                status: StreamStatus::Live { gap_started_timestamp_ms: Some(7) },
                ..
            }
        ));
    }
    let mut c = lane(rig.addr, "binance");
    let (_, after) = joined(&mut c, 1);
    assert_eq!(after, StreamStatus::Live { gap_started_timestamp_ms: Some(7) });
}
