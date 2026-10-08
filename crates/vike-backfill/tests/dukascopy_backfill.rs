//! Offline gate for the Dukascopy → hist-store ingest + resample (docs/decisions/0094):
//! build a synthetic `Vec<QuoteTick>` directly, ingest into a `tempdir` `DataFusionHist`, and assert
//! bit-exact round-trips (`to_bits`). Then resample and assert the stored bars match
//! `vike_model::consolidate_quotes` bit-exactly, plus commit-key idempotency — and drive the whole
//! `TickBars` lane, `backfill_quotes_then_bars`, over aligned and unaligned windows, over
//! windows long enough to be cut into day-sized chunks, and over chunks the store REFUSES to
//! supersede. The fetch half (and its own `#[ignore]`d
//! live test) moved to `crates/bridges/dukascopy/tests/quotes.rs`; this file is deterministic +
//! offline, and — since it no longer names the bridge — ungated.

use vike_backfill::CollectError;
use vike_backfill::venues::dukascopy::{
    VENUE, backfill_quotes_then_bars, ingest_quotes, provisional_quote_commit_key,
    provisional_resample_key, quote_commit_key, resample_and_store_bars, resample_commit_key,
};
use vike_data::{CompactionConfig, DataFusionHist, HistStore, SeriesId, TsRange};
use vike_model::{MS_PER_DAY, QuoteTick, consolidate_quotes};

const SYMBOL: &str = "EURUSD";
const STEP_MS: i64 = 60_000;
const INTERVAL: &str = "1m";

/// A deterministic synthetic quote stream (ascending ts) spanning two 1-minute buckets, so the
/// resample yields >1 bar. Values are arbitrary — the assertions compare bit patterns of whatever
/// we ingest, so no "exact f64 repr" contrivance is needed. Built directly rather than through
/// `tick_to_quote` (bid_vol/ask_vol -> bid_size/ask_size), which moved to the bridge crate.
fn synthetic_quotes() -> Vec<QuoteTick> {
    vec![
        QuoteTick {
            ts: 0,
            local_ts: 0,
            bid: 1.10001,
            ask: 1.10003,
            bid_size: 1.5,
            ask_size: 2.25,
            symbol: SYMBOL.to_string(),
        },
        QuoteTick {
            ts: 1_000,
            local_ts: 0,
            bid: 1.10010,
            ask: 1.10012,
            bid_size: 0.5,
            ask_size: 3.0,
            symbol: SYMBOL.to_string(),
        },
        QuoteTick {
            ts: 2_000,
            local_ts: 0,
            bid: 1.09990,
            ask: 1.09992,
            bid_size: 4.0,
            ask_size: 1.0,
            symbol: SYMBOL.to_string(),
        },
        QuoteTick {
            ts: 60_000,
            local_ts: 0,
            bid: 1.10100,
            ask: 1.10103,
            bid_size: 2.0,
            ask_size: 2.0,
            symbol: SYMBOL.to_string(),
        },
        QuoteTick {
            ts: 61_500,
            local_ts: 0,
            bid: 1.10080,
            ask: 1.10082,
            bid_size: 1.0,
            ask_size: 1.25,
            symbol: SYMBOL.to_string(),
        },
    ]
}

#[test]
fn append_then_scan_is_bit_exact() {
    let dir = tempfile::tempdir().unwrap();
    let hist = DataFusionHist::open(dir.path()).unwrap();
    let quotes = synthetic_quotes();

    let n = hist.append_quotes(VENUE, SYMBOL, &quotes, Some("k1")).unwrap();
    assert_eq!(n, quotes.len());

    let got = hist.scan_quotes(VENUE, SYMBOL, TsRange::all()).unwrap();
    assert_eq!(got.len(), quotes.len());
    for (a, b) in quotes.iter().zip(got.iter()) {
        assert_eq!(a.ts, b.ts);
        assert_eq!(a.bid.to_bits(), b.bid.to_bits());
        assert_eq!(a.ask.to_bits(), b.ask.to_bits());
        assert_eq!(a.bid_size.to_bits(), b.bid_size.to_bits());
        assert_eq!(a.ask_size.to_bits(), b.ask_size.to_bits());
        assert_eq!(a.symbol, b.symbol);
    }
}

#[test]
fn resample_matches_consolidate_quotes_bit_exact() {
    let dir = tempfile::tempdir().unwrap();
    let hist = DataFusionHist::open(dir.path()).unwrap();
    let quotes = synthetic_quotes();
    hist.append_quotes(VENUE, SYMBOL, &quotes, Some("k1")).unwrap();

    let n = resample_and_store_bars(&hist, SYMBOL, INTERVAL, TsRange::all()).unwrap();
    let expect = consolidate_quotes(&quotes, STEP_MS);
    assert_eq!(n, expect.len());
    assert!(expect.len() >= 2, "two buckets should produce >=2 bars");

    let got = hist.load_bars(VENUE, SYMBOL, INTERVAL, TsRange::all()).unwrap();
    assert_eq!(got.len(), expect.len());
    // bid/ask are intentionally NOT persisted by the bar store (schema = OHLCV + funding), so they
    // read back as None; compare the STORED fields bit-exactly instead of a whole-Bar equality.
    for (e, g) in expect.iter().zip(got.iter()) {
        assert_eq!(e.ts, g.ts);
        assert_eq!(e.open.to_bits(), g.open.to_bits());
        assert_eq!(e.high.to_bits(), g.high.to_bits());
        assert_eq!(e.low.to_bits(), g.low.to_bits());
        assert_eq!(e.close.to_bits(), g.close.to_bits());
        assert_eq!(e.volume.to_bits(), g.volume.to_bits());
    }
}

