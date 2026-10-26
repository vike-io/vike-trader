//! THE STOP PROBE: `should_stop`, asked at the top of every chunk (`FiresAfter`).

use super::*;
use std::cell::{Cell, RefCell};

// ── THE STOP PROBE: `should_stop`, asked at the top of every chunk ───────────────────────────────
//
// `docs/superpowers/specs/2026-10-01-backfill-cancel-on-client-drop-design.md` §1 and §6 (T2, T3):
// the lane asks its probe before each chunk's key read and fetch, stops at that boundary when it
// answers `true`, keeps every chunk before it, and answers `Stopped` — never `Ok`, and never the
// `SupersedeRefused` that says a request RAN to its end. Every test above passes a probe that never
// fires and asserts what it always did, which is T3.

/// A probe that answers `false` to its first `k` asks and `true` from then on — "fires after chunk
/// `k`", since the lane asks once at the top of every chunk — counting its asks.
struct FiresAfter {
    k: usize,
    asked: Cell<usize>,
}

impl FiresAfter {
    fn new(k: usize) -> Self {
        FiresAfter { k, asked: Cell::new(0) }
    }
    fn ask(&self) -> bool {
        self.asked.set(self.asked.get() + 1);
        self.asked.get() > self.k
    }
}

/// **T2.** Five settled days, a probe that fires after chunk 2: exactly days 5 and 6 are fetched,
/// exactly their quote keys are spent, and the answer is a `Stopped` naming the boundary and what
/// the two chunks wrote. A re-run with a quiet probe fetches only days 7, 8 and 9, and the store
/// then holds exactly what one uninterrupted run does.
#[test]
fn a_probe_that_fires_after_chunk_k_stops_the_tick_lane_there_and_a_rerun_writes_the_rest() {
    let (_dir, hist) = open();
    let (start, end) = (5 * MS_PER_DAY, 10 * MS_PER_DAY - 1);
    let day = |n: i64| (n * MS_PER_DAY, (n + 1) * MS_PER_DAY - 1);
    let probe = FiresAfter::new(2);
    let mut calls = Vec::new();

    let stopped = backfill_quotes_then_bars(
        &hist,
        SYMBOL,
        INTERVAL,
        start,
        end,
        &|| probe.ask(),
        |_, f, t| {
            calls.push((f, t));
            Ok(tape_between(f, t))
        },
    );

    let Err(CollectError::Stopped(text)) = &stopped else {
        panic!("a request asked to stop must answer Stopped, never Ok or a failure: {stopped:?}")
    };
    assert_eq!(
        text,
        "dukascopy EURUSD [432000000, 863999999] 1m: asked to stop, and stopped before chunk 3 of 5 \
         [604800000, 691199999] — nothing failed. The 2 chunk(s) before it ran, writing 6 1m bars \
         and 6 quote rows; what they wrote stays stored. This chunk and every one after it were \
         never fetched: repeating the request resumes here."
    );
    assert!(stopped.unwrap_err().to_string().starts_with("stopped: dukascopy EURUSD "));
    assert_eq!(calls, vec![day(5), day(6)], "exactly the two chunks before the boundary");
    assert_eq!(probe.asked.get(), 3, "asked at the top of chunks 1, 2 and 3, and not after");
    for (n, spent) in [(5, true), (6, true), (7, false), (8, false), (9, false)] {
        let key = quote_commit_key(SYMBOL, day(n).0, day(n).1);
        assert_eq!(quote_series_has_commit(&hist, &key), spent, "day {n}'s quote key");
    }
    assert_eq!(stored_bar_ts(&hist).len(), 6, "days 5 and 6 stay written, three bars each");

    let mut rerun_calls = Vec::new();
    let resumed =
        backfill_quotes_then_bars(&hist, SYMBOL, INTERVAL, start, end, &|| false, |_, f, t| {
            rerun_calls.push((f, t));
            Ok(tape_between(f, t))
        })
        .unwrap();

    assert_eq!(rerun_calls, vec![day(7), day(8), day(9)], "the re-run fetches what the stop left");
    assert_eq!(resumed, 9, "three days of three bars");
    let (_control_dir, control) = open();
    backfill_quotes_then_bars(&control, SYMBOL, INTERVAL, start, end, &|| false, |_, f, t| {
        Ok(tape_between(f, t))
    })
    .unwrap();
    assert_eq!(
        stored_bars_at(&hist, INTERVAL),
        stored_bars_at(&control, INTERVAL),
        "stopped then resumed is what one uninterrupted run stores"
    );
    let quotes = hist.scan_quotes(VENUE, SYMBOL, TsRange::all()).unwrap();
    assert_eq!(quotes.len(), 15, "every tick of the five days, each stored once");
}

