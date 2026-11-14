//! The one translator, and each venue's cell read through it with the bridge's word set by hand: not known is no report,
//! up is a working path, an outage reaches the wire in the bridge's own words, and every report reads its cell at each
//! ask (it caches nothing). The words themselves are each bridge's tests.

use std::time::Duration;

use super::*;

/// **The translator**: not known is no report, up is a working path, and a lost path carries its words, its counter and
/// its wait (in milliseconds, saturating) whole.
#[test]
fn each_reading_reaches_the_wire_in_its_own_words_and_unknown_is_no_report() {
    assert_eq!(venue_link_wire(VenueLinkState::Unknown), None);
    assert_eq!(venue_link_wire(VenueLinkState::Up), Some(VenueLinkWire::working()));
    let lost = |retry_in| {
        venue_link_wire(VenueLinkState::Down(VenueLinkDown {
            why: "HTTP 401 on stream open".to_string(),
            effect: "orders still go out",
            failed_restarts: 4,
            retry_in,
        }))
    };
    assert_eq!(
        lost(Duration::from_secs(30)),
        Some(VenueLinkWire {
            up: false,
            why: "HTTP 401 on stream open".to_string(),
            failed_restarts: 4,
            retry_in_ms: 30_000,
            effect: "orders still go out".to_string(),
        })
    );
    assert_eq!(lost(Duration::MAX).map(|l| l.retry_in_ms), Some(u64::MAX));
}

/// **A pump that has not started is not known, never up and never down** (ruled U-6): no report.
#[test]
fn a_pump_that_never_started_is_no_report() {
    assert_eq!(pump(VenueLinkStream::default())(), None);
}

#[test]
fn a_sidecar_that_is_up_reads_up_and_a_down_one_reads_down_with_its_words() {
    let bus = vike_dukascopy::feed::QuoteBus::new();
    let report = {
        let bus = Arc::clone(&bus);
        report_of(move || bus.sidecar())
    };
    assert_eq!(
        report(),
        Some(VenueLinkWire::working()),
        "a bus nobody wrote to has nothing to report"
    );
    bus.set_sidecar(VenueLinkState::Down(VenueLinkDown {
        why: "JForex session disconnected".to_string(),
        effect: "",
        failed_restarts: 2,
        retry_in: Duration::from_secs(20),
    }));
    let down = report().expect("the sidecar always reports");
    assert!(!down.up);
    assert_eq!(down.why, "JForex session disconnected");
    assert_eq!((down.failed_restarts, down.retry_in_ms, down.effect.as_str()), (2, 20_000, ""));
    bus.set_sidecar(VenueLinkState::Up);
    assert_eq!(report(), Some(VenueLinkWire::working()), "the report follows the bus");
}

/// **The cell OANDA, fxcm and ibkr write**: nothing said is not up; the words of an outage reach the wire whole; the
/// return reads up; a word reset is not known again.
#[test]
fn a_bridges_own_cell_is_stated_at_each_ask_and_unknown_is_no_report() {
    let link = VenueLink::default();
    let report = cell(link.clone());
    assert_eq!(report(), None, "nothing said yet is not up");
    link.set(VenueLinkState::Up, None);
    assert_eq!(report(), Some(VenueLinkWire::working()));
    link.set(
        VenueLinkState::Down(VenueLinkDown {
            why: "HTTP 401 on stream open".to_string(),
            effect: "orders still go out",
            failed_restarts: 4,
            retry_in: Duration::from_secs(30),
        }),
        None,
    );
    assert_eq!(
        report(),
        Some(VenueLinkWire {
            up: false,
            why: "HTTP 401 on stream open".to_string(),
            failed_restarts: 4,
            retry_in_ms: 30_000,
            effect: "orders still go out".to_string(),
        })
    );
    link.reset();
    assert_eq!(report(), None);
}

#[test]
fn a_ctrader_session_the_venue_ended_reads_down_and_an_authorized_one_up() {
    use vike_ctrader::transport::connection::grant::SharedGrant;
    let grant = SharedGrant::new("AT", "RT", None);
    let report = {
        let grant = Arc::clone(&grant);
        report_of(move || grant.venue_link_state())
    };
    assert_eq!(report(), None, "a cell no exec actor claimed is not known");
    let claim = grant.claim_execution_path(99);
    grant.set_execution_path(
        99,
        claim,
        Some(VenueLinkDown {
            why: "the trading account is NOT AUTHORIZED".to_string(),
            effect: "orders wait for the session",
            failed_restarts: 2,
            retry_in: Duration::from_secs(60),
        }),
    );
    assert_eq!(
        report(),
        Some(VenueLinkWire {
            up: false,
            why: "the trading account is NOT AUTHORIZED".to_string(),
            failed_restarts: 2,
            retry_in_ms: 60_000,
            effect: "orders wait for the session".to_string(),
        }),
        "the words about orders are the bridge's"
    );
    grant.set_execution_path(99, claim, None);
    assert_eq!(report(), Some(VenueLinkWire::working()), "the report follows the cell");
}

/// Nothing said is unknown, both lanes answering is up, a lost order session is down in the bridge's words.
#[test]
fn the_ig_report_reads_the_bridges_cell_at_each_ask() {
    let link = vike_ig::SessionLink::default();
    let report = {
        let link = link.clone();
        report_of(move || link.state())
    };
    assert_eq!(report(), None);
    link.rest_answered();
    link.stream_answered();
    assert_eq!(report(), Some(VenueLinkWire::working()));
    link.rest_login_lost("IG refused it (IG error 403: error.security.api-key-disabled)");
    let down = report().expect("a lost path is said");
    assert!(!down.up && down.why.starts_with("the order session's login failed"), "{down:?}");
    assert!(down.effect.contains("every order is refused"), "{down:?}");
    assert_eq!((down.failed_restarts, down.retry_in_ms), (0, 0), "IG counts no restarts");
}
