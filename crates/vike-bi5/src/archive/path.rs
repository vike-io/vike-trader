//! The path grammar (§3.1): what a path under a dataset directory is, by its name alone.

use std::fmt;

use chrono::{Datelike, NaiveDate};
use vike_model::MS_PER_DAY;

/// A daily file's name after its two-digit day.
pub(super) const DAILY_SUFFIX: &str = "_ticks.bi5";
/// An hourly-layout file's name after its two-digit hour.
const HOURLY_SUFFIX: &str = "h_ticks.bi5";

// ── the path grammar (§3.1) ─────────────────────────────────────────────────────────────────────

/// A UTC day as the archive's PATH names it — the time base of every tick in that day's file.
///
/// Always a UTC midnight on or after 1970-01-01: built by [`classify_bi5_path`] or checked by
/// [`Bi5Day::from_start_ms`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Bi5Day {
    start_ms: i64,
}

impl Bi5Day {
    /// The day starting at `start_ms`, or `None` when that is not a UTC midnight on or after the
    /// epoch.
    pub fn from_start_ms(start_ms: i64) -> Option<Bi5Day> {
        let midnight = start_ms >= 0 && start_ms % MS_PER_DAY == 0;
        (midnight && chrono::DateTime::from_timestamp_millis(start_ms).is_some())
            .then_some(Bi5Day { start_ms })
    }

    /// Epoch-ms of the day's first millisecond (its UTC midnight).
    pub fn start_ms(self) -> i64 {
        self.start_ms
    }
}

impl fmt::Display for Bi5Day {
    /// The day as a calendar date, `2024-01-15` — the label a plan reports a day by.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match chrono::DateTime::from_timestamp_millis(self.start_ms) {
            Some(dt) => write!(f, "{:04}-{:02}-{:02}", dt.year(), dt.month(), dt.day()),
            // unreachable: `from_start_ms` refuses what chrono cannot represent
            None => write!(f, "{} ms", self.start_ms),
        }
    }
}

/// What one path under a dataset directory is, by the layout grammar alone. Nothing is opened to
/// decide it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bi5Path {
    /// `<YYYY>/<MM>/<DD>_ticks.bi5` — a daily tick file, imported.
    Daily(Bi5Day),
    /// `<YYYY>/<MM>/<DD>/<HH>h_ticks.bi5` — the HTTP datafeed's hourly layout: recognised and
    /// reported, NOT imported in v1.
    Hourly { day: Bi5Day, hour: u32 },
    /// Anything else — counted with its bytes, never opened.
    Other,
}

/// Classify one FILE path under a dataset directory. `rel` is the path's segments RELATIVE to the
/// dataset (`["2024", "00", "15_ticks.bi5"]`); `now_ms` bounds the year, which may not be later
/// than the current one. Names are matched exactly: `15_TICKS.BI5`, a one-digit day or month, or a
/// day file one level too deep is [`Bi5Path::Other`].
pub fn classify_bi5_path(rel: &[&str], now_ms: i64) -> Bi5Path {
    match rel {
        [year, month, file] => file
            .strip_suffix(DAILY_SUFFIX)
            .and_then(|day| layout_day(year, month, day, now_ms))
            .map_or(Bi5Path::Other, Bi5Path::Daily),
        [year, month, day, file] => {
            let hour = file.strip_suffix(HOURLY_SUFFIX).and_then(layout_hour);
            match (layout_day(year, month, day, now_ms), hour) {
                (Some(day), Some(hour)) => Bi5Path::Hourly { day, hour },
                _ => Bi5Path::Other,
            }
        }
        _ => Bi5Path::Other,
    }
}

/// Whether a DIRECTORY at `rel` (segments relative to the dataset, at least one) belongs to the
/// layout, so a walk may descend into it: a year, a year's month, or — the hourly layout's — a real
/// calendar day of that month. Any other directory is an unexpected name or depth, and a walk
/// skips it rather than reading below it.
pub fn is_bi5_layout_dir(rel: &[&str], now_ms: i64) -> bool {
    match rel {
        [year] => layout_year(year, now_ms).is_some(),
        [year, month] => layout_year(year, now_ms).is_some() && layout_month0(month).is_some(),
        [year, month, day] => layout_day(year, month, day, now_ms).is_some(),
        _ => false,
    }
}

/// `s` as a number, when it is exactly `width` ASCII digits.
fn fixed_digits(s: &str, width: usize) -> Option<u32> {
    if s.len() == width && s.bytes().all(|b| b.is_ascii_digit()) { s.parse().ok() } else { None }
}

/// A four-digit year from 1970 to `now_ms`'s year.
fn layout_year(s: &str, now_ms: i64) -> Option<i32> {
    let year = i32::try_from(fixed_digits(s, 4)?).ok()?;
    let current = chrono::DateTime::from_timestamp_millis(now_ms)?.year();
    (1970..=current).contains(&year).then_some(year)
}

/// A two-digit ZERO-BASED month, `00` (January) to `11` (December).
fn layout_month0(s: &str) -> Option<u32> {
    fixed_digits(s, 2).filter(|month0| *month0 <= 11)
}

/// A two-digit hour of the day, `00` to `23`.
fn layout_hour(s: &str) -> Option<u32> {
    fixed_digits(s, 2).filter(|hour| *hour <= 23)
}

/// The day a year, zero-based month and two-digit day name — when they form a real date.
fn layout_day(year: &str, month0: &str, day: &str, now_ms: i64) -> Option<Bi5Day> {
    let date = NaiveDate::from_ymd_opt(
        layout_year(year, now_ms)?,
        layout_month0(month0)? + 1,
        fixed_digits(day, 2)?,
    )?;
    Bi5Day::from_start_ms(date.and_hms_opt(0, 0, 0)?.and_utc().timestamp_millis())
}
