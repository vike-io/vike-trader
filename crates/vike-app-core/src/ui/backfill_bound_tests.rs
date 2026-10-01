use super::backfill_earliest_ts;

const HOUR_MS: i64 = 3_600_000;
const NOW: i64 = 1_700_000_000_000;

/// No bar has synced for this chart key yet: the bound is the hours-only floor.
#[test]
fn no_loaded_bars_falls_back_to_the_hours_floor() {
    assert_eq!(backfill_earliest_ts(NOW, 2.0, None), NOW - 2 * HOUR_MS);
}

/// `.max()`, NOT `.min()`: whichever bound is closer to "now" wins. A chart whose oldest
/// loaded bar is NEWER than the hours floor stops at that bar (nothing older has bars to
/// attach footprints to); one whose oldest bar is OLDER than the floor stops at the floor
/// (the hard cap against paging back to the venue's listing date).
#[test]
fn the_newer_of_the_two_bounds_wins() {
    let floor = NOW - 6 * HOUR_MS;
    // oldest bar NEWER than the floor -> the bar wins
    assert_eq!(backfill_earliest_ts(NOW, 6.0, Some(NOW - HOUR_MS)), NOW - HOUR_MS);
    // oldest bar OLDER than the floor -> the floor wins
    assert_eq!(backfill_earliest_ts(NOW, 6.0, Some(NOW - 99 * HOUR_MS)), floor);
}

/// `of_backfill_hours` is config-controlled (a hand-edited `workspace.json` is not
/// range-checked), so an absurd value must never panic or wrap. TWO independent guards, and
/// they clamp at different places — this pins both.
///
/// 1. The **cast**: `f64::MAX * 3.6e6` is `inf`, and `1e300 * 3.6e6` is merely enormous;
///    `as i64` is a saturating cast, so both become `i64::MAX` milliseconds of lookback. The
///    subtraction `NOW - i64::MAX` is then still perfectly representable (`NOW` is ~1.7e12,
///    nowhere near the ~9.2e18 range), so the answer is that finite, absurdly-old timestamp —
///    NOT `i64::MIN`. Without the saturating cast this would be UB-adjacent nonsense instead.
/// 2. The **subtraction**: `saturating_sub` is what stops a debug-build panic / release-build
///    wraparound when `now_ms` itself is pathological. Proven at `i64::MIN`, the only input
///    that actually makes the subtraction overflow.
#[test]
fn an_absurd_hours_value_saturates_instead_of_overflowing() {
    // (1) the cast saturates; the subtraction does not need to.
    assert_eq!(backfill_earliest_ts(NOW, f64::MAX, None), NOW - i64::MAX);
    assert_eq!(backfill_earliest_ts(NOW, 1e300, None), NOW - i64::MAX);
    // (2) the subtraction saturates when it genuinely would overflow.
    assert_eq!(backfill_earliest_ts(i64::MIN, f64::MAX, None), i64::MIN);
    // …and a loaded bar still clamps the absurd floor back up to something usable.
    assert_eq!(backfill_earliest_ts(NOW, f64::MAX, Some(NOW - HOUR_MS)), NOW - HOUR_MS);
}
