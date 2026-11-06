//! The re-dial policy on a FAKE clock (`Instant` plus an offset: every method takes `now`).

use super::*;

fn s(n: u64) -> Duration {
    Duration::from_secs(n)
}

/// Midday UTC: nowhere near the daily logoff.
const NOON_MS: i64 = 12 * 3600 * 1000;

/// The epoch ms of `hh:mm:ss` UTC on some day (the day is not read, only the time in it).
fn utc(h: i64, m: i64, sec: i64) -> i64 {
    (3 * 86_400 + h * 3600 + m * 60 + sec) * 1000
}

const DOWN: Step = Step::Down { opened: false, attempt: false };
const DUE: Step = Step::Down { opened: false, attempt: true };

/// A healthy stream stays healthy, however often it is looked at.
#[test]
fn a_healthy_stream_is_left_alone() {
    let t = Instant::now();
    let mut r = Redial::default();
    for n in 0..100 {
        assert_eq!(r.observe(t + s(n), NOON_MS, false), Step::Healthy);
    }
    assert!(!r.is_down());
    assert_eq!(r.failed(), 0);
}

/// The outage is OPENED once; no dial is due before the self-heal wait, and then it is due.
#[test]
fn a_death_opens_one_outage_and_leaves_the_first_wait_to_ibapi() {
    let t = Instant::now();
    let mut r = Redial::default();
    assert_eq!(r.observe(t, NOON_MS, true), Step::Down { opened: true, attempt: false });
    for n in 1..SELF_HEAL.as_secs() {
        assert_eq!(r.observe(t + s(n), NOON_MS, true), DOWN, "{n} s in");
    }
    assert_eq!(r.observe(t + SELF_HEAL, NOON_MS, true), DUE);
    assert!(r.is_down());
    assert_eq!(r.quiet_until(), None, "midday has no quiet hint");
    assert_eq!(r.down_for(t + s(7)), s(7));
}

/// **Exactly one dial per backoff step.** A dial that is attempted and fails waits 60 s, 2 min, then 5 min (the cap
/// repeats): looked at every second, the dial is due at 20, 80, 200, 500 and 800 s and at no other second.
#[test]
fn a_failed_dial_waits_the_backoff_and_each_step_dials_once() {
    let t = Instant::now();
    let mut r = Redial::default();
    let mut dials = Vec::new();
    for n in 0..=900 {
        let step = r.observe(t + s(n), NOON_MS, true);
        if matches!(step, Step::Down { attempt: true, .. }) {
            dials.push(n);
            r.dial_failed(t + s(n));
        }
    }
    assert_eq!(dials, [20, 80, 200, 500, 800]);
    assert_eq!(r.failed(), 5);
}

/// A gateway that is not ready (the port is closed, a turn is held) is asked again later and costs NOTHING: no
/// attempt, no backoff step; the dial is made on the first look after it is ready.
#[test]
fn a_gateway_that_is_not_ready_costs_no_attempt() {
    let t = Instant::now();
    let mut r = Redial::default();
    r.observe(t, NOON_MS, true);
    let mut asked = Vec::new();
    for n in SELF_HEAL.as_secs()..200 {
        if matches!(r.observe(t + s(n), NOON_MS, true), Step::Down { attempt: true, .. }) {
            asked.push(n);
            r.not_ready(t + s(n), Duration::ZERO);
        }
    }
    // Asked every PROBE_EVERY (a zero answer is still not sooner), never spent.
    let every = PROBE_EVERY.as_secs();
    assert_eq!(asked, (SELF_HEAL.as_secs()..200).step_by(every as usize).collect::<Vec<_>>());
    assert_eq!(r.failed(), 0, "a closed port is not a failed dial");
    // The answer can say a longer time (the lane's hourly budget): not asked before it.
    r.not_ready(t + s(200), s(3000));
    assert_eq!(r.observe(t + s(3199), NOON_MS, true), DOWN);
    assert_eq!(r.observe(t + s(3200), NOON_MS, true), DUE);
    assert_eq!(r.failed(), 0);
}

/// A re-dial that stands for `LIVE_AFTER` ends the outage; one second short of it the outage is still open and
/// nothing else is due. The ladder starts again for the next outage.
#[test]
fn a_redial_that_stands_ends_the_outage() {
    let t = Instant::now();
    let mut r = Redial::default();
    r.observe(t, NOON_MS, true);
    assert_eq!(r.observe(t + s(20), NOON_MS, true), DUE);
    r.dial_failed(t + s(20));
    assert_eq!(r.observe(t + s(80), NOON_MS, true), DUE);
    r.redialled(t + s(80));
    for n in 0..LIVE_AFTER.as_secs() {
        assert_eq!(r.observe(t + s(80 + n), NOON_MS, false), DOWN, "{n} s into the probation");
    }
    assert_eq!(r.observe(t + s(80) + LIVE_AFTER, NOON_MS, false), Step::Recovered);
    assert!(!r.is_down());
    assert_eq!(r.failed(), 0, "the next outage starts a fresh ladder");
    let next = t + s(1000);
    assert_eq!(r.observe(next, NOON_MS, true), Step::Down { opened: true, attempt: false });
    assert_eq!(r.observe(next + SELF_HEAL, NOON_MS, true), DUE, "...from the first step");
}

