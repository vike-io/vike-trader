//! A REFUSED SUPERSEDE: skipped with its own chunk, never with the rest of the window.

use super::*;

// ── A REFUSED SUPERSEDE: skipped with its own chunk, never with the rest of the window ────────
//
// Background compaction can merge a provisional part into a neighbour's key set before its window
// settles, and the store then REFUSES the settled commit that would supersede it (a supersede it
// cannot perform exactly would double `volume`). The refusal is permanent for that window and says
// nothing about any other, so `backfill_quotes_then_bars` skips the chunk and goes on — and ends in
// an error naming every chunk it skipped, never in `Ok`. These tests build the REAL refusal: the
// planted state is the very construction `crates/vike-store/tests/store/supersede_refusal.rs` refuses on,
// so what the lane meets here is the store's own answer and not a stand-in for it. Every window is
// a run of settled 1970 days except the recent-chunks test, which places itself against today's
// UTC midnight like the RECENT CHUNKS tests above.

/// [`fold_provisional_quotes_into_a_neighbour`]'s BAR-side twin: the chunk's PROVISIONAL resample
/// key folded, in the `1m` bar series, into one part with an unrelated commit's. Nothing is
/// planted on the quote side, so a settled pass stores the chunk's ticks without complaint and is
/// then refused when it resamples them.
fn fold_provisional_bars_into_a_neighbour(hist: &DataFusionHist, (from, to): (i64, i64)) {
    let bars = consolidate_quotes(&tape_between(from, from + 80_000), STEP_MS);
    assert_eq!(bars.len(), 2, "the 10 s and 70 s ticks land in two different minutes");
    let neighbour = format!("a-neighbour-of-{from}");
    hist.append_bars(VENUE, SYMBOL, INTERVAL, &bars[..1], Some(&neighbour)).unwrap();
    let key = provisional_resample_key(SYMBOL, INTERVAL, TsRange::of(from, to));
    hist.append_bars(VENUE, SYMBOL, INTERVAL, &bars[1..], Some(&key)).unwrap();
    let merged = hist
        .compact_series(
            StoreKindId::Bar,
            VENUE,
            SYMBOL,
            Some(INTERVAL),
            &CompactionConfig { min_parts: 2, ..Default::default() },
        )
        .unwrap();
    assert_eq!(merged.parts_merged, 2, "the two same-day parts must fold together: {merged:?}");
}

/// The middle chunk of three is refused, and the request still fetches, stores and resamples the
/// chunks either side of it — where it used to return the refusal at once, so day 7 was never even
/// attempted. It ends in an ERROR that names the chunk, because `Ok` is what the `Backfill` reply
/// reports as "this window is in the store" and day 6 is not. The refused chunk writes NOTHING
/// (the quote side refused, so no ticks and no bars), and the error's `Display` keeps the store's
/// `hist store: ` prefix, which is what text-only classifiers of a stringified error read.
#[test]
fn a_refused_supersede_skips_its_own_chunk_and_the_rest_of_the_window_is_still_written() {
    let (_dir, hist) = open();
    let (start, end) = (5 * MS_PER_DAY, 8 * MS_PER_DAY - 1);
    let day_5 = (5 * MS_PER_DAY, 6 * MS_PER_DAY - 1);
    let day_6 = (6 * MS_PER_DAY, 7 * MS_PER_DAY - 1);
    let day_7 = (7 * MS_PER_DAY, 8 * MS_PER_DAY - 1);
    fold_provisional_quotes_into_a_neighbour(&hist, day_6);
    let mut calls = Vec::new();

    let result =
        backfill_quotes_then_bars(&hist, SYMBOL, INTERVAL, start, end, &|| false, |_, from, to| {
            calls.push((from, to));
            Ok(tape_between(from, to))
        });

    assert_eq!(
        calls,
        vec![day_5, day_6, day_7],
        "every chunk is fetched — the refused one, whose ticks are only refused once fetched, and \
         the one AFTER it, which the abort used to leave unattempted"
    );
    let Err(CollectError::SupersedeRefused(report)) = &result else {
        panic!("a skipped chunk must end the request in SupersedeRefused, never in Ok: {result:?}")
    };
    let prefix = format!("dukascopy {SYMBOL} [{start}, {end}] {INTERVAL}: the store REFUSED");
    assert!(report.starts_with(&prefix), "{report}");
    let named = format!("chunks skipped, 1 of 3: [{}, {}].", day_6.0, day_6.1);
    assert!(report.contains(&named), "the skipped chunk is named by its bounds: {report}");
    assert!(
        report.contains("The other chunks ran to their end, writing 6 1m bars."),
        "days 5 and 7 wrote three bars each: {report}"
    );
    assert!(
        report.contains("vike-cli data hist rm") && report.contains("--produced-by dukascopy"),
        "the remedy is in the error the caller reads, not only in the daemon's log: {report}"
    );
    assert!(result.as_ref().unwrap_err().to_string().starts_with("hist store: dukascopy "));

    assert_eq!(
        stored_bar_ts(&hist),
        vec![
            5 * MS_PER_DAY,
            5 * MS_PER_DAY + 60_000,
            6 * MS_PER_DAY - 60_000,
            7 * MS_PER_DAY,
            7 * MS_PER_DAY + 60_000,
            8 * MS_PER_DAY - 60_000,
        ],
        "days 5 and 7 are written and day 6 wrote no bar"
    );
    let quotes = hist.scan_quotes(VENUE, SYMBOL, TsRange::all()).unwrap();
    assert_eq!(
        quotes.len(),
        2 + 3 + 3,
        "the two planted ticks and days 5 and 7's — the refused chunk's fetched ticks are NOT stored"
    );
    for (name, chunk, spent) in
        [("day 5", day_5, true), ("day 6", day_6, false), ("day 7", day_7, true)]
    {
        let key = quote_commit_key(SYMBOL, chunk.0, chunk.1);
        assert_eq!(quote_series_has_commit(&hist, &key), spent, "{name}'s canonical quote key");
    }
}

