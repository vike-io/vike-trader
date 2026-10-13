//! The stop probe `should_stop`, asked once at the top of every chunk.

use super::*;
use std::assert_matches;
use std::cell::RefCell;

// ── THE STOP PROBE (`should_stop`, asked between chunks) ─────────────────────────────────────────
//
// `docs/superpowers/specs/2026-10-01-backfill-cancel-on-client-drop-design.md` §1 and §6 (T1, T3):
// a chunked request asks its probe at the top of every chunk, stops at that boundary when it
// answers `true`, keeps every chunk before it, and answers `Stopped` — never `Ok`. Every test above
// passes a probe that never fires and is unchanged, which is T3.

/// **T1.** A probe that fires after chunk 2 of 5: exactly two fetches, exactly those two days' keys
/// spent and nothing else, and a `Stopped` that names the boundary, the two chunks done and the bars
/// they wrote — never an `Ok`, because the window is not in the store. Then a re-run with a quiet
/// probe fetches only the three days the stop left, and the store ends up holding exactly what one
/// uninterrupted run holds.
#[test]
fn a_probe_that_fires_after_chunk_k_stops_there_and_a_rerun_fetches_only_the_rest() {
    let (_d, hist) = store();
    let (start, end) = (2 * DAY, 7 * DAY - 1);
    let market = FakeMarket::new(HOUR);
    let probe = FiresAfter::new(2);

    let stopped = run_chunked_stoppable(&hist, &market, "1h", (start, end), LATER, &|| probe.ask());

    let Err(CollectError::Stopped(text)) = &stopped else {
        panic!("a request asked to stop must answer Stopped, never Ok or a failure: {stopped:?}")
    };
    assert_eq!(
        text,
        "fakevenue EURUSD 1h [172800000, 604799999]: asked to stop, and stopped before day chunk 3 \
         of 5 [345600000, 431999999] (1970-01-05T00:00:00Z) — nothing failed. The 2 chunk(s) \
         before it are done (0 already stored, 0 known empty, 2 fetched and stored, 0 empty of \
         which 0 marked) and 48 1h bars were written; they stay stored. This chunk and the 2 after \
         it were never fetched: repeating the request resumes here."
    );
    assert!(stopped.unwrap_err().to_string().starts_with("stopped: fakevenue EURUSD 1h "));
    assert_eq!(
        market.calls(),
        vec![(2 * DAY, 3 * DAY - 1), (3 * DAY, 4 * DAY - 1)],
        "exactly the two chunks before the boundary were fetched"
    );
    assert_eq!(
        probe.asked.get(),
        3,
        "asked at the top of chunks 1, 2 and 3, and not after the stop"
    );
    for (day, want) in [(2, true), (3, true), (4, false), (5, false), (6, false)] {
        let (c0, c1) = (day * DAY, (day + 1) * DAY - 1);
        assert_eq!(spent(&hist, "1h", c0, c1), want, "day {day}'s key");
    }
    assert_eq!(stored_at(&hist, "1h").len(), 48, "the two chunks before the stop stay written");

    let quiet = FakeMarket::new(HOUR);
    let resumed = run_chunked(&hist, &quiet, "1h", (start, end), LATER).unwrap();

    assert_eq!(
        quiet.calls(),
        vec![(4 * DAY, 5 * DAY - 1), (5 * DAY, 6 * DAY - 1), (6 * DAY, 7 * DAY - 1)],
        "the re-run fetches only what the stop left"
    );
    assert_eq!((resumed.skipped, resumed.fetched, resumed.rows), (2, 3, 72));
    assert_partitions(&resumed);
    let (_c, control) = store();
    run_chunked(&control, &FakeMarket::new(HOUR), "1h", (start, end), LATER).unwrap();
    assert_eq!(
        stored_at(&hist, "1h"),
        stored_at(&control, "1h"),
        "stopped then resumed is what one uninterrupted run stores"
    );
}