#[test]
fn append_is_idempotent_by_commit_key() {
    let dir = tempfile::tempdir().unwrap();
    let hist = DataFusionHist::open(dir.path()).unwrap();
    let quotes = synthetic_quotes();
    let key = "dukascopy:EURUSD:0-61500";

    let first = hist.append_quotes(VENUE, SYMBOL, &quotes, Some(key)).unwrap();
    assert_eq!(first, quotes.len());
    let second = hist.append_quotes(VENUE, SYMBOL, &quotes, Some(key)).unwrap();
    assert_eq!(second, 0, "same commit key twice must be a no-op");
}

// ── THE TICK LANE: every tick stored, only WHOLE bars resampled ─────────────────────────────
//
// `backfill_quotes_then_bars` is what the datahub's `TickBars` row runs. Its fetch here returns all
// five `synthetic_quotes` whatever window it is asked for, so "all five stored" proves the quote
// ingest is NOT trimmed to the window, while the bar count proves the resample IS — to the buckets
// lying wholly inside it (the function's doc argues why a partial edge bar cannot be stored). Every
// test gets its own store: a shared one would turn a later call into a commit-key no-op.

/// The `ts` of every bar the lane stored, ascending.
fn stored_bar_ts(hist: &DataFusionHist) -> Vec<i64> {
    let bars = hist.load_bars(VENUE, SYMBOL, INTERVAL, TsRange::all()).unwrap();
    bars.iter().map(|b| b.ts).collect()
}

/// A window STARTING mid-bucket: `[30_000, 119_999]` holds only the second half of the 0 bucket, so
/// that bucket is trimmed and ONE bar — the whole 60_000 one — is resampled, while all five fetched
/// ticks are stored under the untrimmed window.
///
/// ⚠ The bar count alone cannot tell the trim from the old untrimmed resample here — the synthetic
/// tape has no tick in `[30_000, 59_999]`, so both write one bar. The last assertion is what can:
/// the lane must have spent the resample key of `[60_000, 119_999]`, the trimmed range, so a second
/// resample of exactly that range is a no-op. An untrimmed lane spends `[30_000, 119_999]`'s key
/// instead and that second resample writes the bar again.
#[test]
fn an_unaligned_start_resamples_only_the_whole_buckets_inside_the_window() {
    let dir = tempfile::tempdir().unwrap();
    let hist = DataFusionHist::open(dir.path()).unwrap();
    let mut asked = None;

    let bars = backfill_quotes_then_bars(
        &hist,
        SYMBOL,
        INTERVAL,
        30_000,
        119_999,
        &|| false,
        |sym, from, to| {
            asked = Some((sym.to_string(), from, to));
            Ok(synthetic_quotes())
        },
    )
    .unwrap();

    assert_eq!(
        asked,
        Some((SYMBOL.to_string(), 30_000, 119_999)),
        "the fetch is asked for the UNTRIMMED window"
    );
    assert_eq!(bars, 1, "only the 60_000 bucket lies wholly inside [30_000, 119_999]");
    assert_eq!(stored_bar_ts(&hist), vec![60_000]);
    let quotes = hist.scan_quotes(VENUE, SYMBOL, TsRange::all()).unwrap();
    assert_eq!(quotes.len(), 5, "every fetched tick is stored — the trim is the resample's alone");
    assert_eq!(
        resample_and_store_bars(&hist, SYMBOL, INTERVAL, TsRange::of(60_000, 119_999)).unwrap(),
        0,
        "the lane resampled exactly the trimmed range [60_000, 119_999], so its key is spent"
    );
}

/// A window ENDING mid-bucket: `[0, 90_000]` cuts the 60_000 bucket at its 90_000 ms, so only the
/// 0 bucket is whole and ONE bar is written — where an untrimmed resample wrote two, the second a
/// bar keyed at 60_000 but built from part of its minute.
#[test]
fn an_unaligned_end_drops_the_partial_last_bucket() {
    let dir = tempfile::tempdir().unwrap();
    let hist = DataFusionHist::open(dir.path()).unwrap();

    let bars =
        backfill_quotes_then_bars(&hist, SYMBOL, INTERVAL, 0, 90_000, &|| false, |_, _, _| {
            Ok(synthetic_quotes())
        })
        .unwrap();

    assert_eq!(bars, 1, "the 60_000 bucket is cut at 90_000, so only the 0 bucket is whole");
    assert_eq!(stored_bar_ts(&hist), vec![0]);
    let quotes = hist.scan_quotes(VENUE, SYMBOL, TsRange::all()).unwrap();
    assert_eq!(quotes.len(), 5, "the edge ticks are stored for a wider window's resample to find");
}

