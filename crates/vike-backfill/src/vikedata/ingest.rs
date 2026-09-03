//! Drive the pure `data.vike.io` cohort decoders into the `vike-data` hist store: resolve a window,
//! WALK the cursor, then hand one batch to `vike_data::CohortRecorder`. The twin of
//! `crates/vike-backfill/src/tardis/ingest.rs`, which iterates days where this iterates cursor
//! pages.
//!
//! # This module builds no commit key
//!
//! `kind=cohort` already has a write-side entry point and it owns the key:
//! `crates/vike-data/src/cohort_rec.rs`'s `CohortFetch` names the five dimensions the store PATH
//! cannot carry (axis, cohort, grading, label basis, and the asset restated as a column) and is the
//! single `format!` that turns them into `cohort:{venue}:{asset}:{axis}:{grading}:{label_basis}:
//! {start_ms}-{end_ms}`. A second spelling here would be the defect that type exists to make
//! unavailable, so this module fills the struct and lets it do the formatting.
//!
//! # ⚠ Idempotency is BATCH-level, so the WINDOW is the unit
//!
//! Re-running the SAME command is a no-op: the same `(axis, grading, basis, window)` resolves the
//! same key and the store refuses the second batch. Two runs over OVERLAPPING windows are NOT — the
//! store compares keys, never row values, so the overlapping hours land twice. That is the same
//! contract `crates/vike-backfill/src/databento/ingest.rs` has (its key names `{start}-{end}` too),
//! and the remedy is the same: schedule NON-OVERLAPPING windows with
//! [`CohortWindow::range`] rather than re-running a trailing window that slides an hour at a time.
//! Both window ends are hour-floored precisely so a command repeated inside one hour is the no-op
//! an operator expects; nothing here can make a sliding window idempotent, and pretending otherwise
//! is how a series doubles.

use std::collections::BTreeMap;

use vike_data::{CohortFetch, CohortRecorder, CohortRow};

use crate::error::CollectError;
use crate::vikedata::client::{
    check_grading_applies, cohort_metrics_url, fetch_page, Axis, Grading, CTX, MAX_PAGES,
};
use crate::vikedata::parse::{self, MetricRow, LABEL_BASIS_UNSET};
use crate::vikedata::{floor_to_hour, SECS_PER_DAY, SECS_PER_HOUR};

/// Milliseconds in a day — the store's `ts` unit, so every trim/retain bound below is in it.
const MS_PER_DAY: i64 = SECS_PER_DAY * 1_000;

/// How far behind the window's end the newest returned hour may sit before a NON-DEFAULT grading is
/// refused. ONE bucket: the series is hourly, so the hour containing the anchor is the newest that
/// can exist, and anything older means the window came back short.
pub const STALE_TOLERANCE_SECS: i64 = SECS_PER_HOUR;

/// The window ONE fetch covers, already resolved against an instant.
///
/// Two constructors, because the two run shapes differ in more than arithmetic:
/// [`CohortWindow::trailing`] is the periodic "catch up to now" read and [`CohortWindow::range`]
/// the AD-HOC one-shot gap fill an operator types once. Every field is hour-aligned — the endpoint
/// honours a sub-hour `start` by TRUNCATING that bucket's aggregate while still labelling it on the
/// hour ([`crate::vikedata::floor_to_hour`] carries the measurement).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CohortWindow {
    /// The hour-floored `start` actually SENT. Recorded rather than "N days", because "60 days" is
    /// not a fact a later reader can check.
    pub start_secs: i64,
    /// The instant the window resolves against — the hour-floored clock for a trailing read, the
    /// operator's `--end` for a range one. The walk's window break and the trim both anchor here.
    pub anchor_secs: i64,
    /// The window WIDTH in days, used by the walk break and by the trailing trim.
    pub days: u32,
    /// Whether the exact `[start_secs, anchor_secs]` retain is armed (range reads only).
    pub pinned: bool,
}

impl CohortWindow {
    /// The PERIODIC shape: `days` back from `now_secs`.
    ///
    /// A day of slack is added to the `start` SENT and trimmed back client-side against the newest
    /// bucket actually returned, so the window is reproducible from the data the run holds rather
    /// than from the clock it ran on.
    pub fn trailing(days: u32, now_secs: i64) -> Self {
        let anchor_secs = floor_to_hour(now_secs);
        let start_secs = floor_to_hour(anchor_secs - (i64::from(days) + 1) * SECS_PER_DAY);
        Self { start_secs, anchor_secs, days, pinned: false }
    }

