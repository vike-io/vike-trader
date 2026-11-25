//! The one translator, and the account's one cell read through it with the bridge's word set by hand: not known is no
//! report, up is a working path, an outage reaches the wire in the bridge's own words, and every report reads its cell
//! at each ask (it caches nothing). The words, and that each bridge writes the cell, are each bridge's tests.

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

/// **Both ends of the status verb read one state**: the tradehub reads the wire's `up` and `failed_restarts` through the
/// same mapping (`vike_model::feed_status::order_path_state`), and gets the state the bridge's reading has here, for
/// every kind of reading, with no state on the wire.
#[test]
fn the_wire_carries_the_facts_the_tradehub_reads_the_same_state_from() {
    use vike_model::feed_status::order_path_state;
    let down = |failed_restarts| {
        VenueLinkState::Down(VenueLinkDown {
            why: "server closed: 1000 Expired".to_string(),
            failed_restarts,
            ..VenueLinkDown::default()
        })
    };
    for reading in [VenueLinkState::Unknown, VenueLinkState::Up, down(0), down(1), down(7)] {
        let wire = venue_link_wire(reading.clone());
        let read_back = order_path_state(
            wire.as_ref().map(|w| w.up),
            wire.as_ref().map_or(0, |w| w.failed_restarts),
        );
        assert_eq!(read_back, reading.connection_state(), "{reading:?}");
    }
}

/// **The account's cell, read at each ask**: nothing said is not up; the words of an outage reach the wire whole, its
/// wait counting down; the return reads up; a reset is not known again; and a cell of named sub-paths (the pump's
/// streams, ig's two paths, cTrader's sessions) reads the worst of them, not known until every claimed path has spoken.
#[test]
fn the_accounts_cell_is_stated_at_each_ask_and_unknown_is_no_report() {
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
    let down = report().expect("an outage is said");
    assert!((29_000..=30_000).contains(&down.retry_in_ms), "the wait counts down: {down:?}");
    assert_eq!(
        down,
        VenueLinkWire {
            up: false,
            why: "HTTP 401 on stream open".to_string(),
            failed_restarts: 4,
            retry_in_ms: down.retry_in_ms,
            effect: "orders still go out".to_string(),
        }
    );
    link.reset();
    assert_eq!(report(), None);

    let orders = link.claim_path("order session");
    let stream = link.claim_path("trade stream");
    orders.set(VenueLinkState::Up, None);
    assert_eq!(report(), None, "a claimed path that said nothing keeps the cell not known");
    stream.set(VenueLinkState::Up, None);
    assert_eq!(report(), Some(VenueLinkWire::working()), "every path is up");
    stream.set(
        VenueLinkState::Down(VenueLinkDown::failed(
            "the trade stream dropped".to_string(),
            "fills arrive when it is back",
            0,
            Duration::ZERO,
        )),
        None,
    );
    assert!(
        report().is_some_and(|l| !l.up && l.why == "the trade stream dropped"),
        "one loss is the reading"
    );
}

/// **A session the SDK gave up on reaches the wire as a failure**, read back by the tradehub's mapping as `Error` (said at
/// once), and one the SDK is reconnecting by itself as a reopen (`Connecting`, said once it outlives an ask).
#[test]
fn an_fxcm_session_lost_reaches_the_wire_as_a_failure_and_a_reconnect_as_a_reopen() {
    use vike_fxcm::account::venue_link::from_status;
    use vike_model::feed_status::order_path_state;
    let read_back = |status| {
        let wire = venue_link_wire(from_status(Some(status))).expect("a named status is said");
        order_path_state(Some(wire.up), wire.failed_restarts)
    };
    assert_eq!(read_back(6), DataClientConnectionState::Error, "SessionLost");
    assert_eq!(read_back(4), DataClientConnectionState::Connecting, "the SDK's own Reconnecting");
}
