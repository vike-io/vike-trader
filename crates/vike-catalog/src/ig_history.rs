//! **IG's history vocabulary, for the two places that must agree on it**: the data daemon, which links
//! no bridge and refuses and sizes a request before it asks the gatewayhub, and the IG bridge, which
//! refuses a symbol that cannot be an epic before it logs in.
//!
//! Three facts live here and nowhere else:
//!
//! * [`is_ig_epic`] — the one spelling a series is stored under (IG's own, upper case, five dotted
//!   segments). A second spelling of one instrument would be stored as a second series, and nothing
//!   downstream would notice.
//! * [`IG_HISTORY_STEPS`] — the twelve bar intervals IG's `GET /prices/{epic}` serves with a
//!   measurable step, each with the DEPTH IG documents for it (an indication, not a guarantee) and
//!   the CHUNK WIDTH in whole UTC days the credentialed ingest fetches and stores it at.
//! * [`ig_history_step`] — the lookup.
//!
//! # ⚠ The chunk width is part of a series' key identity
//!
//! `vike_backfill`'s chunked ingest keys a chunk by its grid bounds, so the same bars fetched at one
//! width and later at another are different batches to a store that dedups by commit key, and every
//! bar they share is stored twice. The widths below are therefore PINNED
//! (`crate::ig_history_tests`'s `the_chunk_width_table_is_pinned`) and change only with the series
//! deleted and fetched again.
//!
//! The rule that produced them, kept so a reader can check a row and so the pin test can recompute
//! it: a chunk holds at most about [`TARGET_ROWS_A_CHUNK`] calendar-day bars (a chunk's rows are
//! its cost in points, and a chunk is fetched, stored and refused as a unit), never more than
//! [`RULE_MAX_CHUNK_DAYS`] days and never less than one. A wider chunk costs fewer requests and fewer
//! commits (the store republishes a series' whole manifest on every commit) and leaves a longer
//! recent edge unstored (the chunk holding "now" is never written).
//!
//! The depths are IG's own FAQ figures, "an indication"; what IG does past them (cut, empty, a 400)
//! is unmeasured, so the daemon refuses a request whose first chunk starts beyond one
//! (`vike_datahub::backfill`'s `ig_history_refusal`) instead of storing whatever came back.

/// One bar interval IG serves history for, with its documented depth and its pinned chunk width.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IgHistoryStep {
    /// The canonical interval label (`1m`, `1h`, `1d`), the store's.
    pub interval: &'static str,
    /// Bar width in milliseconds — IG's resolution is a calendar width, so this is exact.
    pub step_ms: i64,
    /// How many days back IG documents the interval as served.
    pub depth_days: u32,
    /// The chunk width in whole UTC days. PINNED: see the module doc.
    pub chunk_days: u32,
}

const MINUTE: i64 = 60_000;
const HOUR: i64 = 60 * MINUTE;
const DAY: i64 = 24 * HOUR;

/// The most calendar-day bars a chunk holds, about: the rule behind [`IG_HISTORY_STEPS`]'s widths.
pub const TARGET_ROWS_A_CHUNK: i64 = 500;

/// The widest chunk any IG interval takes, in days (the rule's cap; the ingest's own bound is wider).
pub const RULE_MAX_CHUNK_DAYS: u32 = 28;

/// **The twelve intervals IG's history serves**, in ascending width. `1s` (IG's `SECOND`, four
/// days deep) is not here: the bridge's `resolution` has no second label and the live feed owns that
/// width. `1w` and `1mo` are not here either: a week and a month have no fixed step, and
/// `vike_marketdata::interval::measures_bar_step` refuses them ahead of any source.
///
/// Depths: 1, 2 and 3 minutes 40 days; 5 minutes to 4 hours 360 days; 1 day 15 years. IG states them
/// as "an indication" (its FAQ, read 2026-10-20).
pub const IG_HISTORY_STEPS: [IgHistoryStep; 12] = [
    IgHistoryStep { interval: "1m", step_ms: MINUTE, depth_days: 40, chunk_days: 1 },
    IgHistoryStep { interval: "2m", step_ms: 2 * MINUTE, depth_days: 40, chunk_days: 1 },
    IgHistoryStep { interval: "3m", step_ms: 3 * MINUTE, depth_days: 40, chunk_days: 1 },
    IgHistoryStep { interval: "5m", step_ms: 5 * MINUTE, depth_days: 360, chunk_days: 2 },
    IgHistoryStep { interval: "10m", step_ms: 10 * MINUTE, depth_days: 360, chunk_days: 3 },
    IgHistoryStep { interval: "15m", step_ms: 15 * MINUTE, depth_days: 360, chunk_days: 5 },
    IgHistoryStep { interval: "30m", step_ms: 30 * MINUTE, depth_days: 360, chunk_days: 10 },
    IgHistoryStep { interval: "1h", step_ms: HOUR, depth_days: 360, chunk_days: 21 },
    IgHistoryStep { interval: "2h", step_ms: 2 * HOUR, depth_days: 360, chunk_days: 28 },
    IgHistoryStep { interval: "3h", step_ms: 3 * HOUR, depth_days: 360, chunk_days: 28 },
    IgHistoryStep { interval: "4h", step_ms: 4 * HOUR, depth_days: 360, chunk_days: 28 },
    IgHistoryStep { interval: "1d", step_ms: DAY, depth_days: 5_475, chunk_days: 28 },
];