    /// The AD-HOC shape: fill exactly `[start_secs, end_secs]`, one command, no sliding window.
    ///
    /// ⚠ **The retain this arms is not decoration.** The endpoint keeps serving buckets up to ITS
    /// OWN now regardless of the `start` sent, so a gap fill of last April comes back carrying
    /// today as well; without the upper bound the trailing trim would then anchor on the newest
    /// SERVED bucket instead of on the operator's `--end`, and two runs of the same command hours
    /// apart would store two different tapes under two different keys.
    pub fn range(start_secs: i64, end_secs: i64) -> Result<Self, CollectError> {
        let start_secs = floor_to_hour(start_secs);
        let anchor_secs = floor_to_hour(end_secs);
        if anchor_secs < start_secs {
            return Err(CollectError::Fetch(format!(
                "{CTX}: --end ({anchor_secs}) is before --start ({start_secs}) once both are \
                 floored to the hour"
            )));
        }
        // Ceil, and never zero: `days` is what the walk's window break is measured in, and a width
        // of 0 would make the break fire on the first page of a sub-day request. Spelled out
        // rather than `div_ceil`, which is stable only for the UNSIGNED integers; the span here is
        // non-negative by the check above, so the `+ (d - 1)` form is exact and cannot overflow at
        // any window a `--start`/`--end` pair can express.
        let days = (anchor_secs - start_secs + SECS_PER_DAY - 1) / SECS_PER_DAY;
        let days = days.max(1);
        let days = u32::try_from(days).map_err(|_| {
            CollectError::Fetch(format!("{CTX}: the requested range spans {days} days"))
        })?;
        Ok(Self { start_secs, anchor_secs, days, pinned: true })
    }
}

/// Why the cursor walk stopped.
///
/// ⚠ **THREE stops, not two**, and the third is the one a port loses. An absent cursor and an empty
/// page are the obvious ends of a paged read; [`WalkStop::WindowCovered`] is what caps an ANCHORED
/// one, because this endpoint keeps serving buckets up to its own now whatever `start` it was
/// given. Without it a gap fill of last April pages forward through today — MORE requests than a
/// trailing read makes, against a METERED endpoint — and every one of those extra buckets is then
/// thrown away by the retain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WalkStop {
    /// The server issued no `nextCursor`.
    CursorExhausted,
    /// The page carried no rows.
    EmptyPage,
    /// The oldest bucket in hand already predates the requested window.
    WindowCovered,
}

/// Everything one walk produced, before normalisation.
#[derive(Debug)]
pub struct Walked {
    pub raw: Vec<MetricRow>,
    pub pages: usize,
    pub stop: WalkStop,
    /// The basis every page agreed on — `None` when the server echoed none at all (see
    /// `crates/vike-backfill/src/vikedata/parse.rs`'s `LABEL_BASIS_UNSET`).
    pub label_basis: Option<String>,
}

/// One fetch, normalised, deduped and windowed — what [`backfill_cohort`] hands to the store.
#[derive(Debug)]
pub struct CohortFetchResult {
    pub rows: Vec<CohortRow>,
    pub pages: usize,
    pub stop: WalkStop,
    /// The basis STORED on every row and named in the commit key.
    pub label_basis: String,
    /// Rows the taxonomy filter dropped, by normalised label. Silent drops are how a data question
    /// stays a guess, so they are counted and reported rather than skipped.
    pub dropped: BTreeMap<String, usize>,
}

/// ⚠ **The basis must AGREE across pages, not merely be present on the first one.**
///
/// `crates/vike-backfill/src/vikedata/parse.rs`'s `guard_label_basis` judges ONE page against the
/// contract; this judges the pages against EACH OTHER. They are different failures: a mid-walk
/// change of basis means the pages of one batch were resolved two different ways, and the batch is
/// about to be stored under a commit key that can only name one of them — so half the rows would
/// claim a resolution they did not get. Refusing costs a re-run; storing costs a series nobody can
/// later separate, because the two halves are shape-identical.
pub fn guard_basis_agrees(
    first: Option<&str>,
    page: Option<&str>,
    url: &str,
) -> Result<(), CollectError> {
    if first == page {
        return Ok(());
    }
    Err(CollectError::Fetch(format!(
        "{CTX} {url}: label_basis changed mid-walk — the first page said {first:?} and a later one \
         says {page:?}. One batch cannot carry two resolutions: its commit key names ONE, so the \
         other half of the rows would claim a basis they were not graded under."
    )))
}

