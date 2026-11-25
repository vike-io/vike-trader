use std::sync::Mutex;

use super::*;
use crate::test_support::store;
use vike_data::TextRow;
use vike_data::TimestampRange;
use vike_data::source::SourceError;
use vike_data::text_log::{POLYMARKET_RULES_ARCHIVE_COLLECTOR, TEXT_KIND_POLYMARKET_RULES};

/// 2024-11-05 00:00:00 UTC.
const DAY0: i64 = 1_730_764_800_000;
const SERIES: (&str, &str) = (TEXT_KIND_POLYMARKET_RULES, POLYMARKET_RULES_ARCHIVE_COLLECTOR);

fn row(received_ms: i64, condition_id: &str) -> TextRow {
    TextRow {
        source_id: condition_id.into(),
        title: format!("market {condition_id}"),
        ..TextRow::new(TEXT_KIND_POLYMARKET_RULES, "Polymarket", received_ms)
    }
}

/// A scripted source: the rows of each day it is asked for, every archive window it was asked, and every
/// series window.
struct Scripted {
    days: Vec<(i64, Result<Vec<TextRow>, SourceError>)>,
    asked: Mutex<Vec<(i64, i64)>>,
    series_asked: Mutex<Vec<(String, i64, i64)>>,
}

impl MarketRulesSource for Scripted {
    fn venue(&self) -> &str {
        "polymarket"
    }

    fn closed_ending(&self, start_ms: i64, end_ms: i64) -> Result<Vec<TextRow>, SourceError> {
        self.asked.lock().unwrap().push((start_ms, end_ms));
        self.days.iter().find(|(d, _)| *d == start_ms).map_or(Ok(Vec::new()), |(_, a)| a.clone())
    }

    fn closed_of_series(
        &self,
        series: &str,
        start_ms: i64,
        end_ms: i64,
    ) -> Result<Vec<TextRow>, SourceError> {
        self.series_asked.lock().unwrap().push((series.to_string(), start_ms, end_ms));
        self.days.iter().find(|(d, _)| *d == start_ms).map_or(Ok(Vec::new()), |(_, a)| a.clone())
    }

    fn of_token(&self, _: &str) -> Result<Vec<TextRow>, SourceError> {
        unreachable!("the archive never asks a token")
    }
}

fn scripted(days: Vec<(i64, Result<Vec<TextRow>, SourceError>)>) -> Scripted {
    Scripted { days, asked: Mutex::new(Vec::new()), series_asked: Mutex::new(Vec::new()) }
}

const NOW: i64 = DAY0 + 30 * MS_PER_DAY;

/// A window inside one day stores the WHOLE day's markets under its key, each row at the received time the
/// source gave it (a market's close: before the day for one that resolved early, after it for one that
/// resolved late), and a repeat asks nothing.
#[test]
fn a_day_of_market_ends_is_stored_once_and_a_repeat_is_free() {
    let (_dir, hist) = store();
    let early = DAY0 - 200 * MS_PER_DAY;
    let late = DAY0 + MS_PER_DAY + 3_600_000;
    let source = scripted(vec![(DAY0, Ok(vec![row(early, "a"), row(late, "b"), row(DAY0, "c")]))]);
    let written =
        backfill_market_rules_at(&hist, &source, SERIES, (DAY0 + 5, DAY0 + 6), NOW, &|| false)
            .unwrap();
    assert_eq!(written, 3);
    assert_eq!(*source.asked.lock().unwrap(), [(DAY0, DAY0 + MS_PER_DAY - 1)]);
    let stored = hist.scan_texts(SERIES.0, SERIES.1, TimestampRange::all()).unwrap();
    assert_eq!(stored.iter().map(|r| r.received_ms).collect::<Vec<_>>(), [early, DAY0, late]);
    // The point-in-time read: a market that closed after the decision is not known at it.
    let known = hist.texts_as_of(SERIES.0, SERIES.1, late - 1, 365 * MS_PER_DAY).unwrap();
    assert_eq!(known.len(), 2, "a row is received at its close, never before");

    let again =
        backfill_market_rules_at(&hist, &source, SERIES, (DAY0, DAY0), NOW, &|| false).unwrap();
    assert_eq!(again, 0);
    assert_eq!(source.asked.lock().unwrap().len(), 1, "a stored day is never asked again");
}

