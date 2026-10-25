//! The positional and pushed REST verbs, with the client crate's real seams as the peer: catalog
//! describe / list / search, the other history sources, options REST and the two options feeds, the
//! family resolver and the presence word.
//!
//! What they hold together is the error contract of drift register R12: a lane failure is `Err` in the
//! lane's own words, never an empty answer.

#[path = "support/fake_lanes.rs"]
#[allow(dead_code, clippy::allow_attributes)]
mod fake_lanes;
#[path = "support/harness.rs"]
#[allow(dead_code, clippy::allow_attributes)]
mod harness;
#[path = "support/raw_peer.rs"]
#[allow(dead_code, clippy::allow_attributes)]
mod raw_peer;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use vike_catalog::{CatalogMode, CatalogProvider, SearchFilter, Tab};
use vike_data::live::SymbolResolver;
use vike_data::source::{FundingRatePoint, FundingRateSource};
use vike_gatewayhub::rest::CatalogTraits;
use vike_gatewayhub_client::catalog::RemoteCatalog;
use vike_gatewayhub_client::credential::{CredentialPresence, credential_presence};
use vike_gatewayhub_client::history::{RemoteFunding, RemoteTicks};
use vike_gatewayhub_client::options::{RemoteOptions, RemoteOptionsFeed};
use vike_gatewayhub_client::proto::{
    FetchFailure, FetchRows, FetchWhat, OptionIndexKind, OptionsCall, OptionsFeedSpec,
    OptionsReply, Request, Response, WireCatalogMode, WireFundingPoint, WireTab,
};
use vike_gatewayhub_client::resolve::RemoteRollingFamily;
use vike_hub_proto::auth::Scope;
use vike_marketdata::{Expiry, OptionTicker, QuoteTick};
use vike_model::{AssetClass, Contract, Instrument};

use harness::{Harness, test_keys};
use raw_peer::Raw;

fn instruments(venue: &str, n: usize) -> Vec<Instrument> {
    (0..n)
        .map(|i| {
            Instrument::new(
                venue,
                format!("SYM{i}USDT"),
                format!("SYM{i}"),
                "USDT",
                Contract::CryptoSpot,
            )
        })
        .collect()
}

#[test]
fn a_catalog_is_described_listed_and_searched_through_the_real_provider() {
    let h = Harness::start();
    h.rest.set_describe(Ok(CatalogTraits {
        asset_classes: vec![AssetClass::CryptoSpot, AssetClass::CryptoPerp],
        mode: WireCatalogMode::QueryBacked,
    }));
    let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&seen);
    h.rest.set_fetch(move |_, what, credentialed, _| {
        assert!(!credentialed);
        let line = match what {
            FetchWhat::CatalogList { venue } => format!("list {venue}"),
            FetchWhat::CatalogSearch { venue, query, tab, filter_venue } => {
                format!("search {venue} {query:?} {tab:?} {filter_venue:?}")
            }
            other => panic!("unexpected {other:?}"),
        };
        log.lock().unwrap_or_else(PoisonError::into_inner).push(line);
        Ok(FetchRows::Instruments(instruments("binance", 3)))
    });
    let provider = RemoteCatalog::open(h.link(), "binance").expect("describe");
    assert_eq!(provider.venue(), "binance");
    assert_eq!(provider.asset_classes(), &[AssetClass::CryptoSpot, AssetClass::CryptoPerp]);
    assert_eq!(provider.mode(), CatalogMode::QueryBacked);
    assert_eq!(provider.list_instruments().expect("list"), instruments("binance", 3));
    let filter = SearchFilter { tab: Some(Tab::Perps), venue: Some("bybit".into()) };
    assert_eq!(provider.search_remote("btc", &filter).expect("search").len(), 3);
    let lines = seen.lock().unwrap_or_else(PoisonError::into_inner).clone();
    assert_eq!(lines[0], "list binance");
    assert_eq!(
        lines[1],
        format!("search binance \"btc\" {:?} Some(\"bybit\")", Some(WireTab::Perps))
    );
}

