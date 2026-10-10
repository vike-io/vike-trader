//! Rate-limit DISCOVERY: read a venue's request-weight budget out of its `exchangeInfo` body
//! (Binance, and Aster's identical grammar) instead of hand-copying a number into a const.
//!
//! PURE (a `&str` in, a value out), no I/O: the fetch belongs to the caller,
//! `crates/bridges/binance/src/family/klines.rs`'s `discovery_url` being the worked one.
//!
//! The shape (verbatim, trimmed):
//!
//! ```json
//! "rateLimits":[
//!   {"rateLimitType":"REQUEST_WEIGHT","interval":"MINUTE","intervalNum":1,"limit":2400},
//!   {"rateLimitType":"ORDERS","interval":"SECOND","intervalNum":10,"limit":300}
//! ]
//! ```
//!
//! Only `REQUEST_WEIGHT` is read. `ORDERS` and `RAW_REQUESTS` meter different things and belong to
//! different gates (`crates/bridges/binance/src/ratelimit.rs` carries the `ORDERS` budgets);
//! folding them into one number would understate whichever is not binding.
//!
//! **`None` is a normal answer, never an error.** Most venues publish no `rateLimits`, and every
//! failure mode (absent array, no `REQUEST_WEIGHT` row, unparseable JSON, an unknown interval unit)
//! collapses to `None`: no caller action distinguishes them, and an `Err` would tempt a caller into
//! treating "venue is quiet" as a fault.

use serde_json::Value;

use crate::json::{get_i64, json_int};

/// A venue's published request-weight budget: `limit` weight units per `interval_secs`.
///
/// The raw pair, not a pre-divided rate, because a [`crate::ratelimit::RateGate`] is a SLIDING
/// WINDOW and needs both halves (`RateGate::new(limit, Duration::from_secs(interval_secs))`). Use
/// [`per_minute`](Self::per_minute) only for one comparable scalar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WeightBudget {
    /// Weight units permitted per window (the venue's `limit`).
    pub limit: u64,
    /// Window length in seconds — `intervalNum` scaled by its unit (`MINUTE`/`intervalNum: 1` = 60).
    pub interval_secs: u64,
}

impl WeightBudget {
    /// The budget in weight units per MINUTE, so differently windowed budgets compare (`100` per
    /// 10 s and `600` per minute are the same rate).
    ///
    /// TRUNCATES on purpose: a derived budget is spent, so rounding down can never over-claim
    /// headroom (200000/DAY reads as 138/min). Saturating, so an absurd `limit` cannot wrap small.
    /// A hand-built zero `interval_secs` ([`parse_weight_budget`] rejects one) reads as per-minute
    /// rather than dividing by zero.
    pub fn per_minute(&self) -> u64 {
        if self.interval_secs == 0 {
            return self.limit;
        }
        self.limit.saturating_mul(60) / self.interval_secs
    }
}

/// Parse the `REQUEST_WEIGHT` budget out of an `exchangeInfo` response body.
///
/// `None` whenever the venue does not publish one (module doc), including a non-positive `limit`
/// and an unknown interval unit: a caller falling back to its own conservative const beats a
/// window length guessed from a mis-assumed unit.
///
/// The FIRST `REQUEST_WEIGHT` row wins; a second (no venue serves one) is ignored rather than
/// combined, since "combine" (min? sum?) has no obviously right meaning.
pub fn parse_weight_budget(exchange_info_body: &str) -> Option<WeightBudget> {
    let v: Value = serde_json::from_str(exchange_info_body).ok()?;
    let rows = v.get("rateLimits")?.as_array()?;
    rows.iter().find(|r| is_request_weight(r)).and_then(row_to_budget)
}

/// The row filter: `rateLimitType == "REQUEST_WEIGHT"`, ASCII-case-insensitive (an enum name, not
/// data to be brittle about).
fn is_request_weight(row: &Value) -> bool {
    row.get("rateLimitType")
        .and_then(Value::as_str)
        .is_some_and(|t| t.eq_ignore_ascii_case("REQUEST_WEIGHT"))
}