/// The cursor walk, over an injected page fetcher.
///
/// `fetch` takes the cursor (empty on the first request) and returns the raw body. Injecting it is
/// what makes the three stops testable without a socket — every rule this vendor has is a rule
/// about the REQUESTS a window causes, and a walk reachable only through the live endpoint would
/// have those rules tested by nobody.
///
/// ⚠ Reaching [`MAX_PAGES`] is an ERROR here, where
/// its predecessor `crates/vike-research/src/sources/api.rs`'s `fetch_axis_at` simply stopped.
/// That client re-fetched
/// whenever it liked; this one WRITES, and a truncated walk would store a short window under a
/// commit key that claims the whole one — after which the honest fetch of that window is a silent
/// no-op forever. A short window does not look short: every row in it is real and the ladder is
/// complete, it simply stops early.
pub fn walk_pages<F>(
    mut fetch: F,
    url_for_messages: &str,
    axis: Axis,
    grading: Grading,
    window: &CohortWindow,
) -> Result<Walked, CollectError>
where
    F: FnMut(&str) -> Result<String, CollectError>,
{
    let mut cursor = String::new();
    let mut raw: Vec<MetricRow> = Vec::new();
    let mut basis: Option<String> = None;
    let mut basis_seen = false;

    // `pages` is the LOOP variable rather than a counter beside it: every exit from this loop is a
    // `return` from inside it, so the page number at that moment is exactly the count of pages
    // fetched. One-based for the same reason.
    for pages in 1..=MAX_PAGES {
        let body = fetch(&cursor)?;
        let page = parse::parse_page(&body, url_for_messages)?;
        // Both response guards ride the PNL axis only, and that is not an oversight: `labelBasis`
        // and `grading` are the two parameters this client sends there and nowhere else, so on a
        // size or tier response there is nothing this client ASKED FOR to hold the server to. What
        // keeps the grading honest on those axes is `check_grading_applies` REFUSING the request at
        // the door — a stronger guarantee than an echo, because it never reaches the wire at all.
        //
        // ⚠ MEASURED 2026-08-25, against the live endpoint, because this comment used to say a
        // size or tier response "has nothing to echo back" and that is FALSE. Both axes DO return
        // `label_basis: "point_in_time"`, on the envelope AND on every row, unasked — confirmed by
        // curl and again by a real `--axis size` run whose completion log reads
        // `label_basis=point_in_time`. `grading` is the field that genuinely comes back null there.
        //
        // Nothing below needs changing for that, and the distinction is worth keeping straight: an
        // echo of a parameter this client SENT is evidence the server honoured it, which is what
        // `guard_label_basis` is for; an unasked-for field is just data. It still reaches the
        // commit key through `basis` below, so a size ladder keys on `point_in_time` rather than on
        // `LABEL_BASIS_UNSET` — which is why that sentinel is a fallback here and not the norm.
        if axis == Axis::Pnl {
            parse::guard_label_basis(&page, url_for_messages)?;
            parse::guard_grading(&page, url_for_messages, grading)?;
        }
        if basis_seen {
            guard_basis_agrees(basis.as_deref(), page.label_basis.as_deref(), url_for_messages)?;
        } else {
            basis = page.label_basis.clone();
            basis_seen = true;
        }

        let got = page.metrics.unwrap_or_default();
        let n = got.len();
        raw.extend(got);
        cursor = page.next_cursor.unwrap_or_default();

        // STOP 1 and STOP 2 — the obvious ends of a paged read.
        if cursor.is_empty() {
            return Ok(Walked { raw, pages, stop: WalkStop::CursorExhausted, label_basis: basis });
        }
        if n == 0 {
            return Ok(Walked { raw, pages, stop: WalkStop::EmptyPage, label_basis: basis });
        }
        // STOP 3 — the window break. Measured over EVERY row in hand rather than over this page
        // alone: pages arrive newest-first, so the two coincide today, and taking the minimum of
        // the whole accumulation is what keeps that an observation rather than an assumption.
        if let Some(oldest) = raw.iter().filter_map(|r| parse::parse_hour(&r.ts).ok()).min() {
            if window.anchor_secs - oldest >= i64::from(window.days) * SECS_PER_DAY {
                return Ok(Walked {
                    raw,
                    pages,
                    stop: WalkStop::WindowCovered,
                    label_basis: basis,
                });
            }
        }
    }
    Err(CollectError::Fetch(format!(
        "{CTX} {url_for_messages}: the cursor walk hit its {MAX_PAGES}-page ceiling without \
         covering [{}, {}]. Storing what it has would file a SHORT window under a commit key that \
         claims the whole one, and that key is then spent. Narrow the window and re-run.",
        window.start_secs, window.anchor_secs
    )))
}

/// Retain `ts >= max_ts − days`, anchoring on the newest bucket RETURNED rather than on the clock,
/// so a trailing window is reproducible from the data the run actually holds. Bounds in
/// epoch-MILLISECONDS, the unit `vike_data::CohortRow`'s `ts` is.
fn trim_to_window(rows: Vec<CohortRow>, days: u32) -> Vec<CohortRow> {
    let Some(max_ts) = rows.iter().map(|r| r.ts).max() else { return rows };
    let floor = max_ts - i64::from(days) * MS_PER_DAY;
    rows.into_iter().filter(|r| r.ts >= floor).collect()
}

