//! The REAL REST lane's catalog, options, family and status verbs through the real server, with the
//! client crate's own `CatalogProviderGateway`, `GatewayOptions`, `FeedOptionsGateway` and `SymbolResolverGateway`
//! as the peer.
//!
//! Drift register rows held here: R10 (the catalog roster — client = registry = the production table
//! = the public set of `catalog_availability` — and a non-public venue refused), R11's venue half (ONE
//! persistent Deribit provider for the daemon, observed across connections, and the FULL chain on
//! the wire) and R12 (a family outage is `Err`, never an empty set). Venue calls are scripted or
//! point at a loopback stand-in, so nothing leaves the box.

#![cfg(feature = "data")]

use crate::{harness, rest_server};
use vike_log::poll::wait_for;

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::time::Duration;

use vike_bridge_core::options::{OptionsFeedStop, OptionsMarksDeliver, OptionsTickerDeliver};
use vike_catalog::{
    CatalogError, CatalogMode, CatalogProvider, CatalogSearchFilter, PickerTab,
    catalog_availability,
};
use vike_data::live::SymbolResolver;
use vike_deribit::feed::options::chain::DeribitOptionsProvider;
use vike_deribit::options::{FeedSpawn, OptionsSpecDeribit};
use vike_deribit::options_fixture::spawn_fake_deribit;
use vike_gatewayhub::rest::catalog::{CatalogProviderFactory, CatalogProviders};
use vike_gatewayhub::rest::family::FamilyHub;
use vike_gatewayhub::rest::lane::RestLaneBridge;
use vike_gatewayhub::rest::options::OptionsHub;
use vike_gatewayhub::testkit::fixtures::{CatalogSearches, ScriptedCatalog, market};
use vike_gatewayhub::testkit::gateway::fast_timings;
use vike_gatewayhub_client::catalog::{CATALOG_VENUES, CatalogProviderGateway};
use vike_gatewayhub_client::client::GatewayClient;
use vike_gatewayhub_client::options::{FeedOptionsGateway, GatewayOptions};
use vike_gatewayhub_client::proto::BudgetCounter;
use vike_gatewayhub_client::resolve::SymbolResolverGateway;
use vike_hub_protocol::auth::Scope;
use vike_marketdata::{OptionChain, OptionTicker};
use vike_model::{AssetClass, Contract, Instrument};
use vike_polymarket::catalog::discovery::{FetchSpec, GammaSource};
use vike_polymarket::catalog::gamma::GammaMarket;
use vike_polymarket::catalog::rolling_family::{FamilySpecPolymarket, RollingFamily};
use vike_secrets::venue_setting::seeded_venue_settings;
use vike_venues::venue_urls::VenueUrls;

use rest_server::RestServer;

// ---- catalog ------------------------------------------------------------------------------------

fn instrument(venue: &str, symbol: &str) -> Instrument {
    Instrument::new(venue, symbol, "BTC", "USDT", Contract::CryptoSpot)
}

fn entry(
    venue: &str,
    listing: Result<Vec<Instrument>, CatalogError>,
    searched: &CatalogSearches,
) -> (String, CatalogProviderFactory) {
    let (name, log) = (venue.to_string(), Arc::clone(searched));
    let factory: CatalogProviderFactory = Box::new(move || {
        Box::new(ScriptedCatalog {
            venue: name.clone(),
            classes: &[AssetClass::CryptoSpot, AssetClass::CryptoPerp],
            listing: listing.clone(),
            searched: Arc::clone(&log),
        }) as Box<dyn CatalogProvider>
    });
    (venue.to_string(), factory)
}

