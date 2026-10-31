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
use vike_data::{DataClient, LiveDataError, LiveDataSink, RecordingSink};
use vike_gatewayhub_client::catalog::RemoteCatalog;
use vike_gatewayhub_client::client::{GatewayClient, GatewayLink};
use vike_gatewayhub_client::credential::{CredentialPresence, credential_presence};
use vike_gatewayhub_client::history::{RemoteFunding, RemoteKlines, RemoteTicks};
use vike_gatewayhub_client::opener::GatewayOpener;
use vike_gatewayhub_client::options::RemoteOptions;
use vike_gatewayhub_client::proto::{
    FEATURE_GW_CATALOG, FEATURE_GW_FAMILY, FEATURE_GW_FETCH, FEATURE_GW_FETCH_CREDENTIALED,
    FEATURE_GW_FETCH_PROGRESS, FEATURE_GW_OPTIONS, FEATURE_GW_TICKS, GATEWAYHUB_PLANE_SENTINEL,
    GATEWAYHUB_PROTO_VERSION, Request,
};
use vike_gatewayhub_client::resolve::RemoteRollingFamily;
use vike_hub_proto::auth::{HubKeys, Scope};
use vike_market_stream::market::RemoteDataClient;

#[allow(dead_code, clippy::allow_attributes)]
#[path = "support/fake_peer.rs"]
mod fake_peer;

use fake_peer::{FakePeer, PeerConfig, fast_timings, features, read_only_keys};

/// A peer advertising everything EXCEPT `missing`.
fn peer_without(missing: &str) -> FakePeer {
    let f: Vec<String> =
        features(&["binance"], &["oanda"]).into_iter().filter(|s| s != missing).collect();
    FakePeer::start(PeerConfig::new().with_features(f))
}

fn refusal_text(e: &SourceError) -> &str {
    match e {
        SourceError::Fetch(t) => t,
        SourceError::Refused(t) => panic!("a missing capability is not a Refused source: {t}"),
    }
}

#[test]
fn a_missing_lane_feature_is_refused_by_name_and_nothing_is_sent() {
    // klines and funding need `gw_fetch`
    let peer = peer_without(FEATURE_GW_FETCH);
    let e = RemoteKlines::new(peer.link(), "binance").fetch("X", "1m", 0, 1).unwrap_err();
    assert!(refusal_text(&e).contains(&format!("{FEATURE_GW_FETCH:?}")), "{e}");
    let e = RemoteFunding::new(peer.link(), "binance").fetch("X", 0, 1).unwrap_err();
    assert!(refusal_text(&e).contains(&format!("{FEATURE_GW_FETCH:?}")), "{e}");
    assert!(peer.requests().is_empty(), "nothing followed the handshake: {:?}", peer.requests());

    // the progress contract is required of every fetch, whatever the lane
    let peer = peer_without(FEATURE_GW_FETCH_PROGRESS);
    let e = RemoteKlines::new(peer.link(), "binance").fetch("X", "1m", 0, 1).unwrap_err();
    assert!(refusal_text(&e).contains(FEATURE_GW_FETCH_PROGRESS), "{e}");
    assert!(peer.requests().is_empty());

    // ticks have their own string
    let peer = peer_without(FEATURE_GW_TICKS);
    let e = RemoteTicks::new(peer.link()).fetch_quotes_range("EURUSD", 0, 1, None).unwrap_err();
    assert!(e.contains(FEATURE_GW_TICKS) && e.contains("upgrade vike-gatewayhub"), "{e}");
    assert!(peer.requests().is_empty());
}

#[test]
fn the_credentialed_lane_needs_its_string_and_the_venue_in_the_cd_venue_set() {
    let peer = peer_without(FEATURE_GW_FETCH_CREDENTIALED);
    let e =
        RemoteKlines::credentialed(peer.link(), "oanda").fetch("EUR_USD", "1m", 0, 1).unwrap_err();
    assert!(refusal_text(&e).contains(FEATURE_GW_FETCH_CREDENTIALED), "{e}");
    assert!(peer.requests().is_empty());

    // The lane is advertised but this venue is not in the `cd_venue=` set: the daemon holds no key for it.
    let peer = FakePeer::start(PeerConfig::new().with_features(features(&["binance"], &[])));
    let e =
        RemoteKlines::credentialed(peer.link(), "oanda").fetch("EUR_USD", "1m", 0, 1).unwrap_err();
    assert!(refusal_text(&e).contains("cd_venue=oanda"), "{e}");
    assert!(peer.requests().is_empty());
}

