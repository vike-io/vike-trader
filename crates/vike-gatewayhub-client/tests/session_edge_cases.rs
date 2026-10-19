#![cfg(feature = "data")]
//! The edge cases an independent review of the session code turned up, each held by a test that fails
//! on the defect it names: re-entrancy from a sink callback, a late ack, a stalled dial, an over-long
//! argument, a panicking sink, a volunteered `Bye(ClientClosed)`, the lookup bound, an over-long
//! request and a feed stopped mid-dial.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use vike_catalog::{CatalogProvider, SearchFilter};
use vike_data::{DataClient, LiveDataError, LiveDataSink, RecordingSink};
use vike_gatewayhub_client::catalog::RemoteCatalog;
use vike_gatewayhub_client::client::GatewayTimings;
use vike_gatewayhub_client::credential::{CredentialPresence, credential_presence};
use vike_gatewayhub_client::opener::GatewayOpener;
use vike_gatewayhub_client::options::RemoteOptionsFeed;
use vike_gatewayhub_client::proto::{Request, Response, WireCatalogMode};
use vike_market_stream::market::RemoteDataClient;
use vike_market_stream::wire::{ByeReason, StreamDown, StreamUp};
use vike_marketdata::{Bar, L2Book, QuoteTick, TradeTick};
use vike_model::{AssetClass, StreamStatus};

#[allow(dead_code, clippy::allow_attributes)]
#[path = "support/fake_peer.rs"]
mod fake_peer;

use fake_peer::{FakePeer, PeerConfig, Step, SubAnswer, fast_timings, link_to};

const WAIT: Duration = Duration::from_secs(8);

fn quote(ts: i64) -> QuoteTick {
    QuoteTick {
        ts,
        local_ts: ts,
        bid: 1.0,
        ask: 2.0,
        bid_size: 1.0,
        ask_size: 1.0,
        symbol: String::new(),
    }
}

/// A sink that holds the client and calls back into it from the READER thread, which is what a
/// consumer that reacts to a status by subscribing or closing would do.
struct ReentrantSink {
    client: Mutex<Option<RemoteDataClient>>,
    outcome: Mutex<Option<(String, Duration)>>,
}

impl LiveDataSink for ReentrantSink {
    fn seed_bars(&self, _: &str, _: &str, _: &str, _: Vec<Bar>) {}
    fn close_bar(&self, _: &str, _: &str, _: &str, _: Bar) {}
    fn forming_bar(&self, _: &str, _: &str, _: &str, _: Bar) {}
    fn mark_tick(&self, _: &str, _: &str, _: f64, _: i64) {}
    fn quote(&self, _: &str, _: &str, _: QuoteTick) {
        let started = Instant::now();
        let mut guard = self.client.lock().unwrap();
        let client = guard.as_mut().expect("the test stored the client");
        let refusal = match client.subscribe_trades("Y") {
            Err(LiveDataError::Subscribe(text)) => text,
            other => format!("UNEXPECTED {other:?}"),
        };
        client.shutdown(); // from the reader thread: must neither wait for itself nor join itself
        *self.outcome.lock().unwrap() = Some((refusal, started.elapsed()));
    }
    fn trade(&self, _: &str, _: &str, _: TradeTick) {}
    fn book(&self, _: &str, _: &str, _: Arc<L2Book>) {}
}

#[test]
fn a_subscribe_or_shutdown_from_inside_a_sink_callback_cannot_deadlock_on_itself() {
    let peer = FakePeer::start(PeerConfig::new());
    let sink = Arc::new(ReentrantSink { client: Mutex::new(None), outcome: Mutex::new(None) });
    let dyn_sink: Arc<dyn LiveDataSink> = sink.clone();
    let mut client =
        RemoteDataClient::new(GatewayOpener::new(peer.link()), "binance", dyn_sink).unwrap();
    client.subscribe_quotes("X").unwrap();
    *sink.client.lock().unwrap() = Some(client);
    let s = peer.wait_stream(0, WAIT);
    s.send(&StreamDown::Quote { k: s.key_of("X").unwrap(), seq: 1, quote: quote(1) });

    assert!(
        FakePeer::wait_until(WAIT, || sink.outcome.lock().unwrap().is_some()),
        "the callback never returned: it deadlocked on its own reader thread"
    );
    let (refusal, took) = sink.outcome.lock().unwrap().clone().unwrap();
    assert!(refusal.contains("sink callback"), "{refusal}");
    assert!(took < Duration::from_secs(2), "neither call waited on itself: {took:?}");
    // The reader winds down once the callback returned; the test thread then drops the client.
    let client = sink.client.lock().unwrap().take().unwrap();
    drop(client);
}

