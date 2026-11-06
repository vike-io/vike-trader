//! [`report_of`] over a cell whose actor's word the test sets by hand: up reads up, down reads down with the bridge's
//! own words, and the report follows the cell on every call (it caches nothing). The word itself is the bridge's test
//! (`crates/bridges/ctrader/tests/offline/session_lapse.rs`).

use std::sync::Arc;

use vike_ctrader::conn::grant::ExecPathDown;

use super::*;

#[test]
fn a_session_the_venue_ended_reads_down_and_an_authorized_one_up() {
    let cell = SharedGrant::new("AT", "RT", None);
    let report = report_of(Arc::clone(&cell));
    assert_eq!(report(), WireVenueLink::working(), "a cell nobody wrote to has nothing to report");

    cell.set_exec_path(
        99,
        Some(ExecPathDown {
            why: "the trading account is NOT AUTHORIZED".to_string(),
            effect: "orders wait for the session",
            failed_attempts: 2,
            retry_in_ms: 60_000,
        }),
    );
    let down = report();
    assert!(!down.up);
    assert_eq!(down.why, "the trading account is NOT AUTHORIZED");
    assert_eq!((down.failed_restarts, down.retry_in_ms), (2, 60_000));
    assert_eq!(
        down.effect, "orders wait for the session",
        "the words about orders are the bridge's"
    );

    cell.set_exec_path(99, None);
    assert_eq!(report(), WireVenueLink::working(), "the report follows the cell, it keeps no copy");
}