#[test]
fn a_catalog_for_a_venue_the_build_does_not_list_is_refused_naming_the_roster() {
    let h = Harness::start();
    let err = RemoteCatalog::open(h.link(), "deribit").expect_err("deribit is not served");
    assert!(err.0.contains("`deribit`") && err.0.contains("binance, dukascopy"), "{}", err.0);
}

#[test]
fn a_catalog_failure_is_an_error_in_the_lanes_words_and_never_an_empty_list() {
    let h = Harness::start();
    h.rest.set_describe(Ok(CatalogTraits {
        asset_classes: vec![AssetClass::CryptoSpot],
        mode: WireCatalogMode::Enumerable,
    }));
    h.rest.set_fetch(|_, _, _, _| {
        Err(FetchFailure::Fetch("HTTP 503 from the exchangeInfo endpoint".into()))
    });
    let provider = RemoteCatalog::open(h.link(), "binance").expect("describe");
    let err = provider.list_instruments().expect_err("the venue failed");
    assert_eq!(err.0, "HTTP 503 from the exchangeInfo endpoint");
    // A describe failure is the lane's text, unchanged.
    h.rest.set_describe(Err("the provider table has no such venue".into()));
    let err = RemoteCatalog::open(h.link(), "binance").expect_err("describe failed");
    assert_eq!(err.0, "the provider table has no such venue");
}

#[test]
fn funding_and_tick_sources_cross_bit_exact() {
    let h = Harness::start();
    let funding = vec![
        WireFundingPoint { ts_ms: 1, rate: 0.000_1 + 0.000_2, premium: Some(1.0 / 3.0) },
        WireFundingPoint { ts_ms: 2, rate: -0.000_05, premium: None },
    ];
    let quotes = vec![QuoteTick {
        ts: 5,
        local_ts: 0,
        bid: 1.0 / 3.0,
        ask: 2.0 / 3.0,
        bid_size: 0.0,
        ask_size: 0.0,
        symbol: "EURUSD".into(),
    }];
    let (f, q) = (funding.clone(), quotes.clone());
    h.rest.set_fetch(move |_, what, _, _| match what {
        FetchWhat::Funding { .. } => Ok(FetchRows::Funding(f.clone())),
        FetchWhat::Ticks { .. } => Ok(FetchRows::Quotes(q.clone())),
        other => panic!("unexpected {other:?}"),
    });
    let got = RemoteFunding::new(h.link(), "binance").fetch("BTCUSDT", 0, 1).expect("funding");
    let want: Vec<FundingRatePoint> = funding.into_iter().map(FundingRatePoint::from).collect();
    assert_eq!(got.len(), want.len());
    for (g, w) in got.iter().zip(&want) {
        assert_eq!((g.ts_ms, g.rate.to_bits(), g.premium), (w.ts_ms, w.rate.to_bits(), w.premium));
    }
    let ticks = RemoteTicks::new(h.link()).fetch_quotes_range("EURUSD", 0, 1, None).expect("ticks");
    assert_eq!(ticks, quotes);
}

