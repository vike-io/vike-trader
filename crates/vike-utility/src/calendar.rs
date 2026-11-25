//! The UTC civil calendar's two primitives — Howard Hinnant's proleptic-Gregorian algorithms
//! (<http://howardhinnant.github.io/date_algorithms.html>): integer-exact, chrono-free, valid
//! across the full `i64` range.
//!
//! This is the single home for the day ⇄ date conversion that was previously re-derived per
//! crate — the venue bridges (`ig`, `polymarket`), the backtest schedule/harness, and the
//! collectors each carried a hand copy of `days_from_civil`/`civil_from_days`. One home removes the
//! divergence foot-gun (a fix in one copy silently skewing a partition key or resolution timestamp
//! in another).
//!
//! Two READS of a venue's or an operator's time text sit beside them, one home each so no copy can
//! drift: [`parse_rfc3339_ms`] (an RFC 3339 instant, its zone applied, a day the calendar lacks
//! refused) and [`parse_epoch_seconds_ms`] (a decimal epoch-seconds string, by integer arithmetic).
//! Both FLOOR a sub-millisecond fraction, so a stamp never lands a millisecond later than it says.
//!
//! What is built ON these — the `date=` partition key, the RFC-3339 spellings, the label
//! parsers, calendar-month arithmetic — is `vike_model::time`'s.

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

/// Whether the proleptic Gregorian calendar has this day: a month in `1..=12` and a day that month
/// holds. [`days_from_civil`] would roll 31 February into March, so a parser asks here first. Total
/// over every input, an operator's absurd year included.
pub fn is_real_day(y: i64, m: u32, d: u32) -> bool {
    let last = match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if y.rem_euclid(4) == 0 && (y.rem_euclid(100) != 0 || y.rem_euclid(400) == 0) => 29,
        2 => 28,
        _ => return false,
    };
    (1..=last).contains(&d)
}

/// What [`parse_rfc3339_ms`] does with a stamp that carries no zone designator, the caller's
/// policy: RFC 3339 requires one, and a venue that leaves it out documents what it meant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MissingZone {
    /// Read the stamp as UTC: a field the venue documents as UTC and writes bare (IG's
    /// `snapshotTimeUTC`).
    ReadAsUtc,
    /// Refuse the stamp (`None`): text whose zone nobody stated is no instant.
    Refuse,
}

/// An RFC 3339 instant → epoch ms: `YYYY-MM-DD`, then `T` (or `t`, or a space, which RFC 3339's
/// note admits), then `HH:MM:SS`, an optional `.` fraction of any length, then the zone: `Z` (or
/// `z`), `±HH:MM`, `±HHMM` or `±HH`, or none, decided by `missing_zone`. The zone is APPLIED: a
/// `-04:00` stamp is 4 hours after the same wall clock in UTC.
///
/// Exact by integer arithmetic, and the fraction past its third digit is dropped, so the result
/// FLOORS to the millisecond. Every field is its fixed count of ASCII digits; a day the calendar
/// does not have (`02-30`, see [`is_real_day`]), an hour past 23, a minute or second past 59 (the
/// leap second `60` included: an epoch count has no slot for it), an offset past `23:59` and any
/// text after the zone are `None`, never rolled into a nearby instant. Surrounding whitespace is
/// trimmed. Total over every input: the 4-digit year keeps the arithmetic far inside `i64`.
pub fn parse_rfc3339_ms(text: &str, missing_zone: MissingZone) -> Option<i64> {
    let bytes = text.trim().as_bytes();
    if bytes.len() < 19
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || !matches!(bytes[10], b'T' | b't' | b' ')
        || bytes[13] != b':'
        || bytes[16] != b':'
    {
        return None;
    }
    let (year, month, day) = (digits(&bytes[0..4])?, digits(&bytes[5..7])?, digits(&bytes[8..10])?);
    let (hour, minute, second) =
        (digits(&bytes[11..13])?, digits(&bytes[14..16])?, digits(&bytes[17..19])?);
    // Two-digit fields: `as u32` is exact.
    if !is_real_day(year, month as u32, day as u32) || hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    let mut rest = &bytes[19..];
    let mut millis = 0;
    if let [b'.', fraction @ ..] = rest {
        let count = fraction.iter().take_while(|c| c.is_ascii_digit()).count();
        if count == 0 {
            return None;
        }
        millis = millis_of_fraction(&fraction[..count]);
        rest = &fraction[count..];
    }
    let offset_minutes = match rest {
        [] => match missing_zone {
            MissingZone::ReadAsUtc => 0,
            MissingZone::Refuse => return None,
        },
        [b'Z' | b'z'] => 0,
        [sign @ (b'+' | b'-'), zone @ ..] => {
            let (hours, minutes) = match zone {
                [_, _] => (digits(zone)?, 0),
                [_, _, _, _] => (digits(&zone[..2])?, digits(&zone[2..])?),
                [_, _, b':', _, _] => (digits(&zone[..2])?, digits(&zone[3..])?),
                _ => return None,
            };
            if hours > 23 || minutes > 59 {
                return None;
            }
            let magnitude = hours * 60 + minutes;
            if *sign == b'-' { -magnitude } else { magnitude }
        }
        _ => return None,
    };
    let days = days_from_civil(year, month as u32, day as u32);
    let wall_clock_ms = (((days * 24 + hour) * 60 + minute) * 60 + second) * 1000 + millis;
    Some(wall_clock_ms - offset_minutes * 60_000)
}

/// A decimal epoch-seconds string (`"1478012400.000000000"`, OANDA's `time` under
/// `Accept-Datetime-Format: UNIX`) → epoch ms, by INTEGER arithmetic: the whole seconds and the
/// first three fractional digits, the rest dropped. The result is exact by construction and FLOORS
/// sub-millisecond digits, where a float parse (`(secs * 1000.0) as i64`) can round a time just
/// under a millisecond boundary UP to the next one.
///
/// `None` for anything that is not unsigned digits with an optional fractional part — an RFC 3339
/// spelling included, so a venue that ignored the format header fails the read rather than
/// stamping zeros — and for a count of seconds whose milliseconds overflow `i64`.
pub fn parse_epoch_seconds_ms(time: &str) -> Option<i64> {
    let (seconds, fraction) = time.split_once('.').unwrap_or((time, ""));
    let all_digits = |field: &str| field.bytes().all(|c| c.is_ascii_digit());
    if seconds.is_empty() || !all_digits(seconds) || !all_digits(fraction) {
        return None;
    }
    let seconds: i64 = seconds.parse().ok()?;
    seconds.checked_mul(1000)?.checked_add(millis_of_fraction(fraction.as_bytes()))
}

/// The milliseconds a run of fraction-of-a-second DIGITS holds: its first three, a short run padded
/// with zeros, the rest dropped (the floor).
fn millis_of_fraction(fraction: &[u8]) -> i64 {
    fraction
        .iter()
        .chain(std::iter::repeat(&b'0'))
        .take(3)
        .fold(0, |accumulator, d| accumulator * 10 + i64::from(d - b'0'))
}

/// A non-empty run of ASCII digits as its number; `None` for any other byte or an empty field.
/// Callers pass at most 4 bytes, so it cannot overflow.
fn digits(field: &[u8]) -> Option<i64> {
    if field.is_empty() {
        return None;
    }
    field.iter().try_fold(0i64, |accumulator, &c| {
        c.is_ascii_digit().then(|| accumulator * 10 + i64::from(c - b'0'))
    })
}

#[cfg(test)]
#[path = "calendar_tests.rs"]
mod tests;
