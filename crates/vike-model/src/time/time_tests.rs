use super::*;
use vike_marketdata::interval::interval_ms;

/// The instant formatter, and the property that makes it safe to sit beside
/// [`epoch_ms_to_utc_date`]: the two never disagree about which DAY an instant belongs to,
/// including across the pre-1970 sign boundary where a naive `/` would skew by a day.
#[test]
fn the_utc_timestamp_agrees_with_the_utc_date_on_every_boundary() {
    assert_eq!(epoch_ms_to_utc_timestamp(0), "1970-01-01T00:00:00Z");
    assert_eq!(epoch_ms_to_utc_timestamp(-1), "1969-12-31T23:59:59Z");
    assert_eq!(epoch_ms_to_utc_timestamp(86_399_999), "1970-01-01T23:59:59Z");
    assert_eq!(epoch_ms_to_utc_timestamp(1_787_356_800_000), "2026-08-22T00:00:00Z");
    assert_eq!(epoch_ms_to_utc_timestamp(1_787_356_800_000 + 45_296_000), "2026-08-22T12:34:56Z");
    // Sub-second precision is dropped rather than rounded: a truncation cannot move an instant
    // across a day boundary, which a round-half-up would at 23:59:59.500.
    assert_eq!(epoch_ms_to_utc_timestamp(86_399_500), "1970-01-01T23:59:59Z");
    for ms in [-86_400_001i64, -1, 0, 1, 86_399_999, 1_787_356_800_123] {
        let stamp = epoch_ms_to_utc_timestamp(ms);
        assert_eq!(&stamp[..10], epoch_ms_to_utc_date(ms), "day halves disagree at {ms}");
    }
}

#[test]
fn epoch_and_day_boundaries() {
    assert_eq!(epoch_ms_to_utc_date(0), "1970-01-01");
    assert_eq!(epoch_ms_to_utc_date(86_400_000 - 1), "1970-01-01"); // last ms of day 0
    assert_eq!(epoch_ms_to_utc_date(86_400_000), "1970-01-02"); // first ms of day 1
}

#[test]
fn pre_epoch_floors_toward_negative_infinity() {
    assert_eq!(epoch_ms_to_utc_date(-1), "1969-12-31");
    assert_eq!(epoch_ms_to_utc_date(-86_400_000), "1969-12-31");
    assert_eq!(epoch_ms_to_utc_date(-86_400_000 - 1), "1969-12-30");
}

#[test]
fn floor_ms_to_utc_day_matches_the_date_string_floor() {
    // floor_ms_to_utc_day(ms) must be the epoch-ms whose epoch_ms_to_utc_date is the same day,
    // and re-flooring the floored value must be a no-op (idempotent).
    for ms in [0, 1, 86_400_000 - 1, 86_400_000, -1, -86_400_000, 1_700_000_500_123_i64] {
        let floored = floor_ms_to_utc_day(ms);
        assert_eq!(epoch_ms_to_utc_date(floored), epoch_ms_to_utc_date(ms));
        assert_eq!(floor_ms_to_utc_day(floored), floored, "idempotent");
        assert!(floored <= ms, "floor never moves forward in time");
    }
    assert_eq!(floor_ms_to_utc_day(0), 0);
    assert_eq!(floor_ms_to_utc_day(86_400_000 - 1), 0);
    assert_eq!(floor_ms_to_utc_day(86_400_000), 86_400_000);
    assert_eq!(floor_ms_to_utc_day(-1), -86_400_000, "pre-epoch floors toward -inf");
}

/// The `utc_weekday` anchor, derived rather than asserted: 1970-01-01 is a Thursday, and every
/// weekday index is checked against a date whose day-of-week is a known calendar fact.
#[test]
fn utc_weekday_monday_is_zero() {
    assert_eq!(utc_weekday(0), 3, "1970-01-01 was a Thursday");
    // 2024-01-01 was a Monday; walk a full week from it.
    let monday = days_from_civil(2024, 1, 1) * 86_400_000;
    for (i, ms) in (0..7).map(|d| (d, monday + d * 86_400_000)) {
        assert_eq!(utc_weekday(ms), i as u32, "day +{i} from Monday 2024-01-01");
    }
    // stable across the whole day, and wraps back to Monday after seven
    assert_eq!(utc_weekday(monday + 86_400_000 - 1), 0);
    assert_eq!(utc_weekday(monday + 7 * 86_400_000), 0);
    // pre-1970 floors toward -inf: the day before Thursday 1970-01-01 is a Wednesday.
    assert_eq!(utc_weekday(-1), 2);
}

