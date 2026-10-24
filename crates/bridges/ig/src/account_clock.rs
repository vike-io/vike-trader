//! **The IG demo account's clock** — the offset from UTC that `GET /prices`'s `from` and `to` are read in,
//! as a pure function of the instant. [`crate::klines`] asks every window by it and checks every answer
//! against it.
//!
//! # What was measured, and the rule it fits
//!
//! The demo account's clock is NOT a fixed +480 minutes. The 2026-10-24 acceptance run on the CI box (v0.1.84)
//! found it 480 on 2025-09-25..2025-10-22, 420 on the winter side of the UK clock change of 2025-10-26
//! (2025-12-18..2026-01-14) and 480 again on 2026-04-09..2026-10-21: **London time plus seven hours**, so
//! 480 in British Summer Time and 420 outside it. The UK rule is the last Sunday of March at 01:00 UTC to
//! the last Sunday of October at 01:00 UTC ([`offset_minutes_at`]). That the account moves at those exact
//! instants is the rule's, not a reading: no request was ever cut at one.
//!
//! # What follows for the lane
//!
//! * The offset is **never state**: it is read off the instant a window starts at, so a request is asked in
//!   the clock valid at it, whichever side of a change "now" is on.
//! * A window that holds a change is **cut at it** ([`parts`]): the wall clock repeats an hour at the
//!   October change and skips one at the March change, so no single offset can name such a window.
//! * Every answered row is **checked against the rule** (`klines.rs`'s `read_rows`), and a row that
//!   disagrees fails the request closed, with the numbers. A UK rule that moves, or an account on another
//!   clock, stops the lane instead of storing a shifted window.
//!
//! # NOT MEASURED
//!
//! Whether IG reads `from` and `to` at the offset of the instant they name (the reading the rule assumes) or
//! at one fixed offset. The two differ by [`CHANGE_MS`] at most, so the lane asks a far-side window that
//! much wider at its OUTER edges and keeps only the bars inside the window
//! (`klines.rs`'s `IgKlines::fetch_until`), which is right under either reading.

use vike_util::calendar::{civil_from_days, days_from_civil};

const HOUR_MS: i64 = 3_600_000;
const DAY_MS: i64 = 24 * HOUR_MS;

/// The account clock in British Summer Time, in minutes: London's one hour plus seven.
const SUMMER_MINUTES: i32 = 480;
/// The account clock outside British Summer Time, in minutes: Greenwich time plus seven hours.
const WINTER_MINUTES: i32 = 420;

/// The size of a clock change: the most two readings of the clock differ by.
pub(crate) const CHANGE_MS: i64 = HOUR_MS;

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

/// **The account's offset from UTC at `utc_ms`, in minutes**: 480 from the last Sunday of March 01:00 UTC
/// (inclusive) to the last Sunday of October 01:00 UTC (exclusive), else 420. `None` outside the years the
/// rule is known for (1996 to 9999).
pub(crate) fn offset_minutes_at(utc_ms: i64) -> Option<i32> {
    let year = year_of(utc_ms);
    if !(FIRST_YEAR..=LAST_YEAR).contains(&year) {
        return None;
    }
    let (begins, ends) = summer_time(year);
    Some(if (begins..ends).contains(&utc_ms) { SUMMER_MINUTES } else { WINTER_MINUTES })
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
/// outside the years [`offset_minutes_at`] knows.
pub(crate) fn parts(start_ms: i64, end_ms: i64) -> Option<Vec<Part>> {
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
            offset_minutes: offset_minutes_at(from)?,
        });
        from = cut;
    }
    pieces.push(Part { start_ms: from, end_ms, offset_minutes: offset_minutes_at(from)? });
    Some(pieces)
}

#[cfg(test)]
#[path = "account_clock_tests.rs"]
mod tests;
