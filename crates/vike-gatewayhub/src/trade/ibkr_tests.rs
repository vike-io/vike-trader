//! The gateway address the watch reads, and the probe against a fake gateway socket.

use std::net::TcpListener;

use vike_model::AccountTier;
use vike_secrets::VenueSettingRow;
use vike_secrets::venue_setting::VenueSettings;
use vike_venues::IbkrBackend;

use super::{IbkrGateway, IbkrWatch};

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
    VenueSettings::from_rows("ibkr", &rows)
}

#[test]
fn the_socket_gateway_is_the_tiers_host_and_port() {
    let s = settings(Some(AccountTier::Demo), &[("host", " 127.0.0.1 "), ("port", "4102")]);
    let gw = IbkrGateway::from_settings(AccountTier::Demo, &s).unwrap().unwrap();
    assert_eq!(
        gw,
        IbkrGateway { backend: IbkrBackend::Socket, host: "127.0.0.1".into(), port: 4102 }
    );
}

#[test]
fn a_row_of_another_tier_or_the_machine_scope_is_not_read_as_this_tiers() {
    // The bridge reads EXACTLY the tier's row; a machine-scoped `port` answers nothing.
    let s = settings(None, &[("port", "4102")]);
    let gw = IbkrGateway::from_settings(AccountTier::Demo, &s).unwrap().unwrap();
    assert_eq!(gw.port, 7497, "the machine-scoped row is not the demo tier's port");
    let live =
        IbkrGateway::from_settings(AccountTier::Live, &VenueSettings::default()).unwrap().unwrap();
    assert_eq!(live.port, 7496);
}

#[test]
fn a_blank_row_reads_as_no_row_and_no_backend_row_is_the_socket() {
    let s = settings(Some(AccountTier::Demo), &[("host", "  "), ("backend", ""), ("port", "4102")]);
    let gw = IbkrGateway::from_settings(AccountTier::Demo, &s).unwrap().unwrap();
    assert_eq!((gw.backend, gw.host.as_str(), gw.port), (IbkrBackend::Socket, "127.0.0.1", 4102));
}

/// Review of W14, finding 3: the cpapi backend's pump redials and resyncs by itself
/// (`IbInbound::StreamResync`), and a rebuilt adapter would lose that resync. The daemon supervises the
/// SOCKET backend only; a cpapi (or oauth) account has no gateway to watch.
#[test]
fn only_the_socket_backend_has_a_gateway_to_watch() {
    let s = settings(
        Some(AccountTier::Demo),
        &[("backend", "cpapi"), ("cpapi_url", "https://127.0.0.1:5000/v1/api")],
    );
    assert_eq!(
        IbkrGateway::from_settings(AccountTier::Demo, &s),
        Ok(None),
        "the cpapi pump owns its own redial and resync: the daemon watches nothing"
    );
    let none = IbkrGateway::from_settings(
        AccountTier::Demo,
        &settings(Some(AccountTier::Demo), &[("backend", "oauth")]),
    );
    assert_eq!(none, Ok(None), "nothing connects through oauth");
}

#[test]
fn a_row_the_bridge_would_refuse_is_an_error_here_too() {
    let bad_port = settings(Some(AccountTier::Demo), &[("port", "four")]);
    assert!(IbkrGateway::from_settings(AccountTier::Demo, &bad_port).is_err());
    let bad_backend = settings(Some(AccountTier::Demo), &[("backend", "tws")]);
    assert!(IbkrGateway::from_settings(AccountTier::Demo, &bad_backend).is_err());
}

#[test]
fn the_probe_sees_a_listener_come_and_go() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let gw = IbkrGateway { backend: IbkrBackend::Socket, host: "127.0.0.1".into(), port };
    assert_eq!(gw.probe(), Ok(()), "a listening port answers");
    drop(listener);
    assert!(gw.probe().is_err(), "a closed port is a loss");
}

#[test]
fn the_production_schedule_starts_at_the_ibkr_rows_redial() {
    let s = IbkrWatch::production_schedule();
    assert!(s.is_sane());
    assert_eq!(s.first_wait, vike_venues::venue_timings::IBKR.exec().reconnect.unwrap().initial);
    assert_eq!(s.probe_every, super::PROBE_EVERY);
}
