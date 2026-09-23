//! Rate-limit DISCOVERY — read a venue's request-weight budget out of the payload we already
//! download, instead of hand-copying a number into a const.
//!
//! Binance publishes its own limits in the `exchangeInfo` body this workspace ALREADY fetches for
//! the instrument catalog (`vike_binance::catalog`'s `SPOT_URL`/`PERP_URL`), and Aster serves the
//! same `exchangeInfo` grammar from its own hosts. Nothing parsed them: the budgets in
//! `vike_binance::data` were transcribed BY HAND from a live probe (SPOT 6000/min, fapi 2400/min,
//! "MEASURED 2026-08-04"), which is correct exactly until the venue changes it and nobody
//! re-probes. This module is the parse — the same response, read rather than remembered.
//!
//! It is only the READ half. Nothing here fetches, and today's catalog fetch discards the body
//! after taking `symbols[]`, so wiring a caller that keeps it (and what to do with the answer)
//! belongs to whoever mounts a gate — deliberately out of this module.
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
//! Only `REQUEST_WEIGHT` is read here. `ORDERS` and `RAW_REQUESTS` sit in the same array and are
//! DELIBERATELY ignored: they meter different things (order submissions / raw request counts) and
//! belong to different gates — `vike_binance::ratelimit` already carries the `ORDERS` budgets, and
//! silently folding them into one number would understate whichever is not the binding constraint.
//!
//! **`None` is a normal answer, never an error.** A venue that publishes no `rateLimits` (most of
//! the roster) is not misconfigured — it simply does not tell us, and the caller keeps whatever
//! budget it would have used anyway. So every failure mode here — absent array, absent
//! `REQUEST_WEIGHT` row, unparseable JSON, an interval unit this module does not know — collapses to
//! `None` rather than a `Result`: there is no caller action that distinguishes them, and a parse
//! that returned `Err` would tempt a caller into treating "venue is quiet" as a fault.
//!
//! This module is PURE (a `&str` in, a value out) and does NO I/O — the fetch belongs to whoever
//! already made the request, which is the whole point of reading a body we download regardless.

use serde_json::Value;

use crate::json::{get_i64, json_int};

/// A venue's published request-weight budget: `limit` weight units per `interval_secs`.
///
/// Kept as the raw `(limit, interval)` pair the venue actually published rather than pre-divided
/// into a rate, because a [`crate::ratelimit::RateGate`] is a SLIDING WINDOW and needs both halves:
/// `RateGate::new(limit, Duration::from_secs(interval_secs))` is the direct construction. Use
/// [`per_minute`](Self::per_minute) only when a caller genuinely wants one comparable scalar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WeightBudget {
    /// Weight units permitted per window (the venue's `limit`).
    pub limit: u64,
    /// Window length in seconds — `intervalNum` scaled by its unit (`MINUTE`/`intervalNum: 1` = 60).
    pub interval_secs: u64,
}

