//! The words of each SDK status in the shared `VenueLinkState`, and the exec thread's freshness on the cell: only a fresh
//! "connected" reads up, and nothing unknown does.

use super::*;
use std::time::Instant;

/// **Up, down and unknown on the account's cell.** A cell nobody wrote is not known; the SDK's Connected, said through
/// [`say`], is up until the exec thread has not said it for [`FRESH`]; a lost session stays lost however old the word.
#[test]
fn only_a_fresh_connected_session_reads_up() {
    let link = VenueLink::default();
    assert_eq!(link.state(), VenueLinkState::Unknown, "nothing said yet");

    say(&link, from_status(Some(CONNECTED)));
    assert_eq!(link.state(), VenueLinkState::Up);
    let stale = Instant::now()
        .checked_sub(FRESH + Duration::from_millis(1))
        .expect("a box that has been up for more than a minute");
    link.set_at(from_status(Some(CONNECTED)), Some(FRESH), stale);
    assert_eq!(
        link.state(),
        VenueLinkState::Unknown,
        "a stuck exec thread no longer vouches for it"
    );

    let long_ago = Instant::now().checked_sub(FRESH * 10).expect("a box up for ten minutes");
    link.set_at(from_status(Some(4)), Some(FRESH), long_ago);
    let VenueLinkState::Down(VenueLinkDown { why, effect, .. }) = link.state() else {
        panic!("a reconnecting session is down, however old the word")
    };
    assert!(why.contains("Reconnecting"), "{why}");
    assert_eq!(effect, "", "orders are refused meanwhile");

    link.reset();
    assert_eq!(link.state(), VenueLinkState::Unknown, "a new client is not known yet");
}

/// **A shim that cannot say, or a number the SDK header does not name, is not known**: never up.
#[test]
fn an_old_shim_or_an_unnamed_status_is_unknown() {
    assert_eq!(from_status(None), VenueLinkState::Unknown);
    assert_eq!(from_status(Some(-1)), VenueLinkState::Unknown, "a bad handle");
    assert_eq!(from_status(Some(42)), VenueLinkState::Unknown);
}

/// Every status the SDK names but Connected (and the chart session's reconnect) is down, and a session the SDK gave up on
/// says that nothing logs in again. The exec thread states no restart count and no wait.
#[test]
fn every_named_status_but_connected_is_down_in_its_own_words() {
    assert_eq!(from_status(Some(CHART_SESSION_RECONNECTING)), VenueLinkState::Up);
    for status in [0, 1, 2, 4, 5, 6, 7, 8] {
        let VenueLinkState::Down(down) = from_status(Some(status)) else {
            panic!("status {status} is down")
        };
        assert_eq!((down.failed_restarts, down.retry_in), (0, Duration::ZERO), "status {status}");
    }
    for lost in [0, 5, 6] {
        let VenueLinkState::Down(VenueLinkDown { effect, .. }) = from_status(Some(lost)) else {
            unreachable!()
        };
        assert_eq!(effect, LOST_EFFECT, "status {lost}");
    }
}

/// A failed login is down with the exec thread's reason, and a refused credential says nothing retries it.
#[test]
fn a_failed_login_is_down_and_a_refused_credential_says_it_is_not_retried() {
    let VenueLinkState::Down(VenueLinkDown { why, effect, .. }) =
        login_failed("fxcm session unavailable: ORA-499", false)
    else {
        panic!("a failed login is down")
    };
    assert_eq!(why, "fxcm session unavailable: ORA-499");
    assert!(effect.contains("logs in again by itself"), "{effect}");
    let VenueLinkState::Down(VenueLinkDown { effect, .. }) =
        login_failed("Incorrect user name or password", true)
    else {
        panic!("a refused login is down")
    };
    assert!(effect.contains("nothing logs in again"), "{effect}");
}
