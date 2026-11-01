//! THE CHUNK GRID: a long window is fetched, stored and resampled one day at a time.

use super::*;

// ── THE CHUNK GRID: a long window is fetched, stored and resampled one day at a time ─────────
//
// At `1m` a chunk is one UTC day, so these windows are cut at midnight. Their fake fetch honours
// its bounds, as `vike_dukascopy::fetch_quotes_range` does. The tests above can answer every call
// with the same five ticks only because each of their windows is ONE chunk: a multi-chunk window
// stores whatever each call returns, so a fake that ignored its bounds would store its ticks once
// per chunk.

/// A window across THREE day-chunks — `[30 s, 2 days + 90 s]` — is fetched as three sub-windows,
/// cut at the two midnights inside it, each stored under its own quote key and resampled over its
/// own whole buckets; the bars written are the three chunks' bars summed. Only the window's own two
/// ends are trimmed — day 0 loses its partial first minute, and day 2 keeps only its first (its
/// second is cut at 90 s, its last lies past the window) — and no minute is lost at a midnight,
/// because the chunk edges are bar edges.
#[test]
fn a_window_across_three_day_chunks_is_fetched_stored_and_resampled_chunk_by_chunk() {
    let (_dir, hist) = open();
    let (start, end) = (30_000, 2 * MS_PER_DAY + 90_000);
    let mut calls = Vec::new();

    let bars = backfill_quotes_then_bars(
        &hist,
        SYMBOL,
        INTERVAL,
        start,
        end,
        &|| false,
        |sym, from, to| {
            calls.push((sym.to_string(), from, to));
            Ok(tape_between(from, to))
        },
    )
    .unwrap();

    assert_eq!(
        calls,
        vec![
            (SYMBOL.to_string(), start, MS_PER_DAY - 1),
            (SYMBOL.to_string(), MS_PER_DAY, 2 * MS_PER_DAY - 1),
            (SYMBOL.to_string(), 2 * MS_PER_DAY, end),
        ],
        "one fetch per chunk, in order, cut at each midnight inside the window"
    );
    assert_eq!(bars, 2 + 3 + 1, "the chunks' bars summed: day 0 two, day 1 three, day 2 one");
    assert_eq!(
        stored_bar_ts(&hist),
        vec![
            60_000,
            MS_PER_DAY - 60_000,
            MS_PER_DAY,
            MS_PER_DAY + 60_000,
            2 * MS_PER_DAY - 60_000,
            2 * MS_PER_DAY,
        ]
    );
    let quotes = hist.scan_quotes(VENUE, SYMBOL, TsRange::all()).unwrap();
    assert_eq!(quotes.len(), 7, "every tick inside the window, each stored once");
    // Each chunk spent its own resample key, over its own whole buckets...
    for (first, last) in [
        (60_000, MS_PER_DAY - 1),
        (MS_PER_DAY, 2 * MS_PER_DAY - 1),
        (2 * MS_PER_DAY, 2 * MS_PER_DAY + 59_999),
    ] {
        assert_eq!(
            resample_and_store_bars(&hist, SYMBOL, INTERVAL, TsRange::of(first, last)).unwrap(),
            0,
            "the resample key of [{first}, {last}] is spent"
        );
    }
    // ...and its own quote key, over the sub-window it fetched.
    for (_, from, to) in &calls {
        assert!(quote_key_is_spent(&hist, &quote_commit_key(SYMBOL, *from, *to)), "[{from}, {to}]");
    }
}

/// A window inside ONE day-chunk is the lane exactly as it was before the chunking: one fetch over
/// the window's own bounds, the quote key it always spent — spelled out here as a literal — and the
/// resample range the trim always gave it. `[1 day + 30 s, 1 day + 150 s]` has both ends inside a
/// minute, so its one whole bar is the `1 day + 60 s` one.
#[test]
fn a_window_inside_one_day_chunk_is_fetched_and_keyed_exactly_as_before() {
    let (_dir, hist) = open();
    let (start, end) = (MS_PER_DAY + 30_000, MS_PER_DAY + 150_000);
    let mut calls = Vec::new();

    let bars = backfill_quotes_then_bars(
        &hist,
        SYMBOL,
        INTERVAL,
        start,
        end,
        &|| false,
        |sym, from, to| {
            calls.push((sym.to_string(), from, to));
            Ok(tape_between(from, to))
        },
    )
    .unwrap();

    assert_eq!(calls, vec![(SYMBOL.to_string(), start, end)], "ONE fetch, over the window itself");
    assert_eq!(bars, 1);
    assert_eq!(stored_bar_ts(&hist), vec![MS_PER_DAY + 60_000]);
    let trimmed = TsRange::of(MS_PER_DAY + 60_000, MS_PER_DAY + 119_999);
    assert_eq!(
        resample_and_store_bars(&hist, SYMBOL, INTERVAL, trimmed).unwrap(),
        0,
        "the resample key is the trimmed window's, as it always was"
    );
    assert!(
        quote_key_is_spent(&hist, "dukascopy:EURUSD:86430000-86550000"),
        "the quote key is the whole window's, spelled as it always was"
    );
}