impl WeightBudget {
    /// The budget normalised to weight-units-per-MINUTE, so budgets published over different windows
    /// are comparable (a `100` per `10` SECOND budget and a `600` per `1` MINUTE budget are the same
    /// rate).
    ///
    /// Integer division TRUNCATES, which is the conservative direction on purpose: a budget derived
    /// here is spent, so rounding down under-claims headroom and can never over-claim it (a DAY
    /// budget of 200000 reads as 138/min, not 139). Saturating throughout — an absurd published
    /// `limit` clamps instead of wrapping into a tiny budget.
    ///
    /// A zero `interval_secs` cannot come out of [`parse_weight_budget`] (a window of zero length is
    /// rejected there), but the fields are `pub`, so a hand-built one reads as already-per-minute
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
/// `None` whenever the venue does not publish one, in any of its forms (see the module doc: absent
/// `rateLimits`, no `REQUEST_WEIGHT` row among the `ORDERS`/`RAW_REQUESTS` ones, unparseable JSON, a
/// non-positive `limit`, or an interval unit this module does not recognise). That last case is
/// deliberately `None` and NOT a guess: an unknown unit means we cannot say how long the window is,
/// and a caller falling back to its own conservative const beats a budget invented from a unit we
/// mis-assumed.
///
/// The FIRST `REQUEST_WEIGHT` row wins. Binance and Aster each publish exactly one; a hypothetical
/// second row is ignored rather than combined, because "combine" has no obviously-right meaning
/// (min? sum?) and inventing one here would be untested behaviour on a shape no venue serves.
pub fn parse_weight_budget(exchange_info_body: &str) -> Option<WeightBudget> {
    let v: Value = serde_json::from_str(exchange_info_body).ok()?;
    let rows = v.get("rateLimits")?.as_array()?;
    rows.iter().find(|r| is_request_weight(r)).and_then(row_to_budget)
}

/// The row filter: `rateLimitType == "REQUEST_WEIGHT"`. ASCII-case-insensitive because the value is
/// an enum name the venue chose to spell in caps, not data we should be brittle about.
fn is_request_weight(row: &Value) -> bool {
    row.get("rateLimitType")
        .and_then(Value::as_str)
        .is_some_and(|t| t.eq_ignore_ascii_case("REQUEST_WEIGHT"))
}

/// One `rateLimits` row -> a normalised [`WeightBudget`], or `None` when any of its three fields is
/// missing or nonsensical.
///
/// `limit` rides the shared [`get_i64`] coercion (venues serve numbers as JSON numbers here, but the
/// same field is a decimal STRING on other endpoints, and this workspace decodes that convention in
/// ONE place). A missing or unparseable `limit` yields that helper's `0` default, which the
/// positivity check below rejects — a zero-weight budget is not a real budget.
///
/// ⚠ It is not a limiter that would block forever either, which is what this sentence used to claim.
/// A zero-occurrence [`crate::ratelimit::RateGate`] fails **OPEN**: it admits everything, instantly
/// and forever, while still presenting as a gate. That is the whole reason this check rejects rather
/// than clamps, and why `RateGate::new` now asserts outright instead of `debug_assert`ing — this
/// module's own doc invites a budget parsed from a venue response, which is precisely the runtime
/// path a compile-time check cannot cover.
///
/// `intervalNum` is absent-tolerant but not garbage-tolerant: ABSENT reads as `1` (the natural
/// meaning of a bare `"interval":"MINUTE"`), while a PRESENT-but-unparseable or non-positive value is
/// `None`, because a window of zero or negative length is a contradiction, not a default.
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

/// The `interval` unit -> seconds. Only the units the covered venues actually publish are known;
/// anything else is `None` (see [`parse_weight_budget`] on why an unknown unit must not be guessed).
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
    //! Fixtures are inline `const`s, not files: this crate keeps NO `tests/fixtures/` tree (the
    //! per-venue captured-frame fixtures live in `crates/bridges/<venue>/tests/fixtures/captured/`,
    //! next to the venue whose wire they pin), and every canned payload here and in the sibling
    //! modules is a literal beside the test that reads it.
    //!
    //! Both bodies below are REAL `exchangeInfo` responses, trimmed: the `rateLimits` array is
    //! verbatim, and the surrounding envelope keeps only enough of `symbols`/`exchangeFilters` to
    //! prove the parser reads a full response rather than a hand-shaped `rateLimits` fragment.
    //!
    //! **CAPTURED OFF THE WIRE 2026-08-04 from the CI box**, not reconstructed from documentation. The
    //! distinction earned its keep immediately: a first draft written from the documented values
    //! had `RAW_REQUESTS` at 61000 where the live body says 300000. A fixture that is *plausible*
    //! rather than *captured* is a hypothesis wearing a measurement's clothes — the same mistake
    //! this whole change set exists to remove.
    //!
    //! Aster's `fapi/v3/exchangeInfo` returns a `rateLimits` array BYTE-IDENTICAL to Binance's fapi
    //! one (verified in the same capture, RE-CONFIRMED live 2026-08-05), which is independent
    //! evidence for the weight-5 figure `vike_model::venue_rate_limits::ASTER_PERP` documents — that
    //! number was otherwise inferred from Binance by analogy. `vike_aster::data` READS that array at
    //! runtime now, so the analogy is no longer load-bearing on the mainnet perp host.
    use super::*;

    /// `https://api.binance.com/api/v3/exchangeInfo` — SPOT. `REQUEST_WEIGHT` = 6000/MINUTE, the
    /// number `vike_binance::data`'s SPOT const documents as measured by hand.
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

    /// `https://fapi.binance.com/fapi/v1/exchangeInfo` — USDⓈ-M futures. `REQUEST_WEIGHT` = 2400,
    /// NOT spot's 6000. The two hosts having different budgets is the defect
    /// `vike_binance::data`'s two separate `KlineSpec` consts exist to prevent, so a parser that
    /// cannot tell them apart would be useless.
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

    /// One `rateLimits` row, in the venue's exact field order. `num`/`limit` are `i64` (not `u64`)
    /// so the rejection tests can emit the NEGATIVE values a hostile/garbled body could carry.
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
        // a zero/negative limit is a limiter that blocks forever, not a budget
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
