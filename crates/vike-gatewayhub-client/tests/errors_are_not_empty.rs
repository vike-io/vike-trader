#![cfg(feature = "data")]
//! **R12 — a failure is an `Err`, never an empty answer.** An empty family would unsubscribe every live
//! stream; an empty source would be stored as "the venue had no rows" and spend a commit key for good;
//! an empty catalog would replace a picker's universe. So every remote seam maps a dead link, a dying
//! daemon and a lying frame to the ERROR type of the seam it implements — a transport failure as
//! `Fetch("gatewayhub ...")` and NEVER `Refused` — and a daemon-side failure keeps its variant and its
//! inner text byte for byte.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use vike_catalog::{CatalogProvider, SearchFilter};
use vike_data::live::SymbolResolver;
use vike_data::source::{FundingRateSource, KlineSource, SourceError};
use vike_gatewayhub_client::catalog::RemoteCatalog;
use vike_gatewayhub_client::credential::{CredentialPresence, credential_presence};
use vike_gatewayhub_client::history::{RemoteFunding, RemoteKlines, RemoteTicks};
use vike_gatewayhub_client::options::RemoteOptions;
use vike_gatewayhub_client::proto::{
    FetchFailure, FetchProgress, FetchState, OptionsCall, OptionsReply, Request, Response,
};
use vike_gatewayhub_client::resolve::RemoteRollingFamily;

#[allow(dead_code, clippy::allow_attributes)]
#[path = "support/fake_peer.rs"]
mod fake_peer;

use fake_peer::{FakePeer, PeerConfig, Step, dead_addr, link_to};

fn is_gateway_fetch_error(e: &SourceError, why: &str) {
    match e {
        SourceError::Fetch(text) => {
            assert!(text.contains("gatewayhub"), "{why}: the text names the gateway: {text}")
        }
        SourceError::Refused(text) => {
            panic!("{why}: a transport failure was reported as Refused: {text}")
        }
    }
}

fn progress(rid: u64) -> Response {
    Response::FetchProgress(FetchProgress {
        rid,
        state: FetchState::Running,
        pages: 0,
        rows: 0,
        elapsed_ms: 1,
        next_in_ms: 100,
    })
}

#[test]
fn every_history_source_on_a_dead_gateway_is_fetch_gatewayhub_never_refused_never_empty() {
    let link = link_to(&dead_addr());

    let klines = RemoteKlines::new(link.clone(), "binance").fetch("BTCUSDT.P", "1m", 0, 1);
    is_gateway_fetch_error(&klines.expect_err("not an empty Vec"), "klines");

    let oanda = RemoteKlines::credentialed(link.clone(), "oanda").fetch("EUR_USD", "1m", 0, 1);
    is_gateway_fetch_error(&oanda.expect_err("not an empty Vec"), "credentialed klines");

    let funding = RemoteFunding::new(link.clone(), "binance").fetch("BTCUSDT.P", 0, 1);
    is_gateway_fetch_error(&funding.expect_err("not an empty Vec"), "funding");

    let ticks = RemoteTicks::new(link).fetch_quotes_range("EURUSD", 0, 1, None);
    let text = ticks.expect_err("not an empty Vec");
    assert!(text.contains("gatewayhub"), "{text}");
}

#[test]
fn a_daemon_that_dies_on_contact_or_mid_fetch_is_an_error() {
    let mut dies = PeerConfig::new();
    dies.close_on_accept = true;
    let peer = FakePeer::start(dies);
    let e = RemoteKlines::new(peer.link(), "binance").fetch("X", "1m", 0, 1).unwrap_err();
    is_gateway_fetch_error(&e, "close on accept");

    let peer =
        FakePeer::start(PeerConfig::new().with_rpc(|_| vec![Step::Send(progress(1)), Step::Close]));
    let e = RemoteKlines::new(peer.link(), "binance").fetch("X", "1m", 0, 1).unwrap_err();
    is_gateway_fetch_error(&e, "closed mid-fetch");
}

/// The in-process `SourceError` variants round-trip with identical inner text.
#[test]
fn a_daemon_failure_keeps_its_variant_and_its_text() {
    for failure in [
        FetchFailure::Fetch("venue fetch: HTTP 429 from fapi.binance.com".into()),
        FetchFailure::Refused(
            "hyperliquid HL spot `PURR/USDC` cannot be fetched by one symbol".into(),
        ),
    ] {
        let f = failure.clone();
        let peer = FakePeer::start(PeerConfig::new().with_rpc(move |_| {
            vec![
                Step::Send(progress(1)),
                Step::Send(Response::FetchFailed { rid: 1, failure: f.clone() }),
            ]
        }));
        let got = RemoteKlines::new(peer.link(), "binance").fetch("X", "1m", 0, 1).unwrap_err();
        assert_eq!(got, failure.clone().into_source_error(), "variant and text unchanged");
        let got = RemoteFunding::new(peer.link(), "binance").fetch("X", 0, 1).unwrap_err();
        assert_eq!(got, failure.into_source_error());
    }
}

