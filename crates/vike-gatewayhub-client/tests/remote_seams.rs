#![cfg(feature = "data")]
//! The remote catalog, options and family seams over the fake peer: the happy paths and the exact
//! frames each seam sends. (Their failure behaviour is `errors_are_not_empty.rs`, their feature
//! refusals `feature_refusal.rs`.)

use std::sync::mpsc;
use std::time::{Duration, Instant};

use vike_catalog::{CatalogMode, CatalogProvider, SearchFilter, Tab};
use vike_data::live::SymbolResolver;
use vike_gatewayhub_client::catalog::RemoteCatalog;
use vike_gatewayhub_client::options::{RemoteOptions, RemoteOptionsFeed};
use vike_gatewayhub_client::proto::{
    FetchRows, FetchWhat, OptionIndexKind, OptionsCall, OptionsFeedDown, OptionsFeedSpec,
    OptionsReply, Request, Response, WireCatalogMode, WireTab,
};
use vike_gatewayhub_client::resolve::RemoteRollingFamily;
use vike_marketdata::{Expiry, OptionChain, OptionTicker, UnderlyingKind};
use vike_model::{AssetClass, Contract, Instrument};

#[allow(dead_code, clippy::allow_attributes)]
#[path = "support/fake_peer.rs"]
mod fake_peer;

use fake_peer::{FakePeer, PeerConfig, Step};

const WAIT: Duration = Duration::from_secs(5);

fn instrument(venue: &str, i: usize) -> Instrument {
    Instrument::new(venue, format!("SYM{i}"), "BTC", "USDT", Contract::CryptoSpot)
}

/// The script a healthy daemon writes for `pages`: progress, every page, the totals.
fn page_script(pages: Vec<FetchRows>) -> Vec<Step> {
    let total: usize = pages.iter().map(FetchRows::len).sum();
    let n = u32::try_from(pages.len()).unwrap();
    let mut steps =
        vec![Step::Send(Response::FetchProgress(vike_gatewayhub_client::proto::FetchProgress {
            rid: 1,
            state: vike_gatewayhub_client::proto::FetchState::Running,
            pages: 0,
            rows: 0,
            elapsed_ms: 1,
            next_in_ms: 100,
        }))];
    for (i, rows) in pages.into_iter().enumerate() {
        steps.push(Step::Send(Response::FetchPage {
            rid: 1,
            page: u32::try_from(i).unwrap(),
            rows,
        }));
    }
    steps.push(Step::Send(Response::FetchDone { rid: 1, pages: n, rows: total as u64 }));
    steps
}

#[test]
fn describe_is_asked_once_and_cached_and_a_big_listing_reassembles() {
    let listing: Vec<Instrument> = (0..400).map(|i| instrument("binance", i)).collect();
    let pages: Vec<FetchRows> =
        FetchRows::Instruments(listing.clone()).into_pages(8 * 1024).collect();
    assert!(pages.len() > 3, "the listing really pages");
    let peer = FakePeer::start(PeerConfig::new().with_rpc(move |req| match req {
        Request::CatalogDescribe { .. } => vec![Step::Send(Response::CatalogDescribed {
            asset_classes: vec![AssetClass::CryptoSpot, AssetClass::CryptoPerp],
            mode: WireCatalogMode::Enumerable,
        })],
        Request::Fetch { what: FetchWhat::CatalogList { .. }, .. } => page_script(pages.clone()),
        _ => vec![Step::Send(Response::Error("not scripted".into()))],
    }));

    let catalog = RemoteCatalog::open(peer.link(), "binance").expect("describe");
    assert_eq!(catalog.venue(), "binance");
    assert_eq!(catalog.asset_classes(), [AssetClass::CryptoSpot, AssetClass::CryptoPerp]);
    assert_eq!(catalog.mode(), CatalogMode::Enumerable);
    for _ in 0..2 {
        assert_eq!(catalog.list_instruments().expect("listed"), listing);
    }
    let describes =
        peer.requests().iter().filter(|r| matches!(r, Request::CatalogDescribe { .. })).count();
    let lists = peer
        .requests()
        .iter()
        .filter(|r| matches!(r, Request::Fetch { what: FetchWhat::CatalogList { venue }, .. } if venue == "binance"))
        .count();
    assert_eq!(
        (describes, lists),
        (1, 2),
        "describe is cached; every listing is a fresh paged fetch"
    );
}