/// EVERY refused chunk is named, in window order — the FIRST chunk of four and the LAST, so the
/// error is neither the first refusal alone nor the last alone — and the two between them are
/// written. A refusal at the last chunk is the case with nothing after it to prove the request went
/// on, which is why it must still end in an error rather than in the `Ok` a loop that merely ran to
/// its end would return.
#[test]
fn every_refused_chunk_is_named_in_the_error_in_window_order() {
    let (_dir, hist) = open();
    let (start, end) = (5 * MS_PER_DAY, 9 * MS_PER_DAY - 1);
    let day_5 = (5 * MS_PER_DAY, 6 * MS_PER_DAY - 1);
    let day_8 = (8 * MS_PER_DAY, 9 * MS_PER_DAY - 1);
    fold_provisional_quotes_into_a_neighbour(&hist, day_5);
    fold_provisional_quotes_into_a_neighbour(&hist, day_8);
    let mut fetches = 0;

    let result =
        backfill_quotes_then_bars(&hist, SYMBOL, INTERVAL, start, end, &|| false, |_, from, to| {
            fetches += 1;
            Ok(tape_between(from, to))
        });

    let Err(CollectError::SupersedeRefused(report)) = &result else {
        panic!("two skipped chunks must end the request in SupersedeRefused: {result:?}")
    };
    let named =
        format!("chunks skipped, 2 of 4: [{}, {}], [{}, {}].", day_5.0, day_5.1, day_8.0, day_8.1);
    assert!(report.contains(&named), "both refusals, in window order: {report}");
    assert!(report.contains("writing 6 1m bars."), "days 6 and 7 wrote three each: {report}");
    assert_eq!(fetches, 4, "the first refusal did not stop the three chunks after it");
    assert_eq!(
        stored_bar_ts(&hist),
        vec![
            6 * MS_PER_DAY,
            6 * MS_PER_DAY + 60_000,
            7 * MS_PER_DAY - 60_000,
            7 * MS_PER_DAY,
            7 * MS_PER_DAY + 60_000,
            8 * MS_PER_DAY - 60_000,
        ],
        "days 6 and 7 are written; neither refused day wrote a bar"
    );
}

