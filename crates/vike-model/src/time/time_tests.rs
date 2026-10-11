use super::*;

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
fn interval_ms_units_and_rejects() {
    assert_eq!(interval_ms("30s"), Some(30_000));
    assert_eq!(interval_ms("1s"), Some(1_000));
    assert_eq!(interval_ms("1m"), Some(60_000));
    assert_eq!(interval_ms("5m"), Some(300_000));
    assert_eq!(interval_ms("15m"), Some(900_000));
    assert_eq!(interval_ms("1h"), Some(3_600_000));
    assert_eq!(interval_ms("4h"), Some(14_400_000));
    assert_eq!(interval_ms("1d"), Some(86_400_000));
    // malformed → None
    assert_eq!(interval_ms("m"), None); // len < 2
    assert_eq!(interval_ms("1x"), None); // unknown unit
    assert_eq!(interval_ms("-1m"), None); // non-digit count
    assert_eq!(interval_ms("1.5m"), None); // non-digit count
    assert_eq!(interval_ms("xm"), None); // empty/non-digit count
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
fn civil_roundtrips_days() {
    // days_from_civil ∘ civil_from_days == identity across a wide range.
    for z in [-146_097_i64, -719_468, -1, 0, 1, 18_628, 100_000, 999_999] {
        let (y, m, d) = civil_from_days(z);
        assert_eq!(days_from_civil(y, m, d), z, "roundtrip failed at z={z}");
    }
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

/// [`measures_bar_step`] is [`interval_ms`] with the value dropped — asserted as an
/// EQUIVALENCE rather than by re-listing the vocabulary, so the predicate can never drift from
/// the parser it names, and then pinned on the three spellings
/// `docs/decisions/0059-bars-and-ticks-for-every-venue-are-two-asks-not-one.md`'s bug B is
/// about.
#[test]
fn the_bar_step_predicate_is_the_parser_with_the_value_dropped() {
    for iv in [
        "1m", "5m", "30s", "1h", "4h", "1d", "7d", "1w", "1M", "1mo", "funding", "", "m", "0m",
        "-1m", "1x", "1.5h",
    ] {
        assert_eq!(
            measures_bar_step(iv),
            interval_ms(iv).is_some(),
            "the predicate and the parser disagree about {iv:?}"
        );
    }
}

/// The three spellings bug B is about, named one at a time — a fold over a list would pass over
/// an empty one, and these three are the claim.
///
/// `1w`, `1M` and `1mo` are REAL intervals that three dispatched venues serve, which is why the
/// answer "the guard declines" was right for a garbage string and wrong for these. `7d` is here
/// as the contrast: a week the vocabulary CAN measure, and the step an operator who wants
/// weekly bars from a measurable window should ask for.
#[test]
fn a_week_and_a_month_have_no_bar_width_but_seven_days_does() {
    assert!(!measures_bar_step("1w"), "a week is outside the s/m/h/d vocabulary");
    assert!(!measures_bar_step("1M"), "a calendar month is not a multiple of anything");
    assert!(!measures_bar_step("1mo"), "the two-character month spelling is outside it too");
    assert!(measures_bar_step("7d"), "seven days is a plain multiple and measures fine");
    assert!(
        !measures_bar_step("funding"),
        "the reserved funding-rate label is not a step and must never be read as one"
    );
}
