use super::*;

/// The boundary every copy of this rule spelled, old expression against new on each side of it:
/// `ts + step <= now` is closed, and the bar exactly at its close time is closed.
#[test]
fn a_bar_is_closed_from_its_close_time_on() {
    const STEP: i64 = 60_000;
    const TIMESTAMP: i64 = 1_700_000_000_000;
    let bar = Bar::ohlcv(TIMESTAMP, 1.0, 1.0, 1.0, 1.0, 0.0);
    for (now, closed) in [
        (TIMESTAMP, false),
        (TIMESTAMP + STEP - 1, false),
        (TIMESTAMP + STEP, true),
        (TIMESTAMP + STEP + 1, true),
        (TIMESTAMP - 1, false),
    ] {
        assert_eq!(bar.closed_at(STEP, now), closed, "now {now}");
        assert_eq!(bar.closed_at(STEP, now), TIMESTAMP + STEP <= now, "the plain sum, now {now}");
        assert_eq!(
            !bar.closed_at(STEP, now),
            TIMESTAMP + STEP > now,
            "the forming tail's test, now {now}"
        );
    }
}

/// A timestamp at the edge of the range saturates rather than wrapping into "closed long ago".
#[test]
fn a_bar_at_the_edge_of_the_range_saturates() {
    let edge = Bar::ohlcv(i64::MAX - 1, 1.0, 1.0, 1.0, 1.0, 0.0);
    assert!(!edge.closed_at(60_000, 1_700_000_000_000));
    assert!(edge.closed_at(60_000, i64::MAX));
}

/// The path goes to the extreme nearer the open first, and to the low on a tie.
#[test]
fn the_path_goes_to_the_nearer_extreme_first_the_low_on_a_tie() {
    let path = |o, h, l, c| Bar::ohlcv(0, o, h, l, c, 0.0).path_points();
    assert_eq!(path(100.0, 101.0, 95.0, 98.0), [100.0, 101.0, 95.0, 98.0]);
    assert_eq!(path(100.0, 110.0, 97.0, 98.0), [100.0, 97.0, 110.0, 98.0]);
    assert_eq!(path(100.0, 102.0, 98.0, 99.0), [100.0, 98.0, 102.0, 99.0]);
}
