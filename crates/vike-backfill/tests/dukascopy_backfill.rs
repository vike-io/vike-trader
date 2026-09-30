//! Offline gate for the Dukascopy → hist-store ingest + resample (docs/decisions/0094):
//! build a synthetic `Vec<QuoteTick>` directly, ingest into a `tempdir` `DataFusionHist`, and assert
//! bit-exact round-trips (`to_bits`). Then resample and assert the stored bars match
//! `vike_model::consolidate_quotes` bit-exactly, plus commit-key idempotency — and drive the whole
//! `TickBars` lane, `backfill_quotes_then_bars`, over aligned and unaligned windows, and over
//! windows long enough to be cut into day-sized chunks. The fetch half (and its own `#[ignore]`d
//! live test) moved to `crates/bridges/dukascopy/tests/quotes.rs`; this file is deterministic +
//! offline, and — since it no longer names the bridge — ungated.

use vike_backfill::CollectError;
use vike_backfill::venues::dukascopy::{
    VENUE, backfill_quotes_then_bars, ingest_quotes, provisional_quote_commit_key,
    provisional_resample_key, quote_commit_key, resample_and_store_bars, resample_commit_key,
};
use vike_data::{DataFusionHist, HistStore, TsRange};
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

    let bars =
        backfill_quotes_then_bars(&hist, SYMBOL, INTERVAL, 30_000, 119_999, |sym, from, to| {
            asked = Some((sym.to_string(), from, to));
            Ok(synthetic_quotes())
        })
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

    let bars = backfill_quotes_then_bars(&hist, SYMBOL, INTERVAL, 0, 90_000, |_, _, _| {
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

    let bars = backfill_quotes_then_bars(&hist, SYMBOL, INTERVAL, 0, 119_999, |_, _, _| {
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

    let refused = backfill_quotes_then_bars(&hist, SYMBOL, "0m", 0, 119_999, |_, _, _| {
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

    let bars = backfill_quotes_then_bars(&hist, SYMBOL, INTERVAL, start, end, |sym, from, to| {
        calls.push((sym.to_string(), from, to));
        Ok(tape_between(from, to))
    })
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

    let bars = backfill_quotes_then_bars(&hist, SYMBOL, INTERVAL, start, end, |sym, from, to| {
        calls.push((sym.to_string(), from, to));
        Ok(tape_between(from, to))
    })
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

    let first = backfill_quotes_then_bars(&hist, SYMBOL, INTERVAL, start, end, |_, from, to| {
        Ok(tape_between(from, to))
    })
    .unwrap();
    assert_eq!(first, 3, "day 5 alone: its three minutes with a tick");

    let second = backfill_quotes_then_bars(&hist, SYMBOL, INTERVAL, start, end, |_, _, _| {
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
    let one_minute = backfill_quotes_then_bars(&hist, SYMBOL, "1m", start, end, |_, from, to| {
        Ok(tape_between(from, to))
    })
    .unwrap();
    assert_eq!(one_minute, 9, "three days of three minutes with a tick");
    let one_minute_bars = stored_bars_at(&hist, "1m");

    let five_minute = backfill_quotes_then_bars(&hist, SYMBOL, "5m", start, end, |_, _, _| {
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
    backfill_quotes_then_bars(&control, SYMBOL, "5m", start, end, |_, from, to| {
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
        let repeat = backfill_quotes_then_bars(&hist, SYMBOL, interval, start, end, |_, _, _| {
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
    backfill_quotes_then_bars(&hist, SYMBOL, "1m", start, end, |_, from, to| {
        Ok(tape_between(from, to))
    })
    .unwrap();

    let bars = backfill_quotes_then_bars(&hist, SYMBOL, "5m", start, end, |_, _, _| {
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
    backfill_quotes_then_bars(&control, SYMBOL, "5m", start, end, |_, from, to| {
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

    let bars = backfill_quotes_then_bars(&hist, SYMBOL, INTERVAL, from, to, |_, _, _| {
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

    let failed = backfill_quotes_then_bars(&hist, SYMBOL, INTERVAL, start, end, |_, from, to| {
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
    let retried = backfill_quotes_then_bars(&hist, SYMBOL, INTERVAL, start, end, |_, from, to| {
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

    let failed = backfill_quotes_then_bars(&hist, SYMBOL, INTERVAL, 0, 119_999, |_, _, _| {
        Err("the CDN went away".to_string())
    });

    let Err(CollectError::Fetch(error)) = &failed else {
        panic!("a fetch failure must answer as a Fetch: {failed:?}")
    };
    assert_eq!(error.as_str(), "the CDN went away");
}

// ── THE RECENT TAIL: the chunks the feed may not have finished publishing ────────────────────
//
// Every window above lies in 1970, so each of its chunks is SETTLED and takes the shared keys its
// bounds give it. A chunk whose last millisecond is inside the publication margin is not — the lane
// stores the recent tail as one batch under the REQUEST's own bounds — and "recent" is measured
// against the wall clock, which the lane reads itself, once per request, as
// `crates/vike-backfill/src/klines.rs`'s `ingest_klines` does. So this test's days are placed
// relative to today's UTC midnight, where every chunk's side of the margin holds for the whole of
// today's UTC date: the one way this test can go wrong is for the next UTC midnight to fall between
// its own clock read and the lane's, which would move the day before yesterday out of the margin.

/// Four whole days ending with yesterday. The two oldest END more than two days ago, so they are
/// settled and take their shared day keys; the two newest do not, so they are fetched one at a
/// time like any other chunk, then stored as ONE batch and resampled once, both keyed by the
/// request's own raw bounds.
///
/// The difference from a shared day key is what a LATER request sees. Yesterday alone, asked for
/// again, finds its keys unspent and is written again — where a shared key would have answered 0
/// and kept whatever the first fetch saw, for good — while a settled day, asked for again, is
/// still the no-op the shared keys exist for.
#[test]
fn the_chunks_inside_the_publication_margin_are_keyed_by_the_request_not_the_day() {
    let dir = tempfile::tempdir().unwrap();
    let hist = DataFusionHist::open(dir.path()).unwrap();
    let now = vike_model::now_ms();
    let today = now - now.rem_euclid(MS_PER_DAY);
    // The first millisecond of the UTC day `n` days before today's.
    let day = |n: i64| today - n * MS_PER_DAY;
    let (start, end) = (day(4), day(0) - 1);
    let mut calls = Vec::new();

    let bars = backfill_quotes_then_bars(&hist, SYMBOL, INTERVAL, start, end, |_, from, to| {
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
    // ...and the recent two were stored, and resampled, under the request's own raw bounds.
    assert!(quote_key_is_spent(&hist, &quote_commit_key(SYMBOL, start, end)));
    assert_eq!(
        resample_and_store_bars(&hist, SYMBOL, INTERVAL, TsRange::of(start, end)).unwrap(),
        0,
        "the recent tail's resample spent the request's key"
    );

    let yesterday =
        backfill_quotes_then_bars(&hist, SYMBOL, INTERVAL, day(1), day(0) - 1, |_, from, to| {
            Ok(tape_between(from, to))
        })
        .unwrap();
    assert_eq!(yesterday, 3, "yesterday is written again: no shared key was spent on it");

    let settled_day =
        backfill_quotes_then_bars(&hist, SYMBOL, INTERVAL, day(4), day(3) - 1, |_, from, to| {
            Ok(tape_between(from, to))
        })
        .unwrap();
    assert_eq!(settled_day, 0, "a settled day's shared keys were spent by the first request");
}

/// One day named the only way date labels can name it — `[D, D + 1 day]`, since `--from D --to D`
/// is refused — is TWO chunks: day D and the next day's first millisecond, which holds no bar. So
/// the window's whole bars are exactly day D's own, and a recent tail that keyed its resample by
/// them would spend the key every later request covering D resamples under: that request would
/// store the finished day under D's fresh quote key and still never write its bars. Keyed by the
/// request's raw bounds, it cannot.
///
/// Played out with yesterday, fetched before its last minute is published: the lane writes the
/// two bars it could, and when the finished day is later stored under yesterday's own quote key,
/// yesterday's own resample key is still unspent, and all three bars are written.
#[test]
fn a_recent_one_day_window_leaves_the_days_own_resample_key_unspent() {
    let dir = tempfile::tempdir().unwrap();
    let hist = DataFusionHist::open(dir.path()).unwrap();
    let now = vike_model::now_ms();
    let today = now - now.rem_euclid(MS_PER_DAY);
    let yesterday = today - MS_PER_DAY;
    let mut calls = Vec::new();

    // Yesterday's last minute is not published yet, so the fetch answers without it.
    let bars =
        backfill_quotes_then_bars(&hist, SYMBOL, INTERVAL, yesterday, today, |_, from, to| {
            calls.push((from, to));
            Ok(tape_between(from, to.min(today - 60_001)))
        })
        .unwrap();

    assert_eq!(calls, vec![(yesterday, today - 1), (today, today)], "a day, then one millisecond");
    assert_eq!(bars, 2, "yesterday's first two minutes; its last was not published yet");
    let finished_day = tape_between(yesterday, today - 1);
    assert_eq!(ingest_quotes(&hist, SYMBOL, yesterday, today - 1, &finished_day).unwrap(), 3);
    assert_eq!(
        resample_and_store_bars(&hist, SYMBOL, INTERVAL, TsRange::of(yesterday, today - 1))
            .unwrap(),
        3,
        "yesterday's own resample key was not spent by the early fetch"
    );
}

// ── PROVISIONAL COMMITS: a one-chunk window fetched early, then again once it has settled ────
//
// Closes item 11 (docs/superpowers/specs/2026-09-29-provisional-commits-design.md): before this,
// a window whose whole span fit inside ONE chunk kept the SAME keys whether fetched while recent
// or once settled, so an early partial fetch could permanently spend the key a later, complete
// fetch needed. Settledness is judged against the real wall clock, so — like the RECENT TAIL
// tests above — these place themselves relative to today's UTC midnight.

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
    // really exercises `StoreMode::RecentOneChunk`, storing under the provisional key.
    let early =
        backfill_quotes_then_bars(&hist, SYMBOL, INTERVAL, yesterday, today - 1, |_, from, to| {
            Ok(tape_between(from, to.min(yesterday + 80_000)))
        })
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
        |_, from, to| Ok(tape_between(from, to)),
    )
    .unwrap();
    backfill_quotes_then_bars(
        &hist,
        SYMBOL,
        INTERVAL,
        yesterday,
        yesterday + 200_000,
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
/// `StoreMode::RecentOneChunk` itself would have written), then calls the REAL
/// `backfill_quotes_then_bars` over a genuinely SETTLED window (a 1970-epoch date) and checks that
/// the settled pass's own `StoreMode::Settled` arm supersedes both — not merely that a hand-driven
/// supersede call can.
///
/// Two mutations to production code survive the rest of this suite without this test: changing the
/// `Settled` arm's `supersede_key` argument from `Some(&provisional)` to `None` on either the quote
/// or the bar call in `store_then_resample`, and making `RecentOneChunk`'s resample use the
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
    let bars = backfill_quotes_then_bars(&hist, SYMBOL, INTERVAL, start, end, |_, from, to| {
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
/// above, which only ever checks quotes — and the one test in this file that can catch a DIFFERENT,
/// real mutation: `StoreMode::RecentOneChunk`'s resample using the CANONICAL key instead of the
/// provisional one. If it did, a real early fetch would spend the canonical resample key on its OWN
/// partial bars — a one-chunk window's recent resample always covers the same range the settled
/// pass will later use — so the later settled pass's `resample_quotes_to_bars_superseding` would
/// find that canonical key already spent and return `Ok(0)` BEFORE ever attempting the supersede,
/// leaving the early, partial bars stuck under the canonical key forever with no error.
///
/// Like the sibling test above, this cannot use two real calls to `backfill_quotes_then_bars` to go
/// from "recent" to "settled" — settledness is judged against the real wall clock, which a test
/// cannot fake forward — so it fetches the recent half for real (genuinely exercising
/// `StoreMode::RecentOneChunk`'s resample, not just its quote ingest) and then simulates "now it has
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

    // A REAL early fetch — genuinely exercises `StoreMode::RecentOneChunk`'s resample, storing
    // under whatever key that arm actually uses (the provisional one, correctly).
    let early =
        backfill_quotes_then_bars(&hist, SYMBOL, INTERVAL, yesterday, today - 1, |_, from, to| {
            Ok(tape_between(from, to.min(yesterday + 80_000)))
        })
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
         would fail had RecentOneChunk's resample used the canonical key instead of the \
         provisional one: the simulated settled pass above would then have found the canonical \
         key already spent and returned before ever attempting the supersede)"
    );
    for (e, g) in expect.iter().zip(got.iter()) {
        assert_eq!(e.ts, g.ts);
        assert_eq!(e.volume.to_bits(), g.volume.to_bits());
    }
}