/// The probe is asked ONCE per chunk, at its TOP — before a chunk's key read (so a day an earlier
/// request stored is asked about too) and before its fetch — and never between a fetch and its store
/// step. One event stream, so an ask that moved inside a chunk, or a chunk that skipped its ask,
/// would change the sequence.
#[test]
fn the_tick_lane_asks_the_probe_once_at_the_top_of_every_chunk_and_never_inside_one() {
    let (_dir, hist) = open();
    let day_6 = (6 * MS_PER_DAY, 7 * MS_PER_DAY - 1);
    backfill_quotes_then_bars(&hist, SYMBOL, INTERVAL, day_6.0, day_6.1, &|| false, |_, f, t| {
        Ok(tape_between(f, t))
    })
    .unwrap();
    let events = RefCell::new(Vec::new());

    let bars = backfill_quotes_then_bars(
        &hist,
        SYMBOL,
        INTERVAL,
        5 * MS_PER_DAY,
        8 * MS_PER_DAY - 1,
        &|| {
            events.borrow_mut().push("ask".to_string());
            false
        },
        |_, f, t| {
            events.borrow_mut().push(format!("fetch day {}", f / MS_PER_DAY));
            Ok(tape_between(f, t))
        },
    )
    .unwrap();

    assert_eq!(
        events.into_inner(),
        vec!["ask", "fetch day 5", "ask", "ask", "fetch day 7"],
        "one ask per chunk, each before anything the chunk does — day 6's ticks are stored, so its \
         ask is followed by no fetch"
    );
    assert_eq!(bars, 6, "days 5 and 7; day 6 was already resampled at this interval");
}

/// **T2, the recent chunks.** Two settled days and two inside the publication margin. A stop at
/// the edge of the recent run — before its first chunk — writes NONE of it: no tick, no bar, and
/// neither of its provisional keys. A stop BETWEEN two recent chunks keeps the one before it, stored
/// under its provisional keys the way a failure there leaves it, and fetches none after it.
///
/// ⚠ **This is where the design and the tree disagree, and the test follows the tree.** The design
/// (§3, §6 T2) says the recent tail is "the one gathered batch" that a stop inside writes none of.
/// That stopped being true in the same batch the design landed in: follow-up #9 of
/// `docs/superpowers/specs/2026-09-30-provisional-commits-followups-design.md` stores every recent
/// chunk as soon as it is fetched, under its own provisional keys. So the property that survives is
/// the per-chunk one, and it is the same property a FAILED recent chunk already has.
#[test]
fn a_stop_at_or_inside_the_recent_chunks_writes_none_of_the_chunks_after_it() {
    let now = vike_model::now_ms();
    let today = now - now.rem_euclid(MS_PER_DAY);
    let day = |n: i64| (today - n * MS_PER_DAY, today - (n - 1) * MS_PER_DAY - 1);
    let (start, end) = (day(4).0, day(1).1);
    // `k` chunks run before the stop: 2 stops at the first recent day, 3 between the two.
    for (k, kept_recent) in [(2_usize, 0_usize), (3, 1)] {
        let (_dir, hist) = open();
        let probe = FiresAfter::new(k);
        let mut calls = Vec::new();

        let stopped = backfill_quotes_then_bars(
            &hist,
            SYMBOL,
            INTERVAL,
            start,
            end,
            &|| probe.ask(),
            |_, f, t| {
                calls.push((f, t));
                Ok(tape_between(f, t))
            },
        );

        assert_matches!(stopped, Err(CollectError::Stopped(_)), "k = {k}: {stopped:?}");
        assert_eq!(calls, [day(4), day(3), day(2), day(1)][..k].to_vec(), "k = {k}");
        let recent = [day(2), day(1)];
        for (i, (f, t)) in recent.into_iter().enumerate() {
            let kept = i < kept_recent;
            let range = TsRange::of(f, t);
            assert_eq!(
                quote_series_has_commit(&hist, &provisional_quote_commit_key(SYMBOL, f, t)),
                kept,
                "k = {k}: recent [{f}, {t}]'s provisional quote key"
            );
            assert_eq!(
                bar_series_has_commit(
                    &hist,
                    INTERVAL,
                    &provisional_resample_key(SYMBOL, INTERVAL, range)
                ),
                kept,
                "k = {k}: recent [{f}, {t}]'s provisional resample key"
            );
            assert_eq!(
                stored_quote_ts_within(&hist, (f, t)).len(),
                if kept { 3 } else { 0 },
                "k = {k}: recent [{f}, {t}]'s ticks"
            );
            assert_eq!(
                stored_bars_within(&hist, INTERVAL, (f, t)).len(),
                if kept { 3 } else { 0 },
                "k = {k}: recent [{f}, {t}]'s bars"
            );
        }
    }
}

