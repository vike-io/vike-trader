//! The `Welcome` advertisement and decision 0112's two legs, with the real client as the peer.
//!
//! Drift register R5: what the daemon advertises is a fact about its BUILD, independent of any
//! venue's liveness. And the string discipline: a verb whose string is not advertised is refused BY
//! NAME - by the client locally (nothing sent) and, for a client that does not look, by the server on
//! a connection that survives.

#[path = "support/fake_lanes.rs"]
#[allow(dead_code, clippy::allow_attributes)]
mod fake_lanes;
#[path = "support/harness.rs"]
#[allow(dead_code, clippy::allow_attributes)]
mod harness;
#[path = "support/raw_peer.rs"]
#[allow(dead_code, clippy::allow_attributes)]
mod raw_peer;

use std::sync::{Arc, PoisonError};

use vike_data::source::{FundingRateSource, KlineSource, SourceError};
use vike_data::{DataClient, LiveDataError, LiveDataSink};
use vike_gatewayhub::rest::RestFacts;
use vike_gatewayhub::server::limits::ServerConfig;
use vike_gatewayhub_client::catalog::RemoteCatalog;
use vike_gatewayhub_client::client::GatewayClient;
use vike_gatewayhub_client::credential::{CredentialPresence, credential_presence};
use vike_gatewayhub_client::history::{RemoteFunding, RemoteKlines, RemoteTicks};
use vike_gatewayhub_client::opener::GatewayOpener;
use vike_gatewayhub_client::options::RemoteOptions;
use vike_gatewayhub_client::proto::{
    BudgetCounter, OptionsCall, OptionsFeedSpec, Request, Response, advertised_cd_venues,
    advertised_md_venues,
};
use vike_gatewayhub_client::resolve::RemoteRollingFamily;
use vike_hub_proto::auth::Scope;
use vike_market_stream::market::RemoteDataClient;
use vike_marketdata::{Bar, L2Book, QuoteTick, TradeTick};

use fake_lanes::{FakeData, FakeRest};
use harness::{Harness, test_keys};
use raw_peer::Raw;

/// A sink that discards everything: these tests look at the handshake, not the data.
struct Noop;

impl LiveDataSink for Noop {
    fn seed_bars(&self, _v: &str, _s: &str, _i: &str, _b: Vec<Bar>) {}
    fn close_bar(&self, _v: &str, _s: &str, _i: &str, _b: Bar) {}
    fn forming_bar(&self, _v: &str, _s: &str, _i: &str, _b: Bar) {}
    fn mark_tick(&self, _v: &str, _s: &str, _p: f64, _t: i64) {}
    fn quote(&self, _v: &str, _s: &str, _q: QuoteTick) {}
    fn trade(&self, _v: &str, _s: &str, _t: TradeTick) {}
    fn book(&self, _v: &str, _s: &str, _b: Arc<L2Book>) {}
}

fn features_of(h: &Harness) -> Vec<String> {
    Raw::open(h.addr, &test_keys(), Scope::Read).features.clone()
}

fn bare(facts: RestFacts) -> Harness {
    Harness::start_with(
        FakeData::new(&["binance"]),
        FakeRest::new(facts),
        ServerConfig::PRODUCTION,
        test_keys(),
    )
}

