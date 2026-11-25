#![cfg(feature = "data")]
//! **Decision 0112, the client leg: refuse LOCALLY, naming the missing string, before anything is
//! sent** — plus the handshake's refusals (a version skew naming both numbers, a peer that is not a
//! gatewayhub refused BEFORE a mac is signed, a wrong key, a scope with no key).
//!
//! A real daemon cannot prove "nothing was sent": it answers the frame either way. The fake peer
//! records every frame, so each refusal here ends with an empty request log.

use std::sync::Arc;
use std::time::Duration;

use vike_data::live::SymbolResolver;
use vike_data::source::{FundingRateSource, KlineSource, SourceError};
use vike_data::{DataClient, DataClientError, DataClientSink, RecordingSink};
use vike_gatewayhub_client::catalog::CatalogProviderGateway;
use vike_gatewayhub_client::client::{GatewayClient, GatewayLink, gateway_text};
use vike_gatewayhub_client::credential::credential_presence;
use vike_gatewayhub_client::history::{FundingRateSourceGateway, GatewayTicks, KlineSourceGateway};
use vike_gatewayhub_client::opener::StreamOpenerGateway;
use vike_gatewayhub_client::options::GatewayOptions;
use vike_gatewayhub_client::proto::{
    FEATURE_GATEWAY_CATALOG, FEATURE_GATEWAY_FAMILY, FEATURE_GATEWAY_FETCH,
    FEATURE_GATEWAY_FETCH_CREDENTIALED, FEATURE_GATEWAY_FETCH_PROGRESS, FEATURE_GATEWAY_OPTIONS,
    FEATURE_GATEWAY_OPTIONS_LISTING, FEATURE_GATEWAY_TICKS, GATEWAYHUB_PLANE_SENTINEL,
    GATEWAYHUB_PROTO_VERSION, Request, Response,
};
use vike_gatewayhub_client::resolve::SymbolResolverGateway;
use vike_hub_protocol::auth::{HubKeys, Scope};
use vike_hub_protocol::call::HubCallError;
use vike_hub_protocol::features::FEATURE_GATEWAY_STATUS;
use vike_market_stream::market::DataClientStream;
use vike_market_stream::opener::StreamOpener;
use vike_model::CredentialPresence;
use vike_utility::errors::FailureOutcome;

use vike_gatewayhub_client::test_support::fake_peer;

use fake_peer::{FakePeer, PeerConfig, Step, fast_timings, features, read_only_keys};
use vike_log::poll::wait_until;

/// A peer advertising everything EXCEPT `missing`.
fn peer_without(missing: &str) -> FakePeer {
    let f: Vec<String> =
        features(&["binance"], &["oanda"]).into_iter().filter(|s| s != missing).collect();
    FakePeer::start(PeerConfig::new().with_features(f))
}

fn refusal_text(e: &SourceError) -> &str {
    match e {
        SourceError::Fetch(t) => t.text(),
        SourceError::Refused(t) => panic!("a missing capability is not a Refused source: {t}"),
    }
}

#[test]
fn a_missing_lane_feature_is_refused_by_name_and_nothing_is_sent() {
    // klines and funding need `gateway_fetch`
    let peer = peer_without(FEATURE_GATEWAY_FETCH);
    let e = KlineSourceGateway::new(peer.link(), "binance").fetch("X", "1m", 0, 1).unwrap_err();
    assert!(refusal_text(&e).contains(&format!("{FEATURE_GATEWAY_FETCH:?}")), "{e}");
    let e = FundingRateSourceGateway::new(peer.link(), "binance").fetch("X", 0, 1).unwrap_err();
    assert!(refusal_text(&e).contains(&format!("{FEATURE_GATEWAY_FETCH:?}")), "{e}");
    assert!(peer.requests().is_empty(), "nothing followed the handshake: {:?}", peer.requests());

    // the progress contract is required of every fetch, whatever the lane
    let peer = peer_without(FEATURE_GATEWAY_FETCH_PROGRESS);
    let e = KlineSourceGateway::new(peer.link(), "binance").fetch("X", "1m", 0, 1).unwrap_err();
    assert!(refusal_text(&e).contains(FEATURE_GATEWAY_FETCH_PROGRESS), "{e}");
    assert!(peer.requests().is_empty());

    // ticks have their own string
    let peer = peer_without(FEATURE_GATEWAY_TICKS);
    let e = GatewayTicks::new(peer.link()).fetch_quotes_range("EURUSD", 0, 1, None).unwrap_err();
    assert!(e.contains(FEATURE_GATEWAY_TICKS) && e.contains("upgrade vike-gatewayhub"), "{e}");
    assert!(peer.requests().is_empty());
}

