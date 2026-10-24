//! The TESTKIT end to end: scripted lanes behind a loopback gatewayhub, dialled by the client crate's
//! REAL seams (`RemoteDataClient`, `RemoteKlines`, `RemoteFunding`, `RemoteTicks`, `RemoteCatalog`,
//! `RemoteOptions`, `RemoteOptionsFeed`, `RemoteRollingFamily`, `credential_presence`), exactly as the
//! datahub will. The kit is imported through its public path and nothing else, so this file is also the
//! proof that another crate can use it.
//!
//! The kit links no bridge, so every test here runs with or without the `data` feature.
#![cfg(feature = "test-support")]

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use vike_catalog::{CatalogMode, CatalogProvider, SearchFilter, Tab};
use vike_data::live::SymbolResolver;
use vike_data::source::{FundingRatePoint, FundingRateSource, KlineSource, SourceError};
use vike_data::{DataClient, LiveDataError, RecordingSink, SinkCall, SubscriptionId};
use vike_gatewayhub::rest::{RestFacts, RestLane};
use vike_gatewayhub::testkit::data::{DataCall, Refusal, ScriptedData, SubVerb, SubscribeCall};
use vike_gatewayhub::testkit::events::{
    bar, canonical_script, exact_lines_of, quote, verb_of, wait_for_calls,
};
use vike_gatewayhub::testkit::gateway::LoopbackGateway;
use vike_gatewayhub::testkit::rest::{Answer, FetchCall, RestCall, ScriptedRest};
use vike_gatewayhub_client::catalog::RemoteCatalog;
use vike_gatewayhub_client::client::GatewayClient;
use vike_gatewayhub_client::credential::{CredentialPresence, credential_presence};
use vike_gatewayhub_client::history::{RemoteFunding, RemoteKlines, RemoteTicks};
use vike_gatewayhub_client::opener::GatewayOpener;
use vike_gatewayhub_client::options::{RemoteOptions, RemoteOptionsFeed};
use vike_gatewayhub_client::proto::{
    FetchFailure, FetchWhat, OptionIndexKind, OptionsCall, OptionsFeedSpec, WireCatalogMode,
    WireTab, advertised_cd_venues, advertised_md_venues,
};
use vike_gatewayhub_client::resolve::RemoteRollingFamily;
use vike_hub_proto::auth::Scope;
use vike_market_stream::market::RemoteDataClient;
use vike_marketdata::{Expiry, OptionChain, OptionTicker, StrikeRow, UnderlyingKind};
use vike_model::{AssetClass, Contract, Instrument, StreamStatus};

const SYMBOLS: [&str; 2] = ["BTCUSDT", "ETHUSDT"];

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

fn expiry() -> Expiry {
    Expiry { date: "2026-10-30".into(), dte: 16, label: "30 Oct".into() }
}

fn chain() -> OptionChain {
    OptionChain {
        underlying: "BTC".into(),
        underlying_kind: UnderlyingKind::Crypto,
        underlying_price: Some(1.0 / 3.0),
        expiry: expiry(),
        asof_ms: 1_700_000_000_000,
        source: "deribit".into(),
        rows: vec![StrikeRow { strike: 70_000.0, call: None, put: None }],
    }
}

// ---- the data lane -------------------------------------------------------------------------------

