// Seconds-since-the-epoch → RFC-3339 UTC, with no dependency and no `unsafe`.
//
// ⚠ **`//` and not the house `//!`, and that is forced.** `crates/vike-buildinfo/build.rs`
// `include!`s this file, and an inner doc comment is only legal before the first item of the
// including file — where build.rs's own `//!` header already sits. A plain comment block says the
// same thing and compiles in both places.
//
// ⚠ **This file is compiled TWICE, on purpose.** The build script includes it to stamp
// `BUILD_TIMESTAMP` into the generated constants, and `crates/vike-buildinfo/src/lib.rs` compiles
// it as a normal module and re-exports `iso8601_utc`. That is what makes the conversion TESTABLE: a
// build script's `#[cfg(test)]` code is never compiled as a test by anything, so a calendar routine
// living only in `build.rs` would ship unexercised — and a wrong build timestamp is exactly the
// defect nobody notices until they are reading it during an incident.
// `crates/vike-buildinfo/src/lib.rs`'s `the_two_timestamp_constants_are_one_instant` closes the
// loop by re-deriving the shipped string from the shipped epoch.
//
// The date arithmetic is Howard Hinnant's `civil_from_days` (the algorithm behind `<chrono>`'s
// `year_month_day`), proleptic Gregorian, era-based. Leap seconds do not exist in Unix time, so the
// wall clock is `secs % 86400` with no table to keep current.

/// `1970-01-01T00:00:00Z`-style RFC-3339, at second resolution, always UTC.
///
/// UTC and never local time: this string is read off a binary running on a server in one timezone
/// by a person in another, and a naive local timestamp would be two facts (an instant and an
/// unstated offset) wearing the shape of one.
pub fn iso8601_utc(epoch_secs: u64) -> String {
    let days = (epoch_secs / 86_400) as i64;
    let rem = epoch_secs % 86_400;
    let (hh, mm, ss) = (rem / 3_600, (rem % 3_600) / 60, rem % 60);
    let (y, mo, d) = civil_from_days(days);
    format!("{y:04}-{mo:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z")
}

/// Days since `1970-01-01` → `(year, month, day)` in the proleptic Gregorian calendar.
///
/// Hinnant's era algorithm: shift the epoch to `0000-03-01` so that a leap day lands at the END of
/// a year and the 400-year era becomes a fixed 146 097 days, then invert. Verbatim in structure —
/// re-deriving it by hand is how off-by-one-day bugs are written.
fn civil_from_days(z: i64) -> (i64, u64, u64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11], March = 0
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    (y + i64::from(m <= 2), m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Anchors, not a round trip: a round trip against the same arithmetic proves only that it is
    /// self-consistent. Each of these is an independently known instant.
    #[test]
    fn known_instants_render_exactly() {
        assert_eq!(iso8601_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso8601_utc(1), "1970-01-01T00:00:01Z");
        assert_eq!(iso8601_utc(86_399), "1970-01-01T23:59:59Z");
        assert_eq!(iso8601_utc(86_400), "1970-01-02T00:00:00Z");
        assert_eq!(iso8601_utc(1_000_000_000), "2001-09-09T01:46:40Z");
        assert_eq!(iso8601_utc(1_700_000_000), "2023-11-14T22:13:20Z");
    }

    /// The two leap-year rules that a naive `% 4` gets wrong in opposite directions: 2000 IS a leap
    /// year (divisible by 400) and 1900 is NOT (divisible by 100 but not 400).
    #[test]
    fn the_century_leap_rules_hold_in_both_directions() {
        assert_eq!(iso8601_utc(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(iso8601_utc(951_868_800), "2000-03-01T00:00:00Z");
        assert_eq!(iso8601_utc(1_709_164_800), "2024-02-29T00:00:00Z");
        // 1900 predates the epoch, so the non-leap century is reached forwards instead:
        // 2100-03-01 follows 2100-02-28 directly, with no 29th between them.
        assert_eq!(iso8601_utc(4_107_456_000), "2100-02-28T00:00:00Z");
        assert_eq!(iso8601_utc(4_107_542_400), "2100-03-01T00:00:00Z");
    }

    /// Every field is zero-padded to its RFC-3339 width — a `2026-8-9T7:5:3Z` parses nowhere.
    #[test]
    fn every_field_is_zero_padded() {
        let s = iso8601_utc(1_767_227_103); // 2026-01-01T00:25:03Z
        assert_eq!(s.len(), 20, "{s}");
        assert!(s.ends_with('Z'), "{s}");
        assert_eq!(&s[4..5], "-");
        assert_eq!(&s[10..11], "T");
    }
}
