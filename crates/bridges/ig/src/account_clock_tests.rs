//! Tests of the account clock rule: the offsets MEASURED on two days (one with the UK in summer time, one in
//! winter time), the three UK clock changes both sides of each, the dates of the rule in other years, and the
//! cutting of a window.

use super::*;
use crate::data::parse_ig_time_utc;

fn utc(text: &str) -> i64 {
    parse_ig_time_utc(text)
}

/// 2026-10-23T17:22:23Z, a day the UK is in summer time: the acceptance run's day (its probe's first answer).
const SUMMER_DAY: i64 = 1_792_776_143_000;
/// 2026-10-30T19:50:40Z, a day the UK is in winter time: the day the account's clock was measured again.
const WINTER_DAY: i64 = 1_793_389_840_000;

/// 2025-10-26T01:00:00Z, the change the acceptance run measured the demo account across.
const CHANGE_2025_10_26: i64 = 1_761_440_400_000;
/// 2026-03-29T01:00:00Z.
const CHANGE_2026_03_29: i64 = 1_774_746_000_000;
/// 2026-10-25T01:00:00Z, the day after the acceptance run.
const CHANGE_2026_10_25: i64 = 1_792_890_000_000;

/// The offsets of an instant in British Summer Time and of one outside it, as the account reads a request made
/// on `day`: the two rows of the module doc's table.
fn offsets_on(day: i64) -> (i32, i32) {
    if day == SUMMER_DAY { (480, 420) } else { (540, 480) }
}

/// The instants, as literal epochs and as the dates they are, so neither can drift from the other.
#[test]
fn the_change_instants_and_the_two_days_are_the_dates_they_say() {
    assert_eq!(utc("2025-10-26T01:00:00"), CHANGE_2025_10_26);
    assert_eq!(utc("2026-03-29T01:00:00"), CHANGE_2026_03_29);
    assert_eq!(utc("2026-10-25T01:00:00"), CHANGE_2026_10_25);
    assert_eq!(utc("2026-10-23T17:22:23"), SUMMER_DAY);
    assert_eq!(utc("2026-10-30T19:50:40"), WINTER_DAY);
}

/// **The account's own offset is the login's `timezoneOffset`**: 8 hours, as `POST /session` and `GET /session`
/// both said on 2026-10-30, the same on the winter-time day's rows (a winter-time instant, 480).
#[test]
fn the_base_is_the_logins_timezone_offset_in_minutes() {
    assert_eq!(BASE_MINUTES, 8 * 60);
    assert_eq!(offset_minutes_at(utc("2026-10-30T00:00:00"), WINTER_DAY), Some(BASE_MINUTES));
}

/// **Both days' measurements, as they were read** (the rows' `snapshotTime` against `snapshotTimeUTC`).
/// 2026-10-30, the UK in winter time: 480 at 2026-01-13, 2026-10-26, 2026-10-28 and 2026-10-30, and 540 at
/// 2026-07-14 and 2026-10-22. 2026-10-23/24, the UK in summer time: 480 on 2025-09-25..2025-10-22 and
/// 2026-04-09..2026-10-23, and 420 on 2025-12-18..2026-01-14. ⚠ The old rule ("London time plus seven
/// hours", read off the second day alone) gave 420 and 480 for the first day: this test is what it failed.
#[test]
fn the_offsets_measured_on_each_day_are_the_ones_the_rule_gives() {
    for (instant, offset) in [
        ("2026-10-30T00:00:00", 480),
        ("2026-10-28T02:00:00", 480),
        ("2026-10-26T02:00:00", 480),
        ("2026-10-22T01:00:00", 540),
        ("2026-07-14T01:00:00", 540),
        ("2026-01-13T02:00:00", 480),
    ] {
        assert_eq!(
            offset_minutes_at(utc(instant), WINTER_DAY),
            Some(offset),
            "{instant} read on 2026-10-30"
        );
    }
    for (from, to, offset) in [
        ("2025-08-28T00:00:00", "2025-10-22T23:59:59", 480),
        ("2025-12-18T00:00:00", "2026-01-14T23:59:59", 420),
        ("2026-04-09T00:00:00", "2026-10-23T17:22:23", 480),
    ] {
        assert_eq!(
            offset_minutes_at(utc(from), SUMMER_DAY),
            Some(offset),
            "{from} read on 2026-10-23"
        );
        assert_eq!(offset_minutes_at(utc(to), SUMMER_DAY), Some(offset), "{to} read on 2026-10-23");
    }
}

