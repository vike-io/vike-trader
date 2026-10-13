//! Bit-exact append/scan, resample parity, commit-key idempotency, and THE TICK LANE.

use super::*;

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
    let (_dir, hist) = open();
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
    let (_dir, hist) = open();
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
    let (_dir, hist) = open();
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
    let (_dir, hist) = open();
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
    let (_dir, hist) = open();

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
    let (_dir, hist) = open();

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

/// `0m` parses — `vike_marketdata::interval::interval_ms` reads it as `Some(0)` — but a zero-width bucket is
/// no bar, and `consolidate_quotes` would divide by it. So the lane REFUSES it, before the fetch: the
/// closure panics if it is ever called, and nothing is stored.
#[test]
fn a_zero_width_interval_is_refused_before_anything_is_fetched() {
    let (_dir, hist) = open();

    let refused =
        backfill_quotes_then_bars(&hist, SYMBOL, "0m", 0, 119_999, &|| false, |_, _, _| {
            panic!("a zero-width interval must be refused before the fetch")
        });

    assert_matches!(refused, Err(CollectError::Refused(_)), "0m answered {refused:?}");
    assert!(
        hist.scan_quotes(VENUE, SYMBOL, TsRange::all()).unwrap().is_empty(),
        "a refused window stores no tick"
    );
}
