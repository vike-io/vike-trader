//! The gateway address the gatewayhub's watch reads, its probe against a fake gateway socket, and its cadence.

use std::net::TcpListener;

use vike_bridge_core::venue_link::VenueLinkProbe;
use vike_model::AccountTier;
use vike_secrets::VenueSettingRow;
use vike_secrets::venue_setting::VenueSettings;
use vike_venues::IbkrBackend;
use vike_venues::venue_urls::{UrlChannel, UrlProtocol, UrlType, VenueUrls, seed_url};

use super::{IbkrGateway, MAX_WAIT, PROBE_EVERY};

/// The `venue.ibkr.*` rows at `tier` (`None` = machine-scoped), beside the release's addresses.
fn settings(tier: Option<AccountTier>, rows: &[(&str, &str)]) -> VenueSettings {
    let rows: Vec<VenueSettingRow> = rows
        .iter()
        .map(|(field, value)| VenueSettingRow {
            venue: "ibkr".into(),
            tier,
            field: (*field).into(),
            value: (*value).into(),
        })
        .collect();
    VenueSettings::from_rows("ibkr", &rows).with_urls(VenueUrls::seed("ibkr"))
}

/// The seed's `ibkr <ty> trade socket gateway` url.
fn seed_socket(ty: UrlType) -> &'static str {
    seed_url("ibkr", ty, UrlChannel::Trade, UrlProtocol::Socket, "gateway").expect("a seed row")
}

/// The daemon probes the address the bridge dials: the tier's `venue_url` socket row, which the release
/// seeds at the box gateways' ports (decision 0265).
#[test]
fn the_socket_gateway_is_the_tiers_address_row() {
    for (tier, ty, port) in
        [(AccountTier::Demo, UrlType::Demo, 4102), (AccountTier::Live, UrlType::Live, 4101)]
    {
        let gw = IbkrGateway::supervised(tier, &settings(None, &[])).unwrap().unwrap();
        assert_eq!(format!("{}:{}", gw.host, gw.port), seed_socket(ty));
        assert_eq!(
            gw,
            IbkrGateway { backend: IbkrBackend::Socket, host: "127.0.0.1".into(), port }
        );
    }
}

/// A retired `venue.ibkr.<tier>.{host,port}` row (decision 0265) is read by nothing.
#[test]
fn a_retired_host_or_port_row_is_not_read() {
    let s = settings(Some(AccountTier::Demo), &[("host", "<host>"), ("port", "4002")]);
    let gw = IbkrGateway::supervised(AccountTier::Demo, &s).unwrap().unwrap();
    assert_eq!(format!("{}:{}", gw.host, gw.port), seed_socket(UrlType::Demo));
}

#[test]
fn a_blank_backend_row_reads_as_no_row_and_no_backend_row_is_the_socket() {
    let s = settings(Some(AccountTier::Demo), &[("backend", "  ")]);
    let gw = IbkrGateway::supervised(AccountTier::Demo, &s).unwrap().unwrap();
    assert_eq!(gw.backend, IbkrBackend::Socket);
    // …and a machine-scoped `backend` row is not the tier's.
    let any = settings(None, &[("backend", "cpapi")]);
    assert!(IbkrGateway::supervised(AccountTier::Demo, &any).unwrap().is_some());
}

/// Review of W14, finding 3: the cpapi backend's pump redials and resyncs by itself
/// (`IbkrInbound::StreamResync`), and a rebuilt adapter would lose that resync. The daemon supervises the
/// SOCKET backend only; a cpapi (or oauth) account has no gateway to watch.
#[test]
fn only_the_socket_backend_has_a_gateway_to_watch() {
    let s = settings(Some(AccountTier::Demo), &[("backend", "cpapi")]);
    assert_eq!(
        IbkrGateway::supervised(AccountTier::Demo, &s),
        Ok(None),
        "the cpapi pump owns its own redial and resync: the daemon watches nothing"
    );
    let none = IbkrGateway::supervised(
        AccountTier::Demo,
        &settings(Some(AccountTier::Demo), &[("backend", "oauth")]),
    );
    assert_eq!(none, Ok(None), "nothing connects through oauth");
}

#[test]
fn a_row_the_bridge_would_refuse_is_an_error_here_too() {
    let bad_backend = settings(Some(AccountTier::Demo), &[("backend", "tws")]);
    assert!(IbkrGateway::supervised(AccountTier::Demo, &bad_backend).is_err());
    let why = IbkrGateway::supervised(AccountTier::Demo, &VenueSettings::default()).unwrap_err();
    assert!(why.contains("no address row for demo trade socket product=gateway"), "{why}");
    // The rule a socket row is written by (`UrlSocket::parse`): an empty host, port 0 and a scheme too.
    for bad in ["127.0.0.1:four", ":4102", "127.0.0.1:0", "tcp://127.0.0.1:4102"] {
        let row = ("ibkr", UrlType::Demo, UrlChannel::Trade, UrlProtocol::Socket, "gateway", bad);
        let not_host_port = VenueSettings::default().with_urls(VenueUrls::from_rows("ibkr", [row]));
        let why = IbkrGateway::supervised(AccountTier::Demo, &not_host_port).unwrap_err();
        assert!(why.contains("is not host:port"), "{bad}: {why}");
    }
}

#[test]
fn the_probe_sees_a_listener_come_and_go() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let gw = IbkrGateway { backend: IbkrBackend::Socket, host: "127.0.0.1".into(), port };
    assert_eq!(gw.probe(), Ok(()), "a listening port answers");
    assert_eq!(gw.reachable(), Ok(()), "the watch's probe is the gateway's");
    // Port 1, not `listener` dropped: a released port can be handed to another test's listener
    // before the probe, and nothing listens on port 1. `probe` keeps no state between calls.
    let gw = IbkrGateway { port: 1, ..gw };
    assert!(gw.probe().is_err(), "a closed port is a loss");
}

/// The cadence the watch is built on: the first wait is the IBKR row's redial, the probe every
/// [`PROBE_EVERY`], growing to [`MAX_WAIT`].
#[test]
fn the_production_cadence_starts_at_the_ibkr_rows_redial() {
    let gw = IbkrGateway { backend: IbkrBackend::Socket, host: "127.0.0.1".into(), port: 1 };
    assert_eq!(gw.first_wait(), vike_venues::venue_timings::IBKR.exec().reconnect.unwrap().initial);
    assert_eq!(gw.probe_every(), PROBE_EVERY);
    assert_eq!(gw.max_wait(), MAX_WAIT);
}
