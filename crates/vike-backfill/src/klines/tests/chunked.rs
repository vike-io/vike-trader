//! `ingest_klines_chunked`: the day grid, settling, resume, refusals, strays and tallies.

use super::*;
use std::assert_matches;

// ── THE CHUNKED INGEST (`ingest_klines_chunked`) ───────────────────────────────────────────────
//
// A fake venue that counts and records every fetch, over a real `DataFusionHist` in a temp dir.
// Every test hands the ingest an EXPLICIT `now_ms`, so none of them depends on when it runs: their
// windows live in the 1970s and `LATER` puts the clock far past all of them — except where a test
// aims at the settle boundary and says so, and the one that reads the wall clock
// (`the_public_entry_reads_the_wall_clock_and_leaves_today_alone`), which asserts only what holds at
// any time of any UTC day.

/// **Outward rounding, in the store.** A request that starts and ends mid-day is fetched as the
/// three WHOLE days it touches, one call each, and stored under the grid's own keys — never under
/// the request's ragged bounds, and holding the bars before the request's start too: a request for
/// part of a day stores the whole day.
#[test]
fn a_request_is_rounded_outward_to_whole_days_and_fetched_a_day_at_a_time() {
    let (_d, hist) = store();
    let market = FakeMarket::new(HOUR);
    let (start, end) = (2 * DAY + 5 * HOUR, 4 * DAY + 2 * HOUR);

    let out = run_chunked(&hist, &market, "1h", (start, end), LATER).unwrap();

    assert_eq!(
        market.calls(),
        vec![(2 * DAY, 3 * DAY - 1), (3 * DAY, 4 * DAY - 1), (4 * DAY, 5 * DAY - 1)],
        "one fetch per whole day, in order — each chunk is one day, and the daemon holds one at a time"
    );
    assert_eq!((out.chunks, out.fetched, out.skipped, out.empty, out.unsettled), (3, 3, 0, 0, 0));
    assert_eq!(out.rows, 72);
    assert_partitions(&out);

    let bars = stored_at(&hist, "1h");
    assert_eq!(bars.len(), 72, "three whole days of hourly bars");
    assert_eq!(bars.first().unwrap().0, 2 * DAY, "the bars BEFORE the request's start are stored");
    assert_eq!(bars.last().unwrap().0, 5 * DAY - HOUR, "...and the bars AFTER its end");

    // The key is the shared format over the chunk's own bounds, spelled out; the request's own
    // bounds mint none.
    assert_eq!(
        commit_key(CVENUE, CSYM, "1h", 2 * DAY, 3 * DAY - 1),
        "fakevenue:EURUSD:1h:172800000-259199999"
    );
    let mut keys = hist.series_commits(&chunk_series("1h")).unwrap();
    keys.sort();
    assert_eq!(
        keys,
        vec![
            "fakevenue:EURUSD:1h:172800000-259199999".to_string(),
            "fakevenue:EURUSD:1h:259200000-345599999".to_string(),
            "fakevenue:EURUSD:1h:345600000-431999999".to_string(),
        ],
        "exactly the three grid chunks' keys"
    );
    assert!(!spent(&hist, "1h", start, end), "a ragged request bound is never a key");
}

/// The grid itself, on the pure helper: a window inside one day is that whole day, exactly one day is
/// one chunk and not two, one millisecond into the next day is the whole next day, a midnight
/// straddled by a millisecond each side is both days — and a pre-1970 stamp rounds DOWN like any
/// other, which is why the grid uses `div_euclid` rather than `/`.
#[test]
fn the_chunk_grid_rounds_outward_and_never_cuts_a_day() {
    assert_eq!(chunk_grid(2 * DAY + 3 * HOUR, 2 * DAY + 4 * HOUR, DAY), Some((2 * DAY, 2 * DAY)));
    assert_eq!(
        chunk_grid(2 * DAY, 3 * DAY - 1, DAY),
        Some((2 * DAY, 2 * DAY)),
        "one day is one chunk"
    );
    assert_eq!(
        chunk_grid(2 * DAY, 3 * DAY, DAY),
        Some((2 * DAY, 3 * DAY)),
        "1 ms into the next day"
    );
    assert_eq!(chunk_grid(2 * DAY - 1, 2 * DAY, DAY), Some((DAY, 2 * DAY)), "a midnight straddled");
    assert_eq!(chunk_grid(-1, 0, DAY), Some((-DAY, 0)), "pre-1970 rounds DOWN, not toward zero");
    assert_eq!(chunk_grid(-DAY, -1, DAY), Some((-DAY, -DAY)));
    assert_eq!(chunk_grid(i64::MIN, 0, DAY), None, "the first start would fall below i64::MIN");
    assert_eq!(chunk_grid(0, i64::MAX, DAY), None, "the last chunk's END would pass i64::MAX");
    assert!(
        chunk_grid(0, i64::MAX - DAY, DAY).is_some(),
        "the edge is refused, not the whole range"
    );
}

