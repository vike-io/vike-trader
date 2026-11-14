//! `string_row_to_bar`, the row parser bybit's and okx's `data.rs` each carried a copy of. Every
//! error golden is the word the former copy produced for the same row (bybit: `"bybit kline"` +
//! `startTime`; okx: `"okx candle"` + `ts`). And the paced walk bybit, okx and deribit each wrote,
//! `kline_page_walk_backward` on a `kline_page_pacer` (the walk's own decision is pinned once, beside
//! `vike_data::source::page_walk_backward`).

use serde_json::{Value, json};
use vike_model::rate_limits::PaceSample;

use super::*;

const MIN: i64 = 60_000;
const T0: i64 = 1_700_000_000_000;

fn bar(ts: i64) -> Bar {
    kline_to_bar(ts, 1.0, 1.0, 1.0, 1.0, 1.0)
}

/// The walk the three venues ran, on a fixed pacer that does not sleep: `end` steps one ms below
/// each page's oldest bar (a newest-first page too), an empty page ends it, the bars come back
/// ascending once each, every page is timed, and ONE pace line follows the first page with rows.
/// Kill proof: a pace line per page, a line before any row came back, or no measurement returned.
#[test]
fn the_paced_walk_steps_end_down_times_each_page_and_says_its_pace_once() {
    let mut ends = Vec::new();
    let pacer = kline_page_pacer("bybit", Duration::ZERO, None);
    let (got, events) = vike_log::capture::captured(|| {
        kline_page_walk_backward("bybit", "BTCUSDT", "1m", T0, T0 + 10 * MIN, pacer, |end| {
            ends.push(end);
            Ok(match ends.len() {
                1 => (6..=10).rev().map(|m| bar(T0 + m * MIN)).collect(),
                2 => (1..=5).map(|m| bar(T0 + m * MIN)).collect(),
                _ => Vec::new(),
            })
        })
    });
    let (bars, pace) = got.expect("the walk");
    assert_eq!(ends, vec![T0 + 10 * MIN, T0 + 6 * MIN - 1, T0 + MIN - 1]);
    assert_eq!(
        bars.iter().map(|b| b.timestamp).collect::<Vec<_>>(),
        (1..=10).map(|m| T0 + m * MIN).collect::<Vec<_>>()
    );
    assert_eq!(pace.map(|p| p.samples), Some(3), "the empty page was timed too");
    let lines: Vec<_> = events.iter().filter(|e| e.level == tracing::Level::INFO).collect();
    assert_eq!(lines.len(), 1, "one pace line per walk: {events:?}");
    let line = lines[0];
    assert_eq!(line.message, "kline backfill: measured page cost on the fixed page delay");
    let fields = ["venue", "symbol", "interval", "page_rows", "remaining_pages", "page_delay_ms"]
        .map(|f| line.field(f));
    assert_eq!(
        fields,
        [Some("bybit"), Some("BTCUSDT"), Some("1m"), Some("5"), Some("2"), Some("0")]
    );
}

/// An empty window asks nothing and measures nothing; a page's error is the walk's.
#[test]
fn the_paced_walk_asks_nothing_for_an_empty_window_and_passes_an_error_through() {
    let pacer = kline_page_pacer("okx", Duration::ZERO, None);
    let empty = kline_page_walk_backward("okx", "BTC-USDT", "1m", T0, T0 - 1, pacer, |_| {
        panic!("an empty window asks nothing")
    });
    assert_eq!(empty, Ok((Vec::new(), None)));
    let pacer = kline_page_pacer("okx", Duration::ZERO, None);
    let failed = kline_page_walk_backward("okx", "BTC-USDT", "1m", T0, T0 + MIN, pacer, |_| {
        Err("okx candles HTTP 500: down".to_string())
    });
    assert_eq!(failed, Err("okx candles HTTP 500: down".to_string()));
}

/// A page that does not move `end` (the venue answers the same bars again) is the walk's error, in
/// the shared sentence with THIS walk's venue, symbol and interval, never a short success.
#[test]
fn the_paced_walk_fails_on_a_page_that_does_not_move_its_end() {
    let pacer = kline_page_pacer("okx", Duration::ZERO, None);
    let stuck = kline_page_walk_backward("okx", "BTC-USDT", "1m", T0, T0 + 10 * MIN, pacer, |_| {
        Ok(vec![bar(T0 + 5 * MIN)])
    });
    let point = vike_data::source::PageWalkStuck::Backward { end_ms: T0 + 5 * MIN - 1 };
    assert_eq!(stuck, Err(point.text("okx", "BTC-USDT", "1m")));
}

/// `kline_page_pacer` is the fixed-delay policy the three `page_pacer`s were, seeded or not.
#[test]
fn the_kline_pacer_never_changes_its_fixed_delay() {
    let delay = Duration::from_millis(200);
    let seed =
        PaceSample { request_ms: 480, per_request_weight: 1.0, budget_per_min: None, samples: 20 };
    crate::policy_pins::assert_the_pager_never_changes_its_fixed_delay(
        |seed| kline_page_pacer("deribit", delay, seed),
        delay,
        &seed,
        "deribit",
    );
}

fn row(v: Value) -> Vec<Value> {
    v.as_array().expect("a row").clone()
}

#[test]
fn a_row_keeps_each_strings_exact_bits_and_a_volume_miss_is_zero() {
    for (what, ts) in [("bybit kline", "startTime"), ("okx candle", "ts")] {
        let r = row(json!(["1700000000000", "0.1", "0.30000000000000004", "1e-7", "42", "x", "9"]));
        let bar = string_row_to_bar(&r, what, ts).expect("a well-formed row");
        assert_eq!(bar.timestamp, 1_700_000_000_000);
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
    let number_timestamp = row(json!([1700000000000_i64, "1", "1", "1", "1", "1"]));
    assert_eq!(
        string_row_to_bar(&number_timestamp, "bybit kline", "startTime").unwrap_err(),
        "bybit kline startTime not str"
    );
    assert_eq!(
        string_row_to_bar(&number_timestamp, "okx candle", "ts").unwrap_err(),
        "okx candle ts not str"
    );

    let bad_timestamp = row(json!(["t", "1", "1", "1", "1", "1"]));
    assert_eq!(
        string_row_to_bar(&bad_timestamp, "bybit kline", "startTime").unwrap_err(),
        "bybit kline startTime parse: invalid digit found in string"
    );
    assert_eq!(
        string_row_to_bar(&bad_timestamp, "okx candle", "ts").unwrap_err(),
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
