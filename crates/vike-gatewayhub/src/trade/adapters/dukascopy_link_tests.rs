//! [`report_of`] over a bus whose client's word the test sets by hand: up reads up, down reads down with the
//! sidecar's own words, and the report follows the bus on every call (it caches nothing).

use std::time::Duration;

use vike_dukascopy::SidecarState;
use vike_dukascopy::quotes::QuoteBus;

use super::*;

#[test]
fn a_sidecar_that_is_up_reads_up_and_a_down_one_reads_down_with_its_words() {
    let bus = QuoteBus::new();
    let report = report_of(Arc::clone(&bus));
    assert_eq!(report(), WireVenueLink::working(), "a bus nobody wrote to has nothing to report");

    bus.set_sidecar(SidecarState::Down {
        why: "JForex session disconnected".to_string(),
        failed_restarts: 2,
        retry_in: Duration::from_secs(20),
    });
    let down = report();
    assert!(!down.up);
    assert_eq!(down.why, "JForex session disconnected");
    assert_eq!((down.failed_restarts, down.retry_in_ms), (2, 20_000));

    bus.set_sidecar(SidecarState::Up);
    assert_eq!(report(), WireVenueLink::working(), "the report follows the bus, it keeps no copy");
}