/// A late ack for a subscription the client no longer holds (here: abandoned after a timeout) is
/// answered with an `Unsub`, so the daemon does not stream into a key nobody reads.
#[test]
fn a_late_ack_for_an_abandoned_subscription_is_unsubscribed_again() {
    let peer = FakePeer::start(PeerConfig::new().with_sub(|_, _| SubAnswer::Silent));
    let timings = GatewayTimings { sub_reply: Duration::from_millis(300), ..fast_timings() };
    let sink: Arc<dyn LiveDataSink> = Arc::new(RecordingSink::default());
    let mut c = RemoteDataClient::new(
        GatewayOpener::new(peer.link().with_timings(timings)),
        "binance",
        sink,
    )
    .unwrap();
    assert!(c.subscribe_quotes("X").is_err());
    let s = peer.wait_stream(0, WAIT);
    s.send(&StreamDown::SubOk { sid: 1, k: 7 }); // the ack arrives after the client gave up
    assert!(
        FakePeer::wait_until(WAIT, || {
            peer.ups().iter().filter(|u| **u == StreamUp::Unsub { sid: 1 }).count() >= 2
        }),
        "the tombstone and then the answer to the late ack: {:?}",
        peer.ups()
    );
}

/// The wait for a dial in flight is ONE deadline, not one per call: a daemon that accepted and never
/// answers costs the broker's mutex the first-connect wait once.
#[test]
fn a_stalled_first_dial_costs_the_brokers_mutex_one_wait_not_one_per_call() {
    let quiet = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = quiet.local_addr().unwrap().to_string();
    let sink: Arc<dyn LiveDataSink> = Arc::new(RecordingSink::default());
    let mut c = RemoteDataClient::new(GatewayOpener::new(link_to(&addr)), "binance", sink).unwrap();

    let first = Instant::now();
    assert!(c.subscribe_quotes("X").is_err());
    assert!(first.elapsed() < Duration::from_secs(2), "{:?}", first.elapsed());
    let again = Instant::now();
    assert!(c.subscribe_trades("X").is_err());
    assert!(
        again.elapsed() < Duration::from_millis(200),
        "the dial's one wait is spent: {:?}",
        again.elapsed()
    );
    c.shutdown();
    drop(quiet);
}

#[test]
fn an_over_long_argument_is_refused_without_costing_the_session() {
    let peer = FakePeer::start(PeerConfig::new());
    let sink: Arc<dyn LiveDataSink> = Arc::new(RecordingSink::default());
    let mut c = RemoteDataClient::new(GatewayOpener::new(peer.link()), "binance", sink).unwrap();
    c.subscribe_quotes("X").unwrap();
    let Err(LiveDataError::Subscribe(text)) = c.subscribe_bars("X", &"i".repeat(5_000)) else {
        panic!()
    };
    assert!(text.contains("framing guard"), "{text}");
    assert!(c.is_up(), "one bad call is not a lost session");
    c.subscribe_trades("Y").expect("the session still serves");
    assert_eq!(
        peer.ups().iter().filter(|u| matches!(u, StreamUp::Sub { .. })).count(),
        2,
        "the refused call sent nothing"
    );
}

/// A consumer bug: the sink panics on the QUOTES of one symbol (`bad`) and records everything else.
/// The in-process feeds died one thread per subscription, so a panic cost one `(symbol, lane)`.
struct PanicOnQuotesOf {
    bad: &'static str,
    attempts: std::sync::atomic::AtomicUsize,
    inner: RecordingSink,
}

impl PanicOnQuotesOf {
    fn new(bad: &'static str) -> Arc<Self> {
        Arc::new(Self {
            bad,
            attempts: std::sync::atomic::AtomicUsize::new(0),
            inner: RecordingSink::default(),
        })
    }
}

