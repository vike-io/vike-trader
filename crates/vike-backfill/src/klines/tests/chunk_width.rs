//! The chunk-width knob: `ingest_klines_chunked` at a `chunk_days` above one.
//!
//! Width one is every other test of this folder (and `oanda_golden`, which pins it byte for byte);
//! this file is the new behaviour only — the grid, the keys, the settle rule, the markers, the
//! strays, the refusals and the words, at a width of seven days (or twenty-eight, or the cap).

use super::*;

const WEEK: i64 = 7 * DAY;
const LABEL: &str = "fakevenue EURUSD 1h";

fn wide(
    hist: &DataFusionHist,
    market: &FakeMarket,
    window: (i64, i64),
    chunk_days: u32,
    now_ms: i64,
) -> Result<ChunkedOutcome, CollectError> {
    run_chunked_wide(hist, market, "1h", window, chunk_days, now_ms, &|| false)
}

/// Whether the chunk `[c0, c1]` has spent its EMPTY MARKER in `hist`.
fn marker_spent(hist: &DataFusionHist, c0: i64, c1: i64) -> bool {
    let key = empty_marker_key(CVENUE, CSYM, "1h", c0, c1);
    hist.series_has_commit(&chunk_series("1h"), &key).expect("read the manifest")
}

/// **The grid, on the pure helper.** A wide chunk is a multiple of its width from epoch 0: a window
/// inside one is that whole chunk, exactly one chunk is one chunk and not two, a millisecond past it
/// is two, a boundary straddled is both sides, and a pre-1970 stamp rounds DOWN like any other.
#[test]
fn the_wide_grid_is_a_multiple_of_the_width_anchored_at_epoch_zero() {
    assert_eq!(chunk_grid(3 * DAY, 4 * DAY, WEEK), Some((0, 0)), "inside the first week");
    assert_eq!(chunk_grid(0, WEEK - 1, WEEK), Some((0, 0)), "one chunk is one chunk");
    assert_eq!(chunk_grid(0, WEEK, WEEK), Some((0, WEEK)), "1 ms into the next chunk");
    assert_eq!(chunk_grid(WEEK - 1, WEEK, WEEK), Some((0, WEEK)), "a boundary straddled");
    assert_eq!(chunk_grid(-1, 0, WEEK), Some((-WEEK, 0)), "pre-1970 rounds DOWN");
    assert_eq!(chunk_grid(30 * DAY, 30 * DAY, 28 * DAY), Some((28 * DAY, 28 * DAY)));
    assert_eq!(chunk_grid(i64::MIN, 0, WEEK), None, "the first start would fall below i64::MIN");
    assert!(
        chunk_grid(0, i64::MAX - 2 * WEEK, WEEK).is_some(),
        "the edge is refused, not the range"
    );
}

/// **A request is fetched a chunk at a time under the grid's literal keys.** Days 10 to 20 at seven
/// days a chunk is the two weeks that hold them, days 7 to 20; one fetch each; the keys are the
/// shared format over the chunk's own bounds; and the request's ragged bounds mint none.
#[test]
fn a_request_is_rounded_outward_to_whole_weeks_and_fetched_a_week_at_a_time() {
    let (_d, hist) = store();
    let market = FakeMarket::new(HOUR);
    let window = (10 * DAY + 5 * HOUR, 20 * DAY);

    let out = wide(&hist, &market, window, 7, LATER).unwrap();

    assert_eq!(market.calls(), vec![(7 * DAY, 14 * DAY - 1), (14 * DAY, 21 * DAY - 1)]);
    assert_eq!((out.chunks, out.chunk_days, out.fetched, out.skipped), (2, 7, 2, 0));
    assert_eq!(out.rows, 14 * 24);
    assert_partitions(&out);
    let mut keys = hist.series_commits(&chunk_series("1h")).unwrap();
    keys.sort();
    assert_eq!(
        keys,
        vec![
            "fakevenue:EURUSD:1h:1209600000-1814399999".to_string(),
            "fakevenue:EURUSD:1h:604800000-1209599999".to_string(),
        ]
    );
    assert!(!spent(&hist, "1h", window.0, window.1), "a ragged request bound is never a key");
    let bars = stored_at(&hist, "1h");
    assert_eq!(bars.first().unwrap().0, 7 * DAY, "the bars before the request's start are stored");
    assert_eq!(bars.last().unwrap().0, 21 * DAY - HOUR, "...and the bars after its end");
}