/// The BAR side refuses while the quote side does not: day 6's ticks are stored and its bars are
/// not (its provisional resample key is the one folded), so the chunk is skipped like any other.
/// And a RETRY of the window — extended by a day, so there is something new to write — meets the
/// same refusal on the path that never fetches (`resample_stored_chunk`: the quote key is spent, so
/// the skip-before-fetch check moves past the fetch and re-attempts the bars), skips it again
/// instead of aborting there, and still fetches and writes day 8. The fetch that panics for day 6
/// is what proves that second path was the one taken.
#[test]
fn a_bar_side_refusal_skips_its_chunk_and_a_retry_meets_it_again_without_fetching() {
    let (_dir, hist) = open();
    let day_6 = (6 * MS_PER_DAY, 7 * MS_PER_DAY - 1);
    fold_provisional_bars_into_a_neighbour(&hist, day_6);
    let mut calls = Vec::new();

    let first = backfill_quotes_then_bars(
        &hist,
        SYMBOL,
        INTERVAL,
        5 * MS_PER_DAY,
        8 * MS_PER_DAY - 1,
        &|| false,
        |_, from, to| {
            calls.push((from, to));
            Ok(tape_between(from, to))
        },
    );

    assert_eq!(calls.len(), 3, "day 6 is fetched — its ticks are not stored until this request");
    let Err(CollectError::SupersedeRefused(report)) = &first else {
        panic!("the bar-side refusal must end the request in SupersedeRefused: {first:?}")
    };
    let named = format!("chunks skipped, 1 of 3: [{}, {}].", day_6.0, day_6.1);
    assert!(report.contains(&named), "{report}");
    assert!(report.contains("writing 6 1m bars."), "days 5 and 7 wrote three each: {report}");
    assert!(
        quote_series_has_commit(&hist, &quote_commit_key(SYMBOL, day_6.0, day_6.1)),
        "the QUOTE side succeeded: day 6's ticks are stored, its quote key spent"
    );
    let day_6_bars: Vec<i64> =
        stored_bar_ts(&hist).into_iter().filter(|ts| (day_6.0..=day_6.1).contains(ts)).collect();
    assert_eq!(
        day_6_bars,
        vec![6 * MS_PER_DAY, 6 * MS_PER_DAY + 60_000],
        "day 6 holds only the two planted bars: the refused resample wrote its third"
    );

    // The retry: one more day. Days 5 and 7 are already resampled, day 6 is refused again, and
    // day 8 is new.
    let mut retry_calls = Vec::new();
    let retry = backfill_quotes_then_bars(
        &hist,
        SYMBOL,
        INTERVAL,
        5 * MS_PER_DAY,
        9 * MS_PER_DAY - 1,
        &|| false,
        |_, from, to| {
            assert_ne!(from, day_6.0, "day 6's ticks are stored: it must never be fetched again");
            retry_calls.push((from, to));
            Ok(tape_between(from, to))
        },
    );

    assert_eq!(
        retry_calls,
        vec![(8 * MS_PER_DAY, 9 * MS_PER_DAY - 1)],
        "only day 8 is fetched — days 5, 6 and 7 are skipped before the fetch, and day 6's refusal \
         on that path did not stop the request from reaching day 8"
    );
    let Err(CollectError::SupersedeRefused(report)) = &retry else {
        panic!("the retry must meet the same refusal: {retry:?}")
    };
    let named = format!("chunks skipped, 1 of 4: [{}, {}].", day_6.0, day_6.1);
    assert!(report.contains(&named), "{report}");
    assert!(report.contains("writing 3 1m bars."), "day 8 alone: {report}");
    assert!(stored_bar_ts(&hist).contains(&(9 * MS_PER_DAY - 60_000)), "day 8 was written");
}

/// A refusal does not take the recent chunks with it. Days 4 and 3 back are settled and days 2 and
/// 1 back are inside the publication margin, so the request fetches all four, refuses the OLDEST
/// (settled, folded) and still stores both recent days — each under the provisional keys of its own
/// bounds, exactly as a request that met no refusal does. They come after the refused chunk, so a
/// refusal that returned from the loop would leave the recent days unwritten.
#[test]
fn a_refusal_does_not_stop_the_recent_chunks_from_being_stored() {
    let (_dir, hist) = open();
    let now = vike_model::now_ms();
    let today = now - now.rem_euclid(MS_PER_DAY);
    let day = |n: i64| today - n * MS_PER_DAY;
    let (start, end) = (day(4), day(0) - 1);
    let refused = (day(4), day(3) - 1);
    fold_provisional_quotes_into_a_neighbour(&hist, refused);
    let mut calls = Vec::new();

    let result =
        backfill_quotes_then_bars(&hist, SYMBOL, INTERVAL, start, end, &|| false, |_, from, to| {
            calls.push((from, to));
            Ok(tape_between(from, to))
        });

    assert_eq!(
        calls,
        vec![refused, (day(3), day(2) - 1), (day(2), day(1) - 1), (day(1), day(0) - 1)],
        "the refused chunk is followed by another settled one and by both recent chunks"
    );
    let Err(CollectError::SupersedeRefused(report)) = &result else {
        panic!("the refused chunk must end the request in SupersedeRefused: {result:?}")
    };
    let named = format!("chunks skipped, 1 of 4: [{}, {}].", refused.0, refused.1);
    assert!(report.contains(&named), "{report}");
    assert!(
        report.contains("writing 9 1m bars."),
        "day 3's three, then the recent days' six (days 2 and 1 back): {report}"
    );
    for (from, to) in [(day(2), day(1) - 1), (day(1), day(0) - 1)] {
        let provisional_resample =
            provisional_resample_key(SYMBOL, INTERVAL, TsRange::of(from, to));
        assert!(
            quote_series_has_commit(&hist, &provisional_quote_commit_key(SYMBOL, from, to)),
            "the recent day [{from}, {to}] was stored under its own provisional key"
        );
        assert!(
            bar_series_has_commit(&hist, INTERVAL, &provisional_resample),
            "and resampled under its own provisional resample key"
        );
    }
    assert!(
        quote_series_has_commit(&hist, &quote_commit_key(SYMBOL, day(3), day(2) - 1)),
        "the settled chunk after the refusal took its own shared key"
    );
    assert!(
        !quote_series_has_commit(&hist, &quote_commit_key(SYMBOL, refused.0, refused.1)),
        "and the refused one did not"
    );
}