/// A window on the bucket grid — `[0, 119_999]`, two whole minutes — is untouched by the trim: two
/// bars, exactly as before it existed. The datahub's
/// `crates/vike-datahub/tests/backfill_roundtrip.rs`'s
/// `the_tick_lane_stores_the_quotes_and_answers_with_the_resampled_bars` drives the same window
/// through the wire.
#[test]
fn an_aligned_window_resamples_every_bucket() {
    let dir = tempfile::tempdir().unwrap();
    let hist = DataFusionHist::open(dir.path()).unwrap();

    let bars =
        backfill_quotes_then_bars(&hist, SYMBOL, INTERVAL, 0, 119_999, &|| false, |_, _, _| {
            Ok(synthetic_quotes())
        })
        .unwrap();

    assert_eq!(bars, 2);
    assert_eq!(stored_bar_ts(&hist), vec![0, 60_000]);
    let quotes = hist.scan_quotes(VENUE, SYMBOL, TsRange::all()).unwrap();
    assert_eq!(quotes.len(), 5);
}

/// `0m` parses — `vike_model::time::interval_ms` reads it as `Some(0)` — but a zero-width bucket is
/// no bar, and `consolidate_quotes` would divide by it. So the lane REFUSES it, before the fetch: the
/// closure panics if it is ever called, and nothing is stored.
#[test]
fn a_zero_width_interval_is_refused_before_anything_is_fetched() {
    let dir = tempfile::tempdir().unwrap();
    let hist = DataFusionHist::open(dir.path()).unwrap();

    let refused =
        backfill_quotes_then_bars(&hist, SYMBOL, "0m", 0, 119_999, &|| false, |_, _, _| {
            panic!("a zero-width interval must be refused before the fetch")
        });

    assert!(matches!(refused, Err(CollectError::Refused(_))), "0m answered {refused:?}");
    assert!(
        hist.scan_quotes(VENUE, SYMBOL, TsRange::all()).unwrap().is_empty(),
        "a refused window stores no tick"
    );
}

// ── THE CHUNK GRID: a long window is fetched, stored and resampled one day at a time ─────────
//
// At `1m` a chunk is one UTC day, so these windows are cut at midnight. Their fake fetch honours
// its bounds, as `vike_dukascopy::fetch_quotes_range` does. The tests above can answer every call
// with the same five ticks only because each of their windows is ONE chunk: a multi-chunk window
// stores whatever each call returns, so a fake that ignored its bounds would store its ticks once
// per chunk.

/// A tape of three ticks every UTC day — 10 s and 70 s into the day (its first two minutes) and
/// 30 s before its end (its last minute) — cut to `[from, to]`: a fake fetch that honours its
/// bounds, for any days at all. Values are arbitrary — these tests count rows and read `ts`.
fn tape_between(from: i64, to: i64) -> Vec<QuoteTick> {
    (from.div_euclid(MS_PER_DAY)..=to.div_euclid(MS_PER_DAY))
        .flat_map(|day| [10_000, 70_000, MS_PER_DAY - 30_000].map(|at| day * MS_PER_DAY + at))
        .filter(|ts| (from..=to).contains(ts))
        .map(|ts| QuoteTick {
            ts,
            local_ts: 0,
            bid: 1.10001,
            ask: 1.10003,
            bid_size: 1.0,
            ask_size: 1.0,
            symbol: SYMBOL.to_string(),
        })
        .collect()
}

/// Whether the lane spent `key` on its quote series, asked the way
/// `append_is_idempotent_by_commit_key` pins: an append under a spent key writes nothing. Only ever
/// asserted TRUE — under an unspent key the one-tick probe would land.
fn quote_key_is_spent(hist: &DataFusionHist, key: &str) -> bool {
    hist.append_quotes(VENUE, SYMBOL, &tape_between(0, 10_000), Some(key)).unwrap() == 0
}