#[test]
fn the_credentialed_lane_needs_its_string_and_the_venue_in_the_credentialed_data_venue_set() {
    let peer = peer_without(FEATURE_GATEWAY_FETCH_CREDENTIALED);
    let e = KlineSourceGateway::credentialed(peer.link(), "oanda")
        .fetch("EUR_USD", "1m", 0, 1)
        .unwrap_err();
    assert!(refusal_text(&e).contains(FEATURE_GATEWAY_FETCH_CREDENTIALED), "{e}");
    assert!(peer.requests().is_empty());

    // The lane is advertised but this venue is not in the `credentialed_data_venue=` set: the daemon holds no key for it.
    let peer = FakePeer::start(PeerConfig::new().with_features(features(&["binance"], &[])));
    let e = KlineSourceGateway::credentialed(peer.link(), "oanda")
        .fetch("EUR_USD", "1m", 0, 1)
        .unwrap_err();
    assert!(refusal_text(&e).contains("credentialed_data_venue=oanda"), "{e}");
    assert!(peer.requests().is_empty());
}

#[test]
fn the_catalog_options_family_and_presence_lanes_each_refuse_by_name() {
    let peer = peer_without(FEATURE_GATEWAY_CATALOG);
    let e = CatalogProviderGateway::open(peer.link(), "binance").unwrap_err();
    assert!(e.0.text().contains(FEATURE_GATEWAY_CATALOG), "{e}");
    assert!(peer.requests().is_empty());

    let peer = peer_without(FEATURE_GATEWAY_OPTIONS);
    let e = GatewayOptions::new(peer.link()).list_underlyings().unwrap_err();
    assert!(e.contains(FEATURE_GATEWAY_OPTIONS), "{e}");
    let e = GatewayOptions::new(peer.link()).option_index_instruments().unwrap_err();
    assert!(e.contains(FEATURE_GATEWAY_OPTIONS), "{e}");
    assert!(peer.requests().is_empty());

    // An older daemon serves the options surface but not the listing: refused here, by name.
    let peer = peer_without(FEATURE_GATEWAY_OPTIONS_LISTING);
    let e = GatewayOptions::new(peer.link()).listing(false).unwrap_err();
    assert!(e.contains(FEATURE_GATEWAY_OPTIONS_LISTING), "{e}");
    assert!(peer.requests().is_empty());

    let peer = peer_without(FEATURE_GATEWAY_FAMILY);
    let mut family = SymbolResolverGateway::new(peer.link(), "btc-updown-5m").unwrap();
    let e = family.desired(1).unwrap_err();
    assert!(e.contains(FEATURE_GATEWAY_FAMILY), "{e}");
    assert!(peer.requests().is_empty());

    // presence: an unarmed lane is "nothing was read", never "absent"
    let peer = peer_without("gateway_presence");
    assert_eq!(credential_presence(&peer.link(), "oanda"), CredentialPresence::NotChecked);
    assert!(peer.requests().is_empty());
}

/// A venue the daemon does not list in `market_data_venue=` is refused by name, and `DataOpen` is never sent.
#[test]
fn a_stream_venue_the_daemon_does_not_advertise_is_refused_by_name() {
    let peer = FakePeer::start(PeerConfig::new().with_features(features(&["polymarket"], &[])));
    let sink = Arc::new(RecordingSink::default());
    let dyn_sink: Arc<dyn DataClientSink> = sink;
    let mut c =
        DataClientStream::new(StreamOpenerGateway::new(peer.link()), "binance", dyn_sink).unwrap();
    // The refused open is a link that stays down: NOT NOW, naming why.
    let Err(DataClientError::NotNow(text)) = c.subscribe_quotes("BTCUSDT") else { panic!() };
    assert!(text.contains("market_data_venue=binance"), "{text}");
    assert!(
        !peer.requests().iter().any(|r| matches!(r, Request::DataOpen { .. })),
        "DataOpen was never sent"
    );
    c.shutdown();
}

// ---- The handshake ---------------------------------------------------------------------------------

#[test]
fn a_version_skew_is_refused_naming_both_numbers_before_anything_is_signed() {
    let mut config = PeerConfig::new();
    config.version = 9;
    let peer = FakePeer::start(config);
    let e = GatewayClient::connect(&peer.link(), Scope::Read).unwrap_err();
    assert!(matches!(e, HubCallError::Protocol(_)), "{e:?}");
    let text = e.to_string();
    assert!(text.contains(&GATEWAYHUB_PROTO_VERSION.to_string()) && text.contains('9'), "{text}");
    assert!(wait_until(Duration::from_secs(2), || peer.hellos() == 1), "the Hello was sent");
    assert!(
        peer.auths().is_empty(),
        "no Auth: a mac for a daemon we cannot talk to is never minted"
    );
}

#[test]
fn a_peer_that_is_not_a_gatewayhub_is_refused_before_a_mac_is_signed() {
    // The datahub's plane sentinel instead of ours: a mac signed here would be a mac for another service.
    let peer = FakePeer::start(
        PeerConfig::new().with_features(vec!["load_bars".into(), "backfill".into()]),
    );
    let e = GatewayClient::connect(&peer.link(), Scope::Read).unwrap_err();
    assert!(matches!(e, HubCallError::Protocol(_)), "{e:?}");
    let text = e.to_string();
    assert!(text.contains(GATEWAYHUB_PLANE_SENTINEL) && text.contains("load_bars"), "{text}");
    assert!(wait_until(Duration::from_secs(2), || peer.hellos() == 1));
    assert!(peer.auths().is_empty());
}