impl LiveDataSink for PanicOnQuotesOf {
    fn seed_bars(&self, v: &str, s: &str, i: &str, b: Vec<Bar>) {
        self.inner.seed_bars(v, s, i, b);
    }
    fn close_bar(&self, v: &str, s: &str, i: &str, b: Bar) {
        self.inner.close_bar(v, s, i, b);
    }
    fn forming_bar(&self, v: &str, s: &str, i: &str, b: Bar) {
        self.inner.forming_bar(v, s, i, b);
    }
    fn mark_tick(&self, v: &str, s: &str, p: f64, t: i64) {
        self.inner.mark_tick(v, s, p, t);
    }
    fn quote(&self, v: &str, s: &str, q: QuoteTick) {
        if s == self.bad {
            self.attempts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            panic!("a consumer bug, on purpose (session_edge_cases)");
        }
        self.inner.quote(v, s, q);
    }
    fn trade(&self, v: &str, s: &str, t: TradeTick) {
        self.inner.trade(v, s, t);
    }
    fn book(&self, v: &str, s: &str, b: Arc<L2Book>) {
        self.inner.book(v, s, b);
    }
    fn stream_status(&self, v: &str, s: &str, stream: &str, status: StreamStatus) {
        self.inner.stream_status(v, s, stream, status);
    }
}

fn trade(ts: i64) -> TradeTick {
    TradeTick {
        ts,
        local_ts: ts,
        price: 1.5,
        size: 1.0,
        is_buyer_maker: false,
        symbol: String::new(),
    }
}

/// **A panic in the sink ends ONE subscription, never the venue's session** (in-process it killed one
/// feed thread). The panicking `(symbol, lane)` is disclosed down with a `GapStart` stamped where its
/// last frame left off (so a consumer's status shows it down, not `Live` forever), the daemon is told to
/// stop it, and its late frames are dropped without running the sink again; the symbol's OTHER lane,
/// another symbol, a fresh subscription and the link itself all carry on.
#[test]
fn a_panic_in_the_sink_ends_that_one_subscription_and_the_session_serves_the_rest() {
    let peer = FakePeer::start(PeerConfig::new());
    let sink = PanicOnQuotesOf::new("X");
    let dyn_sink: Arc<dyn LiveDataSink> = sink.clone();
    let mut c =
        RemoteDataClient::new(GatewayOpener::new(peer.link()), "binance", dyn_sink).unwrap();
    c.subscribe_quotes("X").unwrap(); // sid 1: the lane whose callback panics
    c.subscribe_trades("X").unwrap(); // sid 2: the same key, another lane
    c.subscribe_quotes("Y").unwrap(); // sid 3: another key
    let s = peer.wait_stream(0, WAIT);
    let (kx, ky) = (s.key_of("X").unwrap(), s.key_of("Y").unwrap());

    s.send(&StreamDown::Quote { k: kx, seq: 1, quote: quote(10) }); // panics in the sink
    let gap_statuses = || -> Vec<(String, String, StreamStatus)> {
        sink.inner
            .recorded()
            .into_iter()
            .filter_map(|call| match call {
                vike_data::SinkCall::StreamStatus { symbol, stream, status, .. } => {
                    Some((symbol, stream, status))
                }
                _ => None,
            })
            .collect()
    };
    assert!(
        FakePeer::wait_until(WAIT, || !gap_statuses().is_empty()),
        "the dead subscription was never disclosed: {:?}",
        sink.inner.recorded()
    );
    assert_eq!(
        gap_statuses(),
        [("X".to_string(), "quotes".to_string(), StreamStatus::GapStart { at_ts_ms: 10 })],
        "one GapStart for the lane that died, at its last frame's stamp, and no Live"
    );

    // The session was never lost: the same connection, still up, and nothing replayed.
    assert!(c.is_up(), "a sink panic must not end the session");
    assert_eq!(peer.connections(), 1, "no reconnect");

    // The other lane of the SAME key, and another key, keep receiving.
    s.send(&StreamDown::Trade { k: kx, seq: 1, trade: trade(11) });
    s.send(&StreamDown::Quote { k: ky, seq: 1, quote: quote(12) });
    // A late frame of the dead lane is dropped without running the sink again.
    s.send(&StreamDown::Quote { k: kx, seq: 2, quote: quote(13) });
    s.send(&StreamDown::Quote { k: ky, seq: 2, quote: quote(14) });
    let delivered = || {
        let calls = sink.inner.recorded();
        let y_quote_14 = calls.iter().any(|c| {
            matches!(c, vike_data::SinkCall::Quote { symbol, quote, .. }
                if symbol == "Y" && quote.ts == 14)
        });
        let x_trade = calls
            .iter()
            .any(|c| matches!(c, vike_data::SinkCall::Trade { symbol, .. } if symbol == "X"));
        y_quote_14 && x_trade
    };
    assert!(
        FakePeer::wait_until(WAIT, delivered),
        "the rest of the session stopped delivering: {:?}",
        sink.inner.recorded()
    );
    assert_eq!(
        sink.attempts.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "the dead lane's late frame ran the sink again"
    );

    // The daemon was told to stop streaming the dead subscription - and only that one.
    assert!(
        FakePeer::wait_until(WAIT, || peer.ups().contains(&StreamUp::Unsub { sid: 1 })),
        "{:?}",
        peer.ups()
    );
    assert!(!peer.ups().contains(&StreamUp::Unsub { sid: 2 }));
    assert!(!peer.ups().contains(&StreamUp::Unsub { sid: 3 }));

    // Later calls are served at once (the old behaviour: "shut down (or its reader thread ended)").
    let started = Instant::now();
    c.subscribe_trades("Z").expect("the session still serves new subscriptions");
    assert!(started.elapsed() < Duration::from_secs(2), "{:?}", started.elapsed());
    assert_eq!(c.held(), 3, "X trades, Y quotes and Z trades; X quotes is forgotten");
    c.shutdown();
}