/// A catalog is described, listed and searched through the real table seam, uncapped, with the
/// provider's words on failure and the picker's filter carried to the provider's search.
#[test]
fn a_catalog_is_described_listed_and_searched_through_the_lane() {
    let searched: CatalogSearches = Arc::new(Mutex::new(Vec::new()));
    let table = CatalogProviders::new(vec![
        entry(
            "okx",
            Ok(vec![instrument("okx", "BTC-USDT"), instrument("okx", "ETH-USDT")]),
            &searched,
        ),
        entry("bybit", Err(CatalogError("bybit instruments HTTP 503".into())), &searched),
    ]);
    let server =
        RestServer::start(RestLaneBridge::new(false, &seeded_venue_settings()).with_catalog(table));

    let okx = CatalogProviderGateway::open(server.link(), "okx").expect("describe");
    assert_eq!(okx.asset_classes(), &[AssetClass::CryptoSpot, AssetClass::CryptoPerp]);
    assert_eq!(okx.mode(), CatalogMode::QueryBacked);
    assert_eq!(okx.list_instruments().expect("list").len(), 2);
    let filter = CatalogSearchFilter { tab: Some(PickerTab::Perps), venue: Some("bybit".into()) };
    assert_eq!(okx.search_remote("btc", &filter).expect("search").len(), 2);
    assert_eq!(
        *searched.lock().unwrap_or_else(PoisonError::into_inner),
        vec![("btc".to_string(), Some(PickerTab::Perps), Some("bybit".to_string()))]
    );

    // The provider's INNER words cross; the `catalog error: ` prefix is the client error's Display.
    let bybit = CatalogProviderGateway::open(server.link(), "bybit").expect("describe");
    let err = bybit.list_instruments().expect_err("the venue failed");
    assert_eq!(err.0, "bybit instruments HTTP 503");
}

/// **R10 on the wire**: every venue of the client's roster describes itself against the PRODUCTION
/// table (describe is a static fact of each provider: no network), and the one public venue the table
/// leaves out, and the venues that are not public at all, are refused naming the served set in order.
#[test]
fn the_production_table_serves_the_clients_roster_and_refuses_the_rest() {
    let server = RestServer::start(RestLaneBridge::new(false, &seeded_venue_settings()));
    for venue in CATALOG_VENUES {
        let provider = CatalogProviderGateway::open(server.link(), venue)
            .unwrap_or_else(|e| panic!("`{venue}` is in the client's roster: {e}"));
        assert!(!provider.asset_classes().is_empty(), "{venue}");
    }
    for &venue in vike_model::VENUES {
        let servable = catalog_availability(venue).is_served();
        let served = CATALOG_VENUES.contains(&venue);
        if served {
            assert!(servable, "`{venue}` is served and is neither public nor a held session");
            continue;
        }
        let err = CatalogProviderGateway::open(server.link(), venue).expect_err("not served");
        assert!(
            err.0.contains(&format!("(served: [{}])", CATALOG_VENUES.join(", "))),
            "`{venue}`: the refusal must name the roster in order: {}",
            err.0
        );
    }
}

// ---- options ------------------------------------------------------------------------------------

/// The production engine over Deribit's spec (or a suite's variant of it).
fn hub(spec: OptionsSpecDeribit) -> OptionsHub {
    OptionsHub::new(Box::new(spec))
}

/// **ONE provider per daemon, observed across CONNECTIONS**: every `GatewayOptions` call dials its own
/// connection, and a refresh pass of an expiries call and two chain calls costs the venue ONE book
/// summary, because the provider's 5 s cache lives in the daemon and not in a connection. (A provider
/// per request would cost three.) The full chain crosses with every strike; a window narrows it.
#[test]
fn the_daemons_one_provider_serves_every_connection_from_one_book_summary() {
    let venue = spawn_fake_deribit();
    let provider =
        DeribitOptionsProvider::from_urls(&vike_venues::venue_urls::VenueUrls::seed("deribit"))
            .expect("the seed's live rows")
            .with_book_summary_url(venue.url.clone())
            .with_instruments_url(venue.instruments_url.clone());
    let server = RestServer::start(
        RestLaneBridge::new(false, &seeded_venue_settings())
            .with_options(hub(OptionsSpecDeribit::with_provider(provider))),
    );
    let options = GatewayOptions::new(server.link());
    assert_eq!(options.list_underlyings().expect("set"), ["BTC", "ETH", "SOL"]);
    let expiries = options.list_expiries("BTC").expect("expiries");
    assert_eq!(expiries.len(), 1);
    let full: OptionChain =
        options.fetch_chain("BTC", &expiries[0].date, None, 0.03).expect("the full chain");
    assert_eq!(full.rows.len(), 5, "the datahub records the FULL chain before it windows");
    let windowed = options.fetch_chain("BTC", &expiries[0].date, Some(1), 0.03).expect("windowed");
    assert!(windowed.rows.len() < full.rows.len());
    assert_eq!(
        venue.hits.load(Ordering::SeqCst),
        1,
        "the provider's cache must survive across calls AND across connections"
    );
    // A name the provider does not serve is refused before the venue, in words.
    let err = options.list_expiries("DOGE").expect_err("not served");
    assert!(err.contains("served: [BTC, ETH, SOL]"), "{err}");
    assert_eq!(venue.hits.load(Ordering::SeqCst), 1);
}