#[test]
fn a_search_sends_the_pickers_filter_over_the_wire() {
    let hit = vec![instrument("ibkr", 1)];
    let rows = hit.clone();
    let peer = FakePeer::start(PeerConfig::new().with_rpc(move |req| match req {
        Request::CatalogDescribe { .. } => vec![Step::Send(Response::CatalogDescribed {
            asset_classes: vec![AssetClass::Equity],
            mode: WireCatalogMode::QueryBacked,
        })],
        Request::Fetch { what: FetchWhat::CatalogSearch { .. }, .. } => {
            page_script(vec![FetchRows::Instruments(rows.clone())])
        }
        _ => vec![Step::Send(Response::Error("not scripted".into()))],
    }));
    let catalog = RemoteCatalog::open(peer.link(), "ibkr").unwrap();
    assert_eq!(catalog.mode(), CatalogMode::QueryBacked);
    let filter = SearchFilter { tab: Some(Tab::Perps), venue: Some("ibkr".into()) };
    assert_eq!(catalog.search_remote("aapl", &filter).unwrap(), hit);
    assert!(peer.requests().contains(&Request::Fetch {
        rid: 1,
        what: FetchWhat::CatalogSearch {
            venue: "ibkr".into(),
            query: "aapl".into(),
            tab: Some(WireTab::Perps),
            filter_venue: Some("ibkr".into()),
        }
    }));
}

fn chain(window_marker: f64) -> OptionChain {
    OptionChain {
        underlying: "BTC".into(),
        underlying_kind: UnderlyingKind::Crypto,
        underlying_price: Some(65_000.5 + window_marker),
        expiry: Expiry { date: "2026-10-30".into(), dte: 17, label: "30 Oct".into() },
        asof_ms: 1_760_000_000_000,
        source: "deribit".into(),
        rows: vec![],
    }
}

#[test]
fn the_options_rest_calls_map_one_for_one_and_none_is_the_full_chain() {
    let peer = FakePeer::start(PeerConfig::new().with_rpc(|req| match req {
        Request::OptionsRest { call: OptionsCall::Underlyings } => {
            vec![Step::Send(Response::OptionsReplied(OptionsReply::Underlyings(vec![
                "BTC".into(),
                "ETH".into(),
                "SOL".into(),
            ])))]
        }
        Request::OptionsRest { call: OptionsCall::Expiries { .. } } => {
            vec![Step::Send(Response::OptionsReplied(OptionsReply::Expiries(vec![Expiry {
                date: "2026-10-30".into(),
                dte: 17,
                label: "30 Oct".into(),
            }])))]
        }
        Request::OptionsRest { call: OptionsCall::Chain { strike_window, .. } } => {
            let marker = strike_window.map_or(0.0, f64::from);
            vec![Step::Send(Response::OptionsReplied(OptionsReply::Chain(Box::new(chain(marker)))))]
        }
        _ => vec![Step::Send(Response::Error("not scripted".into()))],
    }));
    let options = RemoteOptions::new(peer.link());
    assert_eq!(options.list_underlyings().unwrap(), ["BTC", "ETH", "SOL"]);
    assert_eq!(options.list_expiries("BTC").unwrap()[0].date, "2026-10-30");
    // `None` is the FULL chain (what the recorder needs before it applies its own window).
    assert_eq!(options.fetch_chain("BTC", "2026-10-30", None, 0.04).unwrap(), chain(0.0));
    assert_eq!(options.fetch_chain("BTC", "2026-10-30", Some(30), 0.04).unwrap(), chain(30.0));
    assert!(peer.requests().contains(&Request::OptionsRest {
        call: OptionsCall::Chain {
            underlying: "BTC".into(),
            expiry: "2026-10-30".into(),
            strike_window: None,
            r: 0.04
        }
    }));
}

