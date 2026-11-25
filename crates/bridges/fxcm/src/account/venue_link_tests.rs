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

/// **The probe says up on a fresh `Connected` only**: a rebuild is healthy once the SDK said it, never on a cell nobody
/// wrote (the start returned before its login) or a login still connecting.
#[test]
fn the_probe_is_up_only_on_a_fresh_connected() {
    let link = VenueLink::default().claim();
    let probe = FxcmSessionProbe::new(link.clone());
    assert_eq!(probe.up(), Some(false), "nothing said yet");
    say(&link, from_status(Some(1)));
    assert_eq!(probe.up(), Some(false), "still connecting");
    say(&link, from_status(Some(CONNECTED)));
    assert_eq!(probe.up(), Some(true));
}

/// **A shim that cannot say, or a number the SDK header does not name, is not known**: never up.
#[test]
fn an_old_shim_or_an_unnamed_status_is_unknown() {
    assert_eq!(from_status(None), VenueLinkState::Unknown);
    assert_eq!(from_status(Some(-1)), VenueLinkState::Unknown, "a bad handle");
    assert_eq!(from_status(Some(42)), VenueLinkState::Unknown);
}

/// Every status the SDK names but Connected (and the chart session's reconnect) is down, and a session the SDK gave up on
/// says that nothing logs in again. The exec thread states no restart count and no wait, so a session the SDK is opening
/// or reconnecting reads a reopen (`Connecting`), and one it gave up on a failure (`Error`): said at once, not one ask late.
#[test]
fn every_named_status_but_connected_is_down_in_its_own_words() {
    use vike_model::feed_status::DataClientConnectionState::{Connecting, Error};
    assert_eq!(from_status(Some(CHART_SESSION_RECONNECTING)), VenueLinkState::Up);
    for status in [1, 2, 4, 7, 8] {
        let state = from_status(Some(status));
        assert_eq!(state.connection_state(), Connecting, "status {status}");
        let VenueLinkState::Down(down) = state else { panic!("status {status} is down") };
        assert_eq!((down.failed_restarts, down.retry_in), (0, Duration::ZERO), "status {status}");
    }
    for lost in [0, 5, 6] {
        let state = from_status(Some(lost));
        assert_eq!(state.connection_state(), Error, "status {lost} is a session lost");
        let VenueLinkState::Down(VenueLinkDown { effect, failed_restarts, retry_in, .. }) = state
        else {
            unreachable!()
        };
        assert_eq!(effect, LOST_EFFECT, "status {lost}");
        assert_eq!((failed_restarts, retry_in), (1, Duration::ZERO), "status {lost}");
    }
}

/// **The status-to-probe mapping: only a FINAL state reports the session lost** (decision 0164 verdict 7(b), amended
/// 2026-11-21). The gatewayhub rebuilds the adapter on `Disconnected`, `Disconnecting` and `SessionLost` (the SDK reconnects
/// no more), naming the state; every other word leaves the session to the SDK and the exec thread: up, `Connecting`, the
/// SDK's own `Reconnecting`, a reconnecting price session, a password to change, a status nobody named, a shim that cannot
/// say, a failed first login (retried by the exec thread, refused credentials included), a stuck thread and a new client.
#[test]
fn only_a_final_sdk_state_reports_the_session_lost() {
    let link = VenueLink::default();
    let probe = FxcmSessionProbe::new(link.clone());
    for (status, name) in [(0, "Disconnected"), (5, "Disconnecting"), (6, "SessionLost")] {
        say(&link, from_status(Some(status)));
        let why = probe.lost().unwrap_or_else(|| panic!("status {status} is final"));
        assert_eq!(why, format!("the ForexConnect session is {name}"));
    }
    for status in
        [None, Some(-1), Some(1), Some(2), Some(3), Some(4), Some(7), Some(8), Some(9), Some(42)]
    {
        say(&link, from_status(status));
        assert_eq!(probe.lost(), None, "status {status:?} is not final");
    }
    for refused in [false, true] {
        say(&link, login_failed("fxcm session unavailable: ORA-499", refused));
        assert_eq!(
            probe.lost(),
            None,
            "a failed first login is the exec thread's to retry ({refused})"
        );
    }
    let stale = Instant::now().checked_sub(FRESH * 2).expect("a box up for two minutes");
    link.set_at(from_status(Some(CONNECTED)), Some(FRESH), stale);
    assert_eq!(probe.lost(), None, "a stuck exec thread is not known, never lost");
    say(&link, from_status(Some(6)));
    link.reset();
    assert_eq!(probe.lost(), None, "a new client's cell is not known yet");
}

