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
//! What is built ON these two — the `date=` partition key, the RFC-3339 instants, the label
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

#[cfg(test)]
#[path = "calendar_tests.rs"]
mod tests;
