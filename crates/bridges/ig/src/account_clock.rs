//! **The IG demo account's clock** — the offset from UTC that `GET /prices`'s `from` and `to` are read in,
//! as a pure function of the instant. [`crate::klines`] asks every window by it and checks every answer
//! against it.
//!
//! # What was measured, and the rule it fits
//!
//! The demo account's clock is NOT a fixed number of minutes, and it is NOT a function of the instant
//! alone. Two days of measurement on the one demo account (id 13 on the CI box), each read off the rows' own
//! pair of stamps (`snapshotTime` local, `snapshotTimeUTC`):
//!
//! | measured on | an instant in British Summer Time | an instant outside it |
//! |---|---|---|
//! | 2026-10-24 (the UK in summer time) | 480 (2025-09-25..10-22, 2026-04-09..10-21) | 420 (2025-12-18..2026-01-14) |
//! | 2026-10-30 (the UK in winter time) | 540 (2026-07-14, 2026-10-22) | 480 (2026-01-13, 2026-10-26, 10-28, 10-30) |
//!
//! The same instants read one hour apart on the two days (2026-01-13 is a winter instant: 420 by the first
//! run's rule, 480 on 2026-10-30), and the UK clock changed between them (2026-10-25). What fits both days
//! is **the account's own offset plus the UK's summer hour of the instant, less the UK's summer hour of
//! the day it is asked** ([`offset_minutes_at`]): the account is [`BASE_MINUTES`] (8 hours, also the
//! login's `timezoneOffset`, 8 on both the login and `GET /session` on 2026-10-30) ahead of UTC at "now",
//! and an instant on the other side of a UK clock change from "now" reads an hour apart. The first run
//! took the summer-time day's figures for a rule of the instant ("London time plus seven hours"); that
//! rule is wrong on the winter-time day (it said 420 and 480 where IG says 480 and 540).
//!
//! ⚠ **Not yet measured:** the rule's reading for a request made on the other kind of day than the one it
//! was seen on is a PREDICTION (a summer-time day gives the first row of the table, which is exactly what
//! the first run saw; a winter-time day gives the second). The rule that the account moves at the UK's
//! change instants (the last Sunday of March at 01:00 UTC to the last Sunday of October at 01:00 UTC) is
//! the UK's, not a reading: no request was ever cut at one. If IG ever reads another way the lane fails
//! closed with both numbers (`klines.rs`'s `read_rows`) instead of storing a shifted window; the first
//! request after the next UK clock change (2027-03-28) is the next measurement, and the operator kit's
//! igclock script (read-only) reads it at any time.
//!
//! # What follows for the lane
//!
//! * The offset is **never state**: it is read off the instant a window starts at and the instant it is
//!   asked at, so a request is asked in the clock valid at it, whichever side of a change "now" is on.
//! * A window that holds a change is **cut at it** ([`parts`]): the wall clock repeats an hour at the
//!   October change and skips one at the March change, so no single offset can name such a window.
//! * Every answered row is **checked against the rule** (`klines.rs`'s `read_rows`), and a row that
//!   disagrees fails the request closed, with the numbers. A UK rule that moves, or an account on another
//!   clock, stops the lane instead of storing a shifted window.
//!
//! # How IG reads `from` and `to`, and what is still unobserved
//!
//! IG reads them at the offset of the instant they name (the reading the rule assumes), not at one fixed
//! offset: 359 stored hourly stamps across the 2026-03-29 change are hour-exact against FXCM on both sides
//! of it (2026-10-25), so `klines.rs`'s `IgKlines::fetch_until` asks every piece exactly its own window,
//! with no extra hour at either edge. On 2026-10-30 a two-hour window of local time
//! named for 2026-10-22 (summer time) and one for 2026-10-28 (winter time) were each answered in the offset
//! of the instant they named (540 and 480), so the reading holds on both sides of the October change.
//! NOT observed: an end edge, and a chunk that holds the change. ⚠ **If the first chunk across 2026-10-25
//! (an hourly chunk is stored only once it has ended, so after 2026-11-12) shows a hole, the hedge
//! returns:** the outer edges of a far-side piece asked an hour wider and cut back to the window.

use vike_util::calendar::{civil_from_days, days_from_civil};

const HOUR_MS: i64 = 3_600_000;
const DAY_MS: i64 = 24 * HOUR_MS;