/// Refuse a NON-DEFAULT grading whose newest hour falls short of the window's end.
///
/// [`Grading::Realized`] is live to the current hour and keeps whatever the endpoint serves. The
/// other two are exactly the ones that can run dry without anything downstream noticing —
/// `realized-pit` is a FROZEN upstream artefact and `unrealized` a rollup that can stall — and a
/// short window is invisible in the store: every row is real, the ladder is complete, the series is
/// dense, it just stops early. Louder than a log line because the commit key that names the full
/// window is spent either way.
fn guard_window_reaches_the_anchor(
    grading: Grading,
    anchor_secs: i64,
    last_ts_ms: Option<i64>,
) -> Result<(), CollectError> {
    if grading == Grading::Realized {
        return Ok(());
    }
    let Some(last) = last_ts_ms.map(|ms| ms / 1_000) else { return Ok(()) };
    if anchor_secs - last <= STALE_TOLERANCE_SECS {
        return Ok(());
    }
    Err(CollectError::Fetch(format!(
        "{CTX}: grading={} has no hour newer than {last} (unix) but this window ends at \
         {anchor_secs} — nothing is writing that grading up to the anchor. The batch would be \
         short by {} hours while its commit key claimed the whole window. Re-run with --end {last}, \
         or read --grading realized for the live edge.",
        grading.echoed(),
        (anchor_secs - last) / SECS_PER_HOUR
    )))
}

/// Fetch and normalise one `(exchange, asset, axis, grading)` ladder over `window`. No store write —
/// [`backfill_cohort`] is the half that writes.
#[allow(clippy::too_many_arguments)]
pub fn fetch_cohort(
    api_key: &str,
    base: &str,
    exchange: &str,
    asset: &str,
    axis: Axis,
    grading: Grading,
    window: &CohortWindow,
) -> Result<CohortFetchResult, CollectError> {
    check_grading_applies(axis, grading)?;
    let url = cohort_metrics_url(base, exchange, asset);
    let walked = walk_pages(
        |cursor| {
            fetch_page(api_key, base, exchange, asset, axis, grading, window.start_secs, cursor)
        },
        &url,
        axis,
        grading,
        window,
    )?;
    finish_walk(asset, axis, grading, window, walked)
}

/// The pure half of [`fetch_cohort`]: turn a completed walk into windowed store rows. Split out so
/// the retain/trim rules are testable without a transport.
pub fn finish_walk(
    asset: &str,
    axis: Axis,
    grading: Grading,
    window: &CohortWindow,
    walked: Walked,
) -> Result<CohortFetchResult, CollectError> {
    let label_basis = walked.label_basis.unwrap_or_else(|| LABEL_BASIS_UNSET.to_string());
    let (rows, dropped) =
        parse::rows_from_metrics(asset, axis, grading, &label_basis, &walked.raw)?;
    let mut rows = parse::dedupe_first_wins(rows);
    let rows = if window.pinned {
        // The EXACT bounds an operator asked for. Applied INSTEAD of the data-anchored trim, not
        // before it: that trim rides the newest bucket returned, which for this endpoint is "its
        // own now" rather than the requested end.
        let (lo, hi) = (window.start_secs * 1_000, window.anchor_secs * 1_000);
        rows.retain(|r| r.ts >= lo && r.ts <= hi);
        rows
    } else {
        trim_to_window(rows, window.days)
    };
    guard_window_reaches_the_anchor(grading, window.anchor_secs, rows.last().map(|r| r.ts))?;
    Ok(CohortFetchResult { rows, pages: walked.pages, stop: walked.stop, label_basis, dropped })
}