/// One `rateLimits` row -> a normalised [`WeightBudget`], or `None` when any of its three fields is
/// missing or nonsensical.
///
/// `limit` rides the shared [`get_i64`] coercion (a number here, a decimal STRING on other
/// endpoints). A missing or unparseable `limit` yields its `0` default, which the positivity check
/// rejects: ⚠ a zero-occurrence [`crate::ratelimit::RateGate`] fails **OPEN** (admits everything
/// while presenting as a gate), which is why this rejects rather than clamps and why
/// `RateGate::new` asserts.
///
/// `intervalNum` ABSENT reads as `1` (a bare `"interval":"MINUTE"`); PRESENT but unparseable or
/// non-positive is `None`, since a zero or negative window is a contradiction, not a default.
fn row_to_budget(row: &Value) -> Option<WeightBudget> {
    let limit = u64::try_from(get_i64(row, "limit")).ok().filter(|&l| l > 0)?;
    let unit_secs = interval_unit_secs(row.get("interval")?.as_str()?)?;
    let num = match row.get("intervalNum") {
        None => 1,
        Some(v) => json_int(v).filter(|&n| n > 0)?,
    };
    let interval_secs = unit_secs.checked_mul(u64::try_from(num).ok()?)?;
    Some(WeightBudget { limit, interval_secs })
}

/// The `interval` unit -> seconds: only the units covered venues publish; anything else is `None`
/// ([`parse_weight_budget`]).
fn interval_unit_secs(interval: &str) -> Option<u64> {
    match interval.to_ascii_uppercase().as_str() {
        "SECOND" => Some(1),
        "MINUTE" => Some(60),
        "DAY" => Some(86_400),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    //! Fixtures are inline `const`s (this crate keeps no `tests/fixtures/` tree). Both bodies are
    //! REAL `exchangeInfo` responses CAPTURED off the wire, not reconstructed from documentation (a
    //! documented `RAW_REQUESTS` value was wrong), trimmed: `rateLimits` verbatim, and just enough
    //! envelope to prove the parser reads a full response. Aster's `fapi/v3/exchangeInfo` serves a
    //! `rateLimits` array byte-identical to Binance's fapi one.
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
    fn row(kind: &str, interval: &str, num: i64, limit: i64) -> String {
        format!(
            r#"{{"rateLimitType":"{kind}","interval":"{interval}","intervalNum":{num},"limit":{limit}}}"#
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
        assert_eq!(
            parse_weight_budget(r#"{"rateLimits":[{"rateLimitType":"REQUEST_WEIGHT","#),
            None
        );
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
        assert_eq!(
            parse_weight_budget(&m),
            Some(WeightBudget { limit: 61_000, interval_secs: 300 })
        );
    }

    #[test]
    fn per_minute_makes_differently_windowed_budgets_comparable() {
        assert_eq!(WeightBudget { limit: 6000, interval_secs: 60 }.per_minute(), 6000);
        assert_eq!(
            WeightBudget { limit: 100, interval_secs: 10 }.per_minute(),
            600,
            "100 per 10s is 600/min"
        );
        assert_eq!(
            WeightBudget { limit: 200_000, interval_secs: 86_400 }.per_minute(),
            138,
            "truncates DOWN (138.88…) — under-claiming headroom is the safe direction"
        );
        // the hand-built degenerate case: no panic, read as already-per-minute
        assert_eq!(WeightBudget { limit: 42, interval_secs: 0 }.per_minute(), 42);
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
            parse_weight_budget(
                r#"{"rateLimits":[{"rateLimitType":"REQUEST_WEIGHT","limit":6000}]}"#
            ),
            None
        );
    }

    #[test]
    fn absent_interval_num_defaults_to_one_of_the_unit() {
        // a bare `"interval":"MINUTE"` means one minute; only a PRESENT-but-broken value is fatal
        let b = r#"{"rateLimits":[{"rateLimitType":"REQUEST_WEIGHT","interval":"MINUTE","limit":6000}]}"#;
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
        let b = body(&[
            row("REQUEST_WEIGHT", "MINUTE", 1, 2400),
            row("REQUEST_WEIGHT", "MINUTE", 1, 6000),
        ]);
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
}