/// The option index is two fetches that fail as a whole, with the datahub's former failure text.
#[test]
fn the_option_index_is_two_fetches_and_fails_as_a_whole_in_the_former_words() {
    let spec = OptionsSpecDeribit::mainnet(&VenueUrls::seed("deribit")).with_instruments(Box::new(
        |_: &str, kind: &str| {
            if kind == "future" {
                Err(CatalogError("HTTP 503".to_string()))
            } else {
                Ok(vec![instrument("deribit", "BTC-25JUN37-100000-C")])
            }
        },
    ));
    let server = RestServer::start(
        RestLaneBridge::new(false, &seeded_venue_settings()).with_options(hub(spec)),
    );
    let err = GatewayOptions::new(server.link())
        .option_index_instruments()
        .expect_err("half an index is not an index");
    assert_eq!(err, "deribit future instruments: catalog error: HTTP 503");

    let whole =
        OptionsSpecDeribit::mainnet(&VenueUrls::seed("deribit")).with_instruments(Box::new(
            |_: &str, kind: &str| Ok(vec![instrument("deribit", &format!("BTC-25JUN37-{kind}"))]),
        ));
    let server = RestServer::start(
        RestLaneBridge::new(false, &seeded_venue_settings()).with_options(hub(whole)),
    );
    let all = GatewayOptions::new(server.link()).option_index_instruments().expect("both");
    assert_eq!(all.len(), 2, "options, then futures, concatenated");
}