#[test]
fn the_catalog_options_family_and_presence_lanes_each_refuse_by_name() {
    let peer = peer_without(FEATURE_GW_CATALOG);
    let e = RemoteCatalog::open(peer.link(), "binance").unwrap_err();
    assert!(e.0.contains(FEATURE_GW_CATALOG), "{e}");
    assert!(peer.requests().is_empty());

    let peer = peer_without(FEATURE_GW_OPTIONS);
    let e = RemoteOptions::new(peer.link()).list_underlyings().unwrap_err();
    assert!(e.contains(FEATURE_GW_OPTIONS), "{e}");
    let e = RemoteOptions::new(peer.link()).option_index_instruments().unwrap_err();
    assert!(e.contains(FEATURE_GW_OPTIONS), "{e}");
    assert!(peer.requests().is_empty());

    let peer = peer_without(FEATURE_GW_FAMILY);
    let mut family = RemoteRollingFamily::new(peer.link(), "btc-updown-5m").unwrap();
    let e = family.desired(1).unwrap_err();
    assert!(e.contains(FEATURE_GW_FAMILY), "{e}");
    assert!(peer.requests().is_empty());

    // presence: an unarmed lane is "nothing was read", never "absent"
    let peer = peer_without("gw_presence");
    assert_eq!(credential_presence(&peer.link(), "oanda"), CredentialPresence::NotChecked);
    assert!(peer.requests().is_empty());
}

/// A venue the daemon does not list in `md_venue=` is refused by name, and `DataOpen` is never sent.
#[test]
fn a_stream_venue_the_daemon_does_not_advertise_is_refused_by_name() {
    let peer = FakePeer::start(PeerConfig::new().with_features(features(&["polymarket"], &[])));
    let sink = Arc::new(RecordingSink::default());
    let dyn_sink: Arc<dyn LiveDataSink> = sink;
    let mut c =
        RemoteDataClient::new(GatewayOpener::new(peer.link()), "binance", dyn_sink).unwrap();
    let Err(LiveDataError::Subscribe(text)) = c.subscribe_quotes("BTCUSDT") else { panic!() };
    assert!(text.contains("md_venue=binance"), "{text}");
    assert!(
        !peer.requests().iter().any(|r| matches!(r, Request::DataOpen { .. })),
        "DataOpen was never sent"
    );
    c.shutdown();
}

// ---- The handshake ---------------------------------------------------------------------------------

#[test]
fn a_version_skew_is_refused_naming_both_numbers_before_anything_is_signed() {
    let mut cfg = PeerConfig::new();
    cfg.version = 9;
    let peer = FakePeer::start(cfg);
    let e = GatewayClient::connect(&peer.link(), Scope::Read).unwrap_err();
    let text = e.to_string();
    assert!(text.contains(&GATEWAYHUB_PROTO_VERSION.to_string()) && text.contains('9'), "{text}");
    assert!(
        FakePeer::wait_until(Duration::from_secs(2), || peer.hellos() == 1),
        "the Hello was sent"
    );
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
    let text = e.to_string();
    assert!(text.contains(GATEWAYHUB_PLANE_SENTINEL) && text.contains("load_bars"), "{text}");
    assert!(FakePeer::wait_until(Duration::from_secs(2), || peer.hellos() == 1));
    assert!(peer.auths().is_empty());
}

#[test]
fn a_wrong_key_is_denied_and_a_missing_key_never_dials() {
    let peer = FakePeer::start(PeerConfig::new());
    let wrong = GatewayLink::new(
        peer.addr_string(),
        HubKeys::new(b"not the key".to_vec(), b"nor this".to_vec()),
    )
    .with_timings(fast_timings());
    let e = GatewayClient::connect(&wrong, Scope::Read).unwrap_err();
    assert_eq!(e.kind(), std::io::ErrorKind::PermissionDenied, "{e}");
    assert!(e.to_string().contains("auth denied"), "{e}");
    assert_eq!(peer.auths(), [(Scope::Read, false)]);

    let before = peer.connections();
    let read_only =
        GatewayLink::new(peer.addr_string(), read_only_keys()).with_timings(fast_timings());
    let e = GatewayClient::connect(&read_only, Scope::Write).unwrap_err();
    assert_eq!(e.kind(), std::io::ErrorKind::PermissionDenied);
    assert_eq!(peer.connections(), before, "no key for the scope: nothing is dialled");
    // The same link signs a Read connection fine.
    GatewayClient::connect(&read_only, Scope::Read).expect("the Read key works");
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
