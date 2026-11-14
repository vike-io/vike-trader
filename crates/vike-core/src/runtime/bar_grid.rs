//! A bar series' GRID under its market's trading calendar (decision 0271): which bar periods hold a bar, as
//! the series a venue stores them in does. The inputs read it to know the bar an input should get next and
//! when its market's open time ran past it (`runtime/inputs.rs`), and a restart reads it to count the bar a
//! journaled fill fell in from the `Snapshot`'s bar clock (`runtime/closed_trades.rs`).
//!
//! A bar opens on the grid of its length: an intraday bar every `interval_ms` from a UTC-aligned open, a bar of a
//! day or longer at its market's civil roll (`vike_venues::SessionCalendar::civil_after`: the FX day at 17:00 New
//! York in both seasons). A period holds a bar when the market traded at some minute of it
//! (`SessionCalendar::is_open_civil`), so the market's closed hours hold none. Without a calendar (a market open
//! every minute, or one whose class no kept row states) every period holds a bar. Holidays are not in the calendar: a holiday's
//! periods count as traded. Cold: once per bar of an input, once per fill of a restart's tail.

use vike_venues::SessionCalendar;

/// The open of the bar after the one opened at `open`, `interval_ms` long, on `calendar`'s civil clock.
fn step(open: i64, interval_ms: i64, calendar: Option<&SessionCalendar>) -> i64 {
    match calendar {
        Some(c) => c.civil_after(open, interval_ms),
        None => open.saturating_add(interval_ms),
    }
}

/// The open of the first period at or after the one opened at `period` in which the market trades: `period`
/// itself without a calendar or while the market trades in it, else the period that holds its next open.
fn first_traded(period: i64, interval_ms: i64, calendar: Option<&SessionCalendar>) -> i64 {
    let Some(c) = calendar else { return period };
    let end = step(period, interval_ms, calendar);
    let Some(open) = c.next_open_civil(period) else { return period };
    if open < end {
        return period;
    }
    if interval_ms < vike_model::MS_PER_DAY {
        // UTC-aligned: jump straight to the period that holds the open.
        return period.saturating_add((open - period).div_euclid(interval_ms) * interval_ms);
    }
    let mut p = period;
    while step(p, interval_ms, calendar) <= open {
        p = step(p, interval_ms, calendar);
    }
    p
}

/// **The bar a series whose newest bar opened at `last` should get next**, `(open, close)`: the bar of the first
/// period after it that the market trades in. A shared close waits for its close, and a bar that arrives opened
/// past its open shows a hole.
pub(super) fn next_bar(
    last: i64,
    interval_ms: i64,
    calendar: Option<&SessionCalendar>,
) -> (i64, i64) {
    let open = first_traded(step(last, interval_ms, calendar), interval_ms, calendar);
    (open, step(open, interval_ms, calendar))
}

/// **The instant from which a series whose newest bar CLOSED at `close` is STALE**: the plain rule (two of its
/// bars after the close) without a calendar; with one, once the market's OPEN time since the close passed two of
/// its bars (closed hours do not count, and a bar that is only late holds nothing before that). Walked a minute at
/// a time over the open time and jumped over the closed time.
pub(super) fn stale_from(close: i64, interval_ms: i64, calendar: Option<&SessionCalendar>) -> i64 {
    let margin = interval_ms.saturating_mul(2);
    let Some(c) = calendar else { return close.saturating_add(margin) };
    const MINUTE: i64 = 60_000;
    let (mut t, mut open_ms) = (close, 0_i64);
    while open_ms < margin {
        if c.is_open_civil(t) {
            open_ms += MINUTE;
            t += MINUTE;
        } else {
            match c.next_open_civil(t) {
                Some(next) if next > t => t = next,
                _ => return i64::MAX,
            }
        }
    }
    t
}

/// **The index of the bar a fill at `ts` falls in**, counted from a series' newest closed bar (opened at
/// `clock_ts`, index `clock_index`): `clock_index` plus every period after it, up to the one `ts` falls in, that
/// the market traded in (a series stores no bar for a closed period). What `closed_trades.rs`'s `bar_of` counts over
/// the series itself, without it.
pub(super) fn bar_at(
    (clock_ts, clock_index): (i64, usize),
    interval_ms: i64,
    calendar: Option<&SessionCalendar>,
    ts: i64,
) -> usize {
    if interval_ms <= 0 || ts < clock_ts {
        return clock_index;
    }
    if calendar.is_none() {
        let periods = usize::try_from((ts - clock_ts).div_euclid(interval_ms)).unwrap_or(0);
        return clock_index.saturating_add(periods);
    }
    let mut index = clock_index;
    let mut p = clock_ts;
    loop {
        let next = first_traded(step(p, interval_ms, calendar), interval_ms, calendar);
        if next > ts {
            return index;
        }
        index += 1;
        p = next;
    }
}
