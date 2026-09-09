//! Pure UTC civil-calendar ⇄ epoch conversions — Howard Hinnant's proleptic-Gregorian
//! algorithms (<http://howardhinnant.github.io/date_algorithms.html>): integer-exact,
//! chrono-free, valid across the full `i64` range.
//!
//! This is the single home for calendar math that was previously re-derived per crate — the
//! venue bridges (`ig`, `polymarket`), the backtest schedule/harness, and the collectors each
//! carried a hand copy of `days_from_civil`/`civil_from_days`. Consolidating here removes the
//! divergence foot-gun (a fix in one copy silently skewing a partition key or resolution
//! timestamp in another) and gives every layer a lower-than-everyone home to reach for.

/// Days since 1970-01-01 → `(year, month, day)` (Howard Hinnant's `civil_from_days`,
/// proleptic Gregorian). Valid for the full `i64` day range.
pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468; // shift epoch to 0000-03-01
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // day-of-era [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // year-of-era [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // day-of-year (Mar-based) [0, 365]
    let mp = (5 * doy + 2) / 153; // month-of-year (Mar=0) [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // day [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    (y + if m <= 2 { 1 } else { 0 }, m as u32, d)
}

/// `(year, month, day)` → days since 1970-01-01 (Howard Hinnant's `days_from_civil`,
/// proleptic Gregorian). The exact inverse of [`civil_from_days`].
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let mp = if m > 2 { m - 3 } else { m + 9 } as i64; // month index, Mar-based [0, 11]
    let doy = (153 * mp + 2) / 5 + d as i64 - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146_097 + doe - 719_468
}

/// Epoch-ms (UTC) → `"YYYY-MM-DD"`. Floors to the UTC day; pre-1970 (negative) ms floor toward
/// -inf so the last millisecond of a day and the first of the next never share a bucket
/// (e.g. `-1` → `1969-12-31`, `0` → `1970-01-01`). This is the `date=` partition key the
/// DataFusion hist store splits on.
pub fn epoch_ms_to_utc_date(ms: i64) -> String {
    let (y, m, d) = civil_from_days(ms.div_euclid(86_400_000));
    format!("{y:04}-{m:02}-{d:02}")
}

/// Epoch-ms (UTC) floored to UTC-midnight of its own calendar day — same flooring semantics as
/// [`epoch_ms_to_utc_date`] (pre-1970 floors toward -inf), returning the numeric midnight instant
/// instead of a `"YYYY-MM-DD"` label. The home for any `1d`-interval consolidator that must store
/// its bar `ts` UTC-midnight-floored (e.g. the EOD collector normalizing a source's market-open
/// timestamp) rather than re-deriving the divide-by-day-length by hand at the call site.
pub fn floor_ms_to_utc_day(ms: i64) -> i64 {
    ms.div_euclid(86_400_000) * 86_400_000
}

/// Epoch-ms (UTC) → ISO weekday index, **Monday = 0** through Sunday = 6. Built on the same
/// `div_euclid` day floor as [`floor_ms_to_utc_day`], so pre-1970 timestamps land on the correct
/// weekday rather than skewing by a day.
///
/// The `+ 3` anchor is 1970-01-01 being a **Thursday** (index 3) — pinned by a test that derives it
/// from [`days_from_civil`] rather than trusting the constant. The single home for the weekday math
/// [`crate::session`] needs to map a timestamp onto the trading week.
pub fn utc_weekday(ms: i64) -> u32 {
    (ms.div_euclid(86_400_000) + 3).rem_euclid(7) as u32
}

/// Epoch-ns (UTC) → `"YYYY-MM-DD"`. Same flooring semantics as [`epoch_ms_to_utc_date`] at
/// nanosecond granularity (used by the Polymarket raw-frame tap, whose `local_ns` receive
/// stamps drive the daily gzip rollover).
pub fn epoch_ns_to_utc_date(ns: i64) -> String {
    let (y, m, d) = civil_from_days(ns.div_euclid(86_400_000_000_000));
    format!("{y:04}-{m:02}-{d:02}")
}

/// Parse a `YYYY-MM-DDTHH` hour label into `(year, month, day, hour)`, validating that month,
/// day, and hour fall in range (`1..=12`, `1..=31`, `0..24`). `None` on anything malformed —
/// the strict validation prevents an out-of-range field from being handed to
/// [`days_from_civil`] as garbage.
pub fn parse_hour_label(s: &str) -> Option<(i64, u32, u32, u32)> {
    let (date, hh) = s.split_once('T')?;
    let mut parts = date.splitn(3, '-');
    let y: i64 = parts.next()?.parse().ok()?;
    let m: u32 = parts.next()?.parse().ok()?;
    let d: u32 = parts.next()?.parse().ok()?;
    let h: u32 = hh.parse().ok()?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) || !(0..24).contains(&h) {
        return None;
    }
    Some((y, m, d, h))
}