#[test]
fn the_family_resolver_answers_a_set_and_a_failure_is_an_error_never_the_empty_set() {
    let h = Harness::start();
    h.rest.set_family("btc-updown-5m", Ok(vec!["TOKEN-B".into(), "TOKEN-A".into()]));
    let mut family = RemoteRollingFamily::new(h.link(), "btc-updown-5m").expect("grammar");
    let set = family.desired(1_700_000_000_000).expect("resolved");
    assert_eq!(
        set.into_iter().collect::<Vec<_>>(),
        vec!["TOKEN-A".to_string(), "TOKEN-B".to_string()]
    );
    // A legitimately empty set is `Ok(empty)` and DOES unsubscribe; an error is unknown.
    h.rest.set_family("btc-updown-5m", Ok(Vec::new()));
    assert!(family.desired(1).expect("empty is a statement").is_empty());
    h.rest.set_family("btc-updown-5m", Err("the markets directory is unreachable".into()));
    assert_eq!(family.desired(2).expect_err("unknown"), "the markets directory is unreachable");
    // The venue is checked against the roster before the lane.
    let mut raw = Raw::open(h.addr, &test_keys(), Scope::Read);
    raw.send(&Request::ResolveFamily { venue: "binance".into(), family: "x".into(), now_ms: 0 });
    match raw.recv::<Response>() {
        Response::Error(text) => {
            assert!(text.contains("`binance`") && text.contains("polymarket"), "{text}")
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn every_presence_word_crosses_and_never_a_value() {
    let h = Harness::start();
    for word in CredentialPresence::ALL {
        h.rest.set_presence("oanda", word);
        assert_eq!(credential_presence(&h.link(), "oanda"), word);
    }
    // A venue no lane needs a credential for.
    assert_eq!(credential_presence(&h.link(), "binance"), CredentialPresence::NotNeeded);
}

#[test]
fn options_rest_calls_reach_the_lane_unchanged_and_a_failure_is_its_words() {
    let h = Harness::start();
    h.rest.set_options(Ok(OptionsReply::Underlyings(vec!["BTC".into(), "ETH".into()])));
    let options = RemoteOptions::new(h.link());
    assert_eq!(
        options.list_underlyings().expect("underlyings"),
        vec!["BTC".to_string(), "ETH".to_string()]
    );
    h.rest.set_options(Ok(OptionsReply::Expiries(vec![Expiry {
        date: "2026-10-30".into(),
        dte: 16,
        label: "30 Oct".into(),
    }])));
    assert_eq!(options.list_expiries("BTC").expect("expiries")[0].dte, 16);
    // `strikes: None` is the FULL chain; `Some(n)` windows it.
    h.rest.set_options(Err("deribit: public/get_book_summary_by_currency failed: timeout".into()));
    let err = options.fetch_chain("BTC", "2026-10-30", None, 0.03).expect_err("the lane failed");
    assert_eq!(err, "deribit: public/get_book_summary_by_currency failed: timeout");
    let err =
        options.fetch_chain("BTC", "2026-10-30", Some(30), 0.03).expect_err("the lane failed");
    assert!(err.contains("deribit"));
    let calls = h.rest.options_calls.lock().unwrap_or_else(PoisonError::into_inner).clone();
    assert!(matches!(&calls[0], OptionsCall::Underlyings));
    assert!(matches!(&calls[1], OptionsCall::Expiries { underlying } if underlying == "BTC"));
    assert!(
        matches!(&calls[2], OptionsCall::Chain { strike_window: None, r, .. } if r.to_bits() == 0.03f64.to_bits())
    );
    assert!(matches!(&calls[3], OptionsCall::Chain { strike_window: Some(30), .. }));
}

#[test]
fn the_option_index_is_two_paged_fetches_and_fails_as_a_whole() {
    let h = Harness::start();
    let asked = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&asked);
    h.rest.set_fetch(move |_, what, _, _| match what {
        FetchWhat::OptionIndex { kind } => {
            counter.fetch_add(1, Ordering::SeqCst);
            match kind {
                OptionIndexKind::Option => Ok(FetchRows::Instruments(instruments("deribit", 4))),
                OptionIndexKind::Future => Ok(FetchRows::Instruments(instruments("deribit", 2))),
            }
        }
        other => panic!("unexpected {other:?}"),
    });
    let options = RemoteOptions::new(h.link());
    assert_eq!(options.option_index_instruments().expect("both").len(), 6);
    h.rest.set_fetch(|_, what, _, _| match what {
        FetchWhat::OptionIndex { kind: OptionIndexKind::Option } => {
            Ok(FetchRows::Instruments(instruments("deribit", 4)))
        }
        _ => Err(FetchFailure::Fetch("futures listing failed".into())),
    });
    let err = options.option_index_instruments().expect_err("half an index is not an index");
    assert_eq!(err, "futures listing failed");
}

#[test]
fn both_options_feeds_deliver_rows_and_are_shut_down_with_the_socket() {
    let h = Harness::start();
    let (marks_tx, marks_rx) = mpsc::channel::<usize>();
    let marks = RemoteOptionsFeed::markprice(h.link(), vec!["btc_usd".into()], move |rows| {
        let _ = marks_tx.send(rows.len());
    })
    .expect("marks feed");
    let (tick_tx, tick_rx) = mpsc::channel::<(String, u64)>();
    let ticker =
        RemoteOptionsFeed::ticker(h.link(), vec!["BTC-30OCT26-70000-C".into()], move |row| {
            let _ = tick_tx.send((row.symbol, row.mark.map_or(0, f64::to_bits)));
        })
        .expect("ticker feed");

    let marks_feed = h.rest.feed(0);
    let ticker_feed = h.rest.feed(1);
    // Which is which does not depend on connect order: find them by their spec.
    let (marks_feed, ticker_feed) = if matches!(marks_feed.spec, OptionsFeedSpec::MarkPrice { .. })
    {
        (marks_feed, ticker_feed)
    } else {
        (ticker_feed, marks_feed)
    };
    assert!(
        matches!(&marks_feed.spec, OptionsFeedSpec::MarkPrice { indexes } if indexes == &["btc_usd".to_string()])
    );
    assert!(
        matches!(&ticker_feed.spec, OptionsFeedSpec::Ticker { instruments } if instruments == &["BTC-30OCT26-70000-C".to_string()])
    );

    let mut rows = vec![
        OptionTicker::new(1, "BTC-30OCT26-70000-C"),
        OptionTicker::new(2, "BTC-30OCT26-72000-P"),
    ];
    rows[0].mark = Some(0.1 + 0.2);
    marks_feed.sink.marks(rows);
    assert_eq!(marks_rx.recv_timeout(Duration::from_secs(5)).expect("a marks batch"), 2);

    let mut row = OptionTicker::new(3, "BTC-30OCT26-70000-C");
    row.mark = Some(1.0 / 3.0);
    ticker_feed.sink.ticker(row);
    let (symbol, mark_bits) = tick_rx.recv_timeout(Duration::from_secs(5)).expect("a ticker row");
    assert_eq!(symbol, "BTC-30OCT26-70000-C");
    assert_eq!(mark_bits, (1.0f64 / 3.0).to_bits(), "a float drifted over the hop");

    drop(marks);
    drop(ticker);
    for feed in [&marks_feed, &ticker_feed] {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !feed.shut_down.load(Ordering::SeqCst) {
            assert!(
                std::time::Instant::now() < deadline,
                "a closed feed socket left its lane running"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

#[test]
fn an_options_feed_with_no_focus_set_is_refused_and_the_cap_is_the_servers() {
    let h = Harness::start();
    let mut raw = Raw::open(h.addr, &test_keys(), Scope::Read);
    raw.send(&Request::OptionsFeedOpen {
        feed: OptionsFeedSpec::Ticker { instruments: Vec::new() },
    });
    match raw.recv::<Response>() {
        Response::Error(text) => assert!(text.contains("at least one"), "{text}"),
        other => panic!("{other:?}"),
    }
    let too_many = (0..vike_gatewayhub_client::proto::GW_OPTIONS_MAX_INSTRUMENTS + 1)
        .map(|i| format!("BTC-30OCT26-{i}-C"))
        .collect();
    raw.send(&Request::OptionsFeedOpen { feed: OptionsFeedSpec::Ticker { instruments: too_many } });
    match raw.recv::<Response>() {
        Response::Error(text) => assert!(text.contains("at most"), "{text}"),
        other => panic!("{other:?}"),
    }
    // A lane that cannot start the feed answers in its own words, and the connection survives.
    h.rest.fail_feed("deribit websocket: connection refused");
    raw.send(&Request::OptionsFeedOpen {
        feed: OptionsFeedSpec::MarkPrice { indexes: vec!["btc_usd".into()] },
    });
    match raw.recv::<Response>() {
        Response::Error(text) => assert_eq!(text, "deribit websocket: connection refused"),
        other => panic!("{other:?}"),
    }
    raw.send(&Request::Ping);
    assert!(matches!(raw.recv::<Response>(), Response::Pong));
}