/// **Both sides of each recorded change, on either kind of day**: the instant of the change belongs to the new
/// offset, and the two offsets differ by exactly the UK's summer hour.
#[test]
fn the_offset_flips_exactly_at_each_recorded_change() {
    for day in [SUMMER_DAY, WINTER_DAY] {
        let (summer, winter) = offsets_on(day);
        for (change, before, after) in [
            (CHANGE_2025_10_26, summer, winter),
            (CHANGE_2026_03_29, winter, summer),
            (CHANGE_2026_10_25, summer, winter),
        ] {
            let at = |ms| offset_minutes_at(ms, day);
            assert_eq!(at(change - 1), Some(before), "a millisecond before {change} on {day}");
            assert_eq!(at(change), Some(after), "at {change} on {day}");
            assert_eq!(at(change - HOUR_MS), Some(before));
            assert_eq!(at(change + HOUR_MS), Some(after));
            assert_eq!(at(change - 20 * DAY_MS), Some(before));
            assert_eq!(at(change + 20 * DAY_MS), Some(after));
        }
    }
}

/// **The rule is the last Sunday of March and of October, 01:00 UTC**, in every year it is known for:
/// the UK's published dates, one year at a time.
#[test]
fn british_summer_time_runs_from_the_last_sunday_of_march_to_the_last_sunday_of_october() {
    for (year, march, october) in [
        (1996, 31, 27),
        (2000, 26, 29),
        (2024, 31, 27),
        (2025, 30, 26),
        (2026, 29, 25),
        (2027, 28, 31),
        (2028, 26, 29),
        (2029, 25, 28),
        (2030, 31, 27),
    ] {
        let (begins, ends) = summer_time(year);
        assert_eq!(begins, utc(&format!("{year}-03-{march:02}T01:00:00")), "{year} begins");
        assert_eq!(ends, utc(&format!("{year}-10-{october:02}T01:00:00")), "{year} ends");
    }
}

/// An instant outside 1996..=9999, or a "now" outside it, has no rule: the UK's dates were another before 1996
/// and the lane writes four-digit years. It is `None`, never a guess.
#[test]
fn an_instant_outside_the_years_of_the_rule_has_no_offset() {
    let at = |ms| offset_minutes_at(ms, SUMMER_DAY);
    assert_eq!(at(utc("1995-12-31T23:59:59")), None);
    assert_eq!(at(utc("1996-01-01T00:00:00")), Some(420));
    assert_eq!(at(utc("9999-12-31T23:59:59")), Some(420));
    assert_eq!(at(utc("9999-12-31T23:59:59") + 1_000), None);
    assert_eq!(at(i64::MIN), None);
    assert_eq!(at(i64::MAX), None);
    assert_eq!(offset_minutes_at(SUMMER_DAY, i64::MIN), None, "no rule for a 'now' either");
    assert_eq!(offset_minutes_at(SUMMER_DAY, 0), None, "1970 is before the rule");
}

fn piece(start_ms: i64, end_ms: i64, offset_minutes: i32) -> Part {
    Part { start_ms, end_ms, offset_minutes }
}

/// A window on one side of every change is ONE piece, asked in its offset: a chunk of the winter and one
/// of the summer, as the acceptance run met them, and the same two read on a winter-time day.
#[test]
fn a_window_with_no_change_is_one_piece() {
    let winter = (utc("2025-12-18T00:00:00"), utc("2026-01-14T23:59:59") + 999);
    let summer = (utc("2026-09-25T00:00:00"), utc("2026-10-22T23:59:59") + 999);
    for day in [SUMMER_DAY, WINTER_DAY] {
        let (summer_offset, winter_offset) = offsets_on(day);
        assert_eq!(
            parts(winter.0, winter.1, day),
            Some(vec![piece(winter.0, winter.1, winter_offset)])
        );
        assert_eq!(
            parts(summer.0, summer.1, day),
            Some(vec![piece(summer.0, summer.1, summer_offset)])
        );
    }
}