/// Overlapping requests share every FULL chunk. Day 1 alone, then days 1 and 2: the second request
/// finds day 1's quote key already spent — a full chunk's key is its own bounds, whichever request
/// covered it — so it skips fetching day 1 entirely and writes day 2 alone; day 1's ticks and bars
/// stay exactly as the first request left them.
#[test]
fn a_day_an_earlier_request_ingested_is_not_fetched_or_written_again() {
    let (_dir, hist) = open();
    let day_1 = backfill_quotes_then_bars(
        &hist,
        SYMBOL,
        INTERVAL,
        MS_PER_DAY,
        2 * MS_PER_DAY - 1,
        &|| false,
        |_, from, to| Ok(tape_between(from, to)),
    )
    .unwrap();
    assert_eq!(day_1, 3, "day 1 alone: its three minutes with a tick");
    let mut calls = Vec::new();

    let days_1_and_2 = backfill_quotes_then_bars(
        &hist,
        SYMBOL,
        INTERVAL,
        MS_PER_DAY,
        3 * MS_PER_DAY - 1,
        &|| false,
        |sym, from, to| {
            calls.push((sym.to_string(), from, to));
            Ok(tape_between(from, to))
        },
    )
    .unwrap();

    assert_eq!(days_1_and_2, 3, "day 2's bars alone — day 1's chunk key was already spent");
    assert_eq!(
        calls,
        vec![(SYMBOL.to_string(), 2 * MS_PER_DAY, 3 * MS_PER_DAY - 1)],
        "day 1 is never fetched: its spent key is checked before the fetch, not just at the write"
    );
    assert_eq!(
        stored_bar_ts(&hist),
        vec![
            MS_PER_DAY,
            MS_PER_DAY + 60_000,
            2 * MS_PER_DAY - 60_000,
            2 * MS_PER_DAY,
            2 * MS_PER_DAY + 60_000,
            3 * MS_PER_DAY - 60_000,
        ],
        "no bar is stored twice"
    );
    let quotes = hist.scan_quotes(VENUE, SYMBOL, TsRange::all()).unwrap();
    assert_eq!(quotes.len(), 6, "day 1's three ticks are stored once, not twice");
}

/// The strongest form of the skip: a settled chunk whose key is already spent is never even asked
/// for — proven with a fetch that panics if it is called at all, not merely counted like the test
/// above. A one-chunk window repeated whole exercises the same check the multi-chunk case does,
/// since a settled chunk's key is its own bounds regardless of how many neighbors it has.
#[test]
fn a_settled_chunk_with_a_spent_key_is_never_asked_for() {
    let (_dir, hist) = open();
    let (start, end) = (5 * MS_PER_DAY, 6 * MS_PER_DAY - 1);

    let first =
        backfill_quotes_then_bars(&hist, SYMBOL, INTERVAL, start, end, &|| false, |_, from, to| {
            Ok(tape_between(from, to))
        })
        .unwrap();
    assert_eq!(first, 3, "day 5 alone: its three minutes with a tick");

    let second =
        backfill_quotes_then_bars(&hist, SYMBOL, INTERVAL, start, end, &|| false, |_, _, _| {
            panic!("a settled chunk whose key is already spent must not be fetched at all")
        })
        .unwrap();

    assert_eq!(second, 0, "the whole window is one already-spent chunk: nothing new to write");
}