/// **The rule, on the pure helper.** A chunk is settled when its last possible bar — the one that
/// opens at `c1` — has closed AND the margin has passed, so all three terms are in it, and at the
/// instant itself it IS settled. Pinned here on exact values, then again through the store below.
#[test]
fn the_settle_rule_is_the_close_of_the_last_possible_bar_plus_the_margin() {
    let (c1, step) = (5 * DAY - 1, HOUR);
    let at = c1 + step + SETTLE_MARGIN_MS;
    assert_eq!(settle_instant(c1, step), at);
    assert!(chunk_is_settled(c1, step, at), "AT the instant it is settled");
    assert!(!chunk_is_settled(c1, step, at - 1), "one millisecond before it, it is not");
    assert!(!chunk_is_settled(c1, step, c1 + SETTLE_MARGIN_MS), "the last bar has not closed yet");
    assert!(!chunk_is_settled(c1, step, c1 + step), "the margin has not passed yet");
    assert!(!chunk_is_settled(c1, DAY, at), "a longer step closes later, so it settles later");
    assert!(!chunk_is_settled(i64::MAX, HOUR, i64::MAX - 1), "saturating, never wrapped");
}

/// **A repeat of a stored request fetches NOTHING.** The strongest form of the skip: the second
/// run's fetch panics if it is called at all — not merely counted — and every chunk is answered from
/// the manifest.
#[test]
fn a_repeat_of_a_stored_request_fetches_nothing() {
    let (_d, hist) = store();
    let window = (2 * DAY + 5 * HOUR, 4 * DAY + 2 * HOUR);
    let first = run_chunked(&hist, &FakeMarket::new(HOUR), "1h", window, LATER).unwrap();
    assert_eq!(first.rows, 72);
    let before = stored_at(&hist, "1h");

    let again = ingest_klines_chunked(
        &hist,
        CVENUE,
        CSYM,
        "1h",
        window,
        1,
        LATER,
        &|| false,
        |_, _, _, _| panic!("a settled chunk whose commit key is spent must not be fetched at all"),
    )
    .unwrap();

    assert_eq!((again.rows, again.skipped, again.fetched, again.chunks), (0, 3, 0, 3));
    assert_partitions(&again);
    assert_eq!(stored_at(&hist, "1h"), before, "and nothing was written twice");
}

/// **Resume.** Chunk 3 of 4 fails: the error names the chunk, the request and the bars already
/// written; the chunks before it stay written; the chunk after it is never asked for. A retry then
/// fetches ONLY the failed chunk and the one after it, and the store ends up holding exactly what an
/// uninterrupted run would have.
#[test]
fn a_failed_chunk_keeps_the_chunks_before_it_and_a_retry_resumes_at_it() {
    let (_d, hist) = store();
    let (start, end) = (2 * DAY, 6 * DAY - 1);
    let flaky = FakeMarket::new(HOUR).failing_at(4 * DAY);

    let failed = run_chunked(&hist, &flaky, "1h", (start, end), LATER);

    let Err(CollectError::Fetch(text)) = &failed else {
        panic!("a fetch failure must answer as a Fetch: {failed:?}")
    };
    for part in [
        "chunk 3 of 4".to_string(),
        format!("[{}, {}]", 4 * DAY, 5 * DAY - 1),
        format!("(request [{start}, {end}]"),
        "after 48 1h bars were written".to_string(),
        "the venue went away".to_string(),
    ] {
        assert!(text.contains(&part), "the error must carry {part:?}: {text}");
    }
    assert_eq!(
        flaky.calls(),
        vec![(2 * DAY, 3 * DAY - 1), (3 * DAY, 4 * DAY - 1), (4 * DAY, 5 * DAY - 1)],
        "the chunk after the failed one is never asked for"
    );
    let kept = stored_at(&hist, "1h");
    assert_eq!(kept.len(), 48, "the chunks before the failure stay written");
    assert_eq!(kept.last().unwrap().0, 4 * DAY - HOUR);

    let healthy = FakeMarket::new(HOUR);
    let retried = run_chunked(&hist, &healthy, "1h", (start, end), LATER).unwrap();

    assert_eq!(
        healthy.calls(),
        vec![(4 * DAY, 5 * DAY - 1), (5 * DAY, 6 * DAY - 1)],
        "a retry skips what was written and resumes at the chunk that failed"
    );
    assert_eq!((retried.rows, retried.skipped, retried.fetched), (48, 2, 2));
    assert_partitions(&retried);
    let (_c, control) = store();
    run_chunked(&control, &FakeMarket::new(HOUR), "1h", (start, end), LATER).unwrap();
    assert_eq!(
        stored_at(&hist, "1h"),
        stored_at(&control, "1h"),
        "interrupted then resumed is what one uninterrupted run stores"
    );
}