#[test]
fn a_tick_fetch_failure_is_the_daemons_string_unchanged() {
    let peer = FakePeer::start(PeerConfig::new().with_rpc(|_| {
        vec![
            Step::Send(progress(1)),
            Step::Send(Response::FetchFailed {
                rid: 1,
                failure: FetchFailure::Fetch("dukascopy 2026-10-12T05: HTTP 503".into()),
            }),
        ]
    }));
    let got = RemoteTicks::new(peer.link()).fetch_quotes_range("EURUSD", 0, 1, None);
    assert_eq!(got.unwrap_err(), "dukascopy 2026-10-12T05: HTTP 503");
}

#[test]
fn a_catalog_on_a_dead_gateway_is_a_catalog_error_and_a_dying_daemon_is_too() {
    let e = RemoteCatalog::open(link_to(&dead_addr()), "binance").unwrap_err();
    assert!(e.0.contains("gatewayhub"), "{e}");

    // Opens fine, then the daemon refuses the listing with its own words.
    let peer = FakePeer::start(PeerConfig::new().with_rpc(|req| match req {
        Request::CatalogDescribe { .. } => vec![Step::Send(Response::CatalogDescribed {
            asset_classes: vec![vike_model::AssetClass::CryptoSpot],
            mode: vike_gatewayhub_client::proto::WireCatalogMode::Enumerable,
        })],
        _ => vec![
            Step::Send(progress(1)),
            Step::Send(Response::FetchFailed {
                rid: 1,
                failure: FetchFailure::Fetch("binance exchangeInfo: connection reset".into()),
            }),
        ],
    }));
    let catalog = RemoteCatalog::open(peer.link(), "binance").expect("describe answered");
    let e = catalog.list_instruments().unwrap_err();
    assert_eq!(e.0, "binance exchangeInfo: connection reset", "the provider's text, unchanged");
    let e = catalog.search_remote("btc", &SearchFilter::default()).unwrap_err();
    assert_eq!(e.0, "binance exchangeInfo: connection reset");

    // The daemon goes away entirely: still an error, never an empty listing.
    let link = peer.link();
    let catalog = RemoteCatalog::open(link, "binance").unwrap();
    drop(peer);
    let e = catalog.list_instruments().unwrap_err();
    assert!(e.0.contains("gatewayhub"), "{e}");
}

#[test]
fn the_family_grammar_is_checked_offline_and_a_dead_gateway_is_an_error_not_an_empty_set() {
    // Offline: no socket exists, and the words are the bridge's own (pinned in vike-catalog).
    let link = link_to(&dead_addr());
    let refusal = RemoteRollingFamily::new(link.clone(), "eth-updown-1h").unwrap_err();
    assert_eq!(refusal, vike_catalog::polymarket_family_minutes("eth-updown-1h").unwrap_err());
    assert!(refusal.contains("does not end in an interval like `-5m`"), "{refusal}");

    let mut family = RemoteRollingFamily::new(link, "btc-updown-5m").expect("a legal family");
    assert_eq!(family.group(), Some("btc-updown-5m"));
    let err = family.desired(1_700_000_000_000).expect_err("UNKNOWN, never empty");
    assert!(err.contains("gatewayhub"), "{err}");
}

#[test]
fn the_resolvers_own_failure_arrives_unchanged_and_a_stale_connection_is_retried_once() {
    // Call 1 answers and the daemon hangs up; call 2 finds the held connection dead and redials.
    let peer = FakePeer::start(PeerConfig::new().with_rpc(|req| match req {
        Request::ResolveFamily { family, .. } if family == "boom-5m" => {
            vec![Step::Send(Response::Error("gamma: HTTP 502 for slug boom-5m-1700000100".into()))]
        }
        Request::ResolveFamily { .. } => vec![
            Step::Send(Response::FamilyResolved { symbols: vec!["b".into(), "a".into()] }),
            Step::Close,
        ],
        _ => vec![Step::Close],
    }));
    let mut family = RemoteRollingFamily::new(peer.link(), "btc-updown-5m").unwrap();
    let first = family.desired(1).expect("answered");
    assert_eq!(first.into_iter().collect::<Vec<_>>(), ["a", "b"], "ascending");
    let second = family.desired(2).expect("the stale held connection is retried on a fresh one");
    assert_eq!(second.len(), 2);
    assert_eq!(peer.connections(), 2, "one redial, no more");

    let mut boom = RemoteRollingFamily::new(peer.link(), "boom-5m").unwrap();
    assert_eq!(boom.desired(1).unwrap_err(), "gamma: HTTP 502 for slug boom-5m-1700000100");
}