/// What the recording feeds were opened with: each feed's focus set, and the delivery it was handed.
#[derive(Default)]
struct Opened {
    sets: Mutex<Vec<(&'static str, Vec<String>)>>,
    marks: Mutex<Option<OptionsMarksDeliver>>,
    ticker: Mutex<Option<OptionsTickerDeliver>>,
}

/// A stop that counts itself.
fn counted_stop(shutdowns: &Arc<AtomicUsize>) -> OptionsFeedStop {
    let shutdowns = Arc::clone(shutdowns);
    Box::new(move || {
        shutdowns.fetch_add(1, Ordering::SeqCst);
    })
}

/// A hub whose two feeds record the focus set and the delivery they were opened with, and count their
/// stops; its listing (BTC, ETH, SOL) is the stand-in's, which a feed open checks names against.
fn recording_hub(opened: &Arc<Opened>, shutdowns: &Arc<AtomicUsize>) -> OptionsHub {
    let (marks_opened, marks_stops) = (Arc::clone(opened), Arc::clone(shutdowns));
    let markprice: FeedSpawn<OptionsMarksDeliver> = Box::new(move |set, deliver| {
        marks_opened.sets.lock().unwrap_or_else(PoisonError::into_inner).push(("markprice", set));
        *marks_opened.marks.lock().unwrap_or_else(PoisonError::into_inner) = Some(deliver);
        counted_stop(&marks_stops)
    });
    let (ticker_opened, ticker_stops) = (Arc::clone(opened), Arc::clone(shutdowns));
    let ticker: FeedSpawn<OptionsTickerDeliver> = Box::new(move |set, deliver| {
        ticker_opened.sets.lock().unwrap_or_else(PoisonError::into_inner).push(("ticker", set));
        *ticker_opened.ticker.lock().unwrap_or_else(PoisonError::into_inner) = Some(deliver);
        counted_stop(&ticker_stops)
    });
    let venue = spawn_fake_deribit();
    let provider = DeribitOptionsProvider::from_urls(&VenueUrls::seed("deribit"))
        .expect("the seed's live rows")
        .with_book_summary_url(venue.url.clone())
        .with_instruments_url(venue.instruments_url.clone());
    hub(OptionsSpecDeribit::with_provider(provider).with_feeds(markprice, ticker))
}

/// How long a wait in this file allows before `vike_log::poll::wait_for` fails it.
const WAIT_FOR: Duration = Duration::from_secs(5);

/// Both feeds open over their own focus sets, deliver rows bit-exactly to the client's callback, and
/// are stopped when the client's socket closes: a feed for a socket that closed must not outlive it.
#[test]
fn both_feeds_open_over_their_focus_sets_deliver_rows_and_stop_with_the_socket() {
    let opened = Arc::new(Opened::default());
    let shutdowns = Arc::new(AtomicUsize::new(0));
    let server = RestServer::start(
        RestLaneBridge::new(false, &seeded_venue_settings())
            .with_options(recording_hub(&opened, &shutdowns)),
    );

    let (marks_tx, marks_rx) = mpsc::channel::<usize>();
    let marks = FeedOptionsGateway::markprice(server.link(), vec!["btc_usd".into()], move |rows| {
        let _ = marks_tx.send(rows.len());
    })
    .expect("marks feed");
    let (tick_tx, tick_rx) = mpsc::channel::<(String, u64)>();
    let ticker = FeedOptionsGateway::ticker(
        server.link(),
        vec!["BTC-25JUN37-100000-C".into()],
        move |row| {
            let _ = tick_tx.send((row.symbol, row.mark.map_or(0, f64::to_bits)));
        },
    )
    .expect("ticker feed");
    wait_for(WAIT_FOR, "both feeds to open", || {
        opened.sets.lock().unwrap_or_else(PoisonError::into_inner).len() == 2
    });
    let sets = opened.sets.lock().unwrap_or_else(PoisonError::into_inner).clone();
    let by_kind = |kind: &str| {
        sets.iter().find(|(k, _)| *k == kind).unwrap_or_else(|| panic!("no {kind} feed")).1.clone()
    };
    assert_eq!(by_kind("markprice"), vec!["btc_usd".to_string()]);
    assert_eq!(by_kind("ticker"), vec!["BTC-25JUN37-100000-C".to_string()]);

    let mut rows = vec![
        OptionTicker::new(1, "BTC-25JUN37-100000-C"),
        OptionTicker::new(2, "BTC-25JUN37-90000-P"),
    ];
    rows[0].mark = Some(0.1 + 0.2);
    opened.marks.lock().unwrap_or_else(PoisonError::into_inner).as_ref().expect("marks")(rows);
    assert_eq!(marks_rx.recv_timeout(Duration::from_secs(5)).expect("a marks batch"), 2);
    let mut row = OptionTicker::new(3, "BTC-25JUN37-100000-C");
    row.mark = Some(1.0 / 3.0);
    opened.ticker.lock().unwrap_or_else(PoisonError::into_inner).as_ref().expect("ticker")(row);
    let (symbol, bits) = tick_rx.recv_timeout(Duration::from_secs(5)).expect("a ticker row");
    assert_eq!((symbol.as_str(), bits), ("BTC-25JUN37-100000-C", (1.0f64 / 3.0).to_bits()));

    drop(marks);
    drop(ticker);
    wait_for(WAIT_FOR, "the closed socket's feed to stop", || {
        shutdowns.load(Ordering::SeqCst) == 2
    });
}

/// A feed the server could not start is the lane's own words, and the cap is the server's constant.
#[test]
fn a_feed_over_the_cap_is_refused_and_starts_nothing() {
    let opened = Arc::new(Opened::default());
    let shutdowns = Arc::new(AtomicUsize::new(0));
    let server = RestServer::start(
        RestLaneBridge::new(false, &seeded_venue_settings())
            .with_options(recording_hub(&opened, &shutdowns)),
    );
    let mut client = GatewayClient::connect(&server.link(), Scope::Read).expect("connect");
    assert!(client.ping().is_ok());
    let too_many: Vec<String> = (0
        ..=vike_gatewayhub_client::proto::GATEWAY_OPTIONS_MAX_INSTRUMENTS)
        .map(|i| format!("BTC-25JUN37-{i}-C"))
        .collect();
    let spec = vike_gatewayhub_client::proto::OptionsFeedSpec::Ticker { instruments: too_many };
    let err = client.open_options_feed(&spec).map(|_| ()).expect_err("over the cap");
    assert!(err.to_string().contains("at most"), "{err}");
    assert!(opened.sets.lock().unwrap_or_else(PoisonError::into_inner).is_empty());
}

// ---- family -------------------------------------------------------------------------------------

struct Gamma {
    markets: Vec<GammaMarket>,
    down: Arc<AtomicBool>,
}

impl GammaSource for Gamma {
    fn list(&self, _: bool, _: usize, _: usize) -> Result<Vec<GammaMarket>, String> {
        if self.down.load(Ordering::SeqCst) {
            Err("gamma: connection reset".to_string())
        } else {
            Ok(self.markets.clone())
        }
    }