/// A `Refused` from the source is a refusal and stays one — at the first chunk and at the third — and
/// the chunks before it stay written. Driven through [`backfill_kline_source_chunked_at`], the
/// `KlineSource` face, so it also proves `CollectError::from` is what carries the source's error in.
#[test]
fn a_refusal_from_the_source_passes_through_unchanged_at_any_chunk() {
    for (refused_at, written) in [(2 * DAY, 0), (4 * DAY, 48)] {
        let (_d, hist) = store();
        let market = FakeMarket::new(HOUR).failing_at(refused_at).refusing();

        let err = crate::kline_source::backfill_kline_source_chunked_at(
            &hist,
            &market,
            CSYM,
            "1h",
            2 * DAY,
            6 * DAY - 1,
            1,
            LATER,
            &|| false,
        )
        .unwrap_err();

        assert_matches!(
            &err, CollectError::Refused(why) if why == "the venue went away",
            "a refusal must come back as the source's own Refused, not a Fetch: {err:?}"
        );
        assert_eq!(stored_at(&hist, "1h").len(), written, "refused at day {}", refused_at / DAY);
    }
}

/// **The settled-only policy, through the store, on and around the exact boundary.** Days 2..=5 are
/// requested. Day 3's last possible bar (the 23:00 one) closes at day 4's midnight and the chunk is
/// settled a margin after that: AT that instant days 2 and 3 are written, one millisecond earlier
/// only day 2 is, and long before it nothing is. The unsettled chunk and everything after it are
/// never fetched and never spend a key; the call still answers `Ok`, and the outcome — which is what
/// the info log is built from — says where storage stops and when the first unwritten chunk settles.
#[test]
fn an_unsettled_tail_is_not_written_and_is_reported() {
    let (start, end) = (2 * DAY, 6 * DAY - 1);
    let day3_settles = 4 * DAY - 1 + HOUR + SETTLE_MARGIN_MS;
    for (now, written) in [(LATER, 4_i64), (day3_settles, 2), (day3_settles - 1, 1), (2 * DAY, 0)] {
        let (_d, hist) = store();
        let market = FakeMarket::new(HOUR);
        let context = format!("now = {now}");

        let out = run_chunked(&hist, &market, "1h", (start, end), now).unwrap();

        assert_eq!(out.rows as i64, written * 24, "{context}");
        assert_eq!((out.chunks, out.fetched, out.skipped, out.empty), (4, written as u64, 0, 0));
        assert_eq!(out.unsettled, 4 - written as u64, "{context}");
        assert_partitions(&out);
        assert_eq!(
            market.calls().len() as i64,
            written,
            "unsettled chunks are never fetched: {context}"
        );
        let ts: Vec<i64> = stored_at(&hist, "1h").iter().map(|b| b.0).collect();
        assert_eq!(ts.first().copied(), (written > 0).then_some(2 * DAY), "{context}");
        assert_eq!(
            ts.last().copied(),
            (written > 0).then_some((2 + written) * DAY - HOUR),
            "{context}"
        );
        if written < 4 {
            let first_unsettled = (2 + written) * DAY;
            assert_eq!(out.stops_before, Some(first_unsettled), "{context}");
            assert_eq!(
                out.settles_at,
                Some(first_unsettled + DAY - 1 + HOUR + SETTLE_MARGIN_MS),
                "the instant the first unwritten chunk settles: {context}"
            );
            assert!(
                !spent(&hist, "1h", first_unsettled, first_unsettled + DAY - 1),
                "an unsettled chunk spends no key: {context}"
            );
        } else {
            assert_eq!((out.stops_before, out.settles_at), (None, None), "{context}");
        }
    }
}

