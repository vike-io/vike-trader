//! Tests of the account clock rule: the three UK clock changes the acceptance run and its next days
//! concern, both sides of each, the dates of the rule in other years, and the cutting of a window.

use super::*;
use crate::data::parse_ig_time_utc;

fn utc(text: &str) -> i64 {
    parse_ig_time_utc(text)
}

/// 2025-10-26T01:00:00Z, the change the acceptance run measured the demo account across: 480 -> 420.
const CHANGE_2025_10_26: i64 = 1_761_440_400_000;
/// 2026-03-29T01:00:00Z: 420 -> 480.
const CHANGE_2026_03_29: i64 = 1_774_746_000_000;
/// 2026-10-25T01:00:00Z, the day after the acceptance run: 480 -> 420.
const CHANGE_2026_10_25: i64 = 1_792_890_000_000;

/// The three instants, as literal epochs and as the dates they are, so neither can drift from the other.
#[test]
fn the_change_instants_are_the_uk_ones() {
    assert_eq!(utc("2025-10-26T01:00:00"), CHANGE_2025_10_26);
    assert_eq!(utc("2026-03-29T01:00:00"), CHANGE_2026_03_29);
    assert_eq!(utc("2026-10-25T01:00:00"), CHANGE_2026_10_25);
}

/// **Both sides of each recorded change**: 480 before the October ones and from the March one, 420 after
/// the October ones and before the March one; the change instant itself belongs to the new offset.
#[test]
fn the_offset_flips_exactly_at_each_recorded_change() {
    for (change, before, after) in [
        (CHANGE_2025_10_26, 480, 420),
        (CHANGE_2026_03_29, 420, 480),
        (CHANGE_2026_10_25, 480, 420),
    ] {
        assert_eq!(offset_minutes_at(change - 1), Some(before), "a millisecond before {change}");
        assert_eq!(offset_minutes_at(change), Some(after), "at {change}");
        assert_eq!(offset_minutes_at(change - HOUR_MS), Some(before));
        assert_eq!(offset_minutes_at(change + HOUR_MS), Some(after));
        assert_eq!(offset_minutes_at(change - 20 * DAY_MS), Some(before));
        assert_eq!(offset_minutes_at(change + 20 * DAY_MS), Some(after));
    }
}