/// A window across THREE day-chunks — `[30 s, 2 days + 90 s]` — is fetched as three sub-windows,
/// cut at the two midnights inside it, each stored under its own quote key and resampled over its
/// own whole buckets; the bars written are the three chunks' bars summed. Only the window's own two
/// ends are trimmed — day 0 loses its partial first minute, and day 2 keeps only its first (its
/// second is cut at 90 s, its last lies past the window) — and no minute is lost at a midnight,
/// because the chunk edges are bar edges.
#[test]
fn a_window_across_three_day_chunks_is_fetched_stored_and_resampled_chunk_by_chunk() {
    let dir = tempfile::tempdir().unwrap();
    let hist = DataFusionHist::open(dir.path()).unwrap();
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
    let dir = tempfile::tempdir().unwrap();
    let hist = DataFusionHist::open(dir.path()).unwrap();
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
    let dir = tempfile::tempdir().unwrap();
    let hist = DataFusionHist::open(dir.path()).unwrap();
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
    let dir = tempfile::tempdir().unwrap();
    let hist = DataFusionHist::open(dir.path()).unwrap();
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

/// The bars of `interval` the store holds, ascending, each as its stored fields' bit patterns: the
/// full-fidelity form two runs are compared in.
fn stored_bars_at(hist: &DataFusionHist, interval: &str) -> Vec<(i64, [u64; 5])> {
    let bars = hist.load_bars(VENUE, SYMBOL, interval, TsRange::all()).unwrap();
    bars.iter()
        .map(|b| {
            let fields = [b.open, b.high, b.low, b.close, b.volume];
            (b.ts, fields.map(f64::to_bits))
        })
        .collect()
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
    let dir = tempfile::tempdir().unwrap();
    let hist = DataFusionHist::open(dir.path()).unwrap();
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

    let control_dir = tempfile::tempdir().unwrap();
    let control = DataFusionHist::open(control_dir.path()).unwrap();
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
    let dir = tempfile::tempdir().unwrap();
    let hist = DataFusionHist::open(dir.path()).unwrap();
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

    let control_dir = tempfile::tempdir().unwrap();
    let control = DataFusionHist::open(control_dir.path()).unwrap();
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
    let dir = tempfile::tempdir().unwrap();
    let hist = DataFusionHist::open(dir.path()).unwrap();
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
    let control_dir = tempfile::tempdir().unwrap();
    let control = DataFusionHist::open(control_dir.path()).unwrap();
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
    let dir = tempfile::tempdir().unwrap();
    let hist = DataFusionHist::open(dir.path()).unwrap();
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
    let dir = tempfile::tempdir().unwrap();
    let hist = DataFusionHist::open(dir.path()).unwrap();
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
    let dir = tempfile::tempdir().unwrap();
    let hist = DataFusionHist::open(dir.path()).unwrap();

    let failed =
        backfill_quotes_then_bars(&hist, SYMBOL, INTERVAL, 0, 119_999, &|| false, |_, _, _| {
            Err("the CDN went away".to_string())
        });

    let Err(CollectError::Fetch(error)) = &failed else {
        panic!("a fetch failure must answer as a Fetch: {failed:?}")
    };
    assert_eq!(error.as_str(), "the CDN went away");
}

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

/// The `interval` bar series' twin of [`quote_series_has_commit`]: asked of the manifest, so the
/// answer may be NO.
fn bar_series_has_commit(hist: &DataFusionHist, interval: &str, key: &str) -> bool {
    let id = SeriesId::per_symbol("bar", VENUE, SYMBOL, Some(interval.to_string()));
    hist.series_has_commit(&id, key).unwrap()
}

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

/// [`stored_bars_at`], narrowed to the bars whose `ts` lies inside `[from, to]`.
fn stored_bars_within(
    hist: &DataFusionHist,
    interval: &str,
    (from, to): (i64, i64),
) -> Vec<(i64, [u64; 5])> {
    stored_bars_at(hist, interval).into_iter().filter(|(ts, _)| (from..=to).contains(ts)).collect()
}

/// The `ts` of every stored quote inside `[from, to]`, ascending: a tick stored twice is there
/// twice.
fn stored_quote_ts_within(hist: &DataFusionHist, (from, to): (i64, i64)) -> Vec<i64> {
    let quotes = hist.scan_quotes(VENUE, SYMBOL, TsRange::all()).unwrap();
    quotes.iter().map(|q| q.ts).filter(|ts| (from..=to).contains(ts)).collect()
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
    let dir = tempfile::tempdir().unwrap();
    let hist = DataFusionHist::open(dir.path()).unwrap();
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
    let dir = tempfile::tempdir().unwrap();
    let hist = DataFusionHist::open(dir.path()).unwrap();
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
    let dir = tempfile::tempdir().unwrap();
    let hist = DataFusionHist::open(dir.path()).unwrap();
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
    let dir = tempfile::tempdir().unwrap();
    let hist = DataFusionHist::open(dir.path()).unwrap();
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
    let control_dir = tempfile::tempdir().unwrap();
    let control = DataFusionHist::open(control_dir.path()).unwrap();
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
    let dir = tempfile::tempdir().unwrap();
    let hist = DataFusionHist::open(dir.path()).unwrap();
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
    let dir = tempfile::tempdir().unwrap();
    let hist = DataFusionHist::open(dir.path()).unwrap();
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
    let dir = tempfile::tempdir().unwrap();
    let hist = DataFusionHist::open(dir.path()).unwrap();
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

// ── PROVISIONAL COMMITS: a one-chunk window fetched early, then again once it has settled ────
//
// Closes item 11 (docs/superpowers/specs/2026-09-29-provisional-commits-design.md): before this,
// a window whose whole span fit inside ONE chunk kept the SAME keys whether fetched while recent
// or once settled, so an early partial fetch could permanently spend the key a later, complete
// fetch needed. Settledness is judged against the real wall clock, so — like the RECENT CHUNKS
// tests above — these place themselves relative to today's UTC midnight. (A recent one-chunk
// window takes the very `StoreMode::Recent` path every recent chunk of a longer window does; these
// tests predate that merge, and pin the one-chunk case it started from.)

/// The core scenario item 11 describes: a window fetched once while still recent (partial data —
/// the feed hasn't finished publishing) and once more after settling recovers the COMPLETE data
/// with no inflated volume — where before this fix the settled pass would have silently written
/// nothing, because both passes shared the same key.
#[test]
fn a_settled_one_chunk_window_recovers_data_missed_by_an_earlier_recent_fetch() {
    let dir = tempfile::tempdir().unwrap();
    let hist = DataFusionHist::open(dir.path()).unwrap();
    let now = vike_model::now_ms();
    let today = now - now.rem_euclid(MS_PER_DAY);
    let yesterday = today - MS_PER_DAY;
    let full_tape = tape_between(yesterday, today - 1);

    // Fetched while still recent — only the early ticks have "arrived". This is a real call and
    // really exercises `StoreMode::Recent`, storing under the provisional key.
    let early = backfill_quotes_then_bars(
        &hist,
        SYMBOL,
        INTERVAL,
        yesterday,
        today - 1,
        &|| false,
        |_, from, to| Ok(tape_between(from, to.min(yesterday + 80_000))),
    )
    .unwrap();
    assert_eq!(
        early, 2,
        "the 10s and 70s ticks have arrived, landing in two different minute buckets — two bars"
    );

    // Simulate the window having settled: the exact canonical/provisional keys the settled
    // branch of `backfill_quotes_then_bars` itself would use for this same `(symbol, from, to)`,
    // now with the complete tape.
    let key = quote_commit_key(SYMBOL, yesterday, today - 1);
    let provisional = provisional_quote_commit_key(SYMBOL, yesterday, today - 1);
    let quote_rows = hist
        .append_quotes_superseding(VENUE, SYMBOL, &full_tape, Some(&key), Some(&provisional))
        .unwrap();
    assert_eq!(quote_rows, full_tape.len(), "the complete tape is written");

    let quotes = hist.scan_quotes(VENUE, SYMBOL, TsRange::all()).unwrap();
    assert_eq!(
        quotes.len(),
        full_tape.len(),
        "exactly the complete tape is stored — the provisional rows were REPLACED, not kept \
         alongside it (this is the assertion that would have failed before this fix: the settled \
         write would have found the shared key already spent and written nothing, leaving only \
         the early PARTIAL tape stored forever)"
    );
}

/// Two different recent requests over the same one-chunk window, with different raw bounds, each
/// mint their own provisional key and both legitimately store — the provisional key must
/// incorporate the request's own bounds, not collapse to one shared bucket per symbol. Both
/// windows here are short enough that `tape_between` gives them the SAME two ticks (10s, 70s;
/// the day-end-30s tick falls outside both), so the exact count that distinguishes "both fetches
/// really ran" from "the second was wrongly treated as already-done" is precisely DOUBLE one
/// fetch's own tick count, not merely "at least as many" — a weaker bound would pass even if the
/// second request silently wrote nothing.
#[test]
fn two_different_recent_one_chunk_requests_each_get_their_own_provisional_key() {
    let dir = tempfile::tempdir().unwrap();
    let hist = DataFusionHist::open(dir.path()).unwrap();
    let now = vike_model::now_ms();
    let today = now - now.rem_euclid(MS_PER_DAY);
    let yesterday = today - MS_PER_DAY;
    let one_fetch_ticks = tape_between(yesterday, yesterday + 100_000).len();

    backfill_quotes_then_bars(
        &hist,
        SYMBOL,
        INTERVAL,
        yesterday,
        yesterday + 100_000,
        &|| false,
        |_, from, to| Ok(tape_between(from, to)),
    )
    .unwrap();
    backfill_quotes_then_bars(
        &hist,
        SYMBOL,
        INTERVAL,
        yesterday,
        yesterday + 200_000,
        &|| false,
        |_, from, to| Ok(tape_between(from, to)),
    )
    .unwrap();

    let quotes = hist.scan_quotes(VENUE, SYMBOL, TsRange::all()).unwrap();
    assert_eq!(
        quotes.len(),
        2 * one_fetch_ticks,
        "both recent requests stored their own copy of the ticks under DIFFERENT provisional \
         keys — the accepted duplicate-for-overlapping-requests cost, proving neither was \
         silently skipped as 'already provisional'"
    );
}

/// The gap the test above (and its sibling above it) both leave: both simulate "settled" by
/// calling `append_quotes_superseding` directly with hand-built keys — neither ever drives
/// `backfill_quotes_then_bars` itself through the real `StoreMode::Settled` arm with a provisional
/// entry already sitting in the store. This test does exactly that, on BOTH the quote and the bar
/// side: it PLANTS what a prior recent-fetch pass would have left (a provisional quote commit and
/// a provisional resample, under the real provisional key formatters — precisely what
/// `StoreMode::Recent` itself would have written), then calls the REAL
/// `backfill_quotes_then_bars` over a genuinely SETTLED window (a 1970-epoch date) and checks that
/// the settled pass's own `StoreMode::Settled` arm supersedes both — not merely that a hand-driven
/// supersede call can.
///
/// Two mutations to production code survive the rest of this suite without this test: changing the
/// `Settled` arm's `supersede_key` argument from `Some(&provisional)` to `None` on either the quote
/// or the bar call in `store_then_resample`, and making `Recent`'s resample use the
/// canonical key instead of the provisional one. Both are kill-proofed against this test (see the
/// branch's fix report for the lane output).
#[test]
fn a_settled_window_supersedes_a_real_prior_provisional_entry_quotes_and_bars() {
    let dir = tempfile::tempdir().unwrap();
    let hist = DataFusionHist::open(dir.path()).unwrap();
    // Day 5, 1970-epoch — genuinely settled relative to real wall-clock "now", the same window
    // `a_settled_chunk_with_a_spent_key_is_never_asked_for` above uses.
    let (start, end) = (5 * MS_PER_DAY, 6 * MS_PER_DAY - 1);
    let full_tape = tape_between(start, end);
    let range = TsRange::of(start, end);

    // PLANT what a prior recent-fetch pass would have left: the early partial tape, stored under
    // the real provisional formatters.
    hist.append_quotes(
        VENUE,
        SYMBOL,
        &tape_between(start, start + 80_000),
        Some(&provisional_quote_commit_key(SYMBOL, start, end)),
    )
    .unwrap();
    hist.resample_quotes_to_bars(
        VENUE,
        SYMBOL,
        INTERVAL,
        range,
        Some(&provisional_resample_key(SYMBOL, INTERVAL, range)),
    )
    .unwrap();

    // The REAL settled pass — drives `StoreMode::Settled` end to end, with the provisional entry
    // above already sitting in the store.
    let bars =
        backfill_quotes_then_bars(&hist, SYMBOL, INTERVAL, start, end, &|| false, |_, from, to| {
            Ok(tape_between(from, to))
        })
        .unwrap();

    assert_eq!(bars, 3, "day 5's three minutes with a tick");

    let quotes = hist.scan_quotes(VENUE, SYMBOL, TsRange::all()).unwrap();
    assert_eq!(
        quotes.len(),
        full_tape.len(),
        "exactly the complete tape is stored — the provisional rows were REPLACED, not kept \
         alongside it (doubled would mean the quote-side supersede silently stopped superseding)"
    );

    let expect = consolidate_quotes(&full_tape, STEP_MS);
    let got = hist.load_bars(VENUE, SYMBOL, INTERVAL, TsRange::all()).unwrap();
    assert_eq!(
        got.len(),
        expect.len(),
        "not the provisional bars left sitting beside the settled ones"
    );
    for (e, g) in expect.iter().zip(got.iter()) {
        assert_eq!(e.ts, g.ts);
        assert_eq!(e.open.to_bits(), g.open.to_bits());
        assert_eq!(e.high.to_bits(), g.high.to_bits());
        assert_eq!(e.low.to_bits(), g.low.to_bits());
        assert_eq!(e.close.to_bits(), g.close.to_bits());
        assert_eq!(
            e.volume.to_bits(),
            g.volume.to_bits(),
            "the assertion that actually protects against inflated volume — the spec's whole \
             reason for existing"
        );
    }

    assert!(
        quote_key_is_spent(&hist, &quote_commit_key(SYMBOL, start, end)),
        "the canonical quote key is now spent"
    );
    assert_eq!(
        resample_and_store_bars(&hist, SYMBOL, INTERVAL, range).unwrap(),
        0,
        "the canonical resample key is spent too — re-running the resample writes nothing further"
    );
}

/// The BAR-side twin of `a_settled_one_chunk_window_recovers_data_missed_by_an_earlier_recent_fetch`
/// above, which only ever checks quotes — and a test that can catch a DIFFERENT, real mutation
/// (the RECENT CHUNKS tests above catch it too): `StoreMode::Recent`'s resample using the
/// CANONICAL key instead of the provisional one. If it did, a real early fetch would spend the
/// canonical resample key on its OWN partial bars — a recent chunk's resample always covers the
/// same range the settled
/// pass will later use — so the later settled pass's `resample_quotes_to_bars_superseding` would
/// find that canonical key already spent and return `Ok(0)` BEFORE ever attempting the supersede,
/// leaving the early, partial bars stuck under the canonical key forever with no error.
///
/// Like the sibling test above, this cannot use two real calls to `backfill_quotes_then_bars` to go
/// from "recent" to "settled" — settledness is judged against the real wall clock, which a test
/// cannot fake forward — so it fetches the recent half for real (genuinely exercising
/// `StoreMode::Recent`'s resample, not just its quote ingest) and then simulates "now it has
/// settled" on BOTH sides with the exact canonical/provisional keys `backfill_quotes_then_bars`
/// itself would use.
#[test]
fn a_recent_one_chunk_fetch_leaves_the_canonical_resample_key_unspent_too() {
    let dir = tempfile::tempdir().unwrap();
    let hist = DataFusionHist::open(dir.path()).unwrap();
    let now = vike_model::now_ms();
    let today = now - now.rem_euclid(MS_PER_DAY);
    let yesterday = today - MS_PER_DAY;
    let full_tape = tape_between(yesterday, today - 1);
    let range = TsRange::of(yesterday, today - 1);

    // A REAL early fetch — genuinely exercises `StoreMode::Recent`'s resample, storing
    // under whatever key that arm actually uses (the provisional one, correctly).
    let early = backfill_quotes_then_bars(
        &hist,
        SYMBOL,
        INTERVAL,
        yesterday,
        today - 1,
        &|| false,
        |_, from, to| Ok(tape_between(from, to.min(yesterday + 80_000))),
    )
    .unwrap();
    assert_eq!(early, 2, "the 10s and 70s ticks have arrived — two bars from the partial tape");

    // Simulate the window having settled, on BOTH sides, with the exact keys
    // `backfill_quotes_then_bars`'s own `Settled` arm would use for this same window.
    hist.append_quotes_superseding(
        VENUE,
        SYMBOL,
        &full_tape,
        Some(&quote_commit_key(SYMBOL, yesterday, today - 1)),
        Some(&provisional_quote_commit_key(SYMBOL, yesterday, today - 1)),
    )
    .unwrap();
    hist.resample_quotes_to_bars_superseding(
        VENUE,
        SYMBOL,
        INTERVAL,
        range,
        Some(&resample_commit_key(SYMBOL, INTERVAL, range)),
        Some(&provisional_resample_key(SYMBOL, INTERVAL, range)),
    )
    .unwrap();

    let expect = consolidate_quotes(&full_tape, STEP_MS);
    let got = hist.load_bars(VENUE, SYMBOL, INTERVAL, TsRange::all()).unwrap();
    assert_eq!(
        got.len(),
        expect.len(),
        "exactly the complete tape's bars — not the early PARTIAL bars stuck forever under a \
         canonical key the real early fetch should never have spent (this is the assertion that \
         would fail had Recent's resample used the canonical key instead of the \
         provisional one: the simulated settled pass above would then have found the canonical \
         key already spent and returned before ever attempting the supersede)"
    );
    for (e, g) in expect.iter().zip(got.iter()) {
        assert_eq!(e.ts, g.ts);
        assert_eq!(e.volume.to_bits(), g.volume.to_bits());
    }
}

/// The race the store's PRE-SPEND closes, driven through the real lane: a recent request decides
/// "recent" from its one clock read and then spends its fetch time, while a second request, started
/// after the window crossed the margin, decides "settled" — and finishes first. The settled pass is
/// simulated on BOTH sides with the exact keys its `Settled` arm uses (wall-clock settledness
/// cannot be faked forward, the same reason the tests above simulate it), with no provisional entry
/// in the store, so there is nothing to supersede: the settled commits still spend the provisional
/// twins. Then the REAL recent `backfill_quotes_then_bars` over the same window lands late with a
/// short tape, and must write nothing. Before the pre-spend its ticks and bars were sealed beside
/// the settled ones, and no later settled pass could ever remove them: its canonical keys were
/// already spent, so the skip-before-fetch stopped it before the supersede. (Since #9 the late
/// request meets the pre-spent provisional key at the RECENT skip, before it even fetches; one
/// whose skip ran before the settled commit landed meets it at the store's own write instead, under
/// the series lock — `crates/vike-data/tests/store/superseding_commits.rs`'s
/// `a_provisional_commit_after_its_canonical_twin_writes_nothing` pins that half.)
#[test]
fn a_recent_fetch_after_its_window_was_settled_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let hist = DataFusionHist::open(dir.path()).unwrap();
    let now = vike_model::now_ms();
    let today = now - now.rem_euclid(MS_PER_DAY);
    let yesterday = today - MS_PER_DAY;
    let full_tape = tape_between(yesterday, today - 1);
    let range = TsRange::of(yesterday, today - 1);

    // The settled pass, first: no provisional entry exists, so each side supersedes nothing.
    hist.append_quotes_superseding(
        VENUE,
        SYMBOL,
        &full_tape,
        Some(&quote_commit_key(SYMBOL, yesterday, today - 1)),
        Some(&provisional_quote_commit_key(SYMBOL, yesterday, today - 1)),
    )
    .unwrap();
    hist.resample_quotes_to_bars_superseding(
        VENUE,
        SYMBOL,
        INTERVAL,
        range,
        Some(&resample_commit_key(SYMBOL, INTERVAL, range)),
        Some(&provisional_resample_key(SYMBOL, INTERVAL, range)),
    )
    .unwrap();

    // The recent request, landing late with what it fetched before the day was published.
    let late = backfill_quotes_then_bars(
        &hist,
        SYMBOL,
        INTERVAL,
        yesterday,
        today - 1,
        &|| false,
        |_, from, to| Ok(tape_between(from, to.min(yesterday + 80_000))),
    )
    .unwrap();
    assert_eq!(late, 0, "the settled commits pre-spent both provisional keys: nothing written");

    let quotes = hist.scan_quotes(VENUE, SYMBOL, TsRange::all()).unwrap();
    assert_eq!(quotes.len(), full_tape.len(), "exactly the full tape, not the short one beside it");
    let expect = consolidate_quotes(&full_tape, STEP_MS);
    let got = hist.load_bars(VENUE, SYMBOL, INTERVAL, TsRange::all()).unwrap();
    assert_eq!(got.len(), expect.len(), "the settled bars alone, not the recent ones beside them");
    for (e, g) in expect.iter().zip(got.iter()) {
        assert_eq!(e.ts, g.ts);
        assert_eq!(e.volume.to_bits(), g.volume.to_bits(), "volume inflated by a late duplicate");
    }
}

// ── A REFUSED SUPERSEDE: skipped with its own chunk, never with the rest of the window ────────
//
// Background compaction can merge a provisional part into a neighbour's key set before its window
// settles, and the store then REFUSES the settled commit that would supersede it (a supersede it
// cannot perform exactly would double `volume`). The refusal is permanent for that window and says
// nothing about any other, so `backfill_quotes_then_bars` skips the chunk and goes on — and ends in
// an error naming every chunk it skipped, never in `Ok`. These tests build the REAL refusal: the
// planted state is the very construction `crates/vike-data/tests/store/supersede_refusal.rs` refuses on,
// so what the lane meets here is the store's own answer and not a stand-in for it. Every window is
// a run of settled 1970 days except the recent-chunks test, which places itself against today's
// UTC midnight like the RECENT CHUNKS tests above.

/// Plant what compaction leaves once it has folded a chunk's PROVISIONAL quote entry into a
/// neighbour: that key sharing ONE part with an unrelated commit's — so the store finds it, cannot
/// remove it exactly, and refuses. Both writes land on the chunk's own UTC day, which is what lets
/// compaction merge them. ⚠ The neighbour's key carries the chunk's start: a commit key is spent
/// once per series, so a fixed one would make the SECOND chunk planted in a store write no neighbour
/// at all — one part on its day, nothing to fold, and the plant would say so.
fn fold_provisional_quotes_into_a_neighbour(hist: &DataFusionHist, (from, to): (i64, i64)) {
    let neighbour = format!("a-neighbour-of-{from}");
    hist.append_quotes(VENUE, SYMBOL, &tape_between(from, from + 20_000), Some(&neighbour))
        .unwrap();
    hist.append_quotes(
        VENUE,
        SYMBOL,
        &tape_between(from + 60_000, from + 80_000),
        Some(&provisional_quote_commit_key(SYMBOL, from, to)),
    )
    .unwrap();
    let merged = hist
        .compact_series(
            "quote",
            VENUE,
            SYMBOL,
            None,
            &CompactionConfig { min_parts: 2, ..Default::default() },
        )
        .unwrap();
    assert_eq!(merged.parts_merged, 2, "the two same-day parts must fold together: {merged:?}");
}

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
            "bar",
            VENUE,
            SYMBOL,
            Some(INTERVAL),
            &CompactionConfig { min_parts: 2, ..Default::default() },
        )
        .unwrap();
    assert_eq!(merged.parts_merged, 2, "the two same-day parts must fold together: {merged:?}");
}