/// A chunk whose ticks a request at ONE interval stored is asked for again at ANOTHER. The quote key
/// carries no interval, so it looked spent and the chunk was skipped whole — zero bars, no error.
/// Now only the FETCH is skipped (a fetch that panics if called proves it): the new interval's bars
/// are resampled from the stored ticks, bit for bit what a request at that interval writes on a
/// store that never saw the first, while the ticks and the first interval's bars stay untouched.
#[test]
fn a_second_interval_over_an_ingested_window_is_resampled_from_the_store_without_a_fetch() {
    // Days 5, 6 and 7 — three settled chunks, the tape three ticks a day.
    let (start, end) = (5 * MS_PER_DAY, 8 * MS_PER_DAY - 1);
    let (_dir, hist) = open();
    let one_minute =
        backfill_quotes_then_bars(&hist, SYMBOL, "1m", start, end, &|| false, |_, from, to| {
            Ok(tape_between(from, to))
        })
        .unwrap();
    assert_eq!(one_minute, 9, "three days of three minutes with a tick");
    let one_minute_bars = stored_bars_at(&hist, "1m");

    let five_minute =
        backfill_quotes_then_bars(&hist, SYMBOL, "5m", start, end, &|| false, |_, _, _| {
            panic!("a chunk whose ticks are already stored must not be fetched again")
        })
        .unwrap();

    // A day's first two ticks (10 s and 70 s in) share one 5m bar; its last tick is in the last one.
    assert_eq!(five_minute, 6, "two 5m bars a day, for three days");
    assert_eq!(stored_bars_at(&hist, "1m"), one_minute_bars, "the first interval is untouched");
    let quotes = hist.scan_quotes(VENUE, SYMBOL, TsRange::all()).unwrap();
    assert_eq!(quotes.len(), 9, "every tick is stored once, not once per interval");

    let (_control_dir, control) = open();
    backfill_quotes_then_bars(&control, SYMBOL, "5m", start, end, &|| false, |_, from, to| {
        Ok(tape_between(from, to))
    })
    .unwrap();
    assert_eq!(
        stored_bars_at(&hist, "5m"),
        stored_bars_at(&control, "5m"),
        "resampled from the store is the very bars a fetch at 5m would have written"
    );

    // Both keys of every chunk are spent now: repeating either interval fetches and writes nothing.
    for interval in ["1m", "5m"] {
        let repeat =
            backfill_quotes_then_bars(&hist, SYMBOL, interval, start, end, &|| false, |_, _, _| {
                panic!("a chunk spent at both its keys must not be fetched")
            })
            .unwrap();
        assert_eq!(repeat, 0, "{interval} repeated: nothing new to write");
    }
}

/// A window part-ingested at another interval: day 1 alone at 1m, then days 1 and 2 at 5m. Day 1 is
/// resampled from the store and never fetched; day 2 is fetched, stored and resampled like any chunk
/// nothing has seen — and the two together are what one request at 5m over both days writes.
#[test]
fn a_new_interval_over_a_part_ingested_window_fetches_only_the_days_it_lacks() {
    let (_dir, hist) = open();
    backfill_quotes_then_bars(
        &hist,
        SYMBOL,
        "1m",
        MS_PER_DAY,
        2 * MS_PER_DAY - 1,
        &|| false,
        |_, from, to| Ok(tape_between(from, to)),
    )
    .unwrap();
    let mut calls = Vec::new();

    let bars = backfill_quotes_then_bars(
        &hist,
        SYMBOL,
        "5m",
        MS_PER_DAY,
        3 * MS_PER_DAY - 1,
        &|| false,
        |s, from, to| {
            calls.push((s.to_string(), from, to));
            Ok(tape_between(from, to))
        },
    )
    .unwrap();

    assert_eq!(
        calls,
        vec![(SYMBOL.to_string(), 2 * MS_PER_DAY, 3 * MS_PER_DAY - 1)],
        "day 1's ticks are stored: only day 2 is fetched"
    );
    assert_eq!(bars, 4, "two 5m bars a day, day 1's from the store and day 2's from the fetch");
    let quotes = hist.scan_quotes(VENUE, SYMBOL, TsRange::all()).unwrap();
    assert_eq!(quotes.len(), 6, "day 1's ticks once, day 2's once");

    let (_control_dir, control) = open();
    backfill_quotes_then_bars(
        &control,
        SYMBOL,
        "5m",
        MS_PER_DAY,
        3 * MS_PER_DAY - 1,
        &|| false,
        |_, from, to| Ok(tape_between(from, to)),
    )
    .unwrap();
    assert_eq!(stored_bars_at(&hist, "5m"), stored_bars_at(&control, "5m"));
}

/// The window's own ragged ends: `[30 s, 2 days + 90 s]` at 1m, then at 5m. The two edge chunks are
/// keyed by their own cuts, and the 5m resample of each trims to ITS whole buckets — day 0 loses the
/// 5m bucket its first 30 s cut, day 2 holds no whole 5m bucket at all — exactly as a request at 5m
/// over the same window does, so the two stores agree bar for bar.
#[test]
fn a_second_interval_over_an_unaligned_window_trims_to_its_own_whole_buckets() {
    let (start, end) = (30_000, 2 * MS_PER_DAY + 90_000);
    let (_dir, hist) = open();
    backfill_quotes_then_bars(&hist, SYMBOL, "1m", start, end, &|| false, |_, from, to| {
        Ok(tape_between(from, to))
    })
    .unwrap();

    let bars = backfill_quotes_then_bars(&hist, SYMBOL, "5m", start, end, &|| false, |_, _, _| {
        panic!("every chunk's ticks are stored: nothing is fetched")
    })
    .unwrap();

    assert_eq!(
        bars,
        1 + 2,
        "day 0 keeps its last bucket only and day 1 has two; day 2 holds no whole bucket to add"
    );
    let (_control_dir, control) = open();
    backfill_quotes_then_bars(&control, SYMBOL, "5m", start, end, &|| false, |_, from, to| {
        Ok(tape_between(from, to))
    })
    .unwrap();
    assert_eq!(stored_bars_at(&hist, "5m"), stored_bars_at(&control, "5m"));
}