/// Parse a vike interval string (`"30s"`/`"1m"`/`"5m"`/`"4h"`/`"1d"`) into epoch-**milliseconds**:
/// an integer count followed by exactly one unit char — `s`/`m`/`h`/`d`. Returns `None` for
/// anything malformed (fewer than 2 chars, empty or non-digit count, unknown unit).
///
/// The single home for the interval vocabulary that was re-parsed — divergently — in three places
/// (`vike-backtest`'s `parse_timeframe` rejected seconds, `vike-data`'s took `s`/`m`/`h`/`d`,
/// `vike-ibkr`'s took a fixed whitelist), so a `"30s"` series that loaded fine from the hist store
/// could not be named as a backtest timeframe. Every previously-accepted string maps to the exact
/// same value here, so delegating is a safe monotonic widening.
pub fn interval_ms(interval: &str) -> Option<i64> {
    if interval.len() < 2 {
        return None;
    }
    let (num, unit) = interval.split_at(interval.len() - 1);
    if num.is_empty() || !num.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n: i64 = num.parse().ok()?;
    let mult = match unit {
        "s" => 1_000,
        "m" => 60_000,
        "h" => 3_600_000,
        "d" => 86_400_000,
        _ => return None,
    };
    Some(n * mult)
}

/// Every calendar day from `start` to `end_inclusive` (both `(year, month, day)`), ascending.
/// Built directly over [`days_from_civil`]/[`civil_from_days`] so it can never diverge from the
/// store's `date=` partition-key math — the reason this module exists. Empty when
/// `start > end_inclusive`.
///
/// The single home for the day-range iterator the collectors re-derived (vike-backfill's tardis
/// adapter hand-rolled its own `days_in_month` leap-year loop). Callers that key on
/// `(i32, u32, u32)` day tuples cast the year at the boundary (`y as i64` / `y as i32`).
pub fn days_in_range(
    start: (i64, u32, u32),
    end_inclusive: (i64, u32, u32),
) -> Vec<(i64, u32, u32)> {
    let s = days_from_civil(start.0, start.1, start.2);
    let e = days_from_civil(end_inclusive.0, end_inclusive.1, end_inclusive.2);
    (s..=e).map(civil_from_days).collect()
}

/// Every UTC hour label `"YYYY-MM-DDTHH"` from `from` to `to` **inclusive**, ascending. Both bounds
/// are `YYYY-MM-DDTHH` labels ([`parse_hour_label`]); day/month/year rollovers fold naturally
/// through the hours-since-epoch arithmetic. Empty on an unparsable bound or when `from` is after
/// `to`. The hour-granularity sibling of [`days_in_range`] (the pmxt archive backfill's hour walk).
pub fn hours_in_range(from: &str, to: &str) -> Vec<String> {
    let (Some((fy, fm, fd, fh)), Some((ty, tm, td, th))) =
        (parse_hour_label(from), parse_hour_label(to))
    else {
        return Vec::new();
    };
    let start_hours = days_from_civil(fy, fm, fd) * 24 + fh as i64;
    let end_hours = days_from_civil(ty, tm, td) * 24 + th as i64;
    if start_hours > end_hours {
        return Vec::new();
    }
    (start_hours..=end_hours)
        .map(|total_h| {
            let (days, h) = (total_h.div_euclid(24), total_h.rem_euclid(24));
            let (y, m, d) = civil_from_days(days);
            format!("{y:04}-{m:02}-{d:02}T{h:02}")
        })
        .collect()
}

/// Split a `YYYY-MM-DD` UTC date into a validated `(year, month, day)` tuple, range-checking month
/// (`1..=12`) and day (`1..=31`) before any calendar math so an out-of-range field can't be handed
/// to [`days_from_civil`] as garbage. `Err(String)` (a CLI-printable reason) on the wrong field
/// count or a non-numeric/out-of-range field.
///
/// The tuple-returning sibling of [`parse_date_label`], for callers that iterate calendar days
/// (the tardis day-span backfill) rather than needing a single epoch-ms instant.
pub fn parse_ymd(s: &str) -> Result<(i64, u32, u32), String> {
    let s = s.trim();
    let parts: Vec<&str> = s.split('-').collect();
    if parts.len() != 3 {
        return Err(format!("bad date {s:?} (want epoch-ms or YYYY-MM-DD)"));
    }
    let y: i64 = parts[0].parse().map_err(|_| format!("bad year in {s:?}"))?;
    let m: u32 = parts[1].parse().map_err(|_| format!("bad month in {s:?}"))?;
    let d: u32 = parts[2].parse().map_err(|_| format!("bad day in {s:?}"))?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return Err(format!("out-of-range month/day in {s:?}"));
    }
    Ok((y, m, d))
}

/// Parse a CLI `--start`/`--end`/`--date` label into epoch-**milliseconds** (UTC). A bare integer
/// is taken verbatim as epoch-ms (any sign — pre-1970 allowed); otherwise the string must be a
/// `YYYY-MM-DD` UTC date ([`parse_ymd`]), which maps to that day's midnight-UTC epoch-ms via
/// [`days_from_civil`]. `Err(String)` carries a human-readable reason for the CLI to print.
///
/// The single home for the epoch-ms-or-date arg parse the backfill collectors each hand-rolled —
/// hyperliquid candles + funding carried byte-identical copies, and ibkr a `time`-crate variant
/// (dedup finding A11); consolidating here also drops vike-backfill's direct `time` dependency.
pub fn parse_date_label(s: &str) -> Result<i64, String> {
    let s = s.trim();
    if let Ok(ms) = s.parse::<i64>() {
        return Ok(ms);
    }
    let (y, m, d) = parse_ymd(s)?;
    Ok(days_from_civil(y, m, d) * 86_400_000)
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
