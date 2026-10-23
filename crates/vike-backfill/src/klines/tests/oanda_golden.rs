//! **OANDA's day chunking, pinned byte for byte** — the windows it fetches, the commit keys and
//! markers it spends, and the words it logs and errors with, driven the way OANDA's lane drives the
//! chunked ingest (`oanda` / `EUR_USD`, one UTC day a chunk).
//!
//! It was written BEFORE the chunk-width knob (`chunk_days`) existed, against the ingest as it was,
//! and committed green; the knob's commit changed one thing in this file, the width argument OANDA's
//! row passes (one). Every literal below is the same bytes on both sides of that commit, which is
//! the proof that the knob changed nothing for the one lane that runs the chunked ingest in
//! production. Do not "update" a literal to make a failure pass: a changed key re-fetches and
//! duplicates a series for every operator who ever stored one.

use super::*;

/// The width OANDA's row passes (`crates/vike-datahub/src/backfill.rs`'s `real_backfill_table`): one
/// UTC day a chunk. The only line of this file that the knob's commit added.
const OANDA_CHUNK_DAYS: u32 = 1;
const OANDA: &str = "oanda";
const PAIR: &str = "EUR_USD";
const LABEL: &str = "oanda EUR_USD 1h";

fn oanda_series() -> SeriesId {
    SeriesId::per_symbol(StoreKindId::Bar, OANDA, PAIR, Some("1h".to_string()))
}

fn oanda_run(
    hist: &DataFusionHist,
    market: &FakeMarket,
    interval: &str,
    window: (i64, i64),
    now_ms: i64,
    should_stop: &dyn Fn() -> bool,
) -> Result<ChunkedOutcome, CollectError> {
    ingest_klines_chunked(
        hist,
        OANDA,
        PAIR,
        interval,
        window,
        OANDA_CHUNK_DAYS,
        now_ms,
        should_stop,
        |sym, iv, s, e| market.fetch(sym, iv, s, e).map_err(CollectError::from),
    )
}

/// The grid, the keys and the counts of a request that starts and ends mid-day: three whole days,
/// fetched in order, stored under the three literal keys below, and nothing else.
#[test]
fn oanda_fetches_whole_utc_days_under_the_day_grids_literal_keys() {
    let (_d, hist) = store();
    let market = FakeMarket::new(HOUR);

    let out =
        oanda_run(&hist, &market, "1h", (2 * DAY + 5 * HOUR, 4 * DAY + 2 * HOUR), LATER, &|| false)
            .unwrap();

    assert_eq!(
        market.calls(),
        vec![(172_800_000, 259_199_999), (259_200_000, 345_599_999), (345_600_000, 431_999_999),]
    );
    assert_eq!((out.chunks, out.fetched, out.skipped, out.empty, out.unsettled), (3, 3, 0, 0, 0));
    assert_eq!(out.rows, 72);
    let mut keys = hist.series_commits(&oanda_series()).unwrap();
    keys.sort();
    assert_eq!(
        keys,
        vec![
            "oanda:EUR_USD:1h:172800000-259199999".to_string(),
            "oanda:EUR_USD:1h:259200000-345599999".to_string(),
            "oanda:EUR_USD:1h:345600000-431999999".to_string(),
        ]
    );
}

/// An empty Saturday between two trading days spends its OWN marker key, `:empty` after the day's
/// key, and leaves the day's key free; a repeat fetches nothing.
#[test]
fn oanda_marks_an_empty_day_under_the_days_key_plus_empty() {
    let (_d, hist) = store();
    let market = FakeMarket::new(HOUR).closed_on(3);

    let first = oanda_run(&hist, &market, "1h", (2 * DAY, 5 * DAY - 1), LATER, &|| false).unwrap();

    assert_eq!(
        (first.chunks, first.fetched, first.empty, first.marked, first.rows),
        (3, 2, 1, 1, 48)
    );
    let spent = |key: &str| hist.series_has_commit(&oanda_series(), key).unwrap();
    assert!(spent("oanda:EUR_USD:1h:172800000-259199999"));
    assert!(spent("oanda:EUR_USD:1h:259200000-345599999:empty"), "the empty day's marker");
    assert!(!spent("oanda:EUR_USD:1h:259200000-345599999"), "...and the day's own key stays free");
    assert!(spent("oanda:EUR_USD:1h:345600000-431999999"));

    let again = ingest_klines_chunked(
        &hist,
        OANDA,
        PAIR,
        "1h",
        (2 * DAY, 5 * DAY - 1),
        OANDA_CHUNK_DAYS,
        LATER,
        &|| false,
        |_, _, _, _| panic!("a settled, stored or marked day must never be fetched again"),
    )
    .unwrap();
    assert_eq!((again.skipped, again.known_empty, again.fetched, again.rows), (3, 1, 0, 0));
}