/// A request that stored a chunk's ticks and died before its resample — the ticks are in, the quote
/// key is spent, the resample key is not — used to leave that chunk with no bars for good: every
/// retry saw the spent quote key and skipped it. A retry now writes the bars it never got, from the
/// stored ticks and with no fetch.
#[test]
fn a_retry_writes_the_bars_a_request_that_stored_the_ticks_and_died_never_did() {
    let (_dir, hist) = open();
    let (from, to) = (5 * MS_PER_DAY, 6 * MS_PER_DAY - 1);
    ingest_quotes(&hist, SYMBOL, from, to, &tape_between(from, to)).unwrap();
    assert!(stored_bars_at(&hist, INTERVAL).is_empty(), "the ticks landed, the resample never ran");

    let bars =
        backfill_quotes_then_bars(&hist, SYMBOL, INTERVAL, from, to, &|| false, |_, _, _| {
            panic!("the ticks are stored: the retry must not fetch them again")
        })
        .unwrap();

    assert_eq!(bars, 3, "the day's three minutes with a tick, from the stored ticks");
    assert_eq!(
        stored_bar_ts(&hist),
        vec![5 * MS_PER_DAY, 5 * MS_PER_DAY + 60_000, 6 * MS_PER_DAY - 60_000]
    );
}

/// A chunk that FAILS returns its error at once — naming itself and the bars already written — and
/// the chunks before it stay written. Retrying the same window then skips fetching day 0 (its key is
/// already spent, checked before the fetch) and writes only days 1 and 2, every tick landing exactly
/// once.
#[test]
fn a_failed_chunk_keeps_the_ones_before_it_and_a_retry_writes_the_rest() {
    let (_dir, hist) = open();
    let (start, end) = (0, 3 * MS_PER_DAY - 1);
    let mut fetches = 0;

    let failed =
        backfill_quotes_then_bars(&hist, SYMBOL, INTERVAL, start, end, &|| false, |_, from, to| {
            fetches += 1;
            if from == MS_PER_DAY {
                Err("the CDN went away".to_string())
            } else {
                Ok(tape_between(from, to))
            }
        });

    let Err(CollectError::Fetch(error)) = &failed else {
        panic!("day 1's fetch error must answer, as a Fetch: {failed:?}")
    };
    assert_eq!(
        error.as_str(),
        "chunk [86400000, 172799999] of [0, 259199999] failed after 3 1m bars were written: \
         the CDN went away",
        "one chunk of several names itself and what it leaves written"
    );
    assert_eq!(fetches, 2, "no chunk after the failed one is fetched");
    assert_eq!(stored_bar_ts(&hist), vec![0, 60_000, MS_PER_DAY - 60_000], "day 0 stays written");

    let mut retry_calls = Vec::new();
    let retried =
        backfill_quotes_then_bars(&hist, SYMBOL, INTERVAL, start, end, &|| false, |_, from, to| {
            retry_calls.push((from, to));
            Ok(tape_between(from, to))
        })
        .unwrap();

    assert_eq!(retried, 6, "days 1 and 2 — day 0's key is spent");
    assert_eq!(
        retry_calls,
        vec![(MS_PER_DAY, 2 * MS_PER_DAY - 1), (2 * MS_PER_DAY, 3 * MS_PER_DAY - 1)],
        "day 0 is skipped on retry: its key was already spent before the failure"
    );
    let quotes = hist.scan_quotes(VENUE, SYMBOL, TsRange::all()).unwrap();
    assert_eq!(quotes.len(), 9, "every tick of the three days, each stored once");
}

/// A window of ONE chunk fails with the fetch's own text, exactly as it did before the chunking:
/// only a window of several has a chunk to name.
#[test]
fn a_one_chunk_window_fails_with_the_fetchs_own_text() {
    let (_dir, hist) = open();

    let failed =
        backfill_quotes_then_bars(&hist, SYMBOL, INTERVAL, 0, 119_999, &|| false, |_, _, _| {
            Err("the CDN went away".to_string())
        });

    let Err(CollectError::Fetch(error)) = &failed else {
        panic!("a fetch failure must answer as a Fetch: {failed:?}")
    };
    assert_eq!(error.as_str(), "the CDN went away");
}