/// **The log lines say what the outcome knows, in the words an operator reads.** The same request as
/// the settle-boundary test, with "now" at noon on day 4: days 2 and 3 are stored, days 4 and 5 are
/// not. The line names where storage stops (day 4's midnight, in UTC and in milliseconds), how many of
/// how many chunks were left, "now", and the instant the first of them settles — a step and a margin
/// past day 4's last millisecond, truncated to the second. A fully settled request has no such line,
/// and the one summary line carries every count.
#[test]
fn the_log_says_where_storage_stops_and_when_the_rest_settles() {
    let (_d, hist) = store();
    let now = 4 * DAY + 12 * HOUR; // 1970-01-05T12:00:00Z
    let out =
        run_chunked(&hist, &FakeMarket::new(HOUR), "1h", (2 * DAY, 6 * DAY - 1), now).unwrap();
    let label = "fakevenue EURUSD 1h";

    let line = unsettled_line(&out, label, now).expect("two chunks were left unwritten");

    assert!(
        line.starts_with(
            "fakevenue EURUSD 1h: storage stops before 1970-01-05T00:00:00Z (345600000)."
        ),
        "{line}"
    );
    assert!(
        line.contains("2 of 4 day chunks are not settled at 1970-01-05T12:00:00Z (388800000)"),
        "{line}"
    );
    assert!(line.contains("the first settles at 1970-01-06T01:09:59Z (436199999)"), "{line}");
    assert!(line.contains("live feed"), "it says who owns the recent edge: {line}");

    let everything =
        run_chunked(&hist, &FakeMarket::new(HOUR), "1h", (2 * DAY, 6 * DAY - 1), LATER).unwrap();
    assert_eq!(unsettled_line(&everything, label, LATER), None, "nothing was left unwritten");

    let window = (2 * DAY, 6 * DAY - 1);
    let summary = summary_line(&out, label, window, std::time::Duration::from_millis(1500));
    assert_eq!(
        summary,
        "fakevenue EURUSD 1h [172800000, 518399999]: 4 day chunks: 0 skipped (0 known empty), \
         2 fetched, 0 empty (0 marked), 2 unsettled; 48 bars written in 1.5s"
    );
    let with_strays = ChunkedOutcome { out_of_range: 6, ..ChunkedOutcome::default() };
    let elapsed = std::time::Duration::ZERO;
    assert!(
        summary_line(&with_strays, label, window, elapsed)
            .ends_with("; 6 out-of-range bars dropped")
    );
    assert!(!summary.contains("out-of-range"), "a request with no strays says nothing of them");
}

/// A step the store cannot measure — and a zero-width one, which measures — is refused BEFORE any
/// fetch and before any manifest is touched: the fetch panics if it is called, and no series exists
/// afterwards. Through both entry points, the `KlineSource` face with a source that panics too. The
/// two refusals say different things, and the test holds them to it: a zero-width step is ALSO
/// something `interval_ms` cannot make a bar of, so without the message the predicate's own check
/// could be deleted and this would stay green.
#[test]
fn an_unmeasurable_or_zero_width_interval_is_refused_before_anything_is_fetched_chunked() {
    let (_d, hist) = store();
    let cases = [
        ("1w", "no bar width"),
        ("1M", "no bar width"),
        ("1mo", "no bar width"),
        ("", "no bar width"),
        ("x", "no bar width"),
        ("5", "no bar width"),
        ("0m", "zero width"),
        ("0s", "zero width"),
        ("00d", "zero width"),
    ];
    for (interval, says) in cases {
        let refused = ingest_klines_chunked(
            &hist,
            CVENUE,
            CSYM,
            interval,
            (0, DAY - 1),
            1,
            LATER,
            &|| false,
            |_, _, _, _| panic!("the chunked ingest fetched {interval:?}"),
        );
        let Err(CollectError::Refused(why)) = &refused else {
            panic!("{interval:?} must be refused, got {refused:?}")
        };
        assert!(why.contains(says), "{interval:?} must be refused as {says:?}: {why}");
        let dispatched = crate::kline_source::backfill_kline_source_chunked_at(
            &hist,
            &NeverFetches,
            CSYM,
            interval,
            0,
            DAY - 1,
            1,
            LATER,
            &|| false,
        );
        assert_matches!(
            dispatched,
            Err(CollectError::Refused(_)),
            "the KlineSource face answered {interval:?} with {dispatched:?}"
        );
    }
    assert!(hist.list_series().unwrap().is_empty(), "a refused request touched the store");
}

