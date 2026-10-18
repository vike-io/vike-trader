//! The strategy lane's CLIENTS against a real gatewayhub server whose lanes are scripted: what the
//! datahub's venue table (`vike_datahub::feeds::venues`) asks the gatewayhub for when the tradehub's
//! strategy wants Hyperliquid's test network, a credentialed venue, or a CEX tick pump.
//!
//! The gatewayhub here is the real `serve` over `FeaturedLane`, a scripted data lane that ALSO
//! advertises the strategy lane's strings (`stream_network`, `gw_pump`, `cs_venue=`), which the
//! testkit's `ScriptedData` does not. Its log is the witness: the datahub reads no venue credential,
//! so the only thing to check is WHAT it asks, under which key.

use std::net::TcpListener;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
#[cfg(feature = "venue-hyperliquid")]
use std::time::Duration;

#[cfg(feature = "venue-hyperliquid")]
use vike_data::SinkCall;
use vike_data::{DataClient, LiveDataSink, RecordingSink};
use vike_datahub::feeds::venues::{
    CREDENTIALED_VENUES, PUMP_VENUES, build_client, build_ticks_client, client_label, split_label,
    strat_venues,
};
use vike_gatewayhub::data::{DataLane, Holder, PumpLease, PumpSink};
use vike_gatewayhub::server::accept::serve;
use vike_gatewayhub::server::bind::bind_loopback;
use vike_gatewayhub::server::lanes::Lanes;
use vike_gatewayhub::server::limits::ServerConfig;
use vike_gatewayhub::testkit::data::ScriptedData;
use vike_gatewayhub::testkit::gateway::{fast_timings, keys, read_only_keys};
use vike_gatewayhub::testkit::rest::ScriptedRest;
use vike_gatewayhub_client::client::GatewayLink;
use vike_hub_proto::auth::HubKeys;

/// A scripted data lane that advertises what `ScriptedData` does not: networks, a pump and credentialed
/// venues. Every open it serves is recorded as a line (`open binance`, `open_on hyperliquid testnet`,
/// `credentialed alpaca Write`, `pump binance BTCUSDT.P 0.1`).
#[derive(Clone)]
struct FeaturedLane {
    inner: ScriptedData,
    networks: Vec<String>,
    pumps: Vec<String>,
    credentialed: Vec<String>,
    seen: Arc<Mutex<Vec<String>>>,
}

impl FeaturedLane {
    fn new(venues: &[&str]) -> Self {
        Self {
            inner: ScriptedData::new(venues),
            networks: Vec::new(),
            pumps: Vec::new(),
            credentialed: Vec::new(),
            seen: Arc::default(),
        }
    }
    fn with_networks(mut self, venues: &[&str]) -> Self {
        self.networks = venues.iter().map(|v| (*v).to_string()).collect();
        self
    }
    fn with_pumps(mut self, venues: &[&str]) -> Self {
        self.pumps = venues.iter().map(|v| (*v).to_string()).collect();
        self
    }
    fn with_credentialed(mut self, venues: &[&str]) -> Self {
        self.credentialed = venues.iter().map(|v| (*v).to_string()).collect();
        self
    }
    fn seen(&self) -> Vec<String> {
        self.seen.lock().unwrap().clone()
    }
    fn note(&self, line: String) {
        self.seen.lock().unwrap().push(line);
    }
}

type BuiltClient = Result<Box<dyn DataClient + Send>, String>;

impl DataLane for FeaturedLane {
    fn venues(&self) -> Vec<String> {
        self.inner.venues()
    }
    fn open(&self, venue: &str, holder: &Holder, sink: Arc<dyn LiveDataSink>) -> BuiltClient {
        self.note(format!("open {venue}"));
        self.inner.open(venue, holder, sink)
    }
    fn network_venues(&self) -> Vec<String> {
        self.networks.clone()
    }
    fn open_on(
        &self,
        venue: &str,
        network: &str,
        holder: &Holder,
        sink: Arc<dyn LiveDataSink>,
    ) -> BuiltClient {
        self.note(format!("open_on {venue} {network}"));
        self.inner.open(venue, holder, sink)
    }
    fn pump_venues(&self) -> Vec<String> {
        self.pumps.clone()
    }
    fn open_pump(
        &self,
        venue: &str,
        _holder: &Holder,
        symbol: &str,
        tick_size: f64,
        _sink: PumpSink,
    ) -> Result<PumpLease, String> {
        self.note(format!("pump {venue} {symbol} {tick_size}"));
        Ok(PumpLease::new(()))
    }
    fn credentialed_venues(&self) -> Vec<String> {
        self.credentialed.clone()
    }
    fn open_credentialed(
        &self,
        venue: &str,
        holder: &Holder,
        sink: Arc<dyn LiveDataSink>,
    ) -> BuiltClient {
        self.note(format!("credentialed {venue} {:?}", holder.scope));
        self.inner.open(venue, holder, sink)
    }
}

