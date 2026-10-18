//! THE RECENT CHUNKS: the ones the feed may not have finished publishing.

use super::*;

// ── THE RECENT CHUNKS: the ones the feed may not have finished publishing ──────────────────────
//
// Every window above lies in 1970, so each of its chunks is SETTLED and takes the shared keys its
// bounds give it. A chunk whose last millisecond is inside the publication margin is not: the lane
// stores it like any other chunk — at once, one chunk at a time — but under the PROVISIONAL twins
// of those keys, which the chunk's settled commit later supersedes. "Recent" is measured against
// the wall clock, which the lane reads itself, once per request, as
// `crates/vike-backfill/src/klines.rs`'s `ingest_klines` does. So these tests' days are placed
// relative to today's UTC midnight, where every chunk's side of the margin holds for the whole of
// today's UTC date: the one way they can go wrong is for the next UTC midnight to fall between a
// test's own clock read and the lane's, which would move the day before yesterday out of the
// margin. And since settledness cannot be faked FORWARD inside a test, a recent chunk "settling" is
// simulated with `settle_quotes`/`settle_bars` below: the calls the lane's `Settled` arm makes,
// with the keys it makes them with.

/// Simulate the chunk `(from, to)` having SETTLED and been fetched again, answering `tape`: the
/// quote-side call the lane's `Settled` arm makes, under the chunk's canonical key and superseding
/// its provisional twin. Returns the quote rows written.
fn settle_quotes(hist: &DataFusionHist, (from, to): (i64, i64), tape: &[QuoteTick]) -> usize {
    let key = quote_commit_key(SYMBOL, from, to);
    let provisional = provisional_quote_commit_key(SYMBOL, from, to);
    hist.append_quotes_superseding(VENUE, SYMBOL, tape, Some(&key), Some(&provisional)).unwrap()
}

/// [`settle_quotes`]' bar-side twin: the `interval` resample of `(from, to)` — whole bars, as a
/// full chunk always is — under its canonical resample key, superseding its provisional twin.
/// Returns the bars written.
fn settle_bars(hist: &DataFusionHist, interval: &str, (from, to): (i64, i64)) -> usize {
    let range = TsRange::of(from, to);
    let key = resample_commit_key(SYMBOL, interval, range);
    let provisional = provisional_resample_key(SYMBOL, interval, range);
    hist.resample_quotes_to_bars_superseding(
        VENUE,
        SYMBOL,
        interval,
        range,
        Some(&key),
        Some(&provisional),
    )
    .unwrap()
}

/// What `consolidate_quotes` makes of `tape` at `step`, in [`stored_bars_at`]'s form: the bars of a
/// store that holds `tape` exactly once and nothing beside it — so a bar left beside its settled
/// twin, or a `volume` counting a tick twice, cannot compare equal.
fn bars_of(tape: &[QuoteTick], step: i64) -> Vec<(i64, [u64; 5])> {
    consolidate_quotes(tape, step)
        .iter()
        .map(|b| (b.ts, [b.open, b.high, b.low, b.close, b.volume].map(f64::to_bits)))
        .collect()
}

/// The `ts` of every tick in `tape`, in its order.
fn ts_of(tape: &[QuoteTick]) -> Vec<i64> {
    tape.iter().map(|q| q.ts).collect()
}

