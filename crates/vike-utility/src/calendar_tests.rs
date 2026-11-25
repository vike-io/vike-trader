use super::*;

/// The one rule against the primitives it guards: a day is real exactly when the civil round trip
/// hands it back, over two full 400-year leap cycles and past both ends of every field.
#[test]
fn a_real_day_is_one_the_civil_round_trip_hands_back() {
    for y in -400..=2800_i64 {
        for m in 1..=12_u32 {
            for d in 1..=32_u32 {
                let round_trip = civil_from_days(days_from_civil(y, m, d)) == (y, m, d);
                assert_eq!(is_real_day(y, m, d), round_trip, "{y}-{m}-{d}");
            }
        }
    }
    for (y, m, d) in [(2026, 0, 1), (2026, 13, 1), (2026, 1, 0), (2026, 1, u32::MAX)] {
        assert!(!is_real_day(y, m, d), "{y}-{m}-{d}");
    }
    assert!(is_real_day(2000, 2, 29) && !is_real_day(2100, 2, 29) && !is_real_day(2026, 2, 31));
    assert!(is_real_day(i64::MAX, 1, 1) && is_real_day(i64::MIN, 1, 1), "no overflow at the edges");
}

#[test]
fn civil_roundtrips_days() {
    // days_from_civil ∘ civil_from_days == identity across a wide range.
    for z in [-146_097_i64, -719_468, -1, 0, 1, 18_628, 100_000, 999_999] {
        let (y, m, d) = civil_from_days(z);
        assert_eq!(days_from_civil(y, m, d), z, "roundtrip failed at z={z}");
    }
}

/// 2026-11-22T10:00:00Z, the instant most cases below are spelled around.
const TEN_O_CLOCK: i64 = 1_795_341_600_000;

#[test]
fn an_rfc3339_instant_reads_every_zone_spelling_and_applies_it() {
    let read = |text: &str| parse_rfc3339_ms(text, MissingZone::Refuse);
    assert_eq!(days_from_civil(2026, 11, 22) * 86_400_000 + 10 * 3_600_000, TEN_O_CLOCK);
    assert_eq!(read("2026-11-22T10:00:00Z"), Some(TEN_O_CLOCK));
    assert_eq!(read("2026-11-22t10:00:00z"), Some(TEN_O_CLOCK), "lower case, as RFC 3339 admits");
    assert_eq!(read("2026-11-22 10:00:00Z"), Some(TEN_O_CLOCK), "a space for the T");
    assert_eq!(read("  2026-11-22T10:00:00Z\n"), Some(TEN_O_CLOCK), "trimmed");
    // E8: the offset is applied, never ignored: a `-04:00` stamp is 4 h after the same UTC wall clock.
    for zone in ["+00:00", "-00:00", "+0000", "+00"] {
        assert_eq!(read(&format!("2026-11-22T10:00:00{zone}")), Some(TEN_O_CLOCK), "{zone}");
    }
    for zone in ["-04:00", "-0400", "-04"] {
        assert_eq!(read(&format!("2026-11-22T06:00:00{zone}")), Some(TEN_O_CLOCK), "{zone}");
    }
    assert_eq!(read("2026-11-22T15:30:00+05:30"), Some(TEN_O_CLOCK));
    assert_eq!(read("2026-11-22T15:30:00+0530"), Some(TEN_O_CLOCK));
    assert_eq!(read("2026-11-23T00:00:00+14:00"), Some(TEN_O_CLOCK), "across the date line");
    assert_eq!(read("1969-12-31T23:59:59.999Z"), Some(-1), "before the epoch");
    assert_eq!(read("2024-02-29T00:00:00Z"), Some(days_from_civil(2024, 2, 29) * 86_400_000));
}

#[test]
fn an_rfc3339_fraction_floors_to_the_millisecond() {
    let read = |text: &str| parse_rfc3339_ms(text, MissingZone::Refuse);
    assert_eq!(read("2026-11-22T10:00:00.5Z"), Some(TEN_O_CLOCK + 500));
    assert_eq!(read("2026-11-22T10:00:00.07Z"), Some(TEN_O_CLOCK + 70));
    assert_eq!(read("2026-11-22T10:00:00.123Z"), Some(TEN_O_CLOCK + 123));
    // E14: a nanosecond stamp a hair under the next millisecond stays in its own.
    assert_eq!(read("2026-11-22T10:00:00.123999999Z"), Some(TEN_O_CLOCK + 123));
    assert_eq!(read("2026-11-22T06:00:00.999999-04:00"), Some(TEN_O_CLOCK + 999));
    assert_eq!(read("1969-12-31T23:59:59.9999Z"), Some(-1), "the floor holds before the epoch");
}