/// **The probe reads the ACCOUNT's cell**, the one its exec thread writes (found by the route key), and asks nothing else:
/// there is no endpoint to probe beside the session itself, and a refused rebuild waits as a failed login does.
#[test]
fn the_probe_reads_the_accounts_own_cell_on_the_supervisors_cadence() {
    let probe = FxcmSessionProbe::new(vike_bridge_core::venue_link::venue_link("fxcm#PROBE"));
    let other = FxcmSessionProbe::new(vike_bridge_core::venue_link::venue_link("fxcm#OTHER"));
    say(&vike_bridge_core::venue_link::venue_link("fxcm#PROBE"), from_status(Some(6)));
    assert!(probe.lost().is_some(), "what the exec thread wrote for this account");
    assert_eq!(other.lost(), None, "another account's cell is its own");
    assert_eq!(
        probe.reachable(),
        Ok(()),
        "nothing to probe beside the session: the FX week is the daemon's calendar"
    );
    assert_eq!(probe.probe_every(), PROBE_EVERY);
    assert_eq!(probe.first_wait(), pause_after(1), "the login ladder's first pause (30 s)");
    assert_eq!(probe.max_wait(), Duration::from_secs(600), "the login ladder's ceiling");
    assert_eq!(probe.probation(), Duration::from_secs(30), "the owner's 30 s up (2026-11-22)");
    assert_eq!(probe.not_ready_words(), None, "the daemon's own words");
}

/// A failed login is down with the exec thread's reason, a failure (`Error`, said at once) whether it is retried or not,
/// and a refused credential says nothing retries it.
#[test]
fn a_failed_login_is_down_and_a_refused_credential_says_it_is_not_retried() {
    for refused in [false, true] {
        assert_eq!(
            login_failed("refused", refused).connection_state(),
            vike_model::feed_status::DataClientConnectionState::Error,
            "{refused}"
        );
    }
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

/// **One line per CHANGE of the SDK's status, never one per pass**, over a scripted sequence read every pass: the first
/// reading is a change from `none`, a status read again says nothing, and each change names the old status, the new one
/// and how long the old one held.
#[test]
fn a_status_change_is_said_once_with_the_old_status_and_how_long_it_held() {
    let t0 = Instant::now();
    let reads: [(Option<i32>, u64); 8] = [
        (Some(CONNECTED), 0),
        (Some(CONNECTED), 100),
        (Some(4), 200),
        (Some(4), 300),
        (Some(CONNECTED), 5_200),
        (Some(CONNECTED), 5_300),
        (Some(6), 9_000),
        (None, 9_100),
    ];
    let ((), events) = vike_log::capture::captured(|| {
        let mut changes = StatusChanges::default();
        for (status, ms) in reads {
            changes.note(status, t0 + Duration::from_millis(ms));
        }
    });
    let said: Vec<(&str, &str, Option<&str>, Option<&str>)> = events
        .iter()
        .map(|e| {
            assert_eq!(e.level, tracing::Level::INFO, "{e:?}");
            assert_eq!(e.message, "session status change", "{e:?}");
            assert_eq!(e.field("venue"), Some("fxcm"), "{e:?}");
            (
                e.field("old").expect("old"),
                e.field("new").expect("new"),
                e.field("held_ms"),
                e.field("code"),
            )
        })
        .collect();
    assert_eq!(
        said,
        [
            ("none", "Connected", None, Some("3")),
            ("Connected", "Reconnecting", Some("200"), Some("4")),
            ("Reconnecting", "Connected", Some("5000"), Some("3")),
            ("Connected", "SessionLost", Some("3800"), Some("6")),
            (
                "SessionLost",
                "not known (the shim exports no forex_connect_session_status)",
                Some("100"),
                None,
            ),
        ]
    );
}
