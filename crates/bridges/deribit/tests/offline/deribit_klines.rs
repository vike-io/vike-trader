//! The pure half of [`vike_deribit::data`] — no network, no credentials.
//!
//! Two contracts are gated here, and the second is the load-bearing one:
//!
//! 1. **The COLUMNAR parse, BY KEY.** `public/get_tradingview_chart_data` answers with parallel
//!    columns whose wire order is `volume, ticks, status, open, LOW, HIGH, cost, close` — not
//!    `ohlc`. A positional read would swap high/low and book quote notional (`cost`) as base
//!    `volume`. The captured fixture (`fixtures/deribit_chart_btc_perpetual_1m.json`, a real
//!    `BTC-PERPETUAL` `resolution=1` response) pins the mapping, and a scrambled all-distinct
//!    synthetic pins it beyond the fixture's numeric coincidences. `status:"no_data"` is a normal
//!    EMPTY result; ragged columns are an `Err`, never a zip-truncate (which would fabricate a gap
//!    in the tape).
//!
//! 2. **The BACKWARD page walk.** The endpoint is END-ANCHORED: given a window wider than its cap
//!    it silently drops the OLDER tail and still answers `status:"ok"`. A test that exercises one
//!    page cannot see that — so the walk is driven here over a SYNTHETIC end-anchored venue with a
//!    deliberately tiny cap, proving a multi-page window comes back WHOLE and that
//!    `end_timestamp` moves DOWN.

use vike_deribit::data::{
    PageStep, VENUE, next_page_step, parse_deribit_klines, walk_backward_pages,
};
use vike_model::Bar;

/// The captured response the module's parser was written against (real venue, 2026-08-04).
const FIXTURE: &str = include_str!("../fixtures/deribit_chart_btc_perpetual_1m.json");

/// A real `no_data` body — captured from a year-2001 window on the live venue.
const NO_DATA: &str = r#"{"usOut":1785845984945458,"usIn":1785845984940946,"usDiff":4512,
    "testnet":false,"result":{"volume":[],"ticks":[],"status":"no_data","open":[],"low":[],
    "high":[],"cost":[],"close":[]},"jsonrpc":"2.0"}"#;

/// A real JSON-RPC error envelope — captured by asking for an instrument that does not exist.
const ERROR_ENVELOPE: &str = r#"{"usOut":1785845985138575,"usIn":1785845985138551,"usDiff":24,
    "testnet":false,"jsonrpc":"2.0","error":{"message":"Invalid params",
    "data":{"reason":"instrument not found","param":"instrument_name"},"code":-32602}}"#;

#[test]
fn venue_id_is_pinned() {
    assert_eq!(VENUE, "deribit");
}

// ---- 1. the columnar parse ---------------------------------------------------------------------

