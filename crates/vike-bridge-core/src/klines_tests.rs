//! `string_row_to_bar`, the row parser bybit's and okx's `data.rs` each carried a copy of. Every
//! error golden is the word the former copy produced for the same row (bybit: `"bybit kline"` +
//! `startTime`; okx: `"okx candle"` + `ts`). And `next_page_step`, the backward walk's decision the
//! bybit and deribit suites each pinned on their own.

use serde_json::{Value, json};

use super::*;

fn row(v: Value) -> Vec<Value> {
    v.as_array().expect("a row").clone()
}

#[test]
fn a_row_keeps_each_strings_exact_bits_and_a_volume_miss_is_zero() {
    for (what, ts) in [("bybit kline", "startTime"), ("okx candle", "ts")] {
        let r = row(json!(["1700000000000", "0.1", "0.30000000000000004", "1e-7", "42", "x", "9"]));
        let bar = string_row_to_bar(&r, what, ts).expect("a well-formed row");
        assert_eq!(bar.ts, 1_700_000_000_000);
        assert_eq!(bar.open.to_bits(), 0.1f64.to_bits());
        assert_eq!(bar.high.to_bits(), "0.30000000000000004".parse::<f64>().unwrap().to_bits());
        assert_eq!((bar.low, bar.close, bar.volume), (1e-7, 42.0, 0.0));
        assert_eq!((bar.funding, bar.bid, bar.ask, bar.symbol), (None, None, None, None));
        let short = row(json!(["1", "2", "3", "4", "5"]));
        assert_eq!(string_row_to_bar(&short, what, ts).expect("no volume field").volume, 0.0);
    }
}

#[test]
fn each_venue_words_a_bad_row_as_its_own_copy_did() {
    let num_ts = row(json!([1700000000000_i64, "1", "1", "1", "1", "1"]));
    assert_eq!(
        string_row_to_bar(&num_ts, "bybit kline", "startTime").unwrap_err(),
        "bybit kline startTime not str"
    );
    assert_eq!(
        string_row_to_bar(&num_ts, "okx candle", "ts").unwrap_err(),
        "okx candle ts not str"
    );

    let bad_ts = row(json!(["t", "1", "1", "1", "1", "1"]));
    assert_eq!(
        string_row_to_bar(&bad_ts, "bybit kline", "startTime").unwrap_err(),
        "bybit kline startTime parse: invalid digit found in string"
    );
    assert_eq!(
        string_row_to_bar(&bad_ts, "okx candle", "ts").unwrap_err(),
        "okx candle ts parse: invalid digit found in string"
    );

    let bad_low = row(json!(["1", "1", "1", "low?", "1", "1"]));
    assert_eq!(
        string_row_to_bar(&bad_low, "bybit kline", "startTime").unwrap_err(),
        "bybit kline low parse: invalid float literal"
    );
    let missing_close = row(json!(["1", "1", "1", "1"]));
    assert_eq!(
        string_row_to_bar(&missing_close, "okx candle", "ts").unwrap_err(),
        "okx candle close not str"
    );
}

/// Every case the bybit (`data_tests.rs`) and deribit (`tests/offline/deribit_klines.rs`) copies of
/// this test held, the short-page row bybit alone had included.
#[test]
fn next_page_step_decisions() {
    const MIN: i64 = 60_000;
    const T0: i64 = 1_700_000_000_000;
    let end = T0 + 10 * MIN;
    // Normal: step to one ms below the page's OLDEST tick (contiguous, no overlap, no gap).
    assert_eq!(
        next_page_step(&[T0 + 5 * MIN, T0 + 6 * MIN], T0, end),
        PageStep::Next(T0 + 5 * MIN - 1)
    );
    // Wire order is irrelevant — the MINIMUM decides, not the first element.
    assert_eq!(
        next_page_step(&[T0 + 6 * MIN, T0 + 5 * MIN], T0, end),
        PageStep::Next(T0 + 5 * MIN - 1)
    );
    // The window start is covered (exactly, then overshot).
    assert_eq!(next_page_step(&[T0, T0 + MIN], T0, end), PageStep::Done);
    assert_eq!(next_page_step(&[T0 - MIN], T0, end), PageStep::Done);
    // Empty page.
    assert_eq!(next_page_step(&[], T0, end), PageStep::Done);
    // Ticks NEWER than the `end` asked for cannot move the cursor backward (it must not spin).
    assert_eq!(next_page_step(&[end + MIN], T0, end), PageStep::Done);
    // A SHORT page is NOT a stop signal: termination is by observed ticks, never a page count.
    assert_eq!(next_page_step(&[T0 + 9 * MIN], T0, end), PageStep::Next(T0 + 9 * MIN - 1));
}