#[test]
fn all_twelve_verbs_play_through_the_hop_bit_exact_and_in_order_per_key() {
    let gw = LoopbackGateway::start(ScriptedData::new(&["binance"]), ScriptedRest::new());
    let got = Arc::new(RecordingSink::default());
    let mut client = RemoteDataClient::new(GatewayOpener::new(gw.link()), "binance", got.clone())
        .expect("a reader thread");
    for sym in SYMBOLS {
        client.subscribe_bars(sym, "1m").expect("bars");
        client.subscribe_quotes(sym).expect("quotes");
        client.subscribe_trades(sym).expect("trades");
        client.subscribe_book(sym).expect("book");
        client.subscribe_depth(sym).expect("depth");
    }
    let session = gw.data().session(0);
    assert_eq!(session.venue, "binance");

    let script = canonical_script("binance", &SYMBOLS);
    let verbs: BTreeSet<&str> = script.iter().map(verb_of).collect();
    assert_eq!(verbs.len(), 12, "the script must exercise every LiveDataSink verb: {verbs:?}");

    session.emit_all(&script);
    wait_for_calls(&got, script.len(), Duration::from_secs(10));
    // A frame the daemon invented (a gap marker, a duplicate) would arrive after the last real one.
    std::thread::sleep(Duration::from_millis(150));
    let received = got.recorded();
    assert_eq!(received.len(), script.len(), "extra events arrived: {received:?}");
    for sym in SYMBOLS {
        let (want, have) = (exact_lines_of(&script, sym), exact_lines_of(&received, sym));
        assert_eq!(have.len(), want.len(), "{sym}: {have:?}");
        for (i, (w, h)) in want.iter().zip(&have).enumerate() {
            assert_eq!(h, w, "{sym}: event {i} differs from what the venue client emitted");
        }
    }
    client.shutdown();
}

#[test]
fn a_seed_emitted_inside_subscribe_arrives_through_the_hop_after_the_sub_ok() {
    let data = ScriptedData::new(&["binance"]);
    data.set_default_hook(Arc::new(|sink, call: &SubscribeCall| {
        if call.verb == SubVerb::Bars {
            sink.seed_bars("binance", &call.symbol, "1m", vec![bar(0, 1.0), bar(60_000, 2.0)]);
            sink.stream_status(
                "binance",
                &call.symbol,
                "1m",
                StreamStatus::Live { gap_started_ts_ms: None },
            );
        }
    }));
    let gw = LoopbackGateway::start(data, ScriptedRest::new());
    let got = Arc::new(RecordingSink::default());
    let mut client = RemoteDataClient::new(GatewayOpener::new(gw.link()), "binance", got.clone())
        .expect("a reader");
    client.subscribe_bars("ETHUSDT", "1m").expect("bars");
    wait_for_calls(&got, 2, Duration::from_secs(10));
    let calls = got.recorded();
    assert_eq!(verb_of(&calls[0]), "seed_bars", "{calls:?}");
    assert_eq!(verb_of(&calls[1]), "stream_status", "{calls:?}");
    assert!(matches!(&calls[0], SinkCall::SeedBars { bars, .. } if bars.len() == 2));
    client.shutdown();
}

#[test]
fn the_recorder_sees_open_subscribe_unsubscribe_and_shutdown() {
    let gw = LoopbackGateway::start(ScriptedData::new(&["binance"]), ScriptedRest::new());
    let mut client = RemoteDataClient::new(
        GatewayOpener::new(gw.link()),
        "binance",
        Arc::new(RecordingSink::default()),
    )
    .expect("a reader");
    let quotes = client.subscribe_quotes("BTCUSDT").expect("quotes");
    client.subscribe_bars("ETHUSDT", "5m").expect("bars");
    client.unsubscribe(SubscriptionId(9_999)); // never issued: a no-op
    client.unsubscribe(quotes);
    client.shutdown();

    let session = gw.data().session(0);
    session.wait_for_call(|c| *c == DataCall::Shutdown);
    let calls = session.calls();
    assert_eq!(
        calls[..3],
        [
            DataCall::Subscribe(SubscribeCall {
                verb: SubVerb::Quotes,
                symbol: "BTCUSDT".into(),
                interval: None
            }),
            DataCall::Subscribe(SubscribeCall {
                verb: SubVerb::Bars,
                symbol: "ETHUSDT".into(),
                interval: Some("5m".into())
            }),
            DataCall::Unsubscribe(1),
        ],
        "an id nobody issued must not reach the venue client: {calls:?}"
    );
    assert_eq!(session.shutdowns(), 1);
    assert!(session.begin_shutdowns() >= 1, "the two-phase teardown starts with begin_shutdown");
    let line: Vec<String> = calls.iter().take(3).map(ToString::to_string).collect();
    assert_eq!(line, ["subscribe_quotes BTCUSDT", "subscribe_bars[5m] ETHUSDT", "unsubscribe 1"]);
    // The lane log opens with the open, then carries the same calls under the session's index.
    let log = gw.data().log();
    assert_eq!(log[0], (0, DataCall::Open { venue: "binance".into() }));
    assert!(log[1..].iter().all(|(session, _)| *session == 0));
    assert_eq!(gw.data().opened(), 1);
}