/// **No flapping storm.** A connection accepted and dropped again inside the probation is a FAILED dial of the SAME
/// outage: no second opening, no recovery, and the ladder climbs (the next dial waits the second step, 2 min).
#[test]
fn a_flapping_connection_is_one_outage_and_climbs_the_ladder() {
    let t = Instant::now();
    let mut r = Redial::default();
    r.observe(t, NOON_MS, true);
    assert_eq!(r.observe(t + s(20), NOON_MS, true), DUE);
    r.redialled(t + s(20));
    // Dead again 2 s later.
    assert_eq!(r.observe(t + s(22), NOON_MS, true), DOWN, "the first flap waits the first step");
    assert_eq!(r.failed(), 1);
    assert_eq!(r.observe(t + s(81), NOON_MS, true), DOWN);
    assert_eq!(r.observe(t + s(82), NOON_MS, true), DUE);
    r.redialled(t + s(82));
    assert_eq!(r.observe(t + s(84), NOON_MS, true), DOWN, "...and again");
    assert_eq!(r.failed(), 2);
    assert_eq!(r.observe(t + s(84 + 119), NOON_MS, true), DOWN);
    assert_eq!(r.observe(t + s(84 + 120), NOON_MS, true), DUE, "the second step is 2 minutes");
    // It holds this time.
    r.redialled(t + s(204));
    assert_eq!(r.observe(t + s(204) + LIVE_AFTER, NOON_MS, false), Step::Recovered);
}

/// A dead symbol that went away (unsubscribed) leaves nothing to dial: the outage is over on the next look.
#[test]
fn an_outage_with_nothing_left_dead_is_over_without_a_dial() {
    let t = Instant::now();
    let mut r = Redial::default();
    r.observe(t, NOON_MS, true);
    assert_eq!(r.observe(t + s(3), NOON_MS, false), Step::Recovered);
    assert!(!r.is_down());
}

/// **IB's daily logoff** (23:45:00 UTC; the gateway is back about 85 s later): an outage that began in its window
/// holds the first dial until 80 s past the logoff, whatever the self-heal wait says.
#[test]
fn an_outage_in_the_daily_logoff_window_is_quiet_until_the_gateway_can_be_back() {
    let t = Instant::now();
    let mut r = Redial::default();
    // The socket dies at 23:45:02.
    assert_eq!(
        r.observe(t, utc(23, 45, 2), true),
        Step::Down { opened: true, attempt: false },
        "the outage is disclosed at once"
    );
    // 23:46:20 = logoff + 80 s = 78 s after the death.
    assert_eq!(r.quiet_until(), Some(t + s(78)));
    assert_eq!(r.next_dial(), Some(t + s(78)));
    assert_eq!(
        r.observe(t + s(77), utc(23, 46, 19), true),
        DOWN,
        "past the self-heal wait, still quiet"
    );
    assert_eq!(r.observe(t + s(78), utc(23, 46, 20), true), DUE);
}

/// The window's edges: half a minute before the logoff (a clock a little ahead) is inside it, the logoff's own
/// 80 s are inside it, anything else (including the same minute the day before) is the plain schedule.
#[test]
fn only_an_outage_near_the_logoff_is_in_its_window() {
    let quiet = daily_logoff_quiet;
    assert_eq!(quiet(utc(23, 44, 29)), None, "more than 30 s before");
    assert_eq!(quiet(utc(23, 44, 30)), Some(s(110)), "30 s before: 110 s to go");
    assert_eq!(quiet(utc(23, 45, 0)), Some(LOGOFF_BACK_AFTER));
    assert_eq!(quiet(utc(23, 46, 19)), Some(s(1)));
    assert_eq!(quiet(utc(23, 46, 20)), None, "the gateway can be back");
    assert_eq!(quiet(utc(12, 0, 0)), None);
    assert_eq!(quiet(utc(0, 0, 1)), None);
    // The day does not matter, the time in it does.
    assert_eq!(quiet(utc(23, 45, 0) + 86_400_000), Some(LOGOFF_BACK_AFTER));
    assert_eq!(quiet(utc(23, 45, 0) - 5 * 86_400_000), Some(LOGOFF_BACK_AFTER));
}

/// The ladder is the gate's: 60 s, 2 min, then 5 min for ever.
#[test]
fn the_backoff_table_is_the_gates() {
    assert_eq!(backoff(0), s(60), "no failure yet reads as the first step");
    assert_eq!([1, 2, 3, 4, 40].map(backoff), [s(60), s(120), s(300), s(300), s(300)]);
}