/// `interval`'s row, or `None` for an interval IG's history does not serve here.
pub fn ig_history_step(interval: &str) -> Option<&'static IgHistoryStep> {
    IG_HISTORY_STEPS.iter().find(|step| step.interval == interval)
}

/// **Whether `symbol` is spelled as IG spells an epic** — `CS.D.EURUSD.MINI.IP`, `IX.D.DAX.IFD.IP`,
/// `CC.D.CL.BMU.IP`: five dot-separated segments of upper-case ASCII letters, digits, `_` and `-`,
/// none empty. A SHAPE test, not a catalog lookup: this workspace maps no symbol to an epic (an
/// instrument has several, MINI, CFD, TODAY, and choosing one would be a guess), so the symbol IS the
/// epic and the only thing to refuse is what cannot be one — a bare pair (`EURUSD`), another case
/// (`cs.d.eurusd.mini.ip`), a name with spaces or a path in it.
pub fn is_ig_epic(symbol: &str) -> bool {
    let mut segments = 0;
    for segment in symbol.split('.') {
        segments += 1;
        let ok = !segment.is_empty()
            && segment
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_' || b == b'-');
        if !ok {
            return false;
        }
    }
    segments == 5
}

/// **The intervals the lane serves today: the hour and the day.** The 2026-10-23 probe read only the
/// `HOUR` and `DAY` resolutions. The stamp convention of the minute and the multi-hour resolutions is
/// unmeasured, and a chunk width becomes a series' key identity at its first fetch, so the other ten rows
/// of [`IG_HISTORY_STEPS`] keep their pinned widths but are refused until a measured acceptance run admits
/// them (a change to this list, with the run that justifies it in the decision record).
pub const IG_ADMITTED_INTERVALS: [&str; 2] = ["1h", "1d"];

/// Whether `interval` is one of [`IG_ADMITTED_INTERVALS`].
pub fn ig_interval_admitted(interval: &str) -> bool {
    IG_ADMITTED_INTERVALS.contains(&interval)
}

/// **The refusal for an interval that is not admitted**, before any login. The client's text is echoed
/// bounded.
pub fn ig_unmeasured_words(interval: &str) -> String {
    let shown: String = interval.chars().take(40).collect();
    format!(
        "ig serves history for 1h and 1d bars today, and {shown:?} is unmeasured: run the lean \
         acceptance first. Only the hour and day resolutions were read by the 2026-10-23 probe, and \
         a series' chunk width becomes its key identity at its first fetch, so the other intervals \
         wait for a measured run (the IG history ops page names it). Nothing was fetched or written"
    )
}

/// IG's weekly historical-data allowance on the demo key, in points (measured 2026-10-23).
pub const IG_WEEKLY_POINTS: u32 = 10_000;

/// The share of the week ONE request's whole window may cost, in percent: a wrong window (a typo for
/// the interval, a decade of hours) is refused before any login instead of spending most of the week.
pub const IG_WINDOW_SHARE_PERCENT: u32 = 25;

/// **What a window could cost, in points**: the calendar bound — one point a row, one row a bar step —
/// summed over the whole chunks the window touches (a chunk is fetched whole, so a window that starts one
/// millisecond into a chunk costs that chunk). `0` for an inverted window. Above what a week of FX really
/// holds (the market is shut at weekends), so it can only refuse early.
pub fn ig_window_points(step: &IgHistoryStep, start_ms: i64, end_ms: i64) -> u64 {
    if end_ms < start_ms || step.step_ms <= 0 {
        return 0;
    }
    let width = i64::from(step.chunk_days) * DAY;
    let chunks = i128::from(end_ms.div_euclid(width)) - i128::from(start_ms.div_euclid(width)) + 1;
    let per_chunk = u128::from((width / step.step_ms).unsigned_abs());
    u64::try_from(chunks.unsigned_abs().saturating_mul(per_chunk)).unwrap_or(u64::MAX)
}

#[cfg(test)]
#[path = "ig_history_tests.rs"]
mod ig_history_tests;