/// **A chunk that straddles a change is cut AT it**: the piece before ends a millisecond earlier, the piece
/// after begins on the change instant, each in its own offset. These are the daily chunk the acceptance run
/// failed on (2025-10-23..2025-11-19), the hourly chunk around the March change, and the chunk that holds
/// the October one of 2026.
#[test]
fn a_window_that_straddles_a_change_is_cut_at_it() {
    for day in [SUMMER_DAY, WINTER_DAY] {
        let (summer, winter) = offsets_on(day);
        for (change, before, after) in [
            (CHANGE_2025_10_26, summer, winter),
            (CHANGE_2026_03_29, winter, summer),
            (CHANGE_2026_10_25, summer, winter),
        ] {
            let (start, end) = (change - 3 * DAY_MS, change + 24 * DAY_MS);
            assert_eq!(
                parts(start, end, day),
                Some(vec![piece(start, change - 1, before), piece(change, end, after)]),
                "the window around {change} on {day}"
            );
        }
    }
    let chunk = (utc("2025-10-23T00:00:00"), utc("2025-11-19T23:59:59") + 999);
    let cut = parts(chunk.0, chunk.1, SUMMER_DAY).expect("a rule for 2025");
    assert_eq!(cut.len(), 2);
    assert_eq!((cut[0].offset_minutes, cut[1].offset_minutes), (480, 420));
    let cut = parts(chunk.0, chunk.1, WINTER_DAY).expect("a rule for 2025");
    assert_eq!((cut[0].offset_minutes, cut[1].offset_minutes), (540, 480));
}

/// The edges of "holds a change": a window that BEGINS on the change instant is one piece in the new
/// offset; one that ends a millisecond before it is one piece in the old; one that ends ON it gets a
/// one-millisecond piece after it (the instant belongs to the new offset).
#[test]
fn the_change_instant_belongs_to_the_new_offset() {
    let change = CHANGE_2025_10_26;
    let day = SUMMER_DAY;
    assert_eq!(
        parts(change, change + DAY_MS, day),
        Some(vec![piece(change, change + DAY_MS, 420)])
    );
    assert_eq!(
        parts(change - DAY_MS, change - 1, day),
        Some(vec![piece(change - DAY_MS, change - 1, 480)])
    );
    assert_eq!(
        parts(change - DAY_MS, change, day),
        Some(vec![piece(change - DAY_MS, change - 1, 480), piece(change, change, 420)])
    );
}

/// A window of several months holds several changes, and every piece is in the offset of its own start.
#[test]
fn a_window_holding_two_changes_is_three_pieces() {
    let (start, end) = (utc("2025-10-01T00:00:00"), utc("2026-04-30T23:59:59") + 999);
    assert_eq!(
        parts(start, end, SUMMER_DAY),
        Some(vec![
            piece(start, CHANGE_2025_10_26 - 1, 480),
            piece(CHANGE_2025_10_26, CHANGE_2026_03_29 - 1, 420),
            piece(CHANGE_2026_03_29, end, 480),
        ])
    );
}

/// An inverted window and one outside the years of the rule have no pieces: the caller refuses them in
/// words, before any request.
#[test]
fn an_inverted_window_or_one_outside_the_rule_has_no_pieces() {
    assert_eq!(parts(10, 9, SUMMER_DAY), None);
    assert_eq!(parts(0, DAY_MS, SUMMER_DAY), None, "1970 has no rule");
    assert_eq!(parts(utc("1995-12-01T00:00:00"), utc("1996-02-01T00:00:00"), SUMMER_DAY), None);
    assert_eq!(parts(utc("2026-01-01T00:00:00"), i64::MAX, SUMMER_DAY), None);
}

/// Whatever the window and the day, the pieces tile it: the first starts where it starts, the last ends where
/// it ends, each begins one millisecond after the one before, and none holds a change.
#[test]
fn the_pieces_tile_the_window_and_none_holds_a_change() {
    for day in [SUMMER_DAY, WINTER_DAY] {
        let mut start = utc("2025-01-01T00:00:00");
        while start < utc("2027-06-01T00:00:00") {
            for width in [DAY_MS, 21 * DAY_MS, 28 * DAY_MS, 400 * DAY_MS] {
                let end = start + width - 1;
                let cut = parts(start, end, day).expect("2025..2028 has a rule");
                assert_eq!(cut.first().map(|p| p.start_ms), Some(start));
                assert_eq!(cut.last().map(|p| p.end_ms), Some(end));
                for pair in cut.windows(2) {
                    assert_eq!(pair[1].start_ms, pair[0].end_ms + 1);
                }
                for p in &cut {
                    assert_eq!(offset_minutes_at(p.start_ms, day), Some(p.offset_minutes));
                    assert_eq!(offset_minutes_at(p.end_ms, day), Some(p.offset_minutes), "{p:?}");
                }
            }
            start += 5 * DAY_MS + 7 * HOUR_MS;
        }
    }
}