/// The account's own offset from UTC at "now", in minutes: 8 hours, the login's `timezoneOffset` (measured 8 on
/// `POST /session` and `GET /session`, 2026-10-30) and the offset of every row of the day's own clock
/// regime. The UK's summer hour is added for an instant in summer time and taken away for a "now" in it
/// ([`offset_minutes_at`]).
const BASE_MINUTES: i32 = 480;

/// The first year the UK's present dates hold (the last Sunday of March to the last Sunday of October, in
/// force since 1996): an earlier instant has no rule here, and is refused rather than guessed.
const FIRST_YEAR: i64 = 1996;
/// The last year with four digits, the only years the wall clock the lane writes can name.
const LAST_YEAR: i64 = 9999;

/// The year of `utc_ms`.
fn year_of(utc_ms: i64) -> i64 {
    civil_from_days(utc_ms.div_euclid(DAY_MS)).0
}

/// The last Sunday of `month` (a month of 31 days) of `year` at 01:00 UTC, in epoch milliseconds.
fn last_sunday_0100_utc(year: i64, month: u32) -> i64 {
    let last = days_from_civil(year, month, 31);
    // 1970-01-01 was a Thursday: a day number plus 4, modulo 7, is 0 on a Sunday.
    (last - (last + 4).rem_euclid(7)) * DAY_MS + HOUR_MS
}

/// When British Summer Time begins and ends in `year`, in epoch milliseconds.
fn summer_time(year: i64) -> (i64, i64) {
    (last_sunday_0100_utc(year, 3), last_sunday_0100_utc(year, 10))
}

/// Whether British Summer Time is in force at `utc_ms` (the last Sunday of March 01:00 UTC, inclusive, to the
/// last Sunday of October 01:00 UTC, exclusive). `None` outside the years the rule is known for (1996 to 9999).
fn in_summer_time(utc_ms: i64) -> Option<bool> {
    let year = year_of(utc_ms);
    if !(FIRST_YEAR..=LAST_YEAR).contains(&year) {
        return None;
    }
    let (begins, ends) = summer_time(year);
    Some((begins..ends).contains(&utc_ms))
}

/// **The account's offset from UTC at the instant `utc_ms`, as the account reads a request made at
/// `now_ms`, in minutes**: [`BASE_MINUTES`], plus an hour when `utc_ms` is in British Summer Time, less an
/// hour when `now_ms` is. So on a winter-time day an instant in winter time is 480 and one in summer time 540;
/// on a summer-time day the same instants are 420 and 480 (the module doc's table is the measurement).
/// `None` outside the years the rule is known for (1996 to 9999), for either instant.
pub(crate) fn offset_minutes_at(utc_ms: i64, now_ms: i64) -> Option<i32> {
    let (at, now) = (in_summer_time(utc_ms)?, in_summer_time(now_ms)?);
    Some(BASE_MINUTES + 60 * (i32::from(at) - i32::from(now)))
}

/// One piece of a window that holds no clock change: asked in one offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Part {
    /// The first millisecond of the piece (inclusive).
    pub(crate) start_ms: i64,
    /// The last millisecond of the piece (inclusive).
    pub(crate) end_ms: i64,
    /// The account's offset over the whole piece, in minutes.
    pub(crate) offset_minutes: i32,
}

/// **The window `[start_ms, end_ms]` cut at every clock change inside it**, in order, each piece starting at
/// the instant of the change it follows (the new offset's first millisecond) and the piece before it ending
/// one millisecond earlier. One piece for a window with no change. `None` for an inverted window or one
/// outside the years [`offset_minutes_at`] knows. `now_ms` is the instant the request is made at.
pub(crate) fn parts(start_ms: i64, end_ms: i64, now_ms: i64) -> Option<Vec<Part>> {
    let (first, last) = (year_of(start_ms), year_of(end_ms));
    if end_ms < start_ms || first < FIRST_YEAR || last > LAST_YEAR {
        return None;
    }
    let mut cuts = Vec::new();
    for year in first..=last {
        let (begins, ends) = summer_time(year);
        cuts.extend([begins, ends].into_iter().filter(|cut| *cut > start_ms && *cut <= end_ms));
    }
    let mut pieces = Vec::with_capacity(cuts.len() + 1);
    let mut from = start_ms;
    for cut in cuts {
        pieces.push(Part {
            start_ms: from,
            end_ms: cut - 1,
            offset_minutes: offset_minutes_at(from, now_ms)?,
        });
        from = cut;
    }
    pieces.push(Part { start_ms: from, end_ms, offset_minutes: offset_minutes_at(from, now_ms)? });
    Some(pieces)
}

#[cfg(test)]
#[path = "account_clock_tests.rs"]
mod tests;