/// Four whole days ending with yesterday. The two oldest END more than two days ago, so they are
/// settled and take their shared day keys; the two newest do not, so each is fetched, stored and
/// resampled on its own like any other chunk — under the PROVISIONAL keys of its OWN bounds and its
/// own whole buckets, which are the very keys its settled commit will later supersede, its
/// canonical ones left unspent for that commit. Nothing is keyed by the request: a batch keyed by
/// the request's own bounds is what no settled commit ever superseded (the tests below show what
/// that cost). A repeat of the same request stores nothing new — every key it would spend is spent.
#[test]
fn the_chunks_inside_the_publication_margin_are_keyed_by_their_own_bounds() {
    let (_dir, hist) = open();
    let now = vike_model::now_ms();
    let today = now - now.rem_euclid(MS_PER_DAY);
    // The first millisecond of the UTC day `n` days before today's.
    let day = |n: i64| today - n * MS_PER_DAY;
    let (start, end) = (day(4), day(0) - 1);
    let mut calls = Vec::new();

    let bars =
        backfill_quotes_then_bars(&hist, SYMBOL, INTERVAL, start, end, &|| false, |_, from, to| {
            calls.push((from, to));
            Ok(tape_between(from, to))
        })
        .unwrap();

    assert_eq!(
        calls,
        vec![
            (day(4), day(3) - 1),
            (day(3), day(2) - 1),
            (day(2), day(1) - 1),
            (day(1), day(0) - 1),
        ],
        "every chunk is still fetched on its own — the margin changes keys, not fetches"
    );
    assert_eq!(bars, 12, "four days, each with three minutes that hold a tick");
    // The two settled days spent their shared day keys...
    assert!(quote_key_is_spent(&hist, &quote_commit_key(SYMBOL, day(4), day(3) - 1)));
    assert!(quote_key_is_spent(&hist, &quote_commit_key(SYMBOL, day(3), day(2) - 1)));
    // ...and each recent day the PROVISIONAL keys of its own bounds, and nothing else.
    for (from, to) in [(day(2), day(1) - 1), (day(1), day(0) - 1)] {
        let range = TsRange::of(from, to);
        let provisional_resample = provisional_resample_key(SYMBOL, INTERVAL, range);
        assert!(
            quote_series_has_commit(&hist, &provisional_quote_commit_key(SYMBOL, from, to)),
            "[{from}, {to}] stored its ticks under its own provisional key"
        );
        assert!(
            bar_series_has_commit(&hist, INTERVAL, &provisional_resample),
            "[{from}, {to}] resampled its own whole buckets under its own provisional key"
        );
        assert!(
            !quote_series_has_commit(&hist, &quote_commit_key(SYMBOL, from, to)),
            "[{from}, {to}]'s canonical quote key is left for its settled commit"
        );
        assert!(
            !bar_series_has_commit(&hist, INTERVAL, &resample_commit_key(SYMBOL, INTERVAL, range)),
            "[{from}, {to}]'s canonical resample key is left for its settled commit"
        );
    }
    assert!(
        !quote_series_has_commit(&hist, &quote_commit_key(SYMBOL, start, end)),
        "no quote key is the request's own bounds"
    );
    assert!(
        !bar_series_has_commit(
            &hist,
            INTERVAL,
            &resample_commit_key(SYMBOL, INTERVAL, TsRange::of(start, end))
        ),
        "no resample key is the request's own bounds"
    );

    let repeat =
        backfill_quotes_then_bars(&hist, SYMBOL, INTERVAL, start, end, &|| false, |_, from, to| {
            Ok(tape_between(from, to))
        })
        .unwrap();
    assert_eq!(repeat, 0, "the same request again: every key it would spend is spent");
    let quotes = hist.scan_quotes(VENUE, SYMBOL, TsRange::all()).unwrap();
    assert_eq!(quotes.len(), 12, "every tick stored once, the repeat adding none");
}