/// A window whose ONE chunk is refused ends in the same error, saying `1 of 1` — the request is not
/// special-cased by how many chunks it has, and a one-chunk refusal is no longer the store's bare
/// error (a `Data`) but the report that carries the remedy.
#[test]
fn a_refused_one_chunk_window_ends_in_the_same_error() {
    let (_dir, hist) = open();
    let day_6 = (6 * MS_PER_DAY, 7 * MS_PER_DAY - 1);
    fold_provisional_quotes_into_a_neighbour(&hist, day_6);

    let result = backfill_quotes_then_bars(
        &hist,
        SYMBOL,
        INTERVAL,
        day_6.0,
        day_6.1,
        &|| false,
        |_, from, to| Ok(tape_between(from, to)),
    );

    let Err(CollectError::SupersedeRefused(report)) = &result else {
        panic!("a refused one-chunk window must end in SupersedeRefused: {result:?}")
    };
    let named = format!("chunks skipped, 1 of 1: [{}, {}].", day_6.0, day_6.1);
    assert!(report.contains(&named), "{report}");
    assert!(report.contains("writing 0 1m bars."), "nothing else ran: {report}");
    assert!(stored_bar_ts(&hist).is_empty(), "the refused chunk wrote no bar");
}

/// Only a REFUSAL is stepped over. A store error that is not one — here the store refusing a
/// path-hostile symbol, which is a `Query` like the refusal and comes out of the very same write
/// verb — still aborts the request at the chunk that hit it: the second chunk is never fetched. It
/// is the store failing, not a fact about one chunk's keys, and the next chunk would meet it too.
#[test]
fn a_store_error_that_is_not_a_refusal_still_aborts_at_once() {
    let (_dir, hist) = open();
    let mut calls = Vec::new();

    let result = backfill_quotes_then_bars(
        &hist,
        "EUR/USD",
        INTERVAL,
        5 * MS_PER_DAY,
        7 * MS_PER_DAY - 1,
        &|| false,
        |_, from, to| {
            calls.push((from, to));
            Ok(tape_between(from, to))
        },
    );

    let Err(CollectError::Data(error)) = &result else {
        panic!("a store error that is not a refusal must abort as a Data: {result:?}")
    };
    assert!(!error.is_supersede_refusal(), "it is not the refusal: {error}");
    assert_eq!(calls.len(), 1, "the request aborted at the chunk that failed: {calls:?}");
}

/// A skipped refusal does not soften what comes after it: a FETCH failure at the next chunk still
/// aborts the request at once, as a `Fetch` naming that chunk, and the chunk after that is never
/// asked for. (The refusal skipped before it is in the log, not in this error — the bars count in
/// the message is 0 because the refused chunk wrote none.)
#[test]
fn a_fetch_failure_after_a_skipped_refusal_still_aborts_the_request_at_once() {
    let (_dir, hist) = open();
    let (start, end) = (5 * MS_PER_DAY, 8 * MS_PER_DAY - 1);
    fold_provisional_quotes_into_a_neighbour(&hist, (5 * MS_PER_DAY, 6 * MS_PER_DAY - 1));
    let mut calls = Vec::new();

    let result =
        backfill_quotes_then_bars(&hist, SYMBOL, INTERVAL, start, end, &|| false, |_, from, to| {
            calls.push((from, to));
            if from == 6 * MS_PER_DAY {
                Err("the CDN went away".to_string())
            } else {
                Ok(tape_between(from, to))
            }
        });

    let Err(CollectError::Fetch(error)) = &result else {
        panic!("a fetch failure must answer as a Fetch, not as the skipped refusal: {result:?}")
    };
    assert_eq!(
        error.as_str(),
        format!(
            "chunk [{}, {}] of [{start}, {end}] failed after 0 1m bars were written: the CDN went \
             away",
            6 * MS_PER_DAY,
            7 * MS_PER_DAY - 1
        ),
        "the fetch error's own text, exactly as it is without a refusal before it"
    );
    assert_eq!(calls.len(), 2, "day 7 is never asked for: {calls:?}");
}