/// The probe is asked ONCE per chunk, at its TOP — before a chunk's key read (so a chunk an earlier
/// run stored is asked about too) and before its fetch — and never between a fetch and its commit.
/// Recorded as one event stream, so an ask that moved inside a chunk, or a chunk that skipped its
/// ask, would change the sequence.
#[test]
fn the_probe_is_asked_once_at_the_top_of_every_chunk_and_never_inside_one() {
    let (_d, hist) = store();
    run_chunked(&hist, &FakeMarket::new(HOUR), "1h", (3 * DAY, 4 * DAY - 1), LATER).unwrap();
    let events = RefCell::new(Vec::new());
    let market = FakeMarket::new(HOUR);

    let out = ingest_klines_chunked(
        &hist,
        CVENUE,
        CSYM,
        "1h",
        (2 * DAY, 5 * DAY - 1),
        LATER,
        &|| {
            events.borrow_mut().push("ask".to_string());
            false
        },
        |sym, iv, s, e| {
            events.borrow_mut().push(format!("fetch day {}", s / DAY));
            market.fetch(sym, iv, s, e).map_err(CollectError::from)
        },
    )
    .unwrap();

    assert_eq!(
        events.into_inner(),
        vec!["ask", "fetch day 2", "ask", "ask", "fetch day 4"],
        "one ask per chunk, each before anything the chunk does — day 3 is stored, so its ask is \
         followed by no fetch"
    );
    assert_eq!((out.chunks, out.skipped, out.fetched, out.stopped), (3, 1, 2, 0));
    assert_partitions(&out);
}

/// A probe that fires at the very first ask: nothing is fetched, no manifest is written, and the
/// answer is still `Stopped` — not the `Ok(0)` a request that had nothing to do would give, which a
/// caller would read as "this window is in the store". Through the `KlineSource` face as well, whose
/// source panics if it is asked at all.
#[test]
fn a_probe_that_fires_at_once_fetches_nothing_and_still_answers_stopped() {
    let (_d, hist) = store();

    let direct = ingest_klines_chunked(
        &hist,
        CVENUE,
        CSYM,
        "1h",
        (2 * DAY, 4 * DAY - 1),
        LATER,
        &|| true,
        |_, _, _, _| panic!("a request stopped before its first chunk fetched"),
    );
    let Err(CollectError::Stopped(text)) = &direct else {
        panic!("stopped before the first chunk must still answer Stopped: {direct:?}")
    };
    assert!(text.contains("stopped before day chunk 1 of 2 [172800000, 259199999]"), "{text}");
    assert!(text.contains("The 0 chunk(s) before it are done"), "{text}");
    assert!(text.contains("0 1h bars were written"), "{text}");
    assert!(text.contains("This chunk and the 1 after it were never fetched"), "{text}");

    let face = crate::kline_source::backfill_kline_source_chunked_at(
        &hist,
        &NeverFetches,
        CSYM,
        "1h",
        2 * DAY,
        4 * DAY - 1,
        LATER,
        &|| true,
    );
    assert_matches!(face, Err(CollectError::Stopped(_)), "the KlineSource face: {face:?}");
    assert!(hist.list_series().unwrap().is_empty(), "a request stopped at once touched the store");
}

/// The one summary line still counts every chunk once a request stops — the stopped remainder says
/// so by name — while a request that ran to its end reads exactly as it did before the probe existed
/// (`the_log_says_where_storage_stops_and_when_the_rest_settles` pins that text unchanged).
#[test]
fn the_summary_line_counts_the_chunks_a_stop_left_unreached() {
    let out = ChunkedOutcome { chunks: 5, fetched: 2, stopped: 3, rows: 48, ..Default::default() };
    assert_eq!(
        summary_line(
            &out,
            "fakevenue EURUSD 1h",
            (2 * DAY, 7 * DAY - 1),
            std::time::Duration::from_millis(1500)
        ),
        "fakevenue EURUSD 1h [172800000, 604799999]: 5 day chunks: 0 skipped (0 known empty), 2 \
         fetched, 0 empty (0 marked), 0 unsettled, 3 stopped (asked to stop before them; never \
         fetched); 48 bars written in 1.5s"
    );
    assert_partitions(&out);
}