/// One day named the only way date labels can name it — `[D, D + 1 day]`, since `--from D --to D`
/// is refused — is TWO chunks: day D and the next day's first millisecond, which holds no bar.
/// Played out with yesterday, fetched before its last minute is published: the lane writes the two
/// bars it could, under day D's own PROVISIONAL keys. When the finished day is later stored the
/// way the settled pass stores it — under yesterday's canonical keys, superseding the provisional
/// ones — those canonical keys are still unspent, so all three bars are written; and the early
/// fetch's ticks and bars are REPLACED, not left beside the day's, so every tick is stored once
/// and every bar is the finished day's alone, `volume` included, bit for bit.
///
/// ⚠ This is the reproduction of #9 in
/// `docs/superpowers/specs/2026-09-30-provisional-commits-followups-design.md`. Before #9, this
/// window's two chunks were stored as ONE batch keyed by the request's own bounds, which the
/// settled commit had nothing to supersede: the day's first two ticks ended up stored twice, the
/// two early bars sat beside the settled ones, and the settled bars' `volume` counted the
/// duplicates — while the only thing this test then asserted, how many bars the settled resample
/// wrote, still passed. The two count assertions below are that old test's, made through the
/// superseding calls the settled pass really makes; the two after them are what it lacked.
#[test]
fn a_recent_one_day_window_leaves_the_days_own_resample_key_unspent() {
    let (_dir, hist) = open();
    let now = vike_model::now_ms();
    let today = now - now.rem_euclid(MS_PER_DAY);
    let yesterday = today - MS_PER_DAY;
    let mut calls = Vec::new();

    // Yesterday's last minute is not published yet, so the fetch answers without it.
    let bars = backfill_quotes_then_bars(
        &hist,
        SYMBOL,
        INTERVAL,
        yesterday,
        today,
        &|| false,
        |_, from, to| {
            calls.push((from, to));
            Ok(tape_between(from, to.min(today - 60_001)))
        },
    )
    .unwrap();

    assert_eq!(calls, vec![(yesterday, today - 1), (today, today)], "a day, then one millisecond");
    assert_eq!(bars, 2, "yesterday's first two minutes; its last was not published yet");
    let finished_day = tape_between(yesterday, today - 1);
    assert_eq!(
        settle_quotes(&hist, (yesterday, today - 1), &finished_day),
        3,
        "yesterday's own quote key was not spent by the early fetch"
    );
    assert_eq!(
        settle_bars(&hist, INTERVAL, (yesterday, today - 1)),
        3,
        "yesterday's own resample key was not spent by the early fetch"
    );
    assert_eq!(
        stored_quote_ts_within(&hist, (yesterday, today)),
        ts_of(&finished_day),
        "the finished day's ticks, each once: the early fetch's two were replaced, not kept"
    );
    assert_eq!(
        stored_bars_at(&hist, INTERVAL),
        bars_of(&finished_day, STEP_MS),
        "the finished day's bars alone, bit for bit: no early bar beside them, and no `volume` \
         counting a tick twice"
    );
}

/// A real recent request over TWO days — the day before yesterday and yesterday, each fetched
/// before its last minute was published — and then each day SETTLING in turn. Each settled commit
/// supersedes exactly ITS day's provisional entry: once the older day has settled it holds its full
/// tape once while the newer day still holds its early answer, and once both have settled the
/// store holds exactly the full tape, every bar bit for bit what `consolidate_quotes` makes of it.
#[test]
fn a_multi_day_tail_is_superseded_day_by_day_once_it_settles() {
    let (_dir, hist) = open();
    let now = vike_model::now_ms();
    let today = now - now.rem_euclid(MS_PER_DAY);
    let day = |n: i64| today - n * MS_PER_DAY;
    let (older, newer) = ((day(2), day(1) - 1), (day(1), day(0) - 1));

    // Each day's fetch answers its first two ticks only: its last minute is not published yet.
    let early = backfill_quotes_then_bars(
        &hist,
        SYMBOL,
        INTERVAL,
        older.0,
        newer.1,
        &|| false,
        |_, f, t| Ok(tape_between(f, t.min(f + 80_000))),
    )
    .unwrap();
    assert_eq!(early, 4, "two minutes a day, for two days");

    let full_older = tape_between(older.0, older.1);
    assert_eq!(settle_quotes(&hist, older, &full_older), 3, "the older day's canonical key");
    assert_eq!(settle_bars(&hist, INTERVAL, older), 3, "the older day's canonical resample key");
    assert_eq!(
        stored_quote_ts_within(&hist, older),
        ts_of(&full_older),
        "the settled day holds its full tape once: its early answer was superseded"
    );
    assert_eq!(stored_bars_within(&hist, INTERVAL, older), bars_of(&full_older, STEP_MS));
    let early_newer = tape_between(newer.0, newer.0 + 80_000);
    assert_eq!(
        stored_quote_ts_within(&hist, newer),
        ts_of(&early_newer),
        "the day not yet settled keeps its early answer: the supersede took its own day's entry"
    );
    assert_eq!(stored_bars_within(&hist, INTERVAL, newer), bars_of(&early_newer, STEP_MS));

    let full_newer = tape_between(newer.0, newer.1);
    assert_eq!(settle_quotes(&hist, newer, &full_newer), 3, "the newer day's canonical key");
    assert_eq!(settle_bars(&hist, INTERVAL, newer), 3, "the newer day's canonical resample key");
    let full = tape_between(older.0, newer.1);
    let quotes = hist.scan_quotes(VENUE, SYMBOL, TsRange::all()).unwrap();
    assert_eq!(ts_of(&quotes), ts_of(&full), "exactly the full tape, each tick once");
    assert_eq!(
        stored_bars_at(&hist, INTERVAL),
        bars_of(&full, STEP_MS),
        "every bar bit for bit the full tape's — `volume` above all, which a tick stored twice \
         inflates"
    );
}