/// **T2, a hole before the stop.** Day 6 of days 5..=8 is REFUSED by the store and skipped; the probe
/// then fires before day 8. The answer is a `Stopped` — the request did not run to its end, so it is
/// not the `SupersedeRefused` that says it did — and the stop's text NAMES day 6, its reason and the
/// remedy, so a stop never hides a hole the request had already made.
#[test]
fn a_stop_after_a_refused_chunk_names_the_refused_chunk() {
    let (_dir, hist) = open();
    let (start, end) = (5 * MS_PER_DAY, 9 * MS_PER_DAY - 1);
    let day_6 = (6 * MS_PER_DAY, 7 * MS_PER_DAY - 1);
    fold_provisional_quotes_into_a_neighbour(&hist, day_6);
    let probe = FiresAfter::new(3);
    let mut calls = Vec::new();

    let stopped = backfill_quotes_then_bars(
        &hist,
        SYMBOL,
        INTERVAL,
        start,
        end,
        &|| probe.ask(),
        |_, f, t| {
            calls.push(f / MS_PER_DAY);
            Ok(tape_between(f, t))
        },
    );

    let Err(CollectError::Stopped(text)) = &stopped else {
        panic!("a stop answers Stopped even with a refusal behind it: {stopped:?}")
    };
    assert_eq!(calls, vec![5, 6, 7], "day 8 is past the boundary and never fetched");
    assert!(text.contains("stopped before chunk 4 of 4 [691200000, 777599999]"), "{text}");
    assert!(text.contains("The 3 chunk(s) before it ran, writing 6 1m bars"), "{text}");
    let named = format!(
        "But 1 of those 3 chunk(s) were SKIPPED before the stop, because the store REFUSED to \
         supersede them, and hold no 1m bars: [{}, {}].",
        day_6.0, day_6.1
    );
    assert!(text.contains(&named), "the refused chunk is named by its bounds: {text}");
    assert!(
        text.contains("vike-cli data hist rm") && text.contains("--produced-by dukascopy"),
        "the remedy rides the stop too: {text}"
    );
}

/// A probe that fires at the very first ask: nothing is fetched, nothing is stored, and the answer
/// is still `Stopped` — not the `Ok(0)` a request with nothing to do gives, which a caller reads as
/// "this window is in the store".
#[test]
fn a_probe_that_fires_at_once_stops_the_tick_lane_before_anything_is_fetched() {
    let (_dir, hist) = open();

    let stopped = backfill_quotes_then_bars(
        &hist,
        SYMBOL,
        INTERVAL,
        5 * MS_PER_DAY,
        7 * MS_PER_DAY - 1,
        &|| true,
        |_, _, _| panic!("a request stopped before its first chunk fetched"),
    );

    let Err(CollectError::Stopped(text)) = &stopped else {
        panic!("stopped before the first chunk must still answer Stopped: {stopped:?}")
    };
    assert!(text.contains("stopped before chunk 1 of 2"), "{text}");
    assert!(text.contains("The 0 chunk(s) before it ran, writing 0 1m bars"), "{text}");
    assert!(hist.scan_quotes(VENUE, SYMBOL, TsRange::all()).unwrap().is_empty());
    assert!(stored_bar_ts(&hist).is_empty());
}