/// **A refused key reads ONE address and ONE id through every seam** (`gateway_text` decides on the variant, never on
/// the text): this client's own sentence names the gatewayhub once, under the door's id, whether it is worded by
/// `gateway_text`, by the stream opener (the failure inside the `io::Error`) or by the catalog provider; the daemon's
/// own words are prefixed once.
#[test]
fn a_refused_key_reads_one_address_and_one_id_through_every_seam() {
    let peer = FakePeer::start(PeerConfig::new());
    let addr = peer.address_string();
    let wrong =
        GatewayLink::new(addr.clone(), HubKeys::new(b"not the key".to_vec(), b"nor this".to_vec()))
            .with_timings(fast_timings());
    let line =
        format!("error [hub.access.denied]: gatewayhub {addr}: auth denied (observe): bad mac");
    let denied = GatewayClient::connect(&wrong, Scope::Read).unwrap_err();
    assert_eq!(gateway_text(&addr, &denied), line);
    let opener = StreamOpenerGateway::new(wrong.clone());
    assert_eq!(opener.failure_text(&std::io::Error::from(denied)), line);
    let catalog = CatalogProviderGateway::open(wrong, "binance").unwrap_err();
    // The id rides beside the bare sentence, and the carrier renders it once.
    assert_eq!(catalog.0.rendered(), line);
    assert_eq!(catalog.to_string(), format!("catalog error: {line}"));

    let daemon = HubCallError::Refused { id: None, text: "no adapter for binance".into() };
    assert_eq!(gateway_text(&addr, &daemon), format!("gatewayhub {addr}: no adapter for binance"));
    let socket = std::io::Error::from(std::io::ErrorKind::BrokenPipe);
    assert_eq!(opener.failure_text(&socket), format!("gatewayhub {addr}: {socket}"));
}

#[test]
fn a_wrong_key_is_denied_and_a_missing_key_never_dials() {
    let peer = FakePeer::start(PeerConfig::new());
    let wrong = GatewayLink::new(
        peer.address_string(),
        HubKeys::new(b"not the key".to_vec(), b"nor this".to_vec()),
    )
    .with_timings(fast_timings());
    let e = GatewayClient::connect(&wrong, Scope::Read).unwrap_err();
    assert!(matches!(e, HubCallError::AccessDenied(_)), "{e:?}");
    // The daemon's `AuthDenied`, shown once under the client's one access id.
    assert_eq!(
        e.to_string(),
        format!(
            "error [hub.access.denied]: gatewayhub {}: auth denied (observe): bad mac",
            peer.address_string()
        )
    );
    assert_eq!(peer.auths(), [(Scope::Read, false)]);

    let before = peer.connections();
    let read_only =
        GatewayLink::new(peer.address_string(), read_only_keys()).with_timings(fast_timings());
    let e = GatewayClient::connect(&read_only, Scope::Write).unwrap_err();
    assert!(matches!(e, HubCallError::AccessDenied(_)), "{e:?}");
    assert_eq!(peer.connections(), before, "no key for the scope: nothing is dialled");
    // The same link signs a Read connection fine.
    GatewayClient::connect(&read_only, Scope::Read).expect("the Read key works");
}

/// **What a refusal IS, by its variant** (decision 0301): a verb string the daemon did not advertise is
/// `NotOffered` naming it, nothing sent; the daemon's own `Response::Error` is `Refused` with its words, and the
/// connection that carried that answer stays usable. Both read REFUSED: no retry changes either.
#[test]
fn a_missing_verb_string_is_not_offered_and_the_daemons_error_is_refused() {
    let peer = peer_without(FEATURE_GATEWAY_STATUS);
    let mut c = GatewayClient::connect(&peer.link(), Scope::Read).unwrap();
    let e = c.status().unwrap_err();
    assert!(
        matches!(&e, HubCallError::NotOffered { feature, .. } if feature == FEATURE_GATEWAY_STATUS),
        "{e:?}"
    );
    assert_eq!(e.failure_outcome(), FailureOutcome::Refused);
    assert!(peer.requests().is_empty(), "nothing was sent: {:?}", peer.requests());

    let peer = FakePeer::start(
        PeerConfig::new()
            .with_rpc(|_| vec![Step::Send(Response::Error("gatewayhub: no status here".into()))]),
    );
    let mut c = GatewayClient::connect(&peer.link(), Scope::Read).unwrap();
    let e = c.status().unwrap_err();
    assert!(
        matches!(&e, HubCallError::Refused { id: None, text } if text == "gatewayhub: no status here"),
        "{e:?}"
    );
    assert_eq!(e.failure_outcome(), FailureOutcome::Refused);
    assert!(c.is_usable(), "an answer is not a broken link");
}

#[test]
fn a_connected_client_pings_and_reports_what_it_authenticated_as() {
    let peer = FakePeer::start(PeerConfig::new());
    let mut c = GatewayClient::connect(&peer.link(), Scope::Write).unwrap();
    assert_eq!(c.scope(), Scope::Write);
    c.ping().expect("Ping → Pong");
    assert!(c.features().iter().any(|f| f == GATEWAYHUB_PLANE_SENTINEL));
    assert!(c.is_usable());
}
