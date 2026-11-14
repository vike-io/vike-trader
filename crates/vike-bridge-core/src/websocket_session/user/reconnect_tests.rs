//! [`ReconnectBackoff`]'s one reset rule and refusal wait, and [`ReconnectLog`]'s pace, with no clock.
use super::*;

const S: Duration = Duration::from_secs(1);

/// The pump's own ladder, 1 s doubling to 30 s: a failed open never resets it, and a session resets it
/// only once it lived twice the ceiling.
#[test]
fn the_ladder_doubles_to_its_ceiling_and_resets_only_after_a_long_session() {
    let mut b = ReconnectBackoff::new(S, 30 * S);
    let waits: Vec<_> = (0..7).map(|_| b.failed()).collect();
    assert_eq!(waits, [1, 2, 4, 8, 16, 30, 30].map(|s| s * S));
    b.opened();
    assert_eq!(b.ended(59 * S), 30 * S, "a session under 2 x max keeps the ladder where it was");
    assert_eq!(b.ended(60 * S), S, "a session of 2 x max starts the ladder over");
    assert_eq!(b.failed(), 2 * S, "and it doubles again from there");
}

/// The first wait is `initial` even above `max` (the pump's behaviour under a short test ceiling); a
/// reset lands on the smaller one.
#[test]
fn a_ceiling_under_the_first_wait_capabilities_every_later_wait() {
    let mut b = ReconnectBackoff::new(S, Duration::from_millis(5));
    assert_eq!(b.failed(), S);
    assert_eq!(b.failed(), Duration::from_millis(5));
    assert_eq!(b.ended(Duration::from_millis(10)), Duration::from_millis(5));
}

/// From the `after`-th refusal in a row the venue's wait applies; another failure does not end the run,
/// an open does.
#[test]
fn a_run_of_refusals_waits_the_venues_wait_until_an_open_works() {
    let mut b = ReconnectBackoff::new(S, S).with_refusal_wait(3, 30 * S);
    assert_eq!([b.refused(), b.refused()], [S, S], "inside the run: the ladder's wait");
    assert_eq!(b.failed(), S);
    assert_eq!(b.refused(), 30 * S, "the third refusal in a row, a failure between them or not");
    assert_eq!(b.refused(), 30 * S);
    b.opened();
    assert_eq!(b.refused(), S, "an open that worked ends the run");
    assert_eq!(ReconnectBackoff::new(S, S).refused(), S, "no refusal wait: a refusal is a failure");
}

/// The onset speaks, then nothing until the interval is up, then the interval grows by the decay up to
/// the ceiling, measured from the last line, never from the onset.
#[test]
fn a_failure_speaks_at_its_onset_then_on_a_decaying_heartbeat() {
    let mut log = RECONNECT_LOG;
    assert_eq!(log.tick(Duration::ZERO), ReconnectLogLine::Onset { next_in: 300 * S });
    assert_eq!(log.tick(299 * S), ReconnectLogLine::Quiet);
    assert_eq!(
        log.tick(300 * S),
        ReconnectLogLine::Heartbeat { down_for: 300 * S, next_in: 900 * S }
    );
    assert_eq!(log.tick(1199 * S), ReconnectLogLine::Quiet, "900 s from the LAST line, not 300 s");
    assert_eq!(
        log.tick(1200 * S),
        ReconnectLogLine::Heartbeat { down_for: 1200 * S, next_in: 2700 * S }
    );
    let mut now = 1200 * S;
    let mut next = 2700 * S;
    for _ in 0..40 {
        now += next;
        let ReconnectLogLine::Heartbeat { next_in, .. } = log.tick(now) else {
            panic!("a heartbeat is due at {now:?}")
        };
        assert!(next_in <= 3600 * S, "the ceiling holds: {next_in:?}");
        next = next_in;
    }
    assert_eq!(next, 3600 * S, "past the ceiling the lane still speaks hourly");
}

/// A cleared failure is forgotten: the next one has its own onset and the first interval again.
#[test]
fn a_cleared_failure_starts_the_next_one_responsive() {
    let mut log = ReconnectLog::new(10 * S, 3, 100 * S);
    log.tick(Duration::ZERO);
    log.tick(10 * S);
    log.clear();
    assert_eq!(log.tick(11 * S), ReconnectLogLine::Onset { next_in: 10 * S });
    assert_eq!(log.tick(20 * S), ReconnectLogLine::Quiet);
    assert_eq!(log.tick(21 * S), ReconnectLogLine::Heartbeat { down_for: 10 * S, next_in: 30 * S });
}

/// IG's trade stream, 1 s to 60 s with a budget of three re-login refusals and a reset on every session: inside the
/// budget a refusal waits the flat first step, past it the ladder climbs from it; each kind of failure is its own
/// run, and a session that opened starts the ladder over with its end waiting the first step.
#[test]
fn the_refusal_budget_waits_flat_then_climbs_and_each_kind_is_its_own_run() {
    let ig =
        || ReconnectBackoff::new(S, 60 * S).with_refusal_budget(3).with_reset_after(Duration::ZERO);
    let mut b = ig();
    let refusals: Vec<_> = (0..10).map(|_| b.refused()).collect();
    assert_eq!(refusals, [1, 1, 1, 1, 2, 4, 8, 16, 32, 60].map(|s| s * S));
    assert_eq!(b.refused(), 60 * S, "capped");
    assert_eq!(
        [b.failed(), b.failed(), b.failed()],
        [S, 2 * S, 4 * S],
        "a failed open: its own run"
    );
    assert_eq!(b.refused(), S, "and a refusal after it starts its own, inside the budget");
    assert_eq!([b.failed(), b.failed()], [S, 2 * S]);

    assert_eq!(b.ended(Duration::ZERO), S, "a session that opened waits the first step");
    assert_eq!(b.failed(), S, "and leaves the ladder at its start");
    assert_eq!(b.ended(Duration::ZERO), S, "a drop after a drop waits flat too");
    for _ in 0..3 {
        assert_eq!(b.refused(), S, "a session ended the refusal run: the budget is whole again");
    }
    assert_eq!(b.refused(), S, "the first refusal past the budget is the ladder's first step");
    assert_eq!(b.refused(), 2 * S);

    let mut long = ReconnectBackoff::new(S, 60 * S).with_reset_after(10 * S);
    assert_eq!([long.failed(), long.failed()], [S, 2 * S]);
    assert_eq!(long.ended(9 * S), 4 * S, "a session shorter than the reset keeps climbing");
    assert_eq!(long.ended(10 * S), S, "one that lived it starts over");
    assert_eq!(long.failed(), S);
}