#[test]
fn leap_day_2000() {
    assert_eq!(epoch_ms_to_utc_date(951_782_400_000), "2000-02-29");
    assert_eq!(epoch_ms_to_utc_date(951_868_800_000), "2000-03-01");
}

#[test]
fn known_recent_dates() {
    assert_eq!(epoch_ms_to_utc_date(1_609_459_200_000), "2021-01-01");
    assert_eq!(epoch_ms_to_utc_date(1_609_459_200_000 - 1), "2020-12-31");
}

#[test]
fn ns_date_matches_ms_date() {
    let ms = 1_609_459_200_000_i64;
    assert_eq!(epoch_ns_to_utc_date(ms * 1_000_000), epoch_ms_to_utc_date(ms));
}

#[test]
fn parse_hour_label_validates_ranges() {
    assert_eq!(parse_hour_label("2026-07-12T05"), Some((2026, 7, 12, 5)));
    assert_eq!(parse_hour_label("2026-13-45T05"), None); // out-of-range month/day
    assert_eq!(parse_hour_label("2026-07-12T24"), None); // out-of-range hour
    assert_eq!(parse_hour_label("garbage"), None);
}

#[test]
fn days_in_range_spans_leap_day() {
    // Reproduces the old vike-backfill tardis `days_in_range` output across a leap-day boundary.
    assert_eq!(
        days_in_range((2024, 2, 27), (2024, 3, 1)),
        vec![(2024, 2, 27), (2024, 2, 28), (2024, 2, 29), (2024, 3, 1)]
    );
    assert_eq!(days_in_range((2023, 12, 31), (2024, 1, 1)), vec![(2023, 12, 31), (2024, 1, 1)]);
    assert_eq!(days_in_range((2024, 5, 5), (2024, 5, 5)), vec![(2024, 5, 5)]);
    // start after end → empty (the primitives-based range; the old tardis copy returned
    // `vec![start]` here — a benign, documented behavior change).
    assert!(days_in_range((2024, 5, 6), (2024, 5, 5)).is_empty());
}

#[test]
fn parse_date_label_epoch_ms_passthrough() {
    assert_eq!(parse_date_label("1704067200000"), Ok(1_704_067_200_000));
    assert_eq!(parse_date_label("0"), Ok(0));
    assert_eq!(parse_date_label("-1"), Ok(-1)); // pre-1970 epoch-ms allowed
    assert_eq!(parse_date_label("  42  "), Ok(42)); // trims surrounding whitespace
}

#[test]
fn parse_date_label_ymd_to_midnight_utc_ms() {
    assert_eq!(parse_date_label("1970-01-01"), Ok(0));
    assert_eq!(parse_date_label("2021-01-01"), Ok(1_609_459_200_000));
    assert_eq!(parse_date_label("2024-01-01"), Ok(1_704_067_200_000));
    // leap day maps to its own midnight-UTC instant.
    assert_eq!(parse_date_label("2000-02-29"), Ok(951_782_400_000));
}

#[test]
fn parse_date_label_rejects_garbage() {
    assert!(parse_date_label("garbage").is_err());
    assert!(parse_date_label("2024-01").is_err()); // wrong field count
    assert!(parse_date_label("2024-13-01").is_err()); // out-of-range month
    assert!(parse_date_label("2024-01-32").is_err()); // out-of-range day
    assert!(parse_date_label("2024-xx-01").is_err()); // non-numeric field
}