#[test]
fn fixture_parses_three_bars_mapped_by_key() {
    let bars = parse_deribit_klines(FIXTURE).expect("the captured fixture parses");
    assert_eq!(bars.len(), 3, "three candles in the capture");

    // ticks → Bar.ts, in wire (ascending) order — Deribit serves oldest-first, unlike okx.
    assert_eq!(bars[0].ts, 1_785_840_000_000);
    assert_eq!(bars[1].ts, 1_785_840_060_000);
    assert_eq!(bars[2].ts, 1_785_840_120_000);
    assert!(bars.windows(2).all(|w| w[0].ts < w[1].ts), "ascending by ts");

    // Row 0, read BY KEY. `high` (63480.5) and `low` (63464.5) DIFFER, so the wire's low-before-high
    // ordering cannot be honoured positionally without this failing.
    assert_eq!(bars[0].open.to_bits(), 63464.5f64.to_bits());
    assert_eq!(bars[0].high.to_bits(), 63480.5f64.to_bits());
    assert_eq!(bars[0].low.to_bits(), 63464.5f64.to_bits());
    assert_eq!(bars[0].close.to_bits(), 63480.5f64.to_bits());
    assert!(bars[0].high >= bars[0].low, "high/low must not be swapped");

    // `volume` is BASE units. `cost` (6250.0) is the QUOTE notional and must never land here — the
    // single most plausible column mix-up on this venue.
    assert_eq!(bars[0].volume.to_bits(), 0.09846626f64.to_bits());
    assert_ne!(bars[0].volume.to_bits(), 6250.0f64.to_bits(), "cost is not volume");
    assert_eq!(bars[1].volume.to_bits(), 0.03386421f64.to_bits());
    assert_eq!(bars[2].volume.to_bits(), 0.00173194f64.to_bits());

    // The last row, so a length-off-by-one or a reversed column cannot hide at the tail.
    assert_eq!(bars[2].open.to_bits(), 63507.0f64.to_bits());
    assert_eq!(bars[2].high.to_bits(), 63513.0f64.to_bits());
    assert_eq!(bars[2].low.to_bits(), 63507.0f64.to_bits());
    assert_eq!(bars[2].close.to_bits(), 63513.0f64.to_bits());

    // The series key carries venue/symbol/interval, so these stay unset (every kline venue's shape).
    for b in &bars {
        assert!(b.symbol.is_none() && b.funding.is_none() && b.bid.is_none() && b.ask.is_none());
    }
}

/// The captured rows happen to have `open == low` and `high == close` in all three candles, so the
/// fixture alone cannot catch an open↔low swap. This synthetic gives every column a DISTINCT value
/// and scrambles the key order — the mapping is then pinned with no coincidence left to hide behind.
#[test]
fn every_column_is_read_by_key_not_by_position() {
    const SCRAMBLED: &str = r#"{"jsonrpc":"2.0","result":{
        "cost":[7777.0],"close":[2.0],"volume":[5.0],"low":[0.5],"status":"ok",
        "high":[4.0],"ticks":[1700000000000],"open":[1.0]}}"#;
    let bars = parse_deribit_klines(SCRAMBLED).expect("parses");
    assert_eq!(bars.len(), 1);
    assert_eq!(bars[0].ts, 1_700_000_000_000);
    assert_eq!(bars[0].open.to_bits(), 1.0f64.to_bits());
    assert_eq!(bars[0].high.to_bits(), 4.0f64.to_bits());
    assert_eq!(bars[0].low.to_bits(), 0.5f64.to_bits());
    assert_eq!(bars[0].close.to_bits(), 2.0f64.to_bits());
    assert_eq!(bars[0].volume.to_bits(), 5.0f64.to_bits());
}

// ---- 2. no_data is a normal empty result -------------------------------------------------------

#[test]
fn no_data_is_an_empty_ok_not_an_error() {
    let bars = parse_deribit_klines(NO_DATA).expect("no_data is a NORMAL answer, not an error");
    assert!(bars.is_empty());
}

// ---- 3. ragged columns are an error ------------------------------------------------------------

#[test]
fn ragged_columns_error_instead_of_zip_truncating() {
    // `close` one element SHORT of `ticks`: truncating to the shortest column would silently drop
    // the last candle and fabricate a gap that no later scan could tell from real market silence.
    const SHORT_CLOSE: &str = r#"{"result":{"status":"ok",
        "ticks":[1,2,3],"open":[1.0,2.0,3.0],"high":[1.0,2.0,3.0],"low":[1.0,2.0,3.0],
        "close":[1.0,2.0],"volume":[1.0,2.0,3.0]}}"#;
    let err = parse_deribit_klines(SHORT_CLOSE).expect_err("a short column is malformed");
    assert!(err.contains("ragged"), "{err}");

    // And the other direction — a column LONGER than `ticks` is equally malformed.
    const LONG_VOLUME: &str = r#"{"result":{"status":"ok",
        "ticks":[1,2],"open":[1.0,2.0],"high":[1.0,2.0],"low":[1.0,2.0],
        "close":[1.0,2.0],"volume":[1.0,2.0,3.0]}}"#;
    assert!(parse_deribit_klines(LONG_VOLUME).unwrap_err().contains("ragged"));
}