#[test]
fn a_scripted_refusal_and_a_down_venue_reach_the_client_in_the_lanes_words() {
    let data = ScriptedData::new(&["binance"]);
    data.refuse_all("BADSYM", Refusal::Unsupported("this scripted feed serves no such symbol"));
    data.refuse_all("WORSE", Refusal::Subscribe("the scripted venue rejected it".into()));
    let gw = LoopbackGateway::start(data.clone(), ScriptedRest::new());
    let mut client = RemoteDataClient::new(
        GatewayOpener::new(gw.link()),
        "binance",
        Arc::new(RecordingSink::default()),
    )
    .expect("a reader");
    match client.subscribe_quotes("BADSYM") {
        Err(LiveDataError::Unsupported(text)) => {
            assert_eq!(text, "this scripted feed serves no such symbol");
        }
        other => panic!("{other:?}"),
    }
    match client.subscribe_quotes("WORSE") {
        Err(LiveDataError::Subscribe(text)) => {
            assert!(text.contains("the scripted venue rejected it"), "{text}");
        }
        other => panic!("{other:?}"),
    }
    // A refused subscription still reached the client, and left the session usable.
    assert!(client.subscribe_quotes("BTCUSDT").is_ok());
    client.shutdown();

    // A venue that is down is an `open` that fails; the advertisement does not move.
    data.set_up(false);
    let before = gw.data().opened();
    let mut down = RemoteDataClient::new(
        GatewayOpener::new(gw.link()),
        "binance",
        Arc::new(RecordingSink::default()),
    )
    .expect("a reader");
    match down.subscribe_quotes("BTCUSDT") {
        Err(LiveDataError::Subscribe(text)) => {
            assert!(text.contains("the venue is down"), "the lane's words were lost: {text}");
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(gw.data().opened(), before, "a failed open must not count as a session");
    assert!(gw.data().refused_opens().iter().all(|v| v == "binance"));
    assert!(!gw.data().refused_opens().is_empty());
}

#[test]
fn a_scripted_client_builds_outside_any_server_for_an_in_process_broker() {
    let data = ScriptedData::new(&["binance"]);
    data.set_default_hook(Arc::new(|sink, call: &SubscribeCall| {
        sink.seed_bars("binance", &call.symbol, "1m", vec![bar(0, 1.0)]);
    }));
    let rec = Arc::new(RecordingSink::default());
    let mut client = data.build("binance", None, rec.clone()).expect("a client");
    client.subscribe_bars("BTCUSDT", "1m").expect("bars");
    // The seed was emitted before the call returned, so it is already there: no hop, no wait.
    assert_eq!(rec.recorded().len(), 1);
    let session = data.session(0);
    assert!(session.holder.is_none());
    session.emit(&SinkCall::MarkTick {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        px: 0.1 + 0.2,
        ts: 3,
    });
    assert_eq!(verb_of(&rec.recorded()[1]), "mark_tick");
    client.shutdown();
    assert_eq!(session.shutdowns(), 1);
}

// ---- the REST lane -------------------------------------------------------------------------------

#[test]
fn klines_funding_and_ticks_answer_the_configured_rows_and_the_recorder_holds_the_request() {
    let bars = vec![bar(0, 0.1 + 0.2), bar(60_000, 1.0 / 3.0)];
    let funding = vec![
        FundingRatePoint { ts_ms: 1, rate: 0.000_1 + 0.000_2, premium: Some(1.0 / 3.0) },
        FundingRatePoint { ts_ms: 2, rate: -0.000_05, premium: None },
    ];
    let quotes = vec![quote("EURUSD", 5, 1.0 / 3.0)];
    let rest = ScriptedRest::new()
        .with_klines("binance", Answer::rows(bars.clone()))
        .with_funding("binance", Answer::rows(funding.clone()))
        .with_ticks("dukascopy", Answer::rows(quotes.clone()));
    let gw = LoopbackGateway::start(ScriptedData::new(&[]), rest.clone());

    let got = RemoteKlines::new(gw.link(), "binance").fetch("BTCUSDT", "1m", 1_000, 2_000);
    assert_eq!(format!("{:?}", got.expect("klines")), format!("{bars:?}"), "a float drifted");
    let got = RemoteFunding::new(gw.link(), "binance").fetch("BTCUSDT", 5, 6).expect("funding");
    assert_eq!(got.len(), funding.len());
    for (g, w) in got.iter().zip(&funding) {
        assert_eq!((g.ts_ms, g.rate.to_bits(), g.premium), (w.ts_ms, w.rate.to_bits(), w.premium));
    }
    let got = RemoteTicks::new(gw.link()).fetch_quotes_range("EURUSD", 7, 8, None).expect("ticks");
    assert_eq!(got, quotes);

    // What the datahub ASKED, exactly: the lane, the venue, the symbol and the window.
    assert_eq!(
        rest.fetches(),
        vec![
            FetchCall {
                what: FetchWhat::Klines {
                    venue: "binance".into(),
                    symbol: "BTCUSDT".into(),
                    interval: "1m".into(),
                    start_ms: 1_000,
                    end_ms: 2_000,
                },
                credentialed: false,
            },
            FetchCall {
                what: FetchWhat::Funding {
                    venue: "binance".into(),
                    symbol: "BTCUSDT".into(),
                    start_ms: 5,
                    end_ms: 6,
                },
                credentialed: false,
            },
            FetchCall {
                what: FetchWhat::Ticks {
                    venue: "dukascopy".into(),
                    symbol: "EURUSD".into(),
                    start_ms: 7,
                    end_ms: 8,
                },
                credentialed: false,
            },
        ]
    );
}

#[test]
fn the_credentialed_lane_is_its_own_script_and_needs_the_write_key() {
    let bars = vec![bar(0, 1.0)];
    let rest = ScriptedRest::new()
        .with_klines("binance", Answer::rows(vec![bar(0, 9.0)]))
        .with_credentialed_klines("oanda", Answer::rows(bars.clone()));
    let gw = LoopbackGateway::start(ScriptedData::new(&[]), rest.clone());

    let got = RemoteKlines::credentialed(gw.link(), "oanda")
        .fetch("EUR_USD", "1h", 10, 20)
        .expect("credentialed klines");
    assert_eq!(got, bars);
    let fetches = rest.fetches();
    assert_eq!(fetches.len(), 1);
    assert!(fetches[0].credentialed, "the recorder must show which lane was used");
    assert!(
        matches!(&fetches[0].what, FetchWhat::Klines { venue, start_ms: 10, end_ms: 20, .. } if venue == "oanda")
    );

    // A link holding the Read key only cannot spend the credential, and the lane is never asked.
    let refused =
        RemoteKlines::credentialed(gw.read_link(), "oanda").fetch("EUR_USD", "1h", 10, 20);
    assert!(refused.is_err(), "{refused:?}");
    assert_eq!(rest.fetches().len(), 1, "a refused request must not reach the lane");

    // The advertisement says so too: the credentialed venue is a `cd_venue=`, not an `md_venue=`.
    let client = GatewayClient::connect(&gw.link(), Scope::Read).expect("connect");
    assert_eq!(advertised_cd_venues(client.features()), ["oanda"]);
    assert!(advertised_md_venues(client.features()).is_empty());
}

#[test]
fn a_failure_keeps_its_variant_and_words_and_a_missing_script_is_never_an_empty_answer() {
    let rest = ScriptedRest::new()
        .with_klines("binance", Answer::fetch_error("HTTP 429 from the venue"))
        .with_klines("bybit", Answer::refused("an interval this source cannot express"))
        .with_facts(RestFacts {
            klines: vec!["binance".into(), "bybit".into(), "okx".into()],
            ..RestFacts::default()
        });
    let gw = LoopbackGateway::start(ScriptedData::new(&[]), rest.clone());

    match RemoteKlines::new(gw.link(), "binance").fetch("BTCUSDT", "1m", 0, 1) {
        Err(SourceError::Fetch(text)) => assert_eq!(text, "HTTP 429 from the venue"),
        other => panic!("{other:?}"),
    }
    match RemoteKlines::new(gw.link(), "bybit").fetch("BTCUSDT", "7m", 0, 1) {
        Err(SourceError::Refused(text)) => {
            assert_eq!(text, "an interval this source cannot express");
        }
        other => panic!("{other:?}"),
    }
    match RemoteKlines::new(gw.link(), "okx").fetch("BTCUSDT", "1m", 0, 1) {
        Err(SourceError::Fetch(text)) => {
            assert!(text.contains("no script for a klines fetch of okx"), "{text}");
        }
        other => panic!("an unscripted venue must fail, not answer: {other:?}"),
    }
}

#[test]
fn on_fetch_answers_every_fetch_ahead_of_the_per_venue_scripts() {
    let rest = ScriptedRest::new()
        .with_klines("binance", Answer::rows(vec![bar(0, 1.0)]))
        .on_fetch(|_, _, _, _| Err(FetchFailure::Fetch("the raw script spoke".into())));
    let gw = LoopbackGateway::start(ScriptedData::new(&[]), rest.clone());
    match RemoteKlines::new(gw.link(), "binance").fetch("BTCUSDT", "1m", 0, 1) {
        Err(SourceError::Fetch(text)) => assert_eq!(text, "the raw script spoke"),
        other => panic!("{other:?}"),
    }
    assert_eq!(rest.fetches().len(), 1);
}

#[test]
fn a_slow_answer_stops_at_the_cancel_the_clients_stop_probe_sent() {
    let rest = ScriptedRest::new()
        .with_ticks("dukascopy", Answer::rows(Vec::new()).after(Duration::from_secs(20)));
    let gw = LoopbackGateway::start(ScriptedData::new(&[]), rest.clone());
    let stop = Arc::new(AtomicBool::new(false));
    let raise = Arc::clone(&stop);
    let trigger = std::thread::Builder::new()
        .name("test-stop-probe".into())
        .spawn(move || {
            std::thread::sleep(Duration::from_millis(250));
            raise.store(true, Ordering::SeqCst);
        })
        .expect("spawn");
    let probe = move || stop.load(Ordering::SeqCst);
    let started = Instant::now();
    let err = RemoteTicks::new(gw.link())
        .fetch_quotes_range("EURUSD", 0, 1, Some(&probe))
        .expect_err("the probe fired");
    trigger.join().expect("join");
    assert!(err.contains("cancelled"), "{err}");
    assert!(started.elapsed() < Duration::from_secs(5), "the cancel took {:?}", started.elapsed());
    rest.wait_for_cancels(1);
    assert_eq!(rest.cancels_seen(), 1);
}

#[test]
fn a_catalog_is_described_listed_and_searched_and_a_failure_is_the_lanes_words() {
    let rest = ScriptedRest::new()
        .with_catalog(
            "binance",
            vec![AssetClass::CryptoSpot, AssetClass::CryptoPerp],
            WireCatalogMode::QueryBacked,
            instruments("binance", 3),
        )
        .with_catalog_search("binance", Answer::rows(instruments("binance", 2)))
        .with_catalog(
            "bybit",
            vec![AssetClass::CryptoSpot],
            WireCatalogMode::Enumerable,
            Vec::new(),
        )
        .with_catalog_list("bybit", Answer::fetch_error("HTTP 503 from the exchangeInfo endpoint"));
    let gw = LoopbackGateway::start(ScriptedData::new(&[]), rest.clone());

    let provider = RemoteCatalog::open(gw.link(), "binance").expect("describe");
    assert_eq!(provider.asset_classes(), &[AssetClass::CryptoSpot, AssetClass::CryptoPerp]);
    assert_eq!(provider.mode(), CatalogMode::QueryBacked);
    assert_eq!(provider.list_instruments().expect("list"), instruments("binance", 3));
    let filter = SearchFilter { tab: Some(Tab::Perps), venue: None };
    assert_eq!(provider.search_remote("btc", &filter).expect("search").len(), 2);
    assert!(rest.calls().contains(&RestCall::CatalogDescribe { venue: "binance".into() }));
    let asked: Vec<FetchWhat> = rest.fetches().into_iter().map(|f| f.what).collect();
    assert_eq!(
        asked,
        vec![
            FetchWhat::CatalogList { venue: "binance".into() },
            FetchWhat::CatalogSearch {
                venue: "binance".into(),
                query: "btc".into(),
                tab: Some(WireTab::Perps),
                filter_venue: None,
            },
        ]
    );

    let failing = RemoteCatalog::open(gw.link(), "bybit").expect("describe");
    let err = failing.list_instruments().expect_err("the venue failed");
    assert_eq!(err.0, "HTTP 503 from the exchangeInfo endpoint");
}

#[test]
fn options_rest_the_option_index_and_both_feeds_answer_from_the_script() {
    let rest = ScriptedRest::new()
        .with_options_underlyings(Ok(vec!["BTC".into(), "ETH".into()]))
        .with_options_expiries("BTC", Ok(vec![expiry()]))
        .with_options_chain("BTC", "2026-10-30", Ok(chain()))
        .with_option_index(OptionIndexKind::Option, Answer::rows(instruments("deribit", 4)))
        .with_option_index(OptionIndexKind::Future, Answer::rows(instruments("deribit", 2)));
    let gw = LoopbackGateway::start(ScriptedData::new(&[]), rest.clone());
    let options = RemoteOptions::new(gw.link());

    assert_eq!(options.list_underlyings().expect("underlyings"), ["BTC", "ETH"]);
    assert_eq!(options.list_expiries("BTC").expect("expiries"), vec![expiry()]);
    // The chain is the configured one whatever the window; the window is in the recorder.
    assert_eq!(options.fetch_chain("BTC", "2026-10-30", Some(30), 0.03).expect("chain"), chain());
    let calls = rest.options_calls();
    assert!(matches!(&calls[0], OptionsCall::Underlyings));
    assert!(matches!(&calls[1], OptionsCall::Expiries { underlying } if underlying == "BTC"));
    assert!(
        matches!(&calls[2], OptionsCall::Chain { strike_window: Some(30), r, .. } if r.to_bits() == 0.03f64.to_bits())
    );
    let err = options.fetch_chain("ETH", "2026-10-30", None, 0.03).expect_err("no such script");
    assert!(err.contains("no options script"), "{err}");

    assert_eq!(options.option_index_instruments().expect("both kinds").len(), 6);
    rest.with_option_index(OptionIndexKind::Future, Answer::fetch_error("futures listing failed"));
    let err = options.option_index_instruments().expect_err("half an index is not an index");
    assert_eq!(err, "futures listing failed");

    // Both feeds: the rows the test pushes arrive, and the lane is shut down with the socket.
    let (marks_tx, marks_rx) = mpsc::channel::<usize>();
    let marks = RemoteOptionsFeed::markprice(gw.link(), vec!["btc_usd".into()], move |rows| {
        let _ = marks_tx.send(rows.len());
    })
    .expect("marks feed");
    let feed = rest.feed(0);
    assert!(
        matches!(&feed.spec, OptionsFeedSpec::MarkPrice { indexes } if indexes == &["btc_usd".to_string()])
    );
    let mut row = OptionTicker::new(1, "BTC-30OCT26-70000-C");
    row.mark = Some(0.1 + 0.2);
    feed.sink.marks(vec![row, OptionTicker::new(2, "BTC-30OCT26-72000-P")]);
    assert_eq!(marks_rx.recv_timeout(Duration::from_secs(5)).expect("a marks batch"), 2);
    drop(marks);
    feed.wait_shut_down();
    assert!(feed.is_shut_down());
    assert_eq!(rest.feeds_opened(), 1);
}

#[test]
fn a_rolling_family_and_a_presence_word_are_scripted_and_recorded() {
    let rest = ScriptedRest::new()
        .with_family("polymarket", "btc-updown-5m", Ok(vec!["TOKEN-B".into(), "TOKEN-A".into()]))
        .with_family_by("polymarket", "eth-updown-5m", |now_ms| {
            Ok(vec![format!("TOKEN-{}", now_ms / 300_000)])
        })
        .with_presence("oanda", CredentialPresence::Present);
    let gw = LoopbackGateway::start(ScriptedData::new(&[]), rest.clone());

    let mut family = RemoteRollingFamily::new(gw.link(), "btc-updown-5m").expect("grammar");
    let set = family.desired(1_700_000_000_000);
    assert_eq!(
        set.expect("resolved").into_iter().collect::<Vec<_>>(),
        ["TOKEN-A".to_string(), "TOKEN-B".to_string()]
    );
    let mut rotating = RemoteRollingFamily::new(gw.link(), "eth-updown-5m").expect("grammar");
    let set = rotating.desired(600_000).expect("resolved");
    assert_eq!(set.into_iter().collect::<Vec<_>>(), ["TOKEN-2".to_string()]);
    assert!(rest.calls().contains(&RestCall::ResolveFamily {
        venue: "polymarket".into(),
        family: "eth-updown-5m".into(),
        now_ms: 600_000,
    }));

    assert_eq!(credential_presence(&gw.link(), "oanda"), CredentialPresence::Present);
    assert_eq!(credential_presence(&gw.link(), "binance"), CredentialPresence::NotNeeded);
    assert!(rest.calls().contains(&RestCall::Presence { venue: "oanda".into() }));
}

// ---- the gateway ---------------------------------------------------------------------------------

#[test]
fn the_advertisement_is_derived_from_what_is_scripted_in_the_order_it_was_scripted() {
    let rest = ScriptedRest::new()
        .with_klines("bybit", Answer::rows(Vec::new()))
        .with_klines("binance", Answer::rows(Vec::new()))
        .with_klines("bybit", Answer::rows(vec![bar(0, 1.0)])); // re-scripting keeps its place
    assert_eq!(rest.facts().klines, ["bybit", "binance"]);
    assert!(rest.facts().funding.is_empty());
    assert!(!rest.facts().options);

    let gw = LoopbackGateway::start(ScriptedData::new(&["binance", "polymarket"]), rest);
    let client = GatewayClient::connect(&gw.link(), Scope::Read).expect("connect");
    assert_eq!(advertised_md_venues(client.features()), ["binance", "polymarket"]);
    let features = client.features();
    assert!(features.iter().any(|f| f == "gw_fetch"), "{features:?}");
    assert!(!features.iter().any(|f| f == "gw_ticks"), "{features:?}");
}

#[test]
fn dropping_the_gateway_stops_the_server_and_frees_the_port() {
    let addr;
    {
        let mut gw = LoopbackGateway::start(ScriptedData::new(&["binance"]), ScriptedRest::new());
        addr = gw.addr();
        assert!(addr.ip().is_loopback());
        assert_eq!(credential_presence(&gw.link(), "binance"), CredentialPresence::NotNeeded);
        gw.stop().expect("the server drained and returned");
        gw.stop().expect("stop is idempotent");
    }
    assert!(
        std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(500)).is_err(),
        "the server was still listening after the gateway was dropped"
    );
}
