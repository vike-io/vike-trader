//! [`ReconnectLog`]'s pace, with no clock. The ladder's tests moved with it: `crates/vike-venues/src/reconnect_tests.rs`.
use super::*;

const S: Duration = Duration::from_secs(1);

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
