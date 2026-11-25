//! Fixtures are inline `const`s. Both bodies are REAL `exchangeInfo` responses CAPTURED off the wire, not
//! reconstructed from documentation (a documented `RAW_REQUESTS` value was wrong), trimmed: `rateLimits` verbatim,
//! and just enough envelope to prove the parser reads a full response. Aster's `fapi/v3/exchangeInfo` serves a
//! `rateLimits` array byte-identical to Binance's fapi one. These are the tests `vike_bridge_core`'s
//! `rate_discovery` module held before the parser moved here, unchanged (`WeightBudget::per_minute`'s went with the
//! type, to `crates/vike-bridge-core/src/pacer_tests.rs`).

use super::*;

/// `https://api.binance.com/api/v3/exchangeInfo`: SPOT, `REQUEST_WEIGHT` = 6000/MINUTE.
const SPOT_EXCHANGE_INFO: &str = r#"{
  "timezone": "UTC",
  "serverTime": 1754300000000,
  "rateLimits": [
    {"rateLimitType":"REQUEST_WEIGHT","interval":"MINUTE","intervalNum":1,"limit":6000},
    {"rateLimitType":"ORDERS","interval":"SECOND","intervalNum":10,"limit":100},
    {"rateLimitType":"ORDERS","interval":"DAY","intervalNum":1,"limit":200000},
    {"rateLimitType":"RAW_REQUESTS","interval":"MINUTE","intervalNum":5,"limit":300000}
  ],
  "exchangeFilters": [],
  "symbols": [
    {"symbol":"BTCUSDT","status":"TRADING","baseAsset":"BTC","quoteAsset":"USDT"}
  ]
}"#;

/// `https://fapi.binance.com/fapi/v1/exchangeInfo`: USDⓈ-M futures, `REQUEST_WEIGHT` = 2400,
/// NOT spot's 6000; a parser that cannot tell the two hosts apart is useless.
const FAPI_EXCHANGE_INFO: &str = r#"{
  "timezone": "UTC",
  "serverTime": 1754300000000,
  "futuresType": "U_MARGINED",
  "rateLimits": [
    {"rateLimitType":"REQUEST_WEIGHT","interval":"MINUTE","intervalNum":1,"limit":2400},
    {"rateLimitType":"ORDERS","interval":"MINUTE","intervalNum":1,"limit":1200},
    {"rateLimitType":"ORDERS","interval":"SECOND","intervalNum":10,"limit":300}
  ],
  "exchangeFilters": [],
  "symbols": [
    {"symbol":"BTCUSDT","status":"TRADING","baseAsset":"BTC","quoteAsset":"USDT",
     "contractType":"PERPETUAL"}
  ]
}"#;

/// One `rateLimits` row in the venue's field order (`i64`, so tests can emit negatives).
fn row(kind: &str, interval: &str, number: i64, limit: i64) -> String {
    format!(
        r#"{{"rateLimitType":"{kind}","interval":"{interval}","intervalNum":{number},"limit":{limit}}}"#
    )
}