/// The other refusals, also before any fetch: a symbol that cannot be a directory name, an inverted
/// window, and a window the day grid cannot express at the edge of `i64`.
#[test]
fn a_hostile_symbol_an_inverted_window_and_an_unrepresentable_window_are_refused_first() {
    let (_d, hist) = store();
    let panics = |_: &str, _: &str, _: i64, _: i64| -> Result<Vec<Bar>, CollectError> {
        panic!("a refused request fetched")
    };
    for symbol in ["../x", "a/b", "a\\b", "EUR:USD", "EUR?"] {
        let refused = ingest_klines_chunked(
            &hist,
            CVENUE,
            symbol,
            "1h",
            (0, DAY - 1),
            1,
            LATER,
            &|| false,
            panics,
        );
        assert_matches!(refused, Err(CollectError::Refused(_)), "{symbol:?}: {refused:?}");
    }
    for window in [(DAY, 0), (0, i64::MAX), (i64::MIN, 0)] {
        let refused =
            ingest_klines_chunked(&hist, CVENUE, CSYM, "1h", window, 1, LATER, &|| false, panics);
        assert_matches!(refused, Err(CollectError::Refused(_)), "{window:?}: {refused:?}");
    }
    assert!(hist.list_series().unwrap().is_empty(), "a refused request touched the store");
}

/// **Bars outside the chunk are dropped.** A venue that serves three bars past each end of the window
/// it was asked for would, undropped, store the seam bars in TWO adjacent chunks — the store dedups by
/// commit key, never by row. Dropped, the store holds exactly what a well-behaved venue's run holds,
/// every timestamp once, and the strays are counted.
#[test]
fn bars_outside_the_chunk_are_dropped_counted_and_never_stored_twice() {
    let (_d, hist) = store();
    let (start, end) = (2 * DAY, 5 * DAY - 1);

    let out =
        run_chunked(&hist, &FakeMarket::new(HOUR).straying(3), "1h", (start, end), LATER).unwrap();

    assert_eq!(out.out_of_range, 3 * (3 + 3), "three either side of each of the three chunks");
    assert_eq!(out.rows, 72);
    let stored = stored_at(&hist, "1h");
    assert!(stored.windows(2).all(|pair| pair[0].0 < pair[1].0), "a bar was stored twice");
    let (_c, clean) = store();
    run_chunked(&clean, &FakeMarket::new(HOUR), "1h", (start, end), LATER).unwrap();
    assert_eq!(stored, stored_at(&clean, "1h"), "the strays left no trace in the store");
}

/// A chunk the source answers ONLY with strays — every bar it served is outside the day it was asked
/// for — is an empty chunk: nothing written, no key spent, every stray counted.
#[test]
fn a_chunk_answered_only_with_strays_is_empty_and_spends_no_key() {
    let (_d, hist) = store();
    let elsewhere = vec![market_bar(10 * DAY, HOUR), market_bar(10 * DAY + HOUR, HOUR)];

    let out = ingest_klines_chunked(
        &hist,
        CVENUE,
        CSYM,
        "1h",
        (2 * DAY, 3 * DAY - 1),
        1,
        LATER,
        &|| false,
        |_, _, _, _| Ok(elsewhere.clone()),
    )
    .unwrap();

    assert_eq!((out.chunks, out.empty, out.fetched, out.rows, out.out_of_range), (1, 1, 0, 0, 2));
    assert_partitions(&out);
    assert!(!spent(&hist, "1h", 2 * DAY, 3 * DAY - 1), "an all-strays chunk spent a key");
    assert!(stored_at(&hist, "1h").is_empty(), "a stray was stored");
}