/// The real gatewayhub `serve` over a [`FeaturedLane`], on an ephemeral loopback port.
struct Gateway {
    addr: String,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<std::io::Result<()>>>,
}

impl Gateway {
    fn start(lane: &FeaturedLane) -> Self {
        let listener: TcpListener = bind_loopback("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr").to_string();
        let stop = Arc::new(AtomicBool::new(false));
        let lanes = Lanes { data: Arc::new(lane.clone()), rest: Arc::new(ScriptedRest::new()) };
        let flag = Arc::clone(&stop);
        let thread = std::thread::Builder::new()
            .name("gwh-featured".into())
            .spawn(move || serve(listener, keys(), lanes, ServerConfig::PRODUCTION, flag))
            .expect("spawn");
        Self { addr, stop, thread: Some(thread) }
    }
    fn link_with(&self, keys: HubKeys) -> GatewayLink {
        GatewayLink::new(self.addr.clone(), keys).with_timings(fast_timings())
    }
    fn link(&self) -> GatewayLink {
        self.link_with(keys())
    }
}

impl Drop for Gateway {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[cfg(feature = "venue-hyperliquid")]
fn quote_call(venue: &str, ts: i64) -> SinkCall {
    SinkCall::Quote {
        venue: venue.to_string(),
        symbol: "BTC".to_string(),
        quote: vike_marketdata::QuoteTick {
            ts,
            local_ts: 0,
            bid: 1.0,
            ask: 2.0,
            bid_size: 1.0,
            ask_size: 1.0,
            symbol: String::new(),
        },
    }
}

#[cfg(feature = "venue-hyperliquid")]
fn wait_until(what: &str, cond: impl Fn() -> bool) {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        if cond() {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("timed out waiting for: {what}");
}

// ---- the broker's name for a client --------------------------------------------------------------------

#[test]
fn a_clients_name_carries_its_network_and_splits_back() {
    assert_eq!(client_label("hyperliquid", None), "hyperliquid");
    assert_eq!(client_label("hyperliquid", Some("testnet")), "hyperliquid@testnet");
    assert_eq!(split_label("hyperliquid"), ("hyperliquid", None));
    assert_eq!(split_label("hyperliquid@testnet"), ("hyperliquid", Some("testnet")));
}

// ---- (a) Hyperliquid's test network ------------------------------------------------------------------------

/// **The test network is a stream that NAMES the network**: the gatewayhub is asked to build the venue's
/// client for `testnet` (`open_on`), the main network is the plain open, and the frames the test-network
/// client delivers carry the broker's name for it, so the lane routes them apart from the main network's.
#[cfg(feature = "venue-hyperliquid")]
#[test]
fn a_test_network_client_asks_the_gatewayhub_for_that_network() {
    let lane = FeaturedLane::new(&["hyperliquid"]).with_networks(&["hyperliquid"]);
    let gateway = Gateway::start(&lane);
    let link = gateway.link();

    let main_sink = Arc::new(RecordingSink::default());
    let test_sink = Arc::new(RecordingSink::default());
    let mut main = build_client("hyperliquid", main_sink.clone(), &link).expect("main client");
    let mut test =
        build_client("hyperliquid@testnet", test_sink.clone(), &link).expect("test client");
    main.subscribe_quotes("BTC").expect("main subscribe");
    test.subscribe_quotes("BTC").expect("test subscribe");

    let seen = lane.seen();
    assert!(seen.contains(&"open hyperliquid".to_string()), "{seen:?}");
    assert!(
        seen.contains(&"open_on hyperliquid testnet".to_string()),
        "the network must reach the gatewayhub: {seen:?}"
    );
    assert_eq!(seen.len(), 2, "one open per client: {seen:?}");

    // Session 0 is the main client's, session 1 the test network's (the lane opened them in that order).
    lane.inner.session(0).emit(&quote_call("hyperliquid", 1));
    lane.inner.session(1).emit(&quote_call("hyperliquid", 2));
    wait_until("both clients deliver", || {
        !main_sink.quotes().is_empty() && !test_sink.quotes().is_empty()
    });
    // The engine labels what it delivers with the name the client was built under; the gatewayhub's
    // session knew only the venue.
    assert_eq!(main_sink.quotes()[0].0, "hyperliquid");
    assert_eq!(test_sink.quotes()[0].0, "hyperliquid@testnet");
}

/// **A gatewayhub that does not advertise `stream_network` is never sent a network**: it would ignore the
/// field and open the MAIN network, so a test-network strategy would trade on mainnet prices. The open
/// fails, naming the string, and no stream is opened.
#[cfg(feature = "venue-hyperliquid")]
#[test]
fn a_gatewayhub_without_stream_network_is_never_sent_a_network() {
    let lane = FeaturedLane::new(&["hyperliquid"]);
    let gateway = Gateway::start(&lane);
    let mut test =
        build_client("hyperliquid@testnet", Arc::new(RecordingSink::default()), &gateway.link())
            .expect("the client builds; its reader dials");
    let err = test.subscribe_quotes("BTC").expect_err("no stream_network, no stream");
    assert!(err.to_string().contains("stream_network"), "{err}");
    assert!(lane.seen().is_empty(), "nothing was opened: {:?}", lane.seen());
}

/// A network on a venue that takes none is refused at the table, before any socket is asked.
#[test]
fn a_network_on_another_venue_is_refused_by_the_table() {
    let lane = FeaturedLane::new(&["binance"]).with_networks(&["hyperliquid"]);
    let gateway = Gateway::start(&lane);
    let err = match build_client(
        "binance@testnet",
        Arc::new(RecordingSink::default()),
        &gateway.link(),
    ) {
        Ok(_) => panic!("binance takes no network"),
        Err(e) => e,
    };
    assert!(err.contains("takes no network") && err.contains("binance"), "{err}");
    assert!(lane.seen().is_empty());
}

// ---- (b) the credentialed venues ----------------------------------------------------------------------------

/// **The four credentialed venues are served by the lane** and are the lane's alone: the table builds them,
/// each as a credentialed stream under the CONTROL key (`Scope::Write`), and the datahub names no
/// credential, label or tier anywhere in the ask.
#[test]
fn a_credentialed_venue_is_a_credentialed_stream_under_the_control_key() {
    assert_eq!(CREDENTIALED_VENUES, ["alpaca", "ctrader", "ig", "oanda"]);
    for v in CREDENTIALED_VENUES {
        assert!(strat_venues().contains(&v), "{v} is served by the strategy lane");
    }
    let lane = FeaturedLane::new(&["binance"]).with_credentialed(&CREDENTIALED_VENUES);
    let gateway = Gateway::start(&lane);
    let link = gateway.link();
    let mut clients = Vec::new();
    for v in CREDENTIALED_VENUES {
        let mut client = build_client(v, Arc::new(RecordingSink::default()), &link).expect(v);
        client.subscribe_quotes("EURUSD").unwrap_or_else(|e| panic!("{v}: {e}"));
        clients.push(client);
    }
    let seen = lane.seen();
    for v in CREDENTIALED_VENUES {
        assert!(seen.contains(&format!("credentialed {v} Write")), "{v}: {seen:?}");
    }
    assert!(seen.iter().all(|l| l.starts_with("credentialed ")), "no keyless open: {seen:?}");
}

/// **A datahub that holds no control key cannot open a credentialed stream**: the client refuses locally,
/// naming the scope, and the gatewayhub is never asked.
#[test]
fn a_credentialed_stream_needs_the_control_key() {
    let lane = FeaturedLane::new(&["binance"]).with_credentialed(&["alpaca"]);
    let gateway = Gateway::start(&lane);
    let mut client = build_client(
        "alpaca",
        Arc::new(RecordingSink::default()),
        &gateway.link_with(read_only_keys()),
    )
    .expect("builds");
    let err = client.subscribe_quotes("AAPL").expect_err("a read key cannot");
    assert!(err.to_string().contains("control"), "{err}");
    assert!(lane.seen().is_empty());
}

// ---- (c) the pump requires gw_pump --------------------------------------------------------------------------

/// **The pump client asks for a pump session**: a gatewayhub that hosts one serves the `Ticks`
/// subscription, and the grid reaches it.
#[test]
fn a_pump_client_is_served_by_a_gatewayhub_that_hosts_the_pump() {
    let lane = FeaturedLane::new(&["binance"]).with_pumps(&PUMP_VENUES);
    let gateway = Gateway::start(&lane);
    let client = build_ticks_client("binance", Arc::new(RecordingSink::default()), &gateway.link());
    let mut client = match client {
        Ok(c) => c,
        Err(e) => {
            // A build with no `venue-binance` refuses by name before any socket: nothing to serve.
            assert!(e.contains("venue-binance"), "{e}");
            return;
        }
    };
    client.subscribe_ticks("BTCUSDT.P", 0.1).expect("the pump subscribes");
    assert!(
        lane.seen().contains(&"pump binance BTCUSDT.P 0.1".to_string()),
        "the grid reached the pump: {:?}",
        lane.seen()
    );
}

/// **A gatewayhub that hosts no pump (`gw_pump` not advertised) fails the pump client's open by name**,
/// where `GatewayOpener::new` would have opened a plain session whose `Ticks` the daemon then refuses.
#[test]
fn a_pump_client_requires_gw_pump() {
    let lane = FeaturedLane::new(&["binance"]);
    let gateway = Gateway::start(&lane);
    let client = build_ticks_client("binance", Arc::new(RecordingSink::default()), &gateway.link());
    let Ok(mut client) = client else { return };
    let err = client.subscribe_ticks("BTCUSDT.P", 0.1).expect_err("no pump hosted");
    assert!(err.to_string().contains("gw_pump"), "{err}");
    assert!(lane.seen().is_empty(), "no session was opened: {:?}", lane.seen());
}