/// Whether `key` is spent on the QUOTE series — asked of the manifest, which is the one way to ask
/// when the answer may be NO: [`quote_key_is_spent`] probes with an append, and under an unspent key
/// that probe would land.
fn quote_series_has_commit(hist: &DataFusionHist, key: &str) -> bool {
    let id = SeriesId::per_symbol("quote", VENUE, SYMBOL, None);
    hist.series_has_commit(&id, key).unwrap()
}

/// The middle chunk of three is refused, and the request still fetches, stores and resamples the
/// chunks either side of it — where it used to return the refusal at once, so day 7 was never even
/// attempted. It ends in an ERROR that names the chunk, because `Ok` is what the `Backfill` reply
/// reports as "this window is in the store" and day 6 is not. The refused chunk writes NOTHING
/// (the quote side refused, so no ticks and no bars), and the error's `Display` keeps the store's
/// `hist store: ` prefix, which is what text-only classifiers of a stringified error read.
#[test]
fn a_refused_supersede_skips_its_own_chunk_and_the_rest_of_the_window_is_still_written() {
    let dir = tempfile::tempdir().unwrap();
    let hist = DataFusionHist::open(dir.path()).unwrap();
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
    let dir = tempfile::tempdir().unwrap();
    let hist = DataFusionHist::open(dir.path()).unwrap();
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
    let dir = tempfile::tempdir().unwrap();
    let hist = DataFusionHist::open(dir.path()).unwrap();
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
    let dir = tempfile::tempdir().unwrap();
    let hist = DataFusionHist::open(dir.path()).unwrap();
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
    let dir = tempfile::tempdir().unwrap();
    let hist = DataFusionHist::open(dir.path()).unwrap();
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
    let dir = tempfile::tempdir().unwrap();
    let hist = DataFusionHist::open(dir.path()).unwrap();
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
    let dir = tempfile::tempdir().unwrap();
    let hist = DataFusionHist::open(dir.path()).unwrap();
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

// ── THE STOP PROBE: `should_stop`, asked at the top of every chunk ───────────────────────────────
//
// `docs/superpowers/specs/2026-10-01-backfill-cancel-on-client-drop-design.md` §1 and §6 (T2, T3):
// the lane asks its probe before each chunk's key read and fetch, stops at that boundary when it
// answers `true`, keeps every chunk before it, and answers `Stopped` — never `Ok`, and never the
// `SupersedeRefused` that says a request RAN to its end. Every test above passes a probe that never
// fires and asserts what it always did, which is T3.

use std::cell::{Cell, RefCell};

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
    let dir = tempfile::tempdir().unwrap();
    let hist = DataFusionHist::open(dir.path()).unwrap();
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
    let control_dir = tempfile::tempdir().unwrap();
    let control = DataFusionHist::open(control_dir.path()).unwrap();
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
    let dir = tempfile::tempdir().unwrap();
    let hist = DataFusionHist::open(dir.path()).unwrap();
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
        let dir = tempfile::tempdir().unwrap();
        let hist = DataFusionHist::open(dir.path()).unwrap();
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

        assert!(matches!(stopped, Err(CollectError::Stopped(_))), "k = {k}: {stopped:?}");
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
    let dir = tempfile::tempdir().unwrap();
    let hist = DataFusionHist::open(dir.path()).unwrap();
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
    let dir = tempfile::tempdir().unwrap();
    let hist = DataFusionHist::open(dir.path()).unwrap();

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