/// The RECENT twin of the skip-before-fetch, proven with a fetch that panics if it is called at
/// all. A recent chunk whose provisional key an earlier request spent — the same request again, or
/// a different one that cuts the chunk the same way — is not downloaded again: the store would
/// discard the answer under the spent key. A NEW interval still gets its bars, resampled from the
/// stored ticks under the chunk's PROVISIONAL resample key (so its settled commit still supersedes
/// them), bit for bit what a request at that interval writes on a store that never saw the first.
#[test]
fn a_recent_chunk_an_earlier_request_stored_is_not_fetched_again() {
    let (_dir, hist) = open();
    let now = vike_model::now_ms();
    let today = now - now.rem_euclid(MS_PER_DAY);
    let day = |n: i64| today - n * MS_PER_DAY;
    let (start, end) = (day(2), day(0) - 1);
    let first =
        backfill_quotes_then_bars(&hist, SYMBOL, INTERVAL, start, end, &|| false, |_, from, to| {
            Ok(tape_between(from, to))
        })
        .unwrap();
    assert_eq!(first, 6, "two recent days of three minutes with a tick");

    let repeat =
        backfill_quotes_then_bars(&hist, SYMBOL, INTERVAL, start, end, &|| false, |_, _, _| {
            panic!("a recent chunk whose provisional key is spent must not be fetched again")
        })
        .unwrap();
    assert_eq!(repeat, 0, "the same request again: nothing to fetch, nothing to write");
    let yesterday = backfill_quotes_then_bars(
        &hist,
        SYMBOL,
        INTERVAL,
        day(1),
        day(0) - 1,
        &|| false,
        |_, _, _| panic!("another request cutting yesterday the same way must not fetch it either"),
    )
    .unwrap();
    assert_eq!(yesterday, 0, "yesterday alone is the same cut, already stored");
    let quotes = hist.scan_quotes(VENUE, SYMBOL, TsRange::all()).unwrap();
    assert_eq!(quotes.len(), 6, "every tick stored once");

    let five_minute =
        backfill_quotes_then_bars(&hist, SYMBOL, "5m", start, end, &|| false, |_, _, _| {
            panic!("a new interval over stored recent ticks resamples them, it does not fetch")
        })
        .unwrap();
    assert_eq!(five_minute, 4, "two 5m bars a day, from the stored ticks");
    for (from, to) in [(day(2), day(1) - 1), (day(1), day(0) - 1)] {
        let range = TsRange::of(from, to);
        assert!(
            bar_series_has_commit(&hist, "5m", &provisional_resample_key(SYMBOL, "5m", range)),
            "[{from}, {to}]'s 5m bars are PROVISIONAL, for its settled commit to supersede"
        );
        assert!(
            !bar_series_has_commit(&hist, "5m", &resample_commit_key(SYMBOL, "5m", range)),
            "[{from}, {to}]'s canonical 5m key is left for its settled commit"
        );
    }
    let (_control_dir, control) = open();
    backfill_quotes_then_bars(&control, SYMBOL, "5m", start, end, &|| false, |_, from, to| {
        Ok(tape_between(from, to))
    })
    .unwrap();
    assert_eq!(stored_bars_at(&hist, "5m"), stored_bars_at(&control, "5m"));
}