#[test]
fn parse_ymd_validated_tuple() {
    assert_eq!(parse_ymd("2024-02-29"), Ok((2024, 2, 29)));
    assert_eq!(parse_ymd(" 2023-12-31 "), Ok((2023, 12, 31)));
    assert!(parse_ymd("1704067200000").is_err()); // an epoch-ms is not a YYYY-MM-DD
    assert!(parse_ymd("2024-00-10").is_err()); // month 0 out of range
}

#[test]
fn hours_in_range_labels_and_rollovers() {
    assert_eq!(hours_in_range("2026-04-13T22", "2026-04-13T22"), vec!["2026-04-13T22"]);
    assert_eq!(
        hours_in_range("2026-04-13T22", "2026-04-14T01"),
        vec!["2026-04-13T22", "2026-04-13T23", "2026-04-14T00", "2026-04-14T01"]
    );
    assert_eq!(
        hours_in_range("2026-01-31T23", "2026-02-01T01"),
        vec!["2026-01-31T23", "2026-02-01T00", "2026-02-01T01"]
    );
    assert!(hours_in_range("2026-04-14T01", "2026-04-13T22").is_empty());
    assert!(hours_in_range("garbage", "2026-04-13T22").is_empty());
    assert!(hours_in_range("2026-04-13T22", "not-a-date").is_empty());
    assert!(hours_in_range("2026-04-13", "2026-04-13T22").is_empty()); // missing T-hour
}

/// Every suffix the `[walkforward]` duration grammar accepts, and the refusal that is the
/// point of it existing separately from `interval_ms`: a bare number must not silently
/// become bars or days.
#[test]
fn parse_span_accepts_every_suffix_and_refuses_a_bare_number() {
    assert_eq!(parse_span("90m"), Ok(Span::Ms(90 * 60_000)));
    assert_eq!(parse_span("4h"), Ok(Span::Ms(4 * 3_600_000)));
    assert_eq!(parse_span("30d"), Ok(Span::Ms(30 * 86_400_000)));
    assert_eq!(parse_span("2w"), Ok(Span::Ms(2 * 7 * 86_400_000)));
    assert_eq!(parse_span("3mo"), Ok(Span::Months(3)));
    assert_eq!(parse_span("1y"), Ok(Span::Months(12)));
    assert_eq!(parse_span("5000bars"), Ok(Span::Bars(5000)));
    assert_eq!(parse_span(" 12MO "), Ok(Span::Months(12))); // trimmed, case-insensitive
    let e = parse_span("90").unwrap_err();
    assert!(e.contains("no unit") && e.contains("90bars"), "{e}");
    assert!(parse_span("1s").is_err()); // `s` is deliberately NOT in this grammar
    assert!(parse_span("1x").is_err());
    assert!(parse_span("").is_err());
}

/// The two traps this grammar exists to not inherit. `interval_ms("0m")` answers `Some(0)`
/// — a zero-length window makes every boundary the same instant — and `1M` is the
/// minutes/months case collision the spec bans by name.
#[test]
fn parse_span_refuses_zero_and_refuses_uppercase_m_by_name() {
    assert_eq!(interval_ms("0m"), Some(0)); // the hole this grammar must not copy
    let z = parse_span("0m").unwrap_err();
    assert!(z.contains("zero"), "{z}");
    assert!(parse_span("0bars").is_err());
    let m = parse_span("1M").unwrap_err();
    assert!(m.contains("mo") && m.contains("minute"), "{m}");
}

/// Calendar months clamp to the shorter month and are exact across a leap year — the
/// property that makes `mo` a different answer from `30d`.
#[test]
fn add_calendar_months_clamps_a_short_month_and_keeps_the_time_of_day() {
    let at = |y, m, d, tod: i64| days_from_civil(y, m, d) * 86_400_000 + tod;
    assert_eq!(add_calendar_months(at(2026, 1, 31, 0), 1), at(2026, 2, 28, 0));
    assert_eq!(add_calendar_months(at(2024, 1, 31, 0), 1), at(2024, 2, 29, 0)); // leap
    assert_eq!(add_calendar_months(at(2024, 3, 31, 0), 1), at(2024, 4, 30, 0));
    assert_eq!(add_calendar_months(at(2024, 2, 29, 0), 12), at(2025, 2, 28, 0));
    assert_eq!(add_calendar_months(at(2026, 6, 15, 0), 6), at(2026, 12, 15, 0));
    assert_eq!(add_calendar_months(at(2026, 12, 15, 0), 1), at(2027, 1, 15, 0));
    assert_eq!(add_calendar_months(at(2026, 1, 31, 3_600_000), 1), at(2026, 2, 28, 3_600_000));
}