/// A failed chunk names itself, the request and the bars already written, in exactly these words.
#[test]
fn oanda_failure_text_is_pinned() {
    let (_d, hist) = store();
    let market = FakeMarket::new(HOUR).failing_at(4 * DAY);

    let failed = oanda_run(&hist, &market, "1h", (2 * DAY, 6 * DAY - 1), LATER, &|| false);

    let Err(CollectError::Fetch(text)) = &failed else { panic!("must be a Fetch: {failed:?}") };
    assert_eq!(
        text,
        "chunk 3 of 4 [345600000, 431999999] failed after 48 1h bars were written (request \
         [172800000, 518399999]; the chunks before it stay stored, and repeating the request \
         resumes at this one): the venue went away"
    );
}

/// The three lines an operator reads when a request reaches "now": where storage stops and when
/// the rest settles, the one summary line, and a stop at a chunk boundary.
#[test]
fn oanda_log_lines_and_the_stop_report_are_pinned() {
    let (_d, hist) = store();
    let now = 4 * DAY + 12 * HOUR;
    let window = (2 * DAY, 6 * DAY - 1);
    let out = oanda_run(&hist, &FakeMarket::new(HOUR), "1h", window, now, &|| false).unwrap();

    assert_eq!(
        unsettled_line(&out, LABEL, now).unwrap(),
        "oanda EUR_USD 1h: storage stops before 1970-01-05T00:00:00Z (345600000). 2 of 4 day \
         chunks are not settled at 1970-01-05T12:00:00Z (388800000) and were NOT written; the \
         first settles at 1970-01-06T01:09:59Z (436199999). The recent edge is the live feed's \
         — ask again after that instant for the rest."
    );
    assert_eq!(
        summary_line(&out, LABEL, window, std::time::Duration::from_millis(1500)),
        "oanda EUR_USD 1h [172800000, 518399999]: 4 day chunks: 0 skipped (0 known empty), 2 \
         fetched, 0 empty (0 marked), 2 unsettled; 48 bars written in 1.5s"
    );

    let (_s, fresh) = store();
    let stopped =
        oanda_run(&fresh, &FakeMarket::new(HOUR), "1h", (2 * DAY, 4 * DAY - 1), LATER, &|| true);
    let Err(CollectError::Stopped(text)) = &stopped else { panic!("must be Stopped: {stopped:?}") };
    assert_eq!(
        text,
        "oanda EUR_USD 1h [172800000, 345599999]: asked to stop, and stopped before day chunk 1 of \
         2 [172800000, 259199999] (1970-01-03T00:00:00Z) — nothing failed. The 0 chunk(s) before \
         it are done (0 already stored, 0 known empty, 0 fetched and stored, 0 empty of which 0 \
         marked) and 0 1h bars were written; they stay stored. This chunk and the 1 after it \
         were never fetched: repeating the request resumes here."
    );
}

/// The refusal of a step nobody can measure, in the words it has always had.
#[test]
fn oanda_unmeasurable_interval_refusal_is_pinned() {
    let (_d, hist) = store();

    let refused = ingest_klines_chunked(
        &hist,
        OANDA,
        PAIR,
        "1w",
        (0, DAY - 1),
        OANDA_CHUNK_DAYS,
        LATER,
        &|| false,
        |_, _, _, _| panic!("a refused request fetched"),
    );

    let Err(CollectError::Refused(text)) = &refused else { panic!("must be Refused: {refused:?}") };
    assert_eq!(
        text,
        "interval \"1w\" has no bar width this store can measure (`vike_marketdata::interval::\
         interval_ms` reads a count plus one of s/m/h/d, so `1w`, `1M` and `1mo` are outside it). \
         Refused BEFORE fetching oanda EUR_USD: a day chunk is written only once its last bar has \
         closed, and a step nobody can measure has no close time to wait for. Nothing was fetched \
         or written and no commit key was spent."
    );
}