#[test]
fn malformed_and_error_bodies_are_errors() {
    // the venue's own JSON-RPC error envelope (no `result` at all)
    let err = parse_deribit_klines(ERROR_ENVELOPE).expect_err("an error envelope is an Err");
    assert!(err.contains("-32602") && err.contains("instrument not found"), "{err}");
    // a missing column
    assert!(
        parse_deribit_klines(
            r#"{"result":{"status":"ok","ticks":[1],"open":[1.0],"high":[1.0],"low":[1.0]}}"#
        )
        .is_err()
    );
    // an unknown/missing status fails CLOSED rather than parsing whatever columns happen to exist
    assert!(parse_deribit_klines(r#"{"result":{"ticks":[],"open":[]}}"#).is_err());
    assert!(parse_deribit_klines(r#"{"result":{"status":"weird","ticks":[]}}"#).is_err());
    // not JSON at all
    assert!(parse_deribit_klines("<html>503</html>").is_err());
}

// ---- 4. the BACKWARD page walk (the load-bearing test) -----------------------------------------

const T0: i64 = 1_700_000_000_000;
const MIN: i64 = 60_000;
/// The SYNTHETIC venue's page cap — tiny on purpose. The real one is ~5001 rows and is deliberately
/// hardcoded NOWHERE in the implementation: termination is driven by observed ticks.
const CAP: usize = 3;
/// The synthetic universe: seven contiguous 1-minute bars starting at [`T0`].
const BARS: i64 = 7;

fn synthetic_bar(ts: i64) -> Bar {
    let i = ((ts - T0) / MIN) as f64;
    Bar {
        ts,
        open: 100.0 + i,
        high: 110.0 + i,
        low: 90.0 + i,
        close: 105.0 + i,
        volume: 1.0 + i,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}

/// One page from a venue that behaves EXACTLY like Deribit's: it answers the newest [`CAP`] bars at
/// or before `page_end` and **silently drops the older tail**, with no error and no signal that it
/// truncated. It never even looks at a window start — modelling the measured behaviour where a
/// 6,000-minute request comes back as the newest 5,001 rows.
fn end_anchored_page(page_end: i64) -> Vec<Bar> {
    let mut rows: Vec<Bar> =
        (0..BARS).map(|i| synthetic_bar(T0 + i * MIN)).filter(|b| b.ts <= page_end).collect();
    if rows.len() > CAP {
        rows = rows.split_off(rows.len() - CAP);
    }
    rows
}

#[test]
fn the_walk_goes_backward_and_returns_a_multi_page_window_whole() {
    let start = T0;
    let end = T0 + (BARS - 1) * MIN;

    // The CONTROL — this is what the bug looks like. A single request (the naive implementation)
    // gets `status:"ok"` and CAP plausible bars, silently missing the older majority of the window.
    let naive = end_anchored_page(end);
    assert_eq!(naive.len(), CAP);
    assert_eq!(naive[0].ts, T0 + (BARS - CAP as i64) * MIN, "the newest page only");

    let mut asked: Vec<i64> = Vec::new();
    let bars = walk_backward_pages(start, end, |page_end| {
        asked.push(page_end);
        Ok(end_anchored_page(page_end))
    })
    .expect("the walk succeeds");

    // The whole window, ascending, no gap and no duplicate.
    let got: Vec<i64> = bars.iter().map(|b| b.ts).collect();
    let want: Vec<i64> = (0..BARS).map(|i| T0 + i * MIN).collect();
    assert_eq!(got, want, "a window wider than one page must come back WHOLE");
    assert!(bars.len() > CAP, "strictly more than the newest page ({CAP} rows)");
    // The oldest bar is the one a forward/single-page implementation loses first.
    assert_eq!(bars[0].ts, start);
    assert_eq!(bars[0].open.to_bits(), 100.0f64.to_bits(), "and it is the right bar");

    // The cursor walked DOWN from the window end, and never below the window start.
    assert!(asked.len() >= 3, "7 bars over a {CAP}-row cap needs >= 3 pages, asked {asked:?}");
    assert_eq!(asked[0], end, "the walk seeds at the window END");
    assert!(asked.windows(2).all(|w| w[1] < w[0]), "end_timestamp must DECREASE: {asked:?}");
    assert!(asked.iter().all(|&e| e >= start), "never asks below the window start: {asked:?}");
}

#[test]
fn pages_are_deduped_sorted_and_clipped_to_the_window() {
    let (start, end) = (T0 + MIN, T0 + 3 * MIN);
    let mut n = 0;
    let bars = walk_backward_pages(start, end, |_page_end| {
        n += 1;
        Ok(match n {
            // newest page: carries one bar ABOVE the window end
            1 => vec![
                synthetic_bar(T0 + 2 * MIN),
                synthetic_bar(T0 + 3 * MIN),
                synthetic_bar(T0 + 4 * MIN),
            ],
            // older page: OVERLAPS on T0+2m and carries one bar BELOW the window start
            _ => vec![synthetic_bar(T0), synthetic_bar(T0 + MIN), synthetic_bar(T0 + 2 * MIN)],
        })
    })
    .expect("walk");
    assert_eq!(n, 2, "the second page reaches the start and terminates the walk");
    let got: Vec<i64> = bars.iter().map(|b| b.ts).collect();
    assert_eq!(got, vec![T0 + MIN, T0 + 2 * MIN, T0 + 3 * MIN], "clipped, sorted, deduped");
}

#[test]
fn an_empty_page_terminates_the_walk_and_an_empty_window_never_asks() {
    // A `no_data` page parses to an empty Vec — the walk's natural terminator, not an error.
    let mut n = 0;
    let bars = walk_backward_pages(T0, T0 + 10 * MIN, |_| {
        n += 1;
        Ok(Vec::new())
    })
    .expect("empty page is not an error");
    assert!(bars.is_empty());
    assert_eq!(n, 1, "one empty page ends it");

    // end < start: no request is issued at all.
    let mut asked = 0;
    let bars = walk_backward_pages(T0 + MIN, T0, |_| {
        asked += 1;
        Ok(vec![synthetic_bar(T0)])
    })
    .expect("inverted window");
    assert!(bars.is_empty());
    assert_eq!(asked, 0, "an inverted window must never touch the venue");
}

#[test]
fn a_page_error_aborts_the_walk() {
    let err = walk_backward_pages(T0, T0 + 10 * MIN, |_| Err("deribit chart HTTP 500".to_string()))
        .expect_err("a hard page error propagates");
    assert!(err.contains("HTTP 500"));
}

#[test]
fn next_page_step_decisions() {
    let end = T0 + 10 * MIN;
    // normal: step to one ms before this page's OLDEST tick (contiguous, no overlap, no gap)
    assert_eq!(
        next_page_step(&[T0 + 5 * MIN, T0 + 6 * MIN], T0, end),
        PageStep::Next(T0 + 5 * MIN - 1)
    );
    // wire order is not assumed — the MINIMUM tick drives the step, not the first element
    assert_eq!(
        next_page_step(&[T0 + 6 * MIN, T0 + 5 * MIN], T0, end),
        PageStep::Next(T0 + 5 * MIN - 1)
    );
    // the window start is covered
    assert_eq!(next_page_step(&[T0, T0 + MIN], T0, end), PageStep::Done);
    assert_eq!(next_page_step(&[T0 - MIN], T0, end), PageStep::Done);
    // an empty page
    assert_eq!(next_page_step(&[], T0, end), PageStep::Done);
    // a cursor that cannot move backward (the venue answered NEWER than we asked) must not spin
    assert_eq!(next_page_step(&[end + MIN], T0, end), PageStep::Done);
}
