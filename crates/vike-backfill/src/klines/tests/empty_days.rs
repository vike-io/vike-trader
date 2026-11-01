//! Empty days: the key-only `:empty` marker, written only on ORDER evidence.

use super::*;
use std::assert_matches;

// ── EMPTY DAYS: the key-only marker, written on ORDER evidence ──────────────────────────────────
//
// `docs/superpowers/specs/2026-10-02-oanda-empty-days-design.md` §3 and §7 (PR 2). An empty settled
// day is MARKED (`empty_marker_key`, spent with no row) only once a LATER day of the same request is
// known to hold data — fetched with a bar, or skipped as spent — and a marked day is never asked
// again. Every pending day is dropped unmarked at a stop, a failure, the unsettled edge and the end
// of the request.

/// Whether the day chunk `[c0, c1]` has spent its EMPTY MARKER in `hist`.
fn marked(hist: &DataFusionHist, interval: &str, c0: i64, c1: i64) -> bool {
    let key = empty_marker_key(CVENUE, CSYM, interval, c0, c1);
    hist.series_has_commit(&chunk_series(interval), &key).expect("read the manifest")
}

/// The marker is the day's commit key plus the store's one marker suffix — a SUFFIX, so a
/// `--produced-by {venue}:` assertion over the series still holds for every key.
#[test]
fn the_empty_marker_is_the_days_key_plus_the_stores_suffix() {
    let key = commit_key("oanda", "EUR_USD", "5s", 0, DAY - 1);
    let marker = empty_marker_key("oanda", "EUR_USD", "5s", 0, DAY - 1);
    assert_eq!(marker, format!("{key}{}", vike_data::store::store_kind::EMPTY_MARKER_SUFFIX));
    assert_eq!(marker, "oanda:EUR_USD:5s:0-86399999:empty");
    assert_eq!(
        vike_data::store::store_kind::venue_window_key_prefix(&marker),
        Some("oanda:EUR_USD:5s:")
    );
}

/// **The headline.** Day 3 has no candles (a Saturday): the first run stores days 2 and 4, and —
/// because day 4 held data AFTER it — marks day 3 empty without spending day 3's own key. The second
/// run is then answered entirely from the manifest: its fetch PANICS if it is called for any day.
#[test]
fn a_saturday_between_two_trading_days_is_marked_and_never_asked_again() {
    let (_d, hist) = store();
    let (start, end) = (2 * DAY, 5 * DAY - 1);

    let first =
        run_chunked(&hist, &FakeMarket::new(HOUR).closed_on(3), "1h", (start, end), LATER).unwrap();

    assert_eq!(
        (first.chunks, first.fetched, first.empty, first.marked, first.rows),
        (3, 2, 1, 1, 48)
    );
    assert_partitions(&first);
    assert!(spent(&hist, "1h", 2 * DAY, 3 * DAY - 1));
    assert!(!spent(&hist, "1h", 3 * DAY, 4 * DAY - 1), "the empty day's OWN key stays free");
    assert!(marked(&hist, "1h", 3 * DAY, 4 * DAY - 1), "...and its marker is spent");
    assert!(spent(&hist, "1h", 4 * DAY, 5 * DAY - 1));

    let second = ingest_klines_chunked(
        &hist,
        CVENUE,
        CSYM,
        "1h",
        (start, end),
        1,
        LATER,
        &|| false,
        |_, _, s, _| panic!("day {} was asked again: a marked day must never be fetched", s / DAY),
    )
    .unwrap();

    assert_eq!(
        (second.skipped, second.known_empty, second.empty, second.fetched, second.rows),
        (3, 1, 0, 0, 0)
    );
    assert_partitions(&second);
    let summary = summary_line(&second, "x", (start, end), std::time::Duration::ZERO);
    assert!(
        summary.contains("3 skipped (1 known empty), 0 fetched, 0 empty (0 marked)"),
        "{summary}"
    );
}

/// **THE HAZARD.** A marker is permanent, so an empty answer that may only mean "not published YET"
/// must never write one. Three shapes, none of which has a later day holding data in its request:
/// a trailing empty day before the unsettled edge, a trailing empty day at the end of the request,
/// and a source that answers EVERY day empty (a venue publishing nothing at all). Each marks
/// nothing, and the next run asks each of those days again.
#[test]
fn an_empty_day_with_no_later_data_is_never_marked() {
    // (1) Day 3 is empty and day 4 is not settled yet: the edge, not a later day with data.
    let (_d, hist) = store();
    let now = 4 * DAY - 1 + HOUR + SETTLE_MARGIN_MS; // day 3 settled, day 4 not
    let market = FakeMarket::new(HOUR).closed_on(3);
    let out = run_chunked(&hist, &market, "1h", (2 * DAY, 5 * DAY - 1), now).unwrap();
    assert_eq!((out.fetched, out.empty, out.marked, out.unsettled), (1, 1, 0, 1), "{out:?}");
    assert!(!marked(&hist, "1h", 3 * DAY, 4 * DAY - 1), "the edge is not evidence");
    let again = FakeMarket::new(HOUR).closed_on(3);
    run_chunked(&hist, &again, "1h", (2 * DAY, 5 * DAY - 1), now).unwrap();
    assert_eq!(again.calls(), vec![(3 * DAY, 4 * DAY - 1)], "the unmarked day is asked again");

    // (2) Day 3 is empty and is the request's LAST day.
    let (_d2, hist) = store();
    let out = run_chunked(
        &hist,
        &FakeMarket::new(HOUR).closed_on(3),
        "1h",
        (2 * DAY, 4 * DAY - 1),
        LATER,
    )
    .unwrap();
    assert_eq!((out.fetched, out.empty, out.marked), (1, 1, 0), "{out:?}");
    assert!(!marked(&hist, "1h", 3 * DAY, 4 * DAY - 1), "the end of a request is not evidence");

    // (3) A venue publishing nothing: every day of the request is empty.
    let (_d3, hist) = store();
    let silent = || (2..6).fold(FakeMarket::new(HOUR), FakeMarket::closed_on);
    let out = run_chunked(&hist, &silent(), "1h", (2 * DAY, 6 * DAY - 1), LATER).unwrap();
    assert_eq!((out.chunks, out.empty, out.marked, out.rows), (4, 4, 0, 0), "{out:?}");
    assert_partitions(&out);
    for day in 2..6 {
        assert!(!marked(&hist, "1h", day * DAY, (day + 1) * DAY - 1), "day {day}");
    }
    let rerun = silent();
    run_chunked(&hist, &rerun, "1h", (2 * DAY, 6 * DAY - 1), LATER).unwrap();
    assert_eq!(rerun.calls().len(), 4, "every day is asked again: nothing was proved final");
}