/// A day not yet settled ends the request without asking it; an empty day spends no key.
#[test]
fn an_unsettled_day_is_not_asked_and_an_empty_day_is_asked_again() {
    let (_dir, hist) = store();
    let source = scripted(vec![(DAY0 + MS_PER_DAY, Ok(vec![row(DAY0, "a")]))]);
    // DAY0 settled and empty, DAY0+1 settled, DAY0+2 not settled at `now`.
    let now = DAY0 + 3 * MS_PER_DAY - 1 + MARKET_RULES_SETTLE_MS - 1;
    let written = backfill_market_rules_at(
        &hist,
        &source,
        SERIES,
        (DAY0, DAY0 + 5 * MS_PER_DAY),
        now,
        &|| false,
    )
    .unwrap();
    assert_eq!(written, 1);
    assert_eq!(
        *source.asked.lock().unwrap(),
        [(DAY0, DAY0 + MS_PER_DAY - 1), (DAY0 + MS_PER_DAY, DAY0 + 2 * MS_PER_DAY - 1)],
        "the unsettled day and every day after it are never asked"
    );
    backfill_market_rules_at(&hist, &source, SERIES, (DAY0, DAY0 + MS_PER_DAY), now, &|| false)
        .unwrap();
    assert_eq!(
        source.asked.lock().unwrap().len(),
        3,
        "the empty day is asked again, the stored one not"
    );
}

/// A stop between days, a failed day and a stray row: never an `Ok`, and the days before stay stored.
#[test]
fn a_stop_a_failure_and_a_stray_row_each_say_where_they_ended() {
    let (_dir, hist) = store();
    let source = scripted(vec![
        (DAY0, Ok(vec![row(DAY0, "a")])),
        (DAY0 + MS_PER_DAY, Err(SourceError::Fetch("gamma timed out".into()))),
    ]);
    let asked = std::cell::Cell::new(0);
    let stop_second = || {
        asked.set(asked.get() + 1);
        asked.get() > 1
    };
    let stopped = backfill_market_rules_at(
        &hist,
        &source,
        SERIES,
        (DAY0, DAY0 + MS_PER_DAY),
        NOW,
        &stop_second,
    );
    assert!(
        matches!(&stopped, Err(CollectError::Stopped(m)) if m.contains("2024-11-06")),
        "{stopped:?}"
    );

    let failed =
        backfill_market_rules_at(&hist, &source, SERIES, (DAY0, DAY0 + MS_PER_DAY), NOW, &|| false);
    assert!(
        matches!(&failed, Err(CollectError::Fetch(m)) if m.text().contains("2024-11-06") && m.text().contains("gamma timed out")),
        "{failed:?}"
    );
    assert_eq!(hist.scan_texts(SERIES.0, SERIES.1, TimestampRange::all()).unwrap().len(), 1);

    let stray = scripted(vec![(DAY0 + 2 * MS_PER_DAY, Ok(vec![TextRow::new("news", "x", DAY0)]))]);
    let refused = backfill_market_rules_at(
        &hist,
        &stray,
        SERIES,
        (DAY0 + 2 * MS_PER_DAY, DAY0 + 2 * MS_PER_DAY),
        NOW,
        &|| false,
    );
    assert!(matches!(refused, Err(CollectError::Refused(_))));
    assert!(matches!(
        backfill_market_rules_at(&hist, &source, SERIES, (2, 1), NOW, &|| false),
        Err(CollectError::Refused(_))
    ));
}