/// ⚠ The formatter is no longer `chrono`'s — it is [`civil_from_days`], because the run module
/// moved into the crate at the BOTTOM of the graph and `chrono` may not follow it there
/// (`crates/vike-cli/Cargo.toml`'s whole identity is being light). These instants are the
/// pins: each was MEASURED against the chrono implementation this replaced, in the commit before
/// the move, so a divergence is a REGRESSION in a document already on people's disks rather than
/// a formatting preference.
#[test]
fn the_chrono_free_formatter_is_byte_identical_to_the_one_it_replaced() {
    for (secs, expect) in [
        (0_i64, "1970-01-01T00:00:00Z"),
        (1_756_000_000, "2025-08-24T01:46:40Z"),
        (1_756_000_012, "2025-08-24T01:46:52Z"),
        (951_782_400, "2000-02-29T00:00:00Z"),
        (-1, "1969-12-31T23:59:59Z"),
        (-86_400, "1969-12-31T00:00:00Z"),
        (253_402_300_799, "9999-12-31T23:59:59Z"),
    ] {
        assert_eq!(utc_rfc3339(secs), expect, "{secs}");
    }
}

/// The fallback the old doc promised: a second no calendar date can hold must not panic. With
/// integer-exact civil math there is no such second inside `i64::MIN/86_400`, so the guard is
/// the four-digit-year boundary instead — and it must still return a string rather than abort.
#[test]
fn an_unrepresentable_second_falls_back_to_the_raw_number_instead_of_panicking() {
    assert_eq!(utc_rfc3339(i64::MIN), i64::MIN.to_string());
    assert_eq!(utc_rfc3339(i64::MAX), i64::MAX.to_string());
}

/// The millisecond instant, pinned to the exact strings its two former copies produced:
/// `vike-bridge-core`'s OKX `iso8601_ms` (a SIGNED header, so byte-identity is the contract) and
/// `vike-tradehub-core`'s incident-manifest `utc_stamp_rfc3339` (whose own tests pinned the first and last
/// rows below). Epoch 0, a leap day, a year boundary, negative ms and the year-1/year-0 edge.
#[test]
fn the_millisecond_timestamp_is_byte_identical_to_the_copies_it_replaced() {
    for (ms, expect) in [
        (0_i64, "1970-01-01T00:00:00.000Z"),
        (1, "1970-01-01T00:00:00.001Z"),
        (-1, "1969-12-31T23:59:59.999Z"),
        (-1_000, "1969-12-31T23:59:59.000Z"),
        (-86_400_001, "1969-12-30T23:59:59.999Z"),
        (86_399_999, "1970-01-01T23:59:59.999Z"),
        (951_782_400_000 + 45_296_789, "2000-02-29T12:34:56.789Z"),
        (1_704_067_199_999, "2023-12-31T23:59:59.999Z"),
        (1_704_067_200_000, "2024-01-01T00:00:00.000Z"),
        (1_609_459_200_000 + 3_661_123, "2021-01-01T01:01:01.123Z"),
        (-62_135_596_800_000, "0001-01-01T00:00:00.000Z"),
        (-62_135_596_800_001, "0000-12-31T23:59:59.999Z"),
    ] {
        assert_eq!(epoch_ms_to_utc_timestamp_ms(ms), expect, "{ms}");
    }
    // The second-resolution sibling is this string with the milliseconds cut, and the date half is
    // the `date=` partition key — the three never disagree about the day or the second.
    for ms in [i64::MIN, -86_400_001, -1, 0, 1, 951_782_400_999, 1_704_067_199_999, i64::MAX] {
        let stamp = epoch_ms_to_utc_timestamp_ms(ms);
        assert_eq!(&stamp[..stamp.len() - 5], &epoch_ms_to_utc_timestamp(ms)[..stamp.len() - 5]);
        assert_eq!(&stamp[..stamp.len() - 14], epoch_ms_to_utc_date(ms), "day half at {ms}");
    }
}