/// **A SPENT later day is evidence without a fetch** — the re-run of history stored before markers
/// existed, where every data day is skipped rather than fetched. Sunday (day 4) is already stored;
/// a request over Saturday and Sunday fetches Saturday, finds it empty, then skips Sunday as spent,
/// and that skip marks Saturday. The same holds one step further down the induction: a later day
/// skipped for its MARKER is evidence too, because a marker itself was only ever written on evidence.
#[test]
fn a_spent_later_day_is_evidence_without_a_fetch() {
    let (_d, hist) = store();
    run_chunked(&hist, &FakeMarket::new(HOUR), "1h", (4 * DAY, 5 * DAY - 1), LATER).unwrap();

    let market = FakeMarket::new(HOUR).closed_on(2).closed_on(3);
    let out = run_chunked(&hist, &market, "1h", (3 * DAY, 5 * DAY - 1), LATER).unwrap();

    assert_eq!(market.calls(), vec![(3 * DAY, 4 * DAY - 1)], "Sunday is skipped, not fetched");
    assert_eq!((out.skipped, out.known_empty, out.empty, out.marked), (1, 0, 1, 1), "{out:?}");
    assert!(marked(&hist, "1h", 3 * DAY, 4 * DAY - 1), "a stored later day marks Saturday");

    // ...and a later day known only by its MARKER is evidence for the day before it.
    let out = run_chunked(&hist, &market, "1h", (2 * DAY, 4 * DAY - 1), LATER).unwrap();
    assert_eq!((out.skipped, out.known_empty, out.empty, out.marked), (1, 1, 1, 1), "{out:?}");
    assert!(marked(&hist, "1h", 2 * DAY, 3 * DAY - 1), "a marked later day marks the day before");
}

/// The pending list is DISCARDED at a stop and at a failure: an empty day whose evidence would have
/// come from a day the request never reached is left unmarked, and asked again.
#[test]
fn a_stop_or_a_failure_before_the_evidence_marks_nothing() {
    // A stop right after the empty day 2, before day 3 (which holds data) is reached.
    let (_d, hist) = store();
    let probe = FiresAfter::new(1);
    let stopped = run_chunked_stoppable(
        &hist,
        &FakeMarket::new(HOUR).closed_on(2),
        "1h",
        (2 * DAY, 4 * DAY - 1),
        LATER,
        &|| probe.ask(),
    );
    let Err(CollectError::Stopped(text)) = &stopped else { panic!("{stopped:?}") };
    assert!(text.contains("1 empty of which 0 marked"), "{text}");
    assert!(!marked(&hist, "1h", 2 * DAY, 3 * DAY - 1), "a stop must not mark the pending day");

    // A failure at day 3, after the empty day 2.
    let (_d2, hist) = store();
    let failed = run_chunked(
        &hist,
        &FakeMarket::new(HOUR).closed_on(2).failing_at(3 * DAY),
        "1h",
        (2 * DAY, 4 * DAY - 1),
        LATER,
    );
    assert_matches!(failed, Err(CollectError::Fetch(_)), "{failed:?}");
    assert!(!marked(&hist, "1h", 2 * DAY, 3 * DAY - 1), "a failure must not mark the pending day");
}

/// **A marker stops a FETCH, never a WRITE.** After day 3 is marked empty, rows offered under day 3's
/// own commit key are accepted — the marker is a different key, so a venue that later serves the
/// day loses nothing to it. (Spending the day's own key would answer `Ok(0)` here forever.)
#[test]
fn a_marker_never_blocks_the_days_rows() {
    let (_d, hist) = store();
    run_chunked(&hist, &FakeMarket::new(HOUR).closed_on(3), "1h", (2 * DAY, 5 * DAY - 1), LATER)
        .unwrap();
    assert!(marked(&hist, "1h", 3 * DAY, 4 * DAY - 1));

    let day3: Vec<Bar> = (0..24).map(|h| market_bar(3 * DAY + h * HOUR, HOUR)).collect();
    let key = commit_key(CVENUE, CSYM, "1h", 3 * DAY, 4 * DAY - 1);
    let written = hist.append_bars(CVENUE, CSYM, "1h", &day3, Some(&key)).unwrap();
    assert_eq!(written, 24, "the day's rows were refused: the marker spent the day's own key");
    assert_eq!(stored_at(&hist, "1h").len(), 72);
}