fn body(rows: &[String]) -> String {
    format!(r#"{{"rateLimits":[{}]}}"#, rows.join(","))
}

#[test]
fn reads_the_spot_budget_off_a_real_exchange_info_body() {
    assert_eq!(
        parse_weight_budget(SPOT_EXCHANGE_INFO),
        Some(WeightBudget { limit: 6000, interval_secs: 60 }),
        "spot publishes REQUEST_WEIGHT 6000/MINUTE — the hand-transcribed const's source"
    );
}

#[test]
fn reads_the_fapi_budget_and_it_is_not_the_spot_one() {
    assert_eq!(
        parse_weight_budget(FAPI_EXCHANGE_INFO),
        Some(WeightBudget { limit: 2400, interval_secs: 60 }),
        "fapi publishes 2400, a DIFFERENT budget from spot's 6000"
    );
    assert_ne!(
        parse_weight_budget(FAPI_EXCHANGE_INFO),
        parse_weight_budget(SPOT_EXCHANGE_INFO),
        "the two hosts must never read as the same budget"
    );
}

#[test]
fn orders_and_raw_requests_rows_are_ignored() {
    // ORDERS sits FIRST here, so a parser that took row 0 would answer 100/10s.
    let b = body(&[
        row("ORDERS", "SECOND", 10, 100),
        row("RAW_REQUESTS", "MINUTE", 5, 61000),
        row("REQUEST_WEIGHT", "MINUTE", 1, 6000),
    ]);
    assert_eq!(
        parse_weight_budget(&b),
        Some(WeightBudget { limit: 6000, interval_secs: 60 }),
        "only the REQUEST_WEIGHT row is the weight budget"
    );
}

#[test]
fn no_request_weight_row_is_none_not_a_wrong_budget() {
    let b = body(&[row("ORDERS", "SECOND", 10, 100), row("RAW_REQUESTS", "MINUTE", 5, 61000)]);
    assert_eq!(parse_weight_budget(&b), None);
}

#[test]
fn absent_rate_limits_is_none_a_quiet_venue_is_normal() {
    // The Binance envelope minus the array — the shape most venues serve.
    assert_eq!(parse_weight_budget(r#"{"timezone":"UTC","symbols":[]}"#), None);
    assert_eq!(parse_weight_budget("{}"), None);
    // present but not an array
    assert_eq!(parse_weight_budget(r#"{"rateLimits":{"limit":6000}}"#), None);
    assert_eq!(parse_weight_budget(r#"{"rateLimits":[]}"#), None);
}

#[test]
fn malformed_json_is_none_never_a_panic() {
    assert_eq!(parse_weight_budget(""), None);
    assert_eq!(parse_weight_budget("not json"), None);
    assert_eq!(parse_weight_budget("<html>502 Bad Gateway</html>"), None);
    // truncated mid-array (a cut-off response body)
    assert_eq!(parse_weight_budget(r#"{"rateLimits":[{"rateLimitType":"REQUEST_WEIGHT","#), None);
    // valid JSON, wrong root type
    assert_eq!(parse_weight_budget("[]"), None);
}

#[test]
fn second_and_day_intervals_normalise_to_seconds() {
    let s = body(&[row("REQUEST_WEIGHT", "SECOND", 10, 100)]);
    assert_eq!(
        parse_weight_budget(&s),
        Some(WeightBudget { limit: 100, interval_secs: 10 }),
        "SECOND/intervalNum 10 = a 10-second window"
    );
    let d = body(&[row("REQUEST_WEIGHT", "DAY", 1, 200_000)]);
    assert_eq!(
        parse_weight_budget(&d),
        Some(WeightBudget { limit: 200_000, interval_secs: 86_400 }),
        "DAY/intervalNum 1 = 86400 seconds"
    );
    // intervalNum scales the unit, it does not replace it
    let m = body(&[row("REQUEST_WEIGHT", "MINUTE", 5, 61_000)]);
    assert_eq!(parse_weight_budget(&m), Some(WeightBudget { limit: 61_000, interval_secs: 300 }));
}

#[test]
fn unknown_interval_unit_is_none_rather_than_a_guessed_window() {
    // HOUR is not served by any covered venue; guessing 3600 would be an untested assumption.
    let b = body(&[row("REQUEST_WEIGHT", "HOUR", 1, 6000)]);
    assert_eq!(parse_weight_budget(&b), None);
    let e = body(&[row("REQUEST_WEIGHT", "", 1, 6000)]);
    assert_eq!(parse_weight_budget(&e), None);
}

#[test]
fn nonsense_limits_and_windows_are_rejected() {
    // a zero/negative limit is not a budget (a zero-occurrence RateGate fails OPEN)
    assert_eq!(parse_weight_budget(&body(&[row("REQUEST_WEIGHT", "MINUTE", 1, 0)])), None);
    assert_eq!(parse_weight_budget(&body(&[row("REQUEST_WEIGHT", "MINUTE", 1, -5)])), None);
    // a zero/negative window has no meaning either
    assert_eq!(parse_weight_budget(&body(&[row("REQUEST_WEIGHT", "MINUTE", 0, 6000)])), None);
    assert_eq!(parse_weight_budget(&body(&[row("REQUEST_WEIGHT", "MINUTE", -1, 6000)])), None);
    // missing limit / missing interval
    assert_eq!(
        parse_weight_budget(
            r#"{"rateLimits":[{"rateLimitType":"REQUEST_WEIGHT","interval":"MINUTE"}]}"#
        ),
        None
    );
    assert_eq!(
        parse_weight_budget(r#"{"rateLimits":[{"rateLimitType":"REQUEST_WEIGHT","limit":6000}]}"#),
        None
    );
}

#[test]
fn absent_interval_num_defaults_to_one_of_the_unit() {
    // a bare `"interval":"MINUTE"` means one minute; only a PRESENT-but-broken value is fatal
    let b =
        r#"{"rateLimits":[{"rateLimitType":"REQUEST_WEIGHT","interval":"MINUTE","limit":6000}]}"#;
    assert_eq!(parse_weight_budget(b), Some(WeightBudget { limit: 6000, interval_secs: 60 }));
    let bad = r#"{"rateLimits":[{"rateLimitType":"REQUEST_WEIGHT","interval":"MINUTE","intervalNum":"x","limit":6000}]}"#;
    assert_eq!(parse_weight_budget(bad), None);
}

#[test]
fn string_numerics_parse_like_every_other_venue_field() {
    // Venues serve numbers as decimal STRINGS on many endpoints; the shared json coercion is why
    // this shape costs nothing here.
    let b = r#"{"rateLimits":[{"rateLimitType":"REQUEST_WEIGHT","interval":"MINUTE","intervalNum":"1","limit":"2400"}]}"#;
    assert_eq!(parse_weight_budget(b), Some(WeightBudget { limit: 2400, interval_secs: 60 }));
}

#[test]
fn the_first_request_weight_row_wins() {
    let b =
        body(&[row("REQUEST_WEIGHT", "MINUTE", 1, 2400), row("REQUEST_WEIGHT", "MINUTE", 1, 6000)]);
    assert_eq!(
        parse_weight_budget(&b),
        Some(WeightBudget { limit: 2400, interval_secs: 60 }),
        "documented tie-break: first wins, no invented combination rule"
    );
}

#[test]
fn rate_limit_type_matching_is_case_insensitive() {
    let b = body(&[row("request_weight", "minute", 1, 2400)]);
    assert_eq!(parse_weight_budget(&b), Some(WeightBudget { limit: 2400, interval_secs: 60 }));
}