/// The windows the acceptance run read, with the offset the account answered them in: the two that were
/// stored (480) and the one that was refused as the wrong side (420).
#[test]
fn the_windows_the_acceptance_run_read_have_the_offsets_it_saw() {
    for (from, to, offset) in [
        ("2025-08-28T00:00:00", "2025-10-22T23:59:59", 480),
        ("2025-12-18T00:00:00", "2026-01-14T23:59:59", 420),
        ("2026-04-09T00:00:00", "2026-10-23T17:22:23", 480),
    ] {
        assert_eq!(offset_minutes_at(utc(from)), Some(offset), "{from}");
        assert_eq!(offset_minutes_at(utc(to)), Some(offset), "{to}");
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

/// An instant outside 1996..=9999 has no rule: the UK's dates were another before 1996 and the lane writes
/// four-digit years. It is `None`, never a guess.
#[test]
fn an_instant_outside_the_years_of_the_rule_has_no_offset() {
    assert_eq!(offset_minutes_at(utc("1995-12-31T23:59:59")), None);
    assert_eq!(offset_minutes_at(utc("1996-01-01T00:00:00")), Some(420));
    assert_eq!(offset_minutes_at(utc("9999-12-31T23:59:59")), Some(420));
    assert_eq!(offset_minutes_at(utc("9999-12-31T23:59:59") + 1_000), None);
    assert_eq!(offset_minutes_at(i64::MIN), None);
    assert_eq!(offset_minutes_at(i64::MAX), None);
}

fn piece(start_ms: i64, end_ms: i64, offset_minutes: i32) -> Part {
    Part { start_ms, end_ms, offset_minutes }
}

/// A window on one side of every change is ONE piece, asked in its offset: a chunk of the winter and one
/// of the summer, as the acceptance run met them.
#[test]
fn a_window_with_no_change_is_one_piece() {
    let winter = (utc("2025-12-18T00:00:00"), utc("2026-01-14T23:59:59") + 999);
    assert_eq!(parts(winter.0, winter.1), Some(vec![piece(winter.0, winter.1, 420)]));
    let summer = (utc("2026-09-25T00:00:00"), utc("2026-10-22T23:59:59") + 999);
    assert_eq!(parts(summer.0, summer.1), Some(vec![piece(summer.0, summer.1, 480)]));
}

/// **A chunk that straddles a change is cut AT it**: the piece before ends a millisecond earlier, the piece
/// after begins on the change instant, each in its own offset. These are the daily chunk the acceptance run
/// failed on (2025-10-23..2025-11-19), the hourly chunk around the March change, and the chunk that holds
/// tomorrow's.
#[test]
fn a_window_that_straddles_a_change_is_cut_at_it() {
    for (change, before, after) in [
        (CHANGE_2025_10_26, 480, 420),
        (CHANGE_2026_03_29, 420, 480),
        (CHANGE_2026_10_25, 480, 420),
    ] {
        let (start, end) = (change - 3 * DAY_MS, change + 24 * DAY_MS);
        assert_eq!(
            parts(start, end),
            Some(vec![piece(start, change - 1, before), piece(change, end, after)]),
            "the window around {change}"
        );
    }
    let chunk = (utc("2025-10-23T00:00:00"), utc("2025-11-19T23:59:59") + 999);
    let cut = parts(chunk.0, chunk.1).expect("a rule for 2025");
    assert_eq!(cut.len(), 2);
    assert_eq!((cut[0].offset_minutes, cut[1].offset_minutes), (480, 420));
}

/// The edges of "holds a change": a window that BEGINS on the change instant is one piece in the new
/// offset; one that ends a millisecond before it is one piece in the old; one that ends ON it gets a
/// one-millisecond piece after it (the instant belongs to the new offset).
#[test]
fn the_change_instant_belongs_to_the_new_offset() {
    let change = CHANGE_2025_10_26;
    assert_eq!(parts(change, change + DAY_MS), Some(vec![piece(change, change + DAY_MS, 420)]));
    assert_eq!(
        parts(change - DAY_MS, change - 1),
        Some(vec![piece(change - DAY_MS, change - 1, 480)])
    );
    assert_eq!(
        parts(change - DAY_MS, change),
        Some(vec![piece(change - DAY_MS, change - 1, 480), piece(change, change, 420)])
    );
}

/// A window of several months holds several changes, and every piece is in the offset of its own start.
#[test]
fn a_window_holding_two_changes_is_three_pieces() {
    let (start, end) = (utc("2025-10-01T00:00:00"), utc("2026-04-30T23:59:59") + 999);
    assert_eq!(
        parts(start, end),
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
    assert_eq!(parts(10, 9), None);
    assert_eq!(parts(0, DAY_MS), None, "1970 has no rule");
    assert_eq!(parts(utc("1995-12-01T00:00:00"), utc("1996-02-01T00:00:00")), None);
    assert_eq!(parts(utc("2026-01-01T00:00:00"), i64::MAX), None);
}

/// Whatever the window, the pieces tile it: the first starts where it starts, the last ends where it ends,
/// each begins one millisecond after the one before, and none holds a change.
#[test]
fn the_pieces_tile_the_window_and_none_holds_a_change() {
    let mut start = utc("2025-01-01T00:00:00");
    while start < utc("2027-06-01T00:00:00") {
        for width in [DAY_MS, 21 * DAY_MS, 28 * DAY_MS, 400 * DAY_MS] {
            let end = start + width - 1;
            let cut = parts(start, end).expect("2025..2028 has a rule");
            assert_eq!(cut.first().map(|p| p.start_ms), Some(start));
            assert_eq!(cut.last().map(|p| p.end_ms), Some(end));
            for pair in cut.windows(2) {
                assert_eq!(pair[1].start_ms, pair[0].end_ms + 1);
            }
            for p in &cut {
                assert_eq!(offset_minutes_at(p.start_ms), Some(p.offset_minutes));
                assert_eq!(offset_minutes_at(p.end_ms), Some(p.offset_minutes), "{p:?}");
            }
        }
        start += 5 * DAY_MS + 7 * HOUR_MS;
    }
}
