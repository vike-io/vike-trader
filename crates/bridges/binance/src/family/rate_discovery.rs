//! Rate-limit DISCOVERY: read the family's request-weight budget out of its `exchangeInfo` body
//! (Binance, and Aster's identical grammar) instead of hand-copying a number into a const.
//!
//! PURE (a `&str` in, a value out), no I/O: the fetch belongs to the caller,
//! `crates/bridges/binance/src/family/history/klines.rs`'s `discovery_url` being the worked one. The budget it answers is
//! `vike_bridge_core::pacer`'s [`WeightBudget`], what the shared `Pacer` paces against; this grammar is the
//! family's, so it left `vike-bridge-core` (a one-venue reader never sits in a shared home).
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
//! different gates (`crates/bridges/binance/src/transport/ratelimit.rs` carries the `ORDERS` budgets);
//! folding them into one number would understate whichever is not binding.
//!
//! **`None` is a normal answer, never an error.** Most venues publish no `rateLimits`, and every
//! failure mode (absent array, no `REQUEST_WEIGHT` row, unparseable JSON, an unknown interval unit)
//! collapses to `None`: no caller action distinguishes them, and an `Err` would tempt a caller into
//! treating "venue is quiet" as a fault.

use serde_json::Value;

use vike_bridge_core::json::{get_i64, json_int};
use vike_bridge_core::pacer::WeightBudget;

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
/// rejects: ⚠ a zero-occurrence `vike_bridge_core::ratelimit::RateGate` fails **OPEN** (admits everything
/// while presenting as a gate), which is why this rejects rather than clamps and why
/// `RateGate::new` asserts.
///
/// `intervalNum` ABSENT reads as `1` (a bare `"interval":"MINUTE"`); PRESENT but unparseable or
/// non-positive is `None`, since a zero or negative window is a contradiction, not a default.
fn row_to_budget(row: &Value) -> Option<WeightBudget> {
    let limit = u64::try_from(get_i64(row, "limit")).ok().filter(|&l| l > 0)?;
    let unit_secs = interval_unit_secs(row.get("interval")?.as_str()?)?;
    let number = match row.get("intervalNum") {
        None => 1,
        Some(v) => json_int(v).filter(|&n| n > 0)?,
    };
    let interval_secs = unit_secs.checked_mul(u64::try_from(number).ok()?)?;
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

#[path = "rate_discovery_tests.rs"]
#[cfg(test)]
mod rate_discovery_tests;