    fn by_slug(&self, slug: &str, _: FetchSpec) -> Result<Option<GammaMarket>, String> {
        if self.down.load(Ordering::SeqCst) {
            Err("gamma: connection reset".to_string())
        } else {
            Ok(self.markets.iter().find(|m| m.slug == slug).cloned())
        }
    }
}

/// **R12 on the wire**: the resolver answers the planner's set, a directory outage reaches the peer
/// as an error in the directory's own words and NEVER as the empty set (an empty answer would
/// unsubscribe every live book), and the planner recovers on the same connection.
#[test]
fn a_family_outage_is_an_error_over_the_wire_and_never_the_empty_set() {
    let down = Arc::new(AtomicBool::new(false));
    let switch = Arc::clone(&down);
    let spec = FamilySpecPolymarket::with_factory(Box::new(move |family: &str| {
        let gamma = Gamma {
            markets: vec![
                market("btc-updown-5m-300", &["A_UP", "A_DOWN"]),
                market("btc-updown-5m-600", &["B_UP", "B_DOWN"]),
            ],
            down: Arc::clone(&switch),
        };
        Ok(Box::new(RollingFamily::with_source(family, Box::new(gamma), FetchSpec::default())?)
            as Box<dyn SymbolResolver>)
    }));
    let hub = FamilyHub::new(Box::new(spec));
    let server =
        RestServer::start(RestLaneBridge::new(false, &seeded_venue_settings()).with_families(hub));

    let mut family = SymbolResolverGateway::new(server.link(), "btc-updown-5m").expect("grammar");
    let set = family.desired(450_000).expect("resolved");
    assert_eq!(
        set.into_iter().collect::<Vec<_>>(),
        ["A_DOWN", "A_UP", "B_DOWN", "B_UP"].map(String::from)
    );
    down.store(true, Ordering::SeqCst);
    // A fresh window so the planner cannot answer from its cache.
    let err = family.desired(900_000).expect_err("an outage is UNKNOWN");
    assert!(err.contains("gamma: connection reset"), "{err}");
    down.store(false, Ordering::SeqCst);
    assert!(family.desired(900_000).is_ok(), "the planner recovers on the same connection");

    // The grammar is refused offline, before any daemon is asked.
    let dead = fast_timings();
    let nowhere = vike_gatewayhub_client::client::GatewayLink::new(
        "127.0.0.1:1".to_string(),
        harness::test_keys(),
    )
    .with_timings(dead);
    let err =
        SymbolResolverGateway::new(nowhere, "btc-updown-1h").expect_err("not a rolling family");
    assert_eq!(err, vike_catalog::polymarket_family_minutes("btc-updown-1h").expect_err("refused"));
}

// ---- status -------------------------------------------------------------------------------------

/// The budget seam: empty until the daemon wires a snapshot, then exactly the wired rows reach
/// `GatewayStatus`.
#[test]
fn the_status_report_carries_the_wired_budget_snapshot() {
    let row = BudgetCounter {
        venue: "binance".to_string(),
        host: "fapi".to_string(),
        scope: "ip".to_string(),
        class: "bulk".to_string(),
        used: 7,
        cap: 2_400,
        window_ms: 60_000,
        would_throttle: 0,
    };
    let bare = RestServer::start(RestLaneBridge::new(false, &seeded_venue_settings()));
    let mut client = GatewayClient::connect(&bare.link(), Scope::Read).expect("connect");
    assert!(client.status().expect("status").budget.is_empty());

    let wired = row.clone();
    let server = RestServer::start(
        RestLaneBridge::new(false, &seeded_venue_settings())
            .with_budget_snapshot(Arc::new(move || vec![wired.clone()])),
    );
    let mut client = GatewayClient::connect(&server.link(), Scope::Read).expect("connect");
    assert_eq!(client.status().expect("status").budget, vec![row]);
}