/// **Content equality with one big fetch.** For steps that divide a day, steps that do not, a step of
/// a day and a step longer than one, the chunked store holds bit for bit what a single per-window
/// ingest of the same rounded window holds — a bar belongs to the chunk holding its OPEN time, and the
/// union of the chunks is the window.
#[test]
fn the_chunked_store_holds_what_one_big_fetch_holds() {
    for interval in ["1m", "1h", "7h", "1d", "2d"] {
        let step = vike_marketdata::interval::interval_ms(interval).unwrap();
        let market = FakeMarket::new(step);
        let (_d, chunked) = store();
        run_chunked(&chunked, &market, interval, (3 * DAY + 5 * HOUR, 7 * DAY + 2 * HOUR), LATER)
            .unwrap();

        let (_c, control) = store();
        ingest_klines(&control, CVENUE, CSYM, interval, 3 * DAY, 8 * DAY - 1, |sym, iv, s, e| {
            market.fetch(sym, iv, s, e).map_err(CollectError::from)
        })
        .unwrap();

        let got = stored_at(&chunked, interval);
        assert!(!got.is_empty(), "{interval}: nothing was stored, so nothing was compared");
        assert_eq!(got, stored_at(&control, interval), "{interval}");
    }
}

/// One request that meets every kind of chunk: day 2 was stored by an earlier request (skipped),
/// day 3 has no candles (empty), days 4..=6 are fetched, days 7 and 8 are not yet settled. The tally
/// says so, and says where storage stops.
#[test]
fn a_mixed_request_tallies_every_kind_of_chunk() {
    let (_d, hist) = store();
    run_chunked(&hist, &FakeMarket::new(HOUR).closed_on(3), "1h", (2 * DAY, 3 * DAY - 1), LATER)
        .unwrap();
    // Day 6 settles a step and a margin after its last millisecond; day 7 does not.
    let now = 7 * DAY - 1 + HOUR + SETTLE_MARGIN_MS;
    let market = FakeMarket::new(HOUR).closed_on(3);

    let out = run_chunked(&hist, &market, "1h", (2 * DAY, 9 * DAY - 1), now).unwrap();

    assert_eq!(
        (out.chunks, out.skipped, out.empty, out.fetched, out.unsettled),
        (7, 1, 1, 3, 2),
        "{out:?}"
    );
    assert_partitions(&out);
    assert_eq!(out.marked, 1, "day 3 is marked by day 4, the later day that held data: {out:?}");
    assert_eq!(out.rows, 72);
    assert_eq!(out.stops_before, Some(7 * DAY));
    assert_eq!(out.settles_at, Some(8 * DAY - 1 + HOUR + SETTLE_MARGIN_MS));
    let asked: Vec<i64> = market.calls().iter().map(|(from, _)| from / DAY).collect();
    assert_eq!(
        asked,
        vec![3, 4, 5, 6],
        "day 2 skipped, days 7 and 8 unsettled, the rest asked for"
    );
}

/// **The public entry point reads a clock** — the wall clock, once — and takes its venue from the
/// source. Placed against today's UTC midnight and asserting only what is true at any moment of any
/// day: the two days before yesterday are always settled and always written, and today, which cannot
/// be settled yet, is never fetched. (Yesterday is not asserted: it settles a step and a margin past
/// midnight, so which side of it a run lands on depends on when the test starts.)
#[test]
fn the_public_entry_reads_the_wall_clock_and_leaves_today_alone() {
    let (_d, hist) = store();
    let market = FakeMarket::new(HOUR);
    let now = vike_model::now_ms();
    let today = now - now.rem_euclid(DAY);

    let written = crate::kline_source::backfill_kline_source_chunked(
        &hist,
        &market,
        CSYM,
        "1h",
        today - 3 * DAY,
        today + DAY - 1,
        1,
        &|| false,
    )
    .unwrap();

    let asked: Vec<i64> = market.calls().iter().map(|(from, _)| *from).collect();
    assert!(asked.contains(&(today - 3 * DAY)) && asked.contains(&(today - 2 * DAY)), "{asked:?}");
    assert!(!asked.contains(&today), "today is not settled and must never be fetched: {asked:?}");
    assert_eq!(written, 24 * asked.len(), "every fetched day was a full day of bars");
    let stored = hist
        .load_bars(CVENUE, CSYM, "1h", vike_data::TsRange::all())
        .expect("the rows are under the SOURCE's venue");
    assert_eq!(stored.len(), written);
}
