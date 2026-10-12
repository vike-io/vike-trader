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
    assert_eq!(out.rows[0].ts, window.start_secs * 1_000, "nothing older than --start survived");
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
    assert!(guard_window_reaches_the_anchor(Grading::Unrealized, ANCHOR, Some(one_back)).is_ok());
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
    let series = dir.path().join("kind=cohort").join(format!("venue={VENUE}")).join("symbol=BTC");
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