/// **Resume when a day's rows close on OTHER days** (an early close months before its scheduled end, a close
/// that falls in the NEXT day of the loop): the key is the day of market ENDS, never a row's time, so a stopped
/// request resumes at the day it stopped before, skips the stored day with no request, and stores no row twice.
#[test]
fn a_stopped_request_resumes_by_the_end_day_whatever_the_rows_close_times() {
    let (_dir, hist) = store();
    let day1 = DAY0 + MS_PER_DAY;
    let early = DAY0 - 75 * MS_PER_DAY;
    let source = scripted(vec![
        // Day 0's markets closed 75 days early and during day 1.
        (DAY0, Ok(vec![row(early, "early"), row(day1 + 3_600_000, "late")])),
        (day1, Ok(vec![row(day1 + 7_200_000, "on-time")])),
    ]);
    let asked = std::cell::Cell::new(0);
    let stop_at_day1 = || {
        asked.set(asked.get() + 1);
        asked.get() > 1
    };
    let stopped =
        backfill_market_rules_at(&hist, &source, SERIES, (DAY0, day1), NOW, &stop_at_day1);
    assert!(
        matches!(&stopped, Err(CollectError::Stopped(m)) if m.contains("2024-11-06")),
        "{stopped:?}"
    );

    let written =
        backfill_market_rules_at(&hist, &source, SERIES, (DAY0, day1), NOW, &|| false).unwrap();
    assert_eq!(written, 1, "only day 1 is left to store");
    assert_eq!(
        *source.asked.lock().unwrap(),
        [(DAY0, DAY0 + MS_PER_DAY - 1), (day1, day1 + MS_PER_DAY - 1)],
        "day 0 is never asked again, though its rows sit on other days"
    );
    let stored = hist.scan_texts(SERIES.0, SERIES.1, TimestampRange::all()).unwrap();
    let ids: Vec<&str> = stored.iter().map(|r| r.source_id.as_str()).collect();
    assert_eq!(ids, ["early", "late", "on-time"], "every market once, in close order");
    let at_day1 = hist.texts_as_of(SERIES.0, SERIES.1, day1, 365 * MS_PER_DAY).unwrap();
    assert_eq!(at_day1.len(), 1, "at day 1's start only the early close is known");
}

/// **A collector other than the archive's names ONE recurring series**: its day is asked of the series read
/// (never the archive walk), stored under that collector with a commit key of its own prefix, and a repeat
/// asks nothing; the archive series is untouched.
#[test]
fn a_series_collector_asks_its_series_and_keys_its_days_apart_from_the_archive() {
    let (_dir, hist) = store();
    let updown = (TEXT_KIND_POLYMARKET_RULES, "btc-updown-5m");
    let source =
        scripted(vec![(DAY0, Ok(vec![row(DAY0 + 300_000, "a"), row(DAY0 + 600_000, "b")]))]);
    let written =
        backfill_market_rules_at(&hist, &source, updown, (DAY0, DAY0 + 1), NOW, &|| false).unwrap();
    assert_eq!(written, 2);
    assert_eq!(
        *source.series_asked.lock().unwrap(),
        [("btc-updown-5m".to_string(), DAY0, DAY0 + MS_PER_DAY - 1)]
    );
    assert!(source.asked.lock().unwrap().is_empty(), "the archive walk is never asked");
    let series = SeriesId::per_symbol(StoreKindId::Text, updown.0, updown.1, None);
    let keys = [
        "market-rules-series:polymarket:btc-updown-5m:2024-11-05",
        "market-rules:polymarket:2024-11-05",
    ];
    assert_eq!(hist.series_has_commits(&series, &keys).unwrap(), [true, false]);
    assert_eq!(hist.scan_texts(updown.0, updown.1, TimestampRange::all()).unwrap().len(), 2);
    assert!(hist.scan_texts(SERIES.0, SERIES.1, TimestampRange::all()).unwrap().is_empty());

    let again =
        backfill_market_rules_at(&hist, &source, updown, (DAY0, DAY0), NOW, &|| false).unwrap();
    assert_eq!(again, 0);
    assert_eq!(source.series_asked.lock().unwrap().len(), 1, "a stored day is never asked again");
}