#[test]
fn the_welcome_is_exactly_what_the_lanes_declare() {
    let h = Harness::start();
    let want: Vec<String> = [
        "gatewayhub",
        "gw_data",
        "stream_class",
        "gw_fetch",
        "gw_fetch_progress",
        "gw_fetch_cancel",
        "gw_fetch_credentialed",
        "gw_ticks",
        "gw_catalog",
        "gw_options",
        "gw_family",
        "gw_presence",
        "gw_status",
        "md_venue=binance",
        "md_venue=polymarket",
        "cd_venue=oanda",
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect();
    let got = features_of(&h);
    assert_eq!(got, want);
    assert_eq!(advertised_md_venues(&got), vec!["binance".to_string(), "polymarket".to_string()]);
    assert_eq!(advertised_cd_venues(&got), vec!["oanda".to_string()]);
}

/// **R5.** The advertisement does not move with a venue's health, a failing lane or load.
#[test]
fn the_advertisement_is_independent_of_lane_liveness() {
    let h = Harness::start();
    let before = features_of(&h);
    h.data.set_up(false);
    h.rest.set_fetch(|_, _, _, _| {
        Err(vike_gatewayhub_client::proto::FetchFailure::Fetch("the venue is unreachable".into()))
    });
    h.rest.set_presence("oanda", CredentialPresence::Unreadable);
    // Make the data lane fail for real: an open that is refused.
    let mut client = RemoteDataClient::new(GatewayOpener::new(h.link()), "binance", Arc::new(Noop))
        .expect("client");
    assert!(client.subscribe_quotes("BTCUSDT").is_err());
    assert!(RemoteKlines::new(h.link(), "binance").fetch("BTCUSDT", "1m", 0, 1).is_err());
    let after = features_of(&h);
    assert_eq!(before, after, "the Welcome followed a venue's liveness");
}

#[test]
fn a_lane_the_build_lacks_is_refused_by_the_client_naming_the_string_before_anything_is_sent() {
    let h = bare(RestFacts { klines: vec!["binance".into()], ..RestFacts::default() });
    let link = h.link();
    let text = RemoteTicks::new(link.clone())
        .fetch_quotes_range("EURUSD", 0, 1, None)
        .expect_err("no ticks");
    assert!(text.contains("\"gw_ticks\""), "{text}");

    let err = RemoteCatalog::open(link.clone(), "binance").expect_err("no catalog");
    assert!(err.0.contains("\"gw_catalog\""), "{}", err.0);

    match RemoteKlines::credentialed(link.clone(), "oanda").fetch("EUR_USD", "S5", 0, 1) {
        Err(SourceError::Fetch(text)) => {
            assert!(text.contains("\"gw_fetch_credentialed\""), "{text}");
        }
        other => panic!("{other:?}"),
    }
    match RemoteFunding::new(link.clone(), "binance").fetch("BTCUSDT", 0, 1) {
        Err(SourceError::Fetch(text)) => {
            assert!(text.contains("\"gw_fetch\"") || text.contains("funding"), "{text}")
        }
        other => panic!("{other:?}"),
    }
    let err = RemoteOptions::new(link.clone()).list_underlyings().expect_err("no options");
    assert!(err.contains("\"gw_options\""), "{err}");

    let mut family = RemoteRollingFamily::new(link.clone(), "btc-updown-5m").expect("grammar");
    let err = vike_data::live::SymbolResolver::desired(&mut family, 0).expect_err("no family");
    assert!(err.contains("\"gw_family\""), "{err}");

    // A presence word for a daemon that does not serve the verb is `NotChecked`, never `Absent`.
    assert_eq!(credential_presence(&link, "oanda"), CredentialPresence::NotChecked);

    // The stream lane IS served for binance only: another venue is refused naming `md_venue=`.
    let mut client =
        RemoteDataClient::new(GatewayOpener::new(link), "okx", Arc::new(Noop)).expect("client");
    match client.subscribe_quotes("BTCUSDT") {
        Err(LiveDataError::Subscribe(text)) => assert!(text.contains("md_venue=okx"), "{text}"),
        other => panic!("{other:?}"),
    }
    assert!(h.rest.fetches.lock().unwrap_or_else(PoisonError::into_inner).is_empty());
}

/// The server's own leg, for a peer that does not look at the Welcome: a positional `Error` naming the
/// string, on a connection that survives each refusal.
#[test]
fn a_verb_the_build_does_not_serve_is_a_positional_error_naming_the_string() {
    let h = bare(RestFacts::default());
    let mut raw = Raw::open(h.addr, &test_keys(), Scope::Read);
    let cases: Vec<(Request, &str)> = vec![
        (Request::CatalogDescribe { venue: "binance".into() }, "\"gw_catalog\""),
        (Request::OptionsRest { call: OptionsCall::Underlyings }, "\"gw_options\""),
        (
            Request::ResolveFamily {
                venue: "polymarket".into(),
                family: "btc-updown-5m".into(),
                now_ms: 0,
            },
            "\"gw_family\"",
        ),
        (Request::CredentialPresence { venue: "oanda".into() }, "\"gw_presence\""),
        (
            Request::OptionsFeedOpen {
                feed: OptionsFeedSpec::MarkPrice { indexes: vec!["btc_usd".into()] },
            },
            "\"gw_options\"",
        ),
    ];
    for (request, string) in cases {
        raw.send(&request);
        match raw.recv::<Response>() {
            Response::Error(text) => assert!(text.contains(string), "{text}"),
            other => panic!("{other:?}"),
        }
    }
    raw.send(&Request::Ping);
    assert!(matches!(raw.recv::<Response>(), Response::Pong), "a refusal closed the connection");
}

#[test]
fn the_status_report_names_the_venues_and_counts_the_connections() {
    let h = Harness::start();
    h.rest.set_budget(vec![BudgetCounter {
        venue: "binance".into(),
        host: "spot".into(),
        scope: "ip".into(),
        class: "bulk".into(),
        used: 12,
        cap: 6000,
        window_ms: 60_000,
        would_throttle: 0,
    }]);
    let mut client = GatewayClient::connect(&h.link(), Scope::Read).expect("connect");
    let report = client.status().expect("status");
    assert_eq!(report.md_venues, vec!["binance".to_string(), "polymarket".to_string()]);
    assert_eq!(report.cd_venues, vec!["oanda".to_string()]);
    assert!(report.connections >= 1);
    assert_ne!(report.epoch, 0);
    assert!(report.started_ms > 0);
    assert_eq!(report.budget.len(), 1);
    assert_eq!(report.budget[0].used, 12);
    // The epoch is stable across calls and connections (it identifies the process).
    let mut again = GatewayClient::connect(&h.link(), Scope::Read).expect("connect");
    assert_eq!(again.status().expect("status").epoch, report.epoch);
}

#[test]
fn the_status_counts_live_work_while_it_runs() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let h = Harness::start();
    let release = Arc::new(AtomicBool::new(false));
    let gate = Arc::clone(&release);
    h.rest.set_fetch(move |_, _, _, _| {
        while !gate.load(Ordering::SeqCst) {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        Ok(vike_gatewayhub_client::proto::FetchRows::Bars(Vec::new()))
    });
    let mut stream = Raw::open(h.addr, &test_keys(), Scope::Read);
    stream.open_stream("binance");
    let mut fetch = Raw::open(h.addr, &test_keys(), Scope::Write);
    fetch.send(&Request::Fetch {
        rid: 1,
        what: vike_gatewayhub_client::proto::FetchWhat::Klines {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            interval: "1m".into(),
            start_ms: 0,
            end_ms: 1,
        },
    });
    assert!(matches!(fetch.recv::<Response>(), Response::FetchProgress(_)));
    let mut probe = GatewayClient::connect(&h.link(), Scope::Read).expect("connect");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let report = loop {
        let report = probe.status().expect("status");
        if (report.stream_sessions, report.fetches_running) == (1, 1) {
            break report;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the status never showed the work: {report:?}"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    assert!(report.connections >= 3);
    release.store(true, Ordering::SeqCst);
}

/// The tradehub's single-session and credentialed trading venues stay on the tradehub in wave 1: a
/// daemon built from the registry advertises none of them and refuses every verb for them, by name.
#[test]
fn a_daemon_built_from_the_registry_serves_no_single_session_venue() {
    use vike_gatewayhub_client::proto::{FetchFailure, FetchWhat};
    let h = Harness::start_from_registry();
    let features = features_of(&h);
    let mut read = Raw::open(h.addr, &test_keys(), Scope::Read);
    let mut write = Raw::open(h.addr, &test_keys(), Scope::Write);
    for venue in ["ibkr", "ig", "alpaca", "ctrader"] {
        assert!(!features.iter().any(|f| f.ends_with(venue)), "`{venue}` is advertised");
        read.send(&Request::DataOpen { venue: venue.into() });
        assert!(matches!(read.recv::<Response>(), Response::Error(t) if t.contains(venue)));
        read.send(&Request::CatalogDescribe { venue: venue.into() });
        assert!(matches!(read.recv::<Response>(), Response::Error(t) if t.contains(venue)));
        read.send(&Request::ResolveFamily { venue: venue.into(), family: "x".into(), now_ms: 0 });
        assert!(matches!(read.recv::<Response>(), Response::Error(t) if t.contains(venue)));
        let lanes = [
            FetchWhat::Klines {
                venue: venue.into(),
                symbol: "X".into(),
                interval: "1m".into(),
                start_ms: 0,
                end_ms: 1,
            },
            FetchWhat::Funding { venue: venue.into(), symbol: "X".into(), start_ms: 0, end_ms: 1 },
            FetchWhat::Ticks { venue: venue.into(), symbol: "X".into(), start_ms: 0, end_ms: 1 },
            FetchWhat::CatalogList { venue: venue.into() },
            FetchWhat::CatalogSearch {
                venue: venue.into(),
                query: "x".into(),
                tab: None,
                filter_venue: None,
            },
        ];
        for what in lanes {
            // The window lanes need the Write scope (they would meet the scope refusal on a Read
            // connection before the roster); the catalog lanes are Read.
            let conn = if matches!(
                what,
                FetchWhat::Klines { .. } | FetchWhat::Funding { .. } | FetchWhat::Ticks { .. }
            ) {
                &mut write
            } else {
                &mut read
            };
            conn.send(&Request::Fetch { rid: 1, what });
            match conn.recv::<Response>() {
                Response::FetchFailed { failure: FetchFailure::Refused(t), .. } => {
                    assert!(t.contains(venue), "{t}");
                }
                other => panic!("`{venue}`: {other:?}"),
            }
        }
        write.send(&Request::FetchCredentialed {
            rid: 1,
            what: FetchWhat::Klines {
                venue: venue.into(),
                symbol: "X".into(),
                interval: "S5".into(),
                start_ms: 0,
                end_ms: 1,
            },
        });
        assert!(matches!(
            write.recv::<Response>(),
            Response::FetchFailed { failure: FetchFailure::Refused(_), .. }
        ));
    }
    assert!(
        h.rest.fetches.lock().unwrap_or_else(PoisonError::into_inner).is_empty(),
        "the lane was asked about a venue no roster serves"
    );
    assert_eq!(h.data.opened(), 0, "a venue client was built for a venue no roster serves");
}