/// Only OUR `Close` earns `Bye(ClientClosed)`. One the daemon volunteers is a loss like any other —
/// not a permanent stop with no disclosure.
#[test]
fn an_unprompted_client_closed_is_a_loss_and_the_session_is_rebuilt() {
    let peer = FakePeer::start(PeerConfig::new());
    let sink = Arc::new(RecordingSink::default());
    let dyn_sink: Arc<dyn LiveDataSink> = sink.clone();
    let mut c =
        RemoteDataClient::new(GatewayOpener::new(peer.link()), "binance", dyn_sink).unwrap();
    c.subscribe_quotes("A").unwrap();
    peer.wait_stream(0, WAIT).bye(ByeReason::ClientClosed);

    let statuses = || -> Vec<StreamStatus> {
        sink.recorded()
            .into_iter()
            .filter_map(|c| match c {
                vike_data::SinkCall::StreamStatus { status, .. } => Some(status),
                _ => None,
            })
            .collect()
    };
    assert!(FakePeer::wait_until(WAIT, || statuses().len() >= 2), "{:?}", statuses());
    let st = statuses();
    assert!(matches!(st[0], StreamStatus::GapStart { .. }), "{st:?}");
    assert!(matches!(st[1], StreamStatus::Live { gap_started_ts_ms: Some(_) }), "{st:?}");
    peer.wait_stream(1, WAIT);
    assert!(FakePeer::wait_until(WAIT, || c.is_up()));
}

/// A pure lookup (a describe, a presence word) is bounded by the short lookup window, not by the five
/// minutes a REST-backed call gets: a wedged daemon is called dead in seconds.
#[test]
fn a_wedged_daemon_is_called_dead_by_the_lookup_bound_not_the_rest_bound() {
    let peer = FakePeer::start(PeerConfig::new().with_rpc(|_| vec![Step::Hang]));
    let timings = GatewayTimings {
        lookup_reply: Duration::from_millis(300),
        reply_idle: Duration::from_secs(60),
        ..fast_timings()
    };
    let link = peer.link().with_timings(timings);

    let started = Instant::now();
    let e = RemoteCatalog::open(link.clone(), "binance").unwrap_err();
    assert!(e.0.contains("gatewayhub") && e.0.contains("no reply"), "{e}");
    assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());

    let started = Instant::now();
    assert_eq!(credential_presence(&link, "oanda"), CredentialPresence::NotChecked);
    assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
}