/// Fetch one ladder and persist it as ONE batch. Returns rows written — `0` when the window was
/// already ingested under this key, or when every row was dropped by the taxonomy filter.
///
/// `venue` in the store is the EXCHANGE whose positions were graded, never the metrics service that
/// graded them (`crates/vike-data/src/cohort_rec.rs`'s `CohortFetch` states that identity), so the
/// one `exchange` argument is both the URL path segment and the series venue — they cannot drift.
#[allow(clippy::too_many_arguments)]
pub fn backfill_cohort(
    rec: &CohortRecorder,
    api_key: &str,
    base: &str,
    exchange: &str,
    asset: &str,
    axis: Axis,
    grading: Grading,
    window: &CohortWindow,
) -> Result<usize, CollectError> {
    let fetched = fetch_cohort(api_key, base, exchange, asset, axis, grading, window)?;
    if !fetched.dropped.is_empty() {
        tracing::warn!(
            %asset, axis = axis.as_str(), dropped = ?fetched.dropped,
            "vikedata cohort labels outside the axis taxonomy were dropped"
        );
    }
    let fetch = CohortFetch {
        venue: exchange,
        asset,
        axis: axis.as_str(),
        grading: grading.echoed(),
        label_basis: &fetched.label_basis,
        start_ms: window.start_secs * 1_000,
        end_ms: window.anchor_secs * 1_000,
    };
    let n = rec.record(&fetch, &fetched.rows)?;
    tracing::info!(
        rows = n, fetched = fetched.rows.len(), pages = fetched.pages, stop = ?fetched.stop,
        %exchange, %asset, axis = axis.as_str(), grading = grading.echoed(),
        label_basis = %fetched.label_basis, "vikedata cohort backfill complete"
    );
    Ok(n)
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::sync::Arc;

    use tempfile::TempDir;
    use vike_data::{DataFusionHist, HistStore, TsRange};

    use super::*;
    use crate::vikedata::parse::POINT_IN_TIME;

    const HOUR: i64 = SECS_PER_HOUR;
    /// 2025-08-09T13:00:00Z — the anchor every scripted walk below resolves against.
    const ANCHOR: i64 = 1_754_744_400;
    const VENUE: &str = "hyperliquid";

    /// A scripted endpoint: `hours_per_page` hourly buckets a page, marching BACKWARDS from
    /// `newest` (newest-first, like the real one) and ALWAYS issuing a cursor — an endpoint that
    /// never runs out, which is exactly the shape the third stop exists for.
    struct Endless {
        hours_per_page: usize,
        newest: i64,
        calls: RefCell<Vec<String>>,
        /// Per-page `label_basis` override; pages past the end echo `point_in_time`.
        basis: RefCell<Vec<Option<&'static str>>>,
    }

    impl Endless {
        fn new(hours_per_page: usize, newest: i64) -> Self {
            Self {
                hours_per_page,
                newest,
                calls: RefCell::new(Vec::new()),
                basis: RefCell::new(Vec::new()),
            }
        }

        fn body(&self, cursor: &str) -> String {
            let page: usize = if cursor.is_empty() { 0 } else { cursor.parse().unwrap() };
            let first = self.newest - (page * self.hours_per_page) as i64 * HOUR;
            let rows: Vec<String> = (0..self.hours_per_page)
                .map(|i| {
                    let ts = crate::vikedata::client::fmt_start(first - i as i64 * HOUR);
                    format!(
                        r#"{{"ts":"{ts}","cohort":"4x whale","total_position_value":100.0,"total_position_value_long":60.0}}"#
                    )
                })
                .collect();
            let basis = self
                .basis
                .borrow()
                .get(page)
                .copied()
                .unwrap_or(Some(POINT_IN_TIME))
                .map(|b| format!(r#""label_basis":"{b}","#))
                .unwrap_or_default();
            format!(
                r#"{{{basis}"grading":"realized","nextCursor":"{}","metrics":[{}]}}"#,
                page + 1,
                rows.join(",")
            )
        }

        fn fetcher(&self) -> impl FnMut(&str) -> Result<String, CollectError> + '_ {
            move |cursor: &str| {
                self.calls.borrow_mut().push(cursor.to_string());
                Ok(self.body(cursor))
            }
        }
    }

    fn walk(ep: &Endless, window: &CohortWindow) -> Result<Walked, CollectError> {
        walk_pages(ep.fetcher(), "u", Axis::Size, Grading::Realized, window)
    }

    fn finish(window: &CohortWindow, walked: Walked) -> CohortFetchResult {
        finish_walk("BTC", Axis::Size, Grading::Realized, window, walked).unwrap()
    }

    fn fetch_of<'a>(window: &CohortWindow, basis: &'a str) -> CohortFetch<'a> {
        CohortFetch {
            venue: VENUE,
            asset: "BTC",
            axis: "size",
            grading: "realized",
            label_basis: basis,
            start_ms: window.start_secs * 1_000,
            end_ms: window.anchor_secs * 1_000,
        }
    }

    // ---- the three stops -------------------------------------------------------------------

    #[test]
    fn the_walk_stops_when_the_server_issues_no_cursor() {
        let calls = RefCell::new(0usize);
        let out = walk_pages(
            |_| {
                *calls.borrow_mut() += 1;
                Ok(r#"{"label_basis":"point_in_time","grading":"realized",
                       "metrics":[{"ts":"2025-08-09T13:00:00Z","cohort":"4x whale",
                       "total_position_value":1.0,"total_position_value_long":1.0}]}"#
                    .to_string())
            },
            "u",
            Axis::Size,
            Grading::Realized,
            &CohortWindow::trailing(60, ANCHOR),
        )
        .unwrap();
        assert_eq!(out.stop, WalkStop::CursorExhausted);
        assert_eq!(*calls.borrow(), 1, "an absent cursor ends the walk at once");
    }

    #[test]
    fn the_walk_stops_on_an_empty_page_even_though_the_server_offered_a_cursor() {
        let calls = RefCell::new(0usize);
        let out = walk_pages(
            |_| {
                *calls.borrow_mut() += 1;
                Ok(r#"{"label_basis":"point_in_time","grading":"realized",
                       "nextCursor":"more","metrics":[]}"#
                    .to_string())
            },
            "u",
            Axis::Size,
            Grading::Realized,
            &CohortWindow::trailing(60, ANCHOR),
        )
        .unwrap();
        assert_eq!(out.stop, WalkStop::EmptyPage);
        assert_eq!(*calls.borrow(), 1, "a cursor with no rows is the end, not a reason to page on");
    }

    /// ⚠ **THE stop a port loses.** This endpoint never runs out of pages and never stops issuing a
    /// cursor, so stops 1 and 2 are both unreachable here — only the window break ends the walk.
    /// Delete that break and this test does not merely read a bigger number: the walk runs to the
    /// page ceiling and the fetch FAILS.
    #[test]
    fn the_walk_stops_once_the_window_is_covered_though_the_endpoint_keeps_serving() {
        // 48 buckets a page (47 hours of span), a 2-day window: page 1's oldest bucket is 95 hours
        // back, which is the first to clear 48.
        let ep = Endless::new(48, ANCHOR);
        let out = walk(&ep, &CohortWindow::trailing(2, ANCHOR)).unwrap();
        assert_eq!(out.stop, WalkStop::WindowCovered);
        assert_eq!(out.pages, 2, "the third page is never requested");
        assert_eq!(
            ep.calls.borrow().as_slice(),
            &["".to_string(), "1".to_string()],
            "the first request carries an EMPTY cursor and the second the server's own"
        );
    }

    /// The measurable consequence the third stop exists for: an ANCHORED read must not issue MORE
    /// requests than the trailing read it mirrors, against a METERED endpoint. Without the break
    /// both walks run the whole page budget and then fail.
    #[test]
    fn an_anchored_read_issues_no_more_requests_than_the_trailing_read_it_mirrors() {
        let trailing = CohortWindow::trailing(2, ANCHOR);
        let ranged = CohortWindow::range(ANCHOR - 2 * SECS_PER_DAY, ANCHOR).unwrap();
        assert_eq!(ranged.days, 2);

        let a = Endless::new(48, ANCHOR);
        let b = Endless::new(48, ANCHOR);
        assert_eq!(walk(&a, &trailing).unwrap().pages, 2);
        assert_eq!(walk(&b, &ranged).unwrap().pages, 2);
        assert_eq!(a.calls.borrow().len(), b.calls.borrow().len());
        assert!(b.calls.borrow().len() < MAX_PAGES, "and far short of the ceiling");
    }

    #[test]
    fn a_walk_that_cannot_cover_its_window_refuses_rather_than_storing_a_short_one() {
        // One bucket a page against a 60-day window: the ceiling arrives long before the break.
        let ep = Endless::new(1, ANCHOR);
        let err = walk(&ep, &CohortWindow::trailing(60, ANCHOR)).unwrap_err().to_string();
        assert!(err.contains(&MAX_PAGES.to_string()), "{err}");
        assert!(err.contains("SHORT window"), "{err}");
        assert_eq!(ep.calls.borrow().len(), MAX_PAGES, "…and it really did spend the whole budget");
    }

    // ---- the label basis across pages -------------------------------------------------------

    #[test]
    fn a_basis_that_changes_mid_walk_refuses_the_batch() {
        let ep = Endless::new(48, ANCHOR);
        // Page 0 point-in-time, page 1 something else — one batch, two resolutions.
        *ep.basis.borrow_mut() = vec![Some(POINT_IN_TIME), Some("current")];
        let err = walk(&ep, &CohortWindow::trailing(30, ANCHOR)).unwrap_err().to_string();
        assert!(err.contains("changed mid-walk"), "{err}");
        assert!(err.contains("point_in_time") && err.contains("current"), "{err}");
    }

    #[test]
    fn a_page_that_starts_echoing_a_basis_mid_walk_is_a_disagreement_too() {
        let ep = Endless::new(48, ANCHOR);
        *ep.basis.borrow_mut() = vec![None, Some(POINT_IN_TIME)];
        let err = walk(&ep, &CohortWindow::trailing(30, ANCHOR)).unwrap_err().to_string();
        assert!(err.contains("changed mid-walk"), "{err}");
    }

    #[test]
    fn a_basis_the_server_never_echoes_is_agreement_and_lands_as_the_unset_sentinel() {
        let ep = Endless::new(48, ANCHOR);
        *ep.basis.borrow_mut() = vec![None, None, None];
        let window = CohortWindow::trailing(2, ANCHOR);
        let walked = walk(&ep, &window).unwrap();
        assert_eq!(walked.label_basis, None);
        let out = finish(&window, walked);
        assert_eq!(out.label_basis, LABEL_BASIS_UNSET);
        assert!(out.rows.iter().all(|r| r.label_basis == LABEL_BASIS_UNSET));
    }

    #[test]
    fn the_pnl_ladder_refuses_a_page_with_no_basis_at_all_before_agreement_is_even_asked() {
        let ep = Endless::new(48, ANCHOR);
        *ep.basis.borrow_mut() = vec![None];
        let err = walk_pages(
            ep.fetcher(),
            "u",
            Axis::Pnl,
            Grading::Realized,
            &CohortWindow::trailing(2, ANCHOR),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("lookahead"), "{err}");
    }

    // ---- the two window shapes --------------------------------------------------------------

    #[test]
    fn a_trailing_window_sends_a_day_of_slack_and_floors_both_ends_to_the_hour() {
        let w = CohortWindow::trailing(60, ANCHOR + 1_337);
        assert_eq!(w.anchor_secs, ANCHOR, "the clock is floored — two runs inside one hour agree");
        assert_eq!(w.start_secs, ANCHOR - 61 * SECS_PER_DAY);
        assert_eq!(w.days, 60);
        assert!(!w.pinned);
    }

    #[test]
    fn a_range_window_is_pinned_and_measures_its_own_width_in_days() {
        let w = CohortWindow::range(ANCHOR - 3 * SECS_PER_DAY, ANCHOR).unwrap();
        assert_eq!(
            (w.start_secs, w.anchor_secs, w.days, w.pinned),
            (ANCHOR - 3 * SECS_PER_DAY, ANCHOR, 3, true)
        );
        // A sub-day range still gets a width of one — a zero would make the walk's break fire on
        // the first page whatever it held.
        assert_eq!(CohortWindow::range(ANCHOR - HOUR, ANCHOR).unwrap().days, 1);
        assert_eq!(CohortWindow::range(ANCHOR, ANCHOR).unwrap().days, 1);
        // A partial day rounds UP, so the break never fires before the window is covered.
        assert_eq!(CohortWindow::range(ANCHOR - SECS_PER_DAY - HOUR, ANCHOR).unwrap().days, 2);
        // Sub-hour ends are floored, so a typed timestamp cannot truncate the oldest bucket.
        let sloppy = CohortWindow::range(ANCHOR - SECS_PER_DAY + 137, ANCHOR + 59).unwrap();
        assert_eq!((sloppy.start_secs, sloppy.anchor_secs), (ANCHOR - SECS_PER_DAY, ANCHOR));
    }

    #[test]
    fn a_backwards_range_is_refused_rather_than_silently_emptied() {
        let err = CohortWindow::range(ANCHOR, ANCHOR - SECS_PER_DAY).unwrap_err().to_string();
        assert!(err.contains("before"), "{err}");
    }

    #[test]
    fn a_range_read_keeps_exactly_its_bounds_though_the_endpoint_serves_past_them() {
        // The scripted endpoint's newest bucket is a DAY past the operator's --end, which is what
        // the real one does: `start` does not cap the far side.
        let ep = Endless::new(48, ANCHOR + SECS_PER_DAY);
        let window = CohortWindow::range(ANCHOR - 2 * SECS_PER_DAY, ANCHOR).unwrap();
        let out = finish(&window, walk(&ep, &window).unwrap());
        assert!(!out.rows.is_empty());
        assert_eq!(
            out.rows[0].ts,
            window.start_secs * 1_000,
            "nothing older than --start survived"
        );
        assert_eq!(
            out.rows[out.rows.len() - 1].ts,
            window.anchor_secs * 1_000,
            "nothing newer than --end survived either"
        );
    }

    #[test]
    fn a_trailing_read_anchors_its_trim_on_the_newest_bucket_returned_not_on_the_clock() {
        // The tape stops two days short of the clock. A clock-anchored trim would keep nothing at
        // all here; the data-anchored one keeps a full day back from the newest bucket returned.
        let newest = ANCHOR - 2 * SECS_PER_DAY;
        let ep = Endless::new(48, newest);
        let window = CohortWindow::trailing(1, ANCHOR);
        let out = finish(&window, walk(&ep, &window).unwrap());
        assert_eq!(out.rows.len(), 25, "24 hours back from the newest bucket, inclusive");
        assert_eq!(out.rows[out.rows.len() - 1].ts, newest * 1_000);
        assert_eq!(out.rows[0].ts, (newest - SECS_PER_DAY) * 1_000);
    }

    #[test]
    fn a_non_default_grading_whose_tape_stops_short_of_the_anchor_is_refused() {
        let last_ms = (ANCHOR - 5 * SECS_PER_HOUR) * 1_000;
        assert!(guard_window_reaches_the_anchor(Grading::Realized, ANCHOR, Some(last_ms)).is_ok());
        let err = guard_window_reaches_the_anchor(Grading::RealizedPit, ANCHOR, Some(last_ms))
            .unwrap_err()
            .to_string();
        assert!(err.contains("short by 5 hours"), "{err}");
        // One bucket of slack is tolerated — the hour containing the anchor is the newest possible.
        let one_back = (ANCHOR - SECS_PER_HOUR) * 1_000;
        assert!(
            guard_window_reaches_the_anchor(Grading::Unrealized, ANCHOR, Some(one_back)).is_ok()
        );
        // An empty window is not a stale one; there is nothing to judge.
        assert!(guard_window_reaches_the_anchor(Grading::Unrealized, ANCHOR, None).is_ok());
    }

    // ---- the store write ---------------------------------------------------------------------

    /// The end-to-end property the seconds→milliseconds conversion exists for: a stored row lands
    /// in the `date=` PARTITION its bucket belongs to. An unmultiplied `ts` would put every row of
    /// every window under `date=1970-01-*`, and the commit key it landed under is then spent.
    #[test]
    fn a_fetch_lands_in_the_store_partitioned_by_the_hour_it_actually_covers() {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(DataFusionHist::open(dir.path()).unwrap());
        let rec = CohortRecorder::new(store.clone());
        let window = CohortWindow::range(ANCHOR - SECS_PER_DAY, ANCHOR).unwrap();
        let out = finish(&window, walk(&Endless::new(48, ANCHOR), &window).unwrap());

        assert_eq!(rec.record(&fetch_of(&window, &out.label_basis), &out.rows).unwrap(), 25);

        let back = store.scan_cohort(VENUE, "BTC", TsRange::all()).unwrap();
        assert_eq!(back.len(), 25, "25 hourly buckets, inclusive at both ends");
        assert!(
            back.iter().all(|r| r.ts % (SECS_PER_HOUR * 1_000) == 0),
            "every stored ts is a whole hour IN MILLISECONDS"
        );
        assert_eq!(back.iter().map(|r| r.total_usd).sum::<f64>(), 2_500.0);
        assert_eq!(back[0].label_basis, POINT_IN_TIME);

        // …and the partition on disk, which is the half a scan cannot show.
        let series =
            dir.path().join("kind=cohort").join(format!("venue={VENUE}")).join("symbol=BTC");
        let mut dates: Vec<String> = std::fs::read_dir(&series)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("date="))
            .collect();
        dates.sort();
        assert_eq!(
            dates,
            vec!["date=2025-08-08".to_string(), "date=2025-08-09".to_string()],
            "the window straddles a UTC midnight, and NOTHING is under date=1970-01-01"
        );
    }

    #[test]
    fn re_running_the_same_window_writes_nothing_a_second_time() {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(DataFusionHist::open(dir.path()).unwrap());
        let rec = CohortRecorder::new(store.clone());
        let window = CohortWindow::range(ANCHOR - SECS_PER_DAY, ANCHOR).unwrap();
        let mut wrote = Vec::new();
        for _ in 0..2 {
            let out = finish(&window, walk(&Endless::new(48, ANCHOR), &window).unwrap());
            wrote.push(rec.record(&fetch_of(&window, &out.label_basis), &out.rows).unwrap());
        }
        assert_eq!(wrote, vec![25, 0], "batch idempotency — the second run is a no-op");
        assert_eq!(store.scan_cohort(VENUE, "BTC", TsRange::all()).unwrap().len(), 25);
    }

    /// The consequence stated in this module's doc, PINNED rather than left to prose: two
    /// OVERLAPPING windows are two keys, so the shared hours land twice. An operator scheduling
    /// this must use non-overlapping [`CohortWindow::range`] boundaries — which is why that
    /// constructor exists beside the trailing one.
    #[test]
    fn two_overlapping_windows_are_two_batches_and_the_shared_hours_land_twice() {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(DataFusionHist::open(dir.path()).unwrap());
        let rec = CohortRecorder::new(store.clone());
        let mut total = 0usize;
        for end in [ANCHOR - HOUR, ANCHOR] {
            let window = CohortWindow::range(end - 2 * HOUR, end).unwrap();
            let out = finish(&window, walk(&Endless::new(48, ANCHOR), &window).unwrap());
            total += rec.record(&fetch_of(&window, &out.label_basis), &out.rows).unwrap();
        }
        assert_eq!(total, 6, "3 hours each, and the two shared hours are NOT deduplicated");
        assert_eq!(store.scan_cohort(VENUE, "BTC", TsRange::all()).unwrap().len(), 6);
    }

    #[test]
    fn a_grading_that_ranks_no_pnl_never_reaches_the_wire() {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(DataFusionHist::open(dir.path()).unwrap());
        let rec = CohortRecorder::new(store.clone());
        // A bogus base URL: if the refusal did not happen first this would fail as a transport
        // error instead, which is what the message assertion distinguishes.
        let err = backfill_cohort(
            &rec,
            "key",
            "http://127.0.0.1:1/v1",
            VENUE,
            "BTC",
            Axis::Size,
            Grading::Unrealized,
            &CohortWindow::trailing(1, ANCHOR),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("ranks no PnL"), "{err}");
        assert!(store.scan_cohort(VENUE, "BTC", TsRange::all()).unwrap().is_empty());
    }
}