/// **A repeat fetches nothing, and a resume starts at the failed chunk** — at a wide width, the same
/// two properties the day grid has.
#[test]
fn a_repeat_skips_stored_wide_chunks_and_a_retry_resumes_at_the_failed_one() {
    let (_d, hist) = store();
    let window = (7 * DAY, 28 * DAY - 1);
    let flaky = FakeMarket::new(HOUR).failing_at(14 * DAY);

    let failed = wide(&hist, &flaky, window, 7, LATER);

    let Err(CollectError::Fetch(text)) = &failed else { panic!("must be a Fetch: {failed:?}") };
    assert!(text.contains("chunk 2 of 3"), "{text}");
    assert!(text.contains(&format!("[{}, {}]", 14 * DAY, 21 * DAY - 1)), "{text}");
    assert!(text.contains("after 168 1h bars were written"), "{text}");
    assert_eq!(stored_at(&hist, "1h").len(), 168, "the chunk before the failure stays written");

    let healthy = FakeMarket::new(HOUR);
    let retried = wide(&hist, &healthy, window, 7, LATER).unwrap();

    assert_eq!(healthy.calls(), vec![(14 * DAY, 21 * DAY - 1), (21 * DAY, 28 * DAY - 1)]);
    assert_eq!((retried.skipped, retried.fetched, retried.rows), (1, 2, 336));
    let (_c, control) = store();
    wide(&control, &FakeMarket::new(HOUR), window, 7, LATER).unwrap();
    assert_eq!(stored_at(&hist, "1h"), stored_at(&control, "1h"), "resumed equals uninterrupted");

    let again = ingest_klines_chunked(
        &hist,
        CVENUE,
        CSYM,
        "1h",
        window,
        7,
        LATER,
        &|| false,
        |_, _, _, _| panic!("a stored wide chunk must not be fetched again"),
    )
    .unwrap();
    assert_eq!((again.skipped, again.fetched, again.rows), (3, 0, 0));
}

/// **The settle rule judges the END of the wide chunk.** A chunk is written only once its last
/// possible bar — the one that opens at `c1` — has closed and the margin has passed, so a week that
/// is half over is not stored, and the report says the recent edge is a whole chunk behind.
#[test]
fn a_wide_chunk_settles_only_when_its_last_day_has_closed() {
    let window = (7 * DAY, 21 * DAY - 1);
    let first_settles = 14 * DAY - 1 + HOUR + SETTLE_MARGIN_MS;
    for (now, written) in
        [(LATER, 2_i64), (first_settles, 1), (first_settles - 1, 0), (10 * DAY, 0)]
    {
        let (_d, hist) = store();
        let market = FakeMarket::new(HOUR);
        let context = format!("now = {now}");

        let out = wide(&hist, &market, window, 7, now).unwrap();

        assert_eq!(out.rows as i64, written * 7 * 24, "{context}");
        assert_eq!((out.chunks, out.fetched), (2, written as u64), "{context}");
        assert_eq!(out.unsettled, 2 - written as u64, "{context}");
        assert_partitions(&out);
        assert_eq!(market.calls().len() as i64, written, "unsettled chunks are never fetched");
        if written < 2 {
            let first_unsettled = (7 + 7 * written) * DAY;
            assert_eq!(out.stops_before, Some(first_unsettled), "{context}");
            assert_eq!(
                out.settles_at,
                Some(first_unsettled + WEEK - 1 + HOUR + SETTLE_MARGIN_MS),
                "{context}"
            );
            assert!(!spent(&hist, "1h", first_unsettled, first_unsettled + WEEK - 1), "{context}");
        }
    }
}

/// **A wholly empty chunk is marked, once a later one holds data, under the CHUNK's bounds.** A
/// week with no candle at all (a holiday, a venue's closed stretch) spends `{key}:empty` for the
/// whole week and never its own key; a repeat is answered from the manifest.
#[test]
fn an_empty_wide_chunk_is_marked_under_its_own_bounds_and_never_asked_again() {
    let (_d, hist) = store();
    let mut market = FakeMarket::new(HOUR);
    for day in 7..14 {
        market = market.closed_on(day);
    }
    let window = (7 * DAY, 21 * DAY - 1);

    let first = wide(&hist, &market, window, 7, LATER).unwrap();

    assert_eq!(
        (first.chunks, first.fetched, first.empty, first.marked, first.rows),
        (2, 1, 1, 1, 168)
    );
    assert_partitions(&first);
    assert!(marker_spent(&hist, 7 * DAY, 14 * DAY - 1), "the empty week's marker");
    assert!(!spent(&hist, "1h", 7 * DAY, 14 * DAY - 1), "...and the week's own key stays free");
    assert!(spent(&hist, "1h", 14 * DAY, 21 * DAY - 1));

    let again = ingest_klines_chunked(
        &hist,
        CVENUE,
        CSYM,
        "1h",
        window,
        7,
        LATER,
        &|| false,
        |_, _, _, _| panic!("a marked wide chunk must never be fetched"),
    )
    .unwrap();
    assert_eq!((again.skipped, again.known_empty, again.fetched, again.rows), (2, 1, 0, 0));
}

/// **Strays are judged against the wide chunk.** A venue that serves three bars past each end of the
/// window it was asked for would store the seam bars in two adjacent chunks; dropped, every
/// timestamp is stored once and the store equals a well-behaved venue's.
#[test]
fn bars_outside_a_wide_chunk_are_dropped_counted_and_never_stored_twice() {
    let (_d, hist) = store();
    let window = (7 * DAY, 21 * DAY - 1);

    let out = wide(&hist, &FakeMarket::new(HOUR).straying(3), window, 7, LATER).unwrap();

    assert_eq!(out.out_of_range, 2 * (3 + 3), "three either side of each of the two chunks");
    assert_eq!(out.rows, 14 * 24);
    let stored = stored_at(&hist, "1h");
    assert!(stored.windows(2).all(|pair| pair[0].0 < pair[1].0), "a bar was stored twice");
    let (_c, clean) = store();
    wide(&clean, &FakeMarket::new(HOUR), window, 7, LATER).unwrap();
    assert_eq!(stored, stored_at(&clean, "1h"), "the strays left no trace in the store");
}