/// An operator's instant (`--expires`) reads back as the exact inverse of the formatter, the second- and
/// millisecond-resolution spellings both, and a date the calendar does not have, a field out of range,
/// a missing `Z` or a local offset is `None` rather than a nearby instant.
#[test]
fn an_operators_utc_instant_parses_as_the_inverse_of_the_formatter() {
    for ms in [0_i64, 1, 86_399_999, 951_782_400_000 + 45_296_789, 1_704_067_199_999] {
        assert_eq!(parse_utc_timestamp_ms(&epoch_ms_to_utc_timestamp_ms(ms)), Some(ms), "{ms}");
    }
    assert_eq!(parse_utc_timestamp_ms("2024-01-01T00:00:00Z"), Some(1_704_067_200_000));
    assert_eq!(parse_utc_timestamp_ms(" 2024-01-01T00:00:00.5Z "), Some(1_704_067_200_500));
    for bad in [
        "",
        "2024-01-01",
        "2024-01-01T00:00:00",
        "2024-01-01T00:00:00+02:00",
        "2026-02-30T00:00:00Z",
        "2024-13-01T00:00:00Z",
        "2024-01-01T24:00:00Z",
        "2024-01-01T00:60:00Z",
        "2024-01-01T00:00:00.1234Z",
        "tomorrow",
    ] {
        assert_eq!(parse_utc_timestamp_ms(bad), None, "{bad:?}");
    }
}

/// `MS_PER_DAY` and `utc_day` moved here from `orders::order` unchanged (the root names
/// `vike_model::MS_PER_DAY` / `vike_model::utc_day` did not move).
#[test]
fn utc_day_floors_across_the_epoch() {
    assert_eq!(MS_PER_DAY, 86_400_000);
    assert_eq!(utc_day(0), 0);
    assert_eq!(utc_day(MS_PER_DAY - 1), 0);
    assert_eq!(utc_day(MS_PER_DAY), 1);
    assert_eq!(utc_day(-1), -1, "pre-epoch floors down, so days stay monotone");
    assert_eq!(utc_day(1_709_164_800_000), 19_782, "the 2024 leap day");
    assert_eq!(utc_day(1_704_067_200_000 - 1), 19_722, "the last ms of 2023");
}

/// The values the routed calendar copies answered, now read off the home: `vike-tradehub`'s
/// catalog `today_utc` (its own tests' rows, verbatim) and `vike-datahub-client`'s
/// `days_from_civil` / `format_iso_date` pair (epoch 0, a leap day, a year boundary, pre-1970).
#[test]
fn the_routed_calendar_copies_answer_the_same_from_the_home() {
    assert_eq!(epoch_ms_to_utc_date(1_789_516_800_000), "2026-09-16");
    assert_eq!(epoch_ms_to_utc_date(1_789_516_800_000 - 1), "2026-09-15");
    assert_eq!(epoch_ms_to_utc_date(0), "1970-01-01");
    assert_eq!(epoch_ms_to_utc_date(1_709_164_800_000), "2024-02-29");
    for (ymd, days) in [
        ((1970_i64, 1_u32, 1_u32), 0_i64),
        ((1969, 12, 31), -1),
        ((2000, 2, 29), 11_016),
        ((2000, 3, 1), 11_017),
        ((2023, 12, 31), 19_722),
        ((2024, 1, 1), 19_723),
        ((2024, 2, 29), 19_782),
        ((2100, 3, 1), 47_541),
    ] {
        assert_eq!(days_from_civil(ymd.0, ymd.1, ymd.2), days, "{ymd:?}");
        assert_eq!(civil_from_days(days), ymd, "{days}");
    }
}
