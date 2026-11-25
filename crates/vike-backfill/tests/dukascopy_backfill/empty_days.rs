//! EMPTY DAYS: a whole settled UTC day the feed answered with no tick is marked on ORDER evidence.

use super::*;

// ── EMPTY DAYS: the chunked kline ingest's `:empty` marker, spent on ORDER evidence ──────────────
//
// `backfill_quotes_then_bars`'s EMPTY day bullet. Days 5 to 12 after the epoch are long settled, and
// at `1m` a chunk is one UTC day. A day the fetch ANSWERS empty is marked only once a later day of
// the same request holds ticks; a day whose fetch FAILED is never marked.

/// Day `d`'s EMPTY MARKER, spelled as the shared template spells it, and whether it is spent in the
/// `1m` bar series.
fn marked(hist: &HistoryStoreDataFusion, d: i64) -> bool {
    let key = format!(
        "dukascopy:{SYMBOL}:{INTERVAL}:{}-{}:empty",
        d * MS_PER_DAY,
        (d + 1) * MS_PER_DAY - 1
    );
    bar_series_has_commit(hist, INTERVAL, &key)
}

/// Run days `[first, last]` with a fake feed: `empty` days answer no tick, a `failing` day answers
/// an error, every other day the three-tick tape. Returns the result and the day of every fetch.
fn run(
    hist: &HistoryStoreDataFusion,
    (first, last): (i64, i64),
    empty: &[i64],
    failing: Option<i64>,
) -> (Result<usize, CollectError>, Vec<i64>) {
    let mut days = Vec::new();
    let out = backfill_quotes_then_bars(
        hist,
        SYMBOL,
        INTERVAL,
        first * MS_PER_DAY,
        (last + 1) * MS_PER_DAY - 1,
        &|| false,
        |_, from, to| {
            let day = from.div_euclid(MS_PER_DAY);
            days.push(day);
            if failing == Some(day) {
                return Err("dukascopy: dukascopy 503 (after retries)".to_string());
            }
            Ok(if empty.contains(&day) { Vec::new() } else { tape_between(from, to) })
        },
    );
    (out, days)
}

/// **The headline.** Day 6 has no tick: the first run stores days 5 and 7 and, because day 7 held
/// ticks after it, marks day 6 without spending its quote key. The second run fetches nothing.
#[test]
fn an_empty_day_between_two_trading_days_is_marked_and_never_fetched_again() {
    let (_dir, hist) = open();

    let (first, days) = run(&hist, (5, 7), &[6], None);
    assert_eq!(first.unwrap(), 6, "days 5 and 7, three bars each");
    assert_eq!(days, vec![5, 6, 7]);
    assert!(marked(&hist, 6), "the empty day's marker is spent");
    assert!(!marked(&hist, 5) && !marked(&hist, 7), "a day with ticks is stored, never marked");
    assert!(
        !quote_series_has_commit(
            &hist,
            &quote_commit_key(SYMBOL, 6 * MS_PER_DAY, 7 * MS_PER_DAY - 1)
        ),
        "the empty day's OWN quote key stays free"
    );

    let (second, days) = run(&hist, (5, 7), &[], None);
    assert_eq!(second.unwrap(), 0);
    assert_eq!(days, Vec::<i64>::new(), "days 5 and 7 are stored and day 6 is marked: no fetch");
}

/// A FAILED day is never marked, and neither is the empty day before it: the failure ends the
/// request before any later day held ticks. The retry fetches both again, and only then, with day 6
/// holding ticks, marks day 5.
#[test]
fn a_failed_day_marks_nothing_and_the_retry_fetches_it_again() {
    let (_dir, hist) = open();

    let (failed, days) = run(&hist, (5, 7), &[5], Some(6));
    assert_matches!(failed, Err(CollectError::Fetch(_)));
    assert_eq!(days, vec![5, 6], "the request stops at the failed day");
    assert!(!marked(&hist, 5), "an empty day with no later evidence is dropped unmarked");
    assert!(!marked(&hist, 6), "a failed day is never marked");

    let (retry, days) = run(&hist, (5, 7), &[5], None);
    assert_eq!(retry.unwrap(), 6, "days 6 and 7");
    assert_eq!(days, vec![5, 6, 7], "the empty day and the failed day are both asked again");
    assert!(marked(&hist, 5), "day 6 held ticks, so day 5 is final");
    assert!(!marked(&hist, 6));
}

/// THE HAZARD: an empty day with no later day holding ticks may only be unpublished, so it is
/// never marked. Here it is the request's last day, and a feed answering every day empty marks
/// nothing either.
#[test]
fn an_empty_day_with_no_later_ticks_is_never_marked() {
    let (_dir, hist) = open();

    let (trailing, _) = run(&hist, (5, 6), &[6], None);
    assert_eq!(trailing.unwrap(), 3, "day 5 alone");
    assert!(!marked(&hist, 6), "the trailing empty day is dropped unmarked");
    let (again, days) = run(&hist, (5, 6), &[6], None);
    assert_eq!(again.unwrap(), 0);
    assert_eq!(days, vec![6], "and asked again by the next run");

    let (silent, _) = run(&hist, (10, 12), &[10, 11, 12], None);
    assert_eq!(silent.unwrap(), 0);
    assert!((10..=12).all(|d| !marked(&hist, d)), "a feed publishing nothing marks nothing");
}