/// What the skip's bars are keyed under is what decides whether they ever converge, and this is
/// the test that can tell. Yesterday is fetched early at 1m, then asked for at 5m — no fetch, its
/// 5m bars resampled from the EARLY ticks — and then it settles at both intervals. Under the
/// provisional key the settled 5m resample supersedes those early bars; had the skip spent the
/// CANONICAL key, that resample would find it spent and write nothing, and the day's 5m bars would
/// stay built from its early ticks for good, with no error anywhere.
#[test]
fn a_recent_chunks_bars_resampled_from_the_store_are_superseded_once_it_settles() {
    let (_dir, hist) = open();
    let now = vike_model::now_ms();
    let today = now - now.rem_euclid(MS_PER_DAY);
    let yesterday = (today - MS_PER_DAY, today - 1);

    let early = backfill_quotes_then_bars(
        &hist,
        SYMBOL,
        "1m",
        yesterday.0,
        yesterday.1,
        &|| false,
        |_, f, t| Ok(tape_between(f, t.min(f + 80_000))),
    )
    .unwrap();
    assert_eq!(early, 2, "the 10 s and 70 s ticks have arrived: two 1m bars");
    let five_minute = backfill_quotes_then_bars(
        &hist,
        SYMBOL,
        "5m",
        yesterday.0,
        yesterday.1,
        &|| false,
        |_, _, _| panic!("yesterday's ticks are stored: the 5m request resamples them"),
    )
    .unwrap();
    assert_eq!(five_minute, 1, "both early ticks lie in the day's first 5m bar");

    let finished_day = tape_between(yesterday.0, yesterday.1);
    assert_eq!(settle_quotes(&hist, yesterday, &finished_day), 3);
    assert_eq!(settle_bars(&hist, "1m", yesterday), 3);
    assert_eq!(
        settle_bars(&hist, "5m", yesterday),
        2,
        "the canonical 5m key was left unspent by the skip, so the settled 5m resample runs"
    );
    assert_eq!(stored_quote_ts_within(&hist, yesterday), ts_of(&finished_day));
    assert_eq!(stored_bars_at(&hist, "1m"), bars_of(&finished_day, STEP_MS));
    assert_eq!(
        stored_bars_at(&hist, "5m"),
        bars_of(&finished_day, 5 * STEP_MS),
        "the 5m bars are the finished day's: the early ones were superseded, not kept"
    );
}

/// An interval that does NOT divide a day. At 7m a chunk is the 205 whole bars a UTC day holds —
/// 1,435 minutes — so the grid's cells drift against midnight and a cell usually straddles two UTC
/// days. A recent cell is keyed all the same by its own bounds, and since the grid is anchored at
/// epoch 0 those are the bounds the settled pass cuts it to, whichever request covers it: so once
/// the cell settles, its provisional entry is superseded exactly, and the cell after it — not
/// settled — keeps its own.
#[test]
fn a_recent_cell_of_an_interval_that_does_not_divide_a_day_is_superseded_exactly() {
    const SEVEN_MINUTES: i64 = 7 * 60_000;
    // `chunk_len` at 7m, pinned on the lane's own grid by its `dukascopy_tests.rs` unit test.
    const CELL: i64 = 205 * SEVEN_MINUTES;
    let (_dir, hist) = open();
    let now = vike_model::now_ms();
    // The first cell starting after the margin's edge, and the one after it: both END inside the
    // margin by about a day, so neither can cross it while the test runs.
    let first = (now - 2 * MS_PER_DAY).div_euclid(CELL) + 1;
    let cell = |k: i64| (k * CELL, (k + 1) * CELL - 1);
    let (a, b) = (cell(first), cell(first + 1));
    let mut calls = Vec::new();

    let bars =
        backfill_quotes_then_bars(&hist, SYMBOL, "7m", a.0, b.1, &|| false, |_, from, to| {
            calls.push((from, to));
            Ok(tape_between(from, to))
        })
        .unwrap();

    assert_eq!(calls, vec![a, b], "cut on the 7m grid, not at midnight");
    assert_eq!(bars, bars_of(&tape_between(a.0, b.1), SEVEN_MINUTES).len());
    for (from, to) in [a, b] {
        let range = TsRange::of(from, to);
        assert!(
            quote_series_has_commit(&hist, &provisional_quote_commit_key(SYMBOL, from, to)),
            "the cell [{from}, {to}] stored its ticks under its own provisional key"
        );
        assert!(
            bar_series_has_commit(&hist, "7m", &provisional_resample_key(SYMBOL, "7m", range)),
            "the cell [{from}, {to}] resampled under its own provisional key"
        );
    }

    let full_a = tape_between(a.0, a.1);
    settle_quotes(&hist, a, &full_a);
    settle_bars(&hist, "7m", a);
    assert_eq!(
        stored_quote_ts_within(&hist, a),
        ts_of(&full_a),
        "the settled cell holds its ticks once: its provisional entry was superseded exactly"
    );
    assert_eq!(stored_bars_within(&hist, "7m", a), bars_of(&full_a, SEVEN_MINUTES));
    assert_eq!(
        stored_quote_ts_within(&hist, b),
        ts_of(&tape_between(b.0, b.1)),
        "the cell not yet settled keeps its own entry"
    );
}