#[test]
fn the_option_index_is_options_then_futures_and_fails_as_a_whole() {
    let peer = FakePeer::start(PeerConfig::new().with_rpc(|req| match req {
        Request::Fetch {
            what: FetchWhat::OptionIndex { kind: OptionIndexKind::Option }, ..
        } => page_script(vec![FetchRows::Instruments(vec![
            instrument("deribit", 1),
            instrument("deribit", 2),
        ])]),
        Request::Fetch {
            what: FetchWhat::OptionIndex { kind: OptionIndexKind::Future }, ..
        } => page_script(vec![FetchRows::Instruments(vec![instrument("deribit", 3)])]),
        _ => vec![Step::Send(Response::Error("not scripted".into()))],
    }));
    let all = RemoteOptions::new(peer.link()).option_index_instruments().unwrap();
    let names: Vec<&str> = all.iter().map(|i| i.raw_symbol.as_str()).collect();
    assert_eq!(names, ["SYM1", "SYM2", "SYM3"], "options first, then futures, in order");

    // Futures fail → the whole index fails, so the caller keeps its last-good index.
    let peer = FakePeer::start(PeerConfig::new().with_rpc(|req| match req {
        Request::Fetch {
            what: FetchWhat::OptionIndex { kind: OptionIndexKind::Option }, ..
        } => page_script(vec![FetchRows::Instruments(vec![instrument("deribit", 1)])]),
        _ => vec![Step::Send(Response::Error("deribit future instruments: HTTP 503".into()))],
    }));
    let e = RemoteOptions::new(peer.link()).option_index_instruments().unwrap_err();
    assert!(e.contains("HTTP 503"), "{e}");
}

#[test]
fn the_markprice_feed_delivers_rows_inline_and_reopens_with_the_same_focus_set() {
    let peer = FakePeer::start(PeerConfig::new());
    let (tx, rx) = mpsc::channel::<Vec<OptionTicker>>();
    let indexes = vec!["btc_usd".to_string(), "eth_usd".to_string()];
    let mut feed = RemoteOptionsFeed::markprice(peer.link(), indexes.clone(), move |rows| {
        let _ = tx.send(rows);
    })
    .expect("the feed thread");

    let first = peer.wait_feed(0, WAIT);
    let rows = vec![
        OptionTicker::new(1, "BTC-30OCT26-70000-C"),
        OptionTicker::new(2, "BTC-30OCT26-70000-P"),
    ];
    first.send(&OptionsFeedDown::Heartbeat);
    first.send(&OptionsFeedDown::Marks(rows.clone()));
    assert_eq!(rx.recv_timeout(WAIT).expect("a batch"), rows);

    // A lost socket is re-opened with the same spec (reconnect == resubscribe), silently.
    first.kill();
    let second = peer.wait_feed(1, WAIT);
    second.send(&OptionsFeedDown::Marks(rows.clone()));
    assert_eq!(rx.recv_timeout(WAIT).expect("a batch on the second socket"), rows);

    let opens: Vec<OptionsFeedSpec> = peer
        .requests()
        .into_iter()
        .filter_map(|r| match r {
            Request::OptionsFeedOpen { feed } => Some(feed),
            _ => None,
        })
        .collect();
    assert_eq!(opens, vec![OptionsFeedSpec::MarkPrice { indexes: indexes.clone() }; 2]);

    let started = Instant::now();
    feed.shutdown();
    assert!(started.elapsed() < Duration::from_secs(3), "shutdown stops and joins");
    feed.shutdown(); // idempotent
}