/// **Content equality with one big fetch, at a wide width** — for steps that divide a day, one that
/// does not, and a step of a day: the union of the wide chunks is the rounded window, and a bar
/// belongs to the chunk holding its OPEN time.
#[test]
fn the_wide_store_holds_what_one_big_fetch_holds() {
    for interval in ["1m", "1h", "7h", "1d"] {
        let step = vike_marketdata::interval::interval_ms(interval).unwrap();
        let market = FakeMarket::new(step);
        let (_d, chunked) = store();
        run_chunked_wide(
            &chunked,
            &market,
            interval,
            (9 * DAY + 5 * HOUR, 16 * DAY + 2 * HOUR),
            7,
            LATER,
            &|| false,
        )
        .unwrap();

        let (_c, control) = store();
        ingest_klines(&control, CVENUE, CSYM, interval, 7 * DAY, 21 * DAY - 1, |sym, iv, s, e| {
            market.fetch(sym, iv, s, e).map_err(CollectError::from)
        })
        .unwrap();

        let got = stored_at(&chunked, interval);
        assert!(!got.is_empty(), "{interval}: nothing was stored, so nothing was compared");
        assert_eq!(got, stored_at(&control, interval), "{interval}");
    }
}

/// **A width of zero, above the cap, or absurd is refused BEFORE anything** — no fetch, no manifest,
/// no series — through the entry point the lane uses as well; the cap itself is accepted.
#[test]
fn a_width_outside_one_to_the_cap_is_refused_before_anything_is_fetched() {
    let (_d, hist) = store();
    for width in [0, MAX_CHUNK_DAYS + 1, u32::MAX] {
        let refused = ingest_klines_chunked(
            &hist,
            CVENUE,
            CSYM,
            "1h",
            (0, DAY - 1),
            width,
            LATER,
            &|| false,
            |_, _, _, _| panic!("a refused width fetched"),
        );
        let Err(CollectError::Refused(why)) = &refused else {
            panic!("width {width} must be refused: {refused:?}")
        };
        assert!(why.contains("chunk width"), "width {width}: {why}");
        let face = crate::kline_source::backfill_kline_source_chunked_at(
            &hist,
            &NeverFetches,
            CSYM,
            "1h",
            0,
            DAY - 1,
            width,
            LATER,
            &|| false,
        );
        assert!(matches!(face, Err(CollectError::Refused(_))), "the face, width {width}: {face:?}");
    }
    assert!(hist.list_series().unwrap().is_empty(), "a refused request touched the store");

    let at_the_cap =
        wide(&hist, &FakeMarket::new(HOUR), (0, DAY - 1), MAX_CHUNK_DAYS, LATER).unwrap();
    assert_eq!((at_the_cap.chunks, at_the_cap.fetched), (1, 1), "the cap itself is a width");
}

/// **The words say the width.** At one day (or a default-built outcome) the nouns are the ones an
/// operator has always grepped; at seven they are `7-day chunk`, in the summary, the unsettled
/// line and the stop report alike.
#[test]
fn the_log_words_name_the_width_when_it_is_not_one() {
    let (_d, hist) = store();
    let now = 14 * DAY + 12 * HOUR;
    let window = (7 * DAY, 21 * DAY - 1);
    let out = wide(&hist, &FakeMarket::new(HOUR), window, 7, now).unwrap();

    let summary = summary_line(&out, LABEL, window, std::time::Duration::from_millis(1500));
    assert!(summary.contains(": 2 7-day chunks: 0 skipped"), "{summary}");
    let unsettled = unsettled_line(&out, LABEL, now).expect("one chunk was left unwritten");
    assert!(unsettled.contains("1 of 2 7-day chunks are not settled"), "{unsettled}");

    let one = ChunkedOutcome { chunks: 4, chunk_days: 1, ..ChunkedOutcome::default() };
    let unset = ChunkedOutcome { chunks: 4, ..ChunkedOutcome::default() };
    for plain in [&one, &unset] {
        let line = summary_line(plain, LABEL, window, std::time::Duration::ZERO);
        assert!(line.contains(": 4 day chunks: 0 skipped"), "{line}");
    }

    let (_s, fresh) = store();
    let stopped =
        run_chunked_wide(&fresh, &FakeMarket::new(HOUR), "1h", window, 7, LATER, &|| true);
    let Err(CollectError::Stopped(text)) = &stopped else { panic!("must be Stopped: {stopped:?}") };
    assert!(text.contains("stopped before 7-day chunk 1 of 2 [604800000, 1209599999]"), "{text}");
}