/// A PARTIAL recent chunk — the shape the `[today, now]` end of every `--days N` request has, and
/// its start too when N is 2 or less — is keyed by its OWN cut, never by the whole grid cell it
/// lies in. Two things follow, and this test pins both.
///
/// The reason for the choice: a partial cut does not freeze its day. A later request for the whole
/// of that day finds the day's own provisional key unspent, and writes the part the partial cut
/// never asked for. Keyed by its whole cell, the partial cut would have spent that key, and the
/// later request would have written nothing — the alternative the design rejected.
///
/// What it costs, DECLARED rather than missed: no settled commit cuts the day that way, so none
/// supersedes the partial entry, and once the day settles the partial cut's ticks and bars still
/// sit beside the full day's — the residual only a supersede by CONTAINMENT would close (#9 of
/// `docs/superpowers/specs/2026-09-30-provisional-commits-followups-design.md`, owner question 4).
/// When that primitive lands, the last assertions here are the ones that flip.
#[test]
fn a_partial_recent_chunk_keeps_its_own_cut_and_stays_beside_its_settled_day() {
    let (_dir, hist) = open();
    let now = vike_model::now_ms();
    let today = now - now.rem_euclid(MS_PER_DAY);
    let day = |n: i64| today - n * MS_PER_DAY;
    let whole = (day(2), day(1) - 1);
    // A request starting at noon of the day before yesterday: its first chunk is that day's second
    // half, a partial cut — and still recent, like the yesterday after it.
    let partial = (day(2) + MS_PER_DAY / 2, day(1) - 1);
    let mut calls = Vec::new();
    backfill_quotes_then_bars(
        &hist,
        SYMBOL,
        INTERVAL,
        partial.0,
        day(0) - 1,
        &|| false,
        |_, from, to| {
            calls.push((from, to));
            Ok(tape_between(from, to))
        },
    )
    .unwrap();
    assert_eq!(calls, vec![partial, (day(1), day(0) - 1)]);
    assert!(
        quote_series_has_commit(&hist, &provisional_quote_commit_key(SYMBOL, partial.0, partial.1)),
        "the partial chunk is keyed by its own cut"
    );
    assert!(
        !quote_series_has_commit(&hist, &provisional_quote_commit_key(SYMBOL, whole.0, whole.1)),
        "and not by the whole day it lies in"
    );

    let mut later_calls = Vec::new();
    let later = backfill_quotes_then_bars(
        &hist,
        SYMBOL,
        INTERVAL,
        whole.0,
        day(0) - 1,
        &|| false,
        |_, from, to| {
            later_calls.push((from, to));
            Ok(tape_between(from, to))
        },
    )
    .unwrap();
    assert_eq!(
        later_calls,
        vec![whole],
        "the whole day is fetched — the partial cut did not freeze it — and yesterday, cut the \
         same way as before, is skipped"
    );
    assert_eq!(later, 3, "the whole day's three minutes with a tick, its first half's included");

    let full = tape_between(whole.0, whole.1);
    settle_quotes(&hist, whole, &full);
    settle_bars(&hist, INTERVAL, whole);
    let mut beside = ts_of(&full);
    beside.extend(ts_of(&tape_between(partial.0, partial.1)));
    beside.sort_unstable();
    assert_eq!(
        stored_quote_ts_within(&hist, whole),
        beside,
        "THE DECLARED RESIDUAL: the settled day's ticks, and the partial cut's still beside them"
    );
    let last_minute = day(1) - 60_000;
    let last_minute_bars = stored_bar_ts(&hist).into_iter().filter(|&ts| ts == last_minute).count();
    assert_eq!(last_minute_bars, 2, "the partial cut's bar sits beside the settled day's");
    assert!(
        quote_series_has_commit(&hist, &provisional_quote_commit_key(SYMBOL, partial.0, partial.1)),
        "no settled commit superseded the partial cut's provisional entry"
    );
}