#[test]
fn the_ticker_feed_delivers_rows_and_a_frame_of_the_wrong_feed_is_a_fault_that_reopens() {
    let peer = FakePeer::start(PeerConfig::new());
    let (tx, rx) = mpsc::channel::<OptionTicker>();
    let instruments = vec!["BTC-30OCT26-70000-C".to_string()];
    let mut feed = RemoteOptionsFeed::ticker(peer.link(), instruments.clone(), move |row| {
        let _ = tx.send(row);
    })
    .unwrap();
    let first = peer.wait_feed(0, WAIT);
    let row = OptionTicker::new(5, "BTC-30OCT26-70000-C");
    first.send(&OptionsFeedDown::Ticker(Box::new(row.clone())));
    assert_eq!(rx.recv_timeout(WAIT).unwrap(), row);

    // A markprice batch on a ticker feed is a protocol fault: the client drops the socket and reopens.
    first.send(&OptionsFeedDown::Marks(vec![]));
    let second = peer.wait_feed(1, WAIT);
    second.send(&OptionsFeedDown::Ticker(Box::new(row.clone())));
    assert_eq!(rx.recv_timeout(WAIT).unwrap(), row);
    feed.shutdown();
}

#[test]
fn a_resolver_returns_the_daemons_set_and_stamps_its_group() {
    let peer = FakePeer::start(PeerConfig::new().with_rpc(|req| match req {
        Request::ResolveFamily { venue, family, now_ms } => {
            assert_eq!((venue.as_str(), family.as_str()), ("polymarket", "btc-updown-5m"));
            assert_eq!(*now_ms, 1_700_000_000_000);
            vec![Step::Send(Response::FamilyResolved {
                symbols: vec!["tok-b".into(), "tok-a".into()],
            })]
        }
        _ => vec![Step::Send(Response::Error("not scripted".into()))],
    }));
    let mut family = RemoteRollingFamily::new(peer.link(), "btc-updown-5m").unwrap();
    assert_eq!(family.group(), Some("btc-updown-5m"));
    let got: Vec<String> = family.desired(1_700_000_000_000).unwrap().into_iter().collect();
    assert_eq!(got, ["tok-a", "tok-b"]);
    // A second call rides the same held connection.
    family.desired(1_700_000_000_000).unwrap();
    assert_eq!(peer.connections(), 1);
}

#[test]
fn the_status_verb_returns_the_daemons_counters_and_refuses_by_name_without_its_feature() {
    use vike_gatewayhub_client::client::GatewayClient;
    use vike_gatewayhub_client::proto::{BudgetCounter, FEATURE_GW_STATUS, GwStatusReport};
    use vike_hub_proto::auth::Scope;

    let report = GwStatusReport {
        epoch: 7,
        started_ms: 1,
        uptime_ms: 2,
        connections: 3,
        stream_sessions: 4,
        fetches_running: 5,
        md_venues: vec!["binance".into()],
        cd_venues: vec!["oanda".into()],
        budget: vec![BudgetCounter {
            venue: "binance".into(),
            host: "fapi".into(),
            scope: "ip".into(),
            class: "live".into(),
            used: 10,
            cap: 2_400,
            window_ms: 60_000,
            would_throttle: 0,
        }],
    };
    let r = report.clone();
    let peer = FakePeer::start(PeerConfig::new().with_rpc(move |req| match req {
        Request::GwStatus => vec![Step::Send(Response::StatusIs(Box::new(r.clone())))],
        _ => vec![Step::Send(Response::Error("not scripted".into()))],
    }));
    let mut c = GatewayClient::connect(&peer.link(), Scope::Read).unwrap();
    assert_eq!(c.status().unwrap(), report);

    let features: Vec<String> = fake_peer::features(&["binance"], &[])
        .into_iter()
        .filter(|f| f != FEATURE_GW_STATUS)
        .collect();
    let peer = FakePeer::start(PeerConfig::new().with_features(features));
    let mut c = GatewayClient::connect(&peer.link(), Scope::Read).unwrap();
    let e = c.status().unwrap_err();
    assert!(e.to_string().contains(FEATURE_GW_STATUS), "{e}");
    assert!(peer.requests().is_empty());
}