/// **A wedged daemon costs the family resolve its OWN bound, not the 300 s a REST-backed verb gets.**
/// The caller is the recorder's tick, which reads its stop flag only after the tick returns, so a
/// daemon that accepted and stopped answering must cost the stop path what the in-process Gamma call did
/// (`RESOLVE_REPLY_BOUND`, 30 s), here narrowed to 300 ms for the test while the link's own idle bound
/// is 5 s. It is an `Err` (UNKNOWN, never an empty set), a timeout is NOT a stale connection (one
/// request reaches the daemon for the wedged call, no redial), and the wedged connection is dropped, so
/// the next call dials a fresh one.
#[test]
fn a_wedged_daemon_costs_the_family_resolve_its_own_bound_and_is_not_asked_twice() {
    let calls = Arc::new(AtomicUsize::new(0));
    let n = Arc::clone(&calls);
    let peer = FakePeer::start(PeerConfig::new().with_rpc(move |req| match req {
        Request::ResolveFamily { .. } if n.fetch_add(1, Ordering::SeqCst) == 0 => {
            vec![Step::Send(Response::FamilyResolved { symbols: vec!["a".into()] })]
        }
        // Every later resolve is read and never answered.
        Request::ResolveFamily { .. } => vec![Step::Hang],
        _ => vec![Step::Close],
    }));
    let bound = Duration::from_millis(300);
    let mut family = RemoteRollingFamily::new(peer.link(), "btc-updown-5m")
        .expect("a legal family")
        .with_reply_bound(bound);
    family.desired(1).expect("the first resolve is answered and its connection is held");

    let started = Instant::now();
    let err = family.desired(2).expect_err("a wedged daemon is UNKNOWN, never an empty set");
    let took = started.elapsed();
    assert!(err.contains("gatewayhub") && err.contains("no reply"), "{err}");
    assert!(
        took >= Duration::from_millis(250) && took < Duration::from_secs(3),
        "the family bound fires, not the link's 5 s idle bound: {took:?}"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2, "a timeout is not stale: not asked twice");
    assert_eq!(peer.connections(), 1, "…and no redial");

    // The wedged connection was dropped: the next resolve dials a fresh one (and is wedged again).
    family.desired(3).expect_err("still wedged");
    assert_eq!(peer.connections(), 2);
    assert_eq!(calls.load(Ordering::SeqCst), 3);
}

#[test]
fn an_unreachable_gateway_answers_not_checked_and_never_absent() {
    assert_eq!(
        credential_presence(&link_to(&dead_addr()), "oanda"),
        CredentialPresence::NotChecked
    );

    for word in CredentialPresence::ALL {
        let peer = FakePeer::start(
            PeerConfig::new()
                .with_rpc(move |_| vec![Step::Send(Response::PresenceIs { presence: word })]),
        );
        assert_eq!(credential_presence(&peer.link(), "oanda"), word, "the word survives the wire");
    }
    // A daemon that refuses the question is also "nothing was read".
    let peer = FakePeer::start(PeerConfig::new()); // default script answers Error
    assert_eq!(credential_presence(&peer.link(), "oanda"), CredentialPresence::NotChecked);
}

#[test]
fn the_options_calls_fail_as_errors_and_the_providers_text_is_unchanged() {
    let e = RemoteOptions::new(link_to(&dead_addr())).list_underlyings().unwrap_err();
    assert!(e.contains("gatewayhub"), "{e}");

    let peer = FakePeer::start(PeerConfig::new().with_rpc(|req| match req {
        Request::OptionsRest { call: OptionsCall::Underlyings } => {
            vec![Step::Send(Response::OptionsReplied(OptionsReply::Underlyings(vec![
                "BTC".into(),
                "ETH".into(),
            ])))]
        }
        _ => vec![Step::Send(Response::Error("deribit book summary: HTTP 503".into()))],
    }));
    let options = RemoteOptions::new(peer.link());
    assert_eq!(options.list_underlyings().unwrap(), ["BTC", "ETH"]);
    assert_eq!(options.list_expiries("BTC").unwrap_err(), "deribit book summary: HTTP 503");
}

#[test]
fn a_catalog_listing_the_daemon_cannot_describe_is_refused_not_guessed() {
    let peer = FakePeer::start(PeerConfig::new().with_rpc(|_| {
        vec![Step::Send(Response::Error(
            "catalog: `alpaca` is not served here (credentialed)".into(),
        ))]
    }));
    let e = RemoteCatalog::open(peer.link(), "alpaca").unwrap_err();
    assert_eq!(e.0, "catalog: `alpaca` is not served here (credentialed)");
}