/// A client frame over the ceiling the daemon reads is refused before its first byte, and the
/// connection it would have poisoned is untouched.
#[test]
fn an_over_long_request_is_refused_locally_and_sends_nothing() {
    let peer = FakePeer::start(PeerConfig::new().with_rpc(|req| match req {
        Request::CatalogDescribe { .. } => vec![Step::Send(Response::CatalogDescribed {
            asset_classes: vec![AssetClass::Equity],
            mode: WireCatalogMode::QueryBacked,
        })],
        _ => vec![Step::Send(Response::Error("not scripted".into()))],
    }));
    let catalog = RemoteCatalog::open(peer.link(), "ibkr").unwrap();
    let e = catalog.search_remote(&"q".repeat(70_000), &SearchFilter::default()).unwrap_err();
    assert!(e.0.contains("ceiling") && e.0.contains("nothing was sent"), "{e}");
    let fetches = peer.requests().iter().filter(|r| matches!(r, Request::Fetch { .. })).count();
    assert_eq!(fetches, 0, "the over-long search never reached the wire");
}

/// A stop must break a feed whose dial is parked in the handshake on a daemon that went quiet, not wait
/// out the handshake deadline.
#[test]
fn an_options_feed_shutdown_breaks_a_handshake_parked_on_a_quiet_daemon() {
    let quiet = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = quiet.local_addr().unwrap().to_string();
    let mut feed =
        RemoteOptionsFeed::markprice(link_to(&addr), vec!["btc_usd".into()], |_| {}).unwrap();
    std::thread::sleep(Duration::from_millis(150)); // the feed thread is now parked reading `Welcome`
    let started = Instant::now();
    feed.shutdown();
    assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
    drop(quiet);
}

/// **A long-lived session does not remember every symbol it ever bound.** The daemon never reuses a
/// key id within a session, so the reader's per-key state would otherwise grow with weeks of symbol
/// churn on one Shared session. The key is dropped once the last subscription of its symbol is
/// unsubscribed; a symbol still held by another lane keeps it; a late frame for a dropped key is
/// quiet; and the same symbol subscribed again binds a fresh one.
#[test]
fn a_key_is_given_back_when_the_last_subscription_of_its_symbol_ends() {
    let peer = FakePeer::start(PeerConfig::new());
    let sink = Arc::new(RecordingSink::default());
    let dyn_sink: Arc<dyn LiveDataSink> = sink.clone();
    let mut c = RemoteDataClient::new(
        GatewayOpener::new(link_to(&peer.addr_string())),
        "binance",
        dyn_sink,
    )
    .unwrap();
    let ids: Vec<_> = (0..20).map(|i| c.subscribe_quotes(&format!("SYM{i}")).unwrap()).collect();
    let trades = c.subscribe_trades("SYM0").unwrap();
    let session = peer.wait_stream(0, WAIT);
    assert!(FakePeer::wait_until(WAIT, || c.bound_keys() == 20), "bound {}", c.bound_keys());

    // SYM0 keeps its key while its trades lane is held; the other 19 symbols lose theirs.
    let first = ids[0];
    let mut rest = ids;
    rest.remove(0);
    c.unsubscribe(first);
    for id in rest {
        c.unsubscribe(id);
    }
    // The reader looks at its next frame; a heartbeat is one.
    let beat = || session.send(&StreamDown::Heartbeat);
    assert!(
        FakePeer::wait_until(WAIT, || {
            beat();
            c.bound_keys() == 1
        }),
        "the 19 unsubscribed symbols kept their keys: {}",
        c.bound_keys()
    );
    let k = session.key_of("SYM0").expect("SYM0 was acknowledged");
    session.send(&StreamDown::Quote { k, seq: 1, quote: quote(7) });
    assert!(
        FakePeer::wait_until(WAIT, || !sink.recorded().is_empty()),
        "SYM0 still has a subscription, so its key still delivers"
    );

    // The last subscription of SYM0 goes: its key goes, and a frame still in flight is dropped.
    c.unsubscribe(trades);
    assert!(FakePeer::wait_until(WAIT, || {
        beat();
        c.bound_keys() == 0
    }));
    let seen = sink.recorded().len();
    session.send(&StreamDown::Quote { k, seq: 2, quote: quote(8) });
    session.send(&StreamDown::Heartbeat);
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(sink.recorded().len(), seen, "a frame for a dropped key reached the sink");

    // The same symbol again binds a fresh key and delivers.
    c.subscribe_quotes("SYM0").unwrap();
    assert!(FakePeer::wait_until(WAIT, || c.bound_keys() == 1));
}