#[test]
fn a_stamp_without_a_zone_follows_the_callers_policy() {
    assert_eq!(parse_rfc3339_ms("2026-11-22T10:00:00", MissingZone::ReadAsUtc), Some(TEN_O_CLOCK));
    assert_eq!(
        parse_rfc3339_ms("2026-11-22T10:00:00.250", MissingZone::ReadAsUtc),
        Some(TEN_O_CLOCK + 250)
    );
    assert_eq!(parse_rfc3339_ms("2026-11-22T10:00:00", MissingZone::Refuse), None);
    assert_eq!(parse_rfc3339_ms("2026-11-22T10:00:00.250", MissingZone::Refuse), None);
}

#[test]
fn an_rfc3339_stamp_out_of_shape_or_off_the_calendar_is_refused() {
    for text in [
        "",
        "2026-11-22",
        "2026-02-30T10:00:00Z",
        "2026-04-31T10:00:00Z",
        "2100-02-29T10:00:00Z",
        "2026-13-01T10:00:00Z",
        "2026-00-01T10:00:00Z",
        "2026-11-00T10:00:00Z",
        "2026-11-22T24:00:00Z",
        "2026-11-22T10:60:00Z",
        "2026-11-22T10:00:60Z",
        "2026-1-22T10:00:00Z",
        "26-11-22T10:00:00Z",
        "+026-11-22T10:00:00Z",
        "2026-11-22T1:00:00Z",
        "2026-11-22X10:00:00Z",
        "2026/11/22T10:00:00Z",
        "2026-11-22T10:00:00.Z",
        "2026-11-22T10:00:00.12aZ",
        "2026-11-22T10:00:00ZZ",
        "2026-11-22T10:00:00Z junk",
        "2026-11-22T10:00:00+24:00",
        "2026-11-22T10:00:00+05:60",
        "2026-11-22T10:00:00+5",
        "2026-11-22T10:00:00+053",
        "2026-11-22T10:00:00+05-30",
        "2026-11-22T10:00:00+05:3",
        "2026-11-22T10:00:00+",
        "2026-11-22T10:00:00 +05:00",
        "2026-11-22T10:00:00UTC",
        "2026-11-22T10:00:00\u{20ac}",
        "\u{20ac}026-11-22T10:00:00Z",
    ] {
        for policy in [MissingZone::ReadAsUtc, MissingZone::Refuse] {
            assert_eq!(parse_rfc3339_ms(text, policy), None, "{text:?} under {policy:?}");
        }
    }
}

#[test]
fn an_rfc3339_read_is_the_civil_inverse_over_every_day_of_a_leap_cycle() {
    for days in days_from_civil(2000, 1, 1)..days_from_civil(2004, 1, 1) {
        let (y, m, d) = civil_from_days(days);
        let text = format!("{y:04}-{m:02}-{d:02}T23:59:59.999Z");
        let last_millisecond = (days + 1) * 86_400_000 - 1;
        assert_eq!(parse_rfc3339_ms(&text, MissingZone::Refuse), Some(last_millisecond), "{text}");
    }
}

#[test]
fn epoch_seconds_read_exactly_and_floor_to_the_millisecond() {
    assert_eq!(parse_epoch_seconds_ms("1478012400.000000000"), Some(1_478_012_400_000));
    assert_eq!(parse_epoch_seconds_ms("1478012400.123456789"), Some(1_478_012_400_123));
    assert_eq!(parse_epoch_seconds_ms("1478012400.5"), Some(1_478_012_400_500));
    assert_eq!(parse_epoch_seconds_ms("1478012400"), Some(1_478_012_400_000));
    assert_eq!(parse_epoch_seconds_ms("1478012400."), Some(1_478_012_400_000));
    assert_eq!(parse_epoch_seconds_ms("1478012400.007"), Some(1_478_012_400_007));
    // E14: the float read this replaces can land a time a hair under a millisecond in the next
    // one; the integer read never does.
    assert_eq!(parse_epoch_seconds_ms("1478012400.006999999"), Some(1_478_012_400_006));
    assert_eq!(parse_epoch_seconds_ms("0"), Some(0));
    assert_eq!(parse_epoch_seconds_ms("9223372036854775.807"), Some(i64::MAX));
    for bad in [
        "",
        ".5",
        "-1478012400",
        "+1478012400",
        "1478012400.12a",
        "1478012400.1.2",
        " 1478012400",
        "2016-11-01T15:00:00.000000000Z",
        "9223372036854775.808",
        "9223372036854776",
        "99999999999999999999",
    ] {
        assert_eq!(parse_epoch_seconds_ms(bad), None, "{bad:?}");
    }
}
