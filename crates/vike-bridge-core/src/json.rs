//! Shared venue-JSON coercion — Python `float(x or 0)` / `int(x or 0)` / `str(x)` over
//! string-or-number fields. Every venue REST/WS payload carries numbers as decimal STRINGS
//! ("0.00100000"); this is the ONE place that convention is decoded (was copy-pasted per venue).
//!
//! Two layers: the value-level coercions ([`json_num`]/[`json_str`]/[`json_int`]) and the keyed
//! accessors over them ([`get_f64`]/[`get_str`]/[`get_i64`] + [`get_f64_opt`], plus the Bool-LESS
//! [`get_str_boolless`]) — the `.get(key)` + default wrappers the mappers used to re-declare per
//! module. Read [`get_str`]'s CAUTION before routing any `s(v, key)` site here: a Bool-less `s`
//! routes to [`get_str_boolless`], never to [`get_str`]. Also hosts [`parse_rows`], the shared
//! parse-a-JSON-array-body fold of the venue recon-report parsers.

use serde_json::Value;

/// Python `float(x or 0)` over JSON string-or-number fields.
pub fn json_num(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.parse::<f64>().ok(),
        _ => None,
    }
}

/// Python `str(x)` coercion of a present JSON scalar (was the per-venue `s(v,key)` helper's body):
/// strings pass through, numbers stringify, booleans become Python's `"True"`/`"False"`; any other
/// value (null/array/object) → empty string. Absent keys are the CALLER's concern — the venue does
/// `v.get(key).map(json_str).unwrap_or_default()`, so a missing field yields `""` exactly as before.
///
/// NOTE the `Bool` arm is deliberately included (faithful `str(bool)` port). The sites whose local
/// `s` has NO `Bool` arm — because a bool there must stay `""`, not `"True"` — route through
/// [`get_str_boolless`] (the four venue `history` modules, plus the binance-family perp mapper and
/// deribit's exec `event_mapper`), never through here or [`get_str`].
pub fn json_str(v: &Value) -> String {
    match v {
        Value::String(x) => x.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => if *b { "True" } else { "False" }.to_string(),
        _ => String::new(),
    }
}

/// Python `int(x)` over JSON string-or-number fields (was the per-venue `i(v,key)` helper's body):
/// numbers via `as_i64`, strings parsed as `i64`; any other value → `None`. Callers append
/// `.unwrap_or(0)` to reproduce the venues' `int(x or 0)` default.
pub fn json_int(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => s.parse::<i64>().ok(),
        _ => None,
    }
}

// ---- keyed accessors ---------------------------------------------------------------------------
// The venue mappers re-declared the same `.get(key)` + coerce + default wrappers (`s`/`f`/`i`/`num`)
// module after module; these are the ONE shared home. Each composes a value-level helper above, so
// the fold is identical to the former per-venue bodies — EXCEPT the CAUTION on [`get_str`].

/// Keyed [`json_str`]: `v.get(key)` then [`json_str`], `""` when the key is absent — exactly the
/// per-venue `s(v, key)` wrapper body.
///
/// CAUTION: this inherits [`json_str`]'s `Bool` arm (`"True"`/`"False"`), so it is NOT a drop-in for
/// the **Bool-less** `s` that the perp/history mappers keep (theirs yields `""` for a bool). Only
/// route a site here when its `s` already delegated to [`json_str`]; leave a Bool-less `s` verbatim.
pub fn get_str(v: &Value, key: &str) -> String {
    v.get(key).map(json_str).unwrap_or_default()
}

/// Keyed [`json_num`] with the venues' `float(x or 0)` default — the shared keyed `f(v, key)` /
/// `num(v, key)` accessor the venue mappers re-declared.
pub fn get_f64(v: &Value, key: &str) -> f64 {
    v.get(key).and_then(json_num).unwrap_or(0.0)
}

/// Keyed [`json_int`] with the venues' `int(x or 0)` default — the shared `i(v, key)` accessor.
pub fn get_i64(v: &Value, key: &str) -> i64 {
    v.get(key).and_then(json_int).unwrap_or(0)
}

/// Option-returning twin of [`get_f64`] (no default) — for callers that must tell an absent or
/// unparseable field (`None`) apart from a real `0.0`.
pub fn get_f64_opt(v: &Value, key: &str) -> Option<f64> {
    v.get(key).and_then(json_num)
}

/// Bool-LESS keyed `str(x)` coercion — [`get_str`]'s twin WITHOUT the `Bool` arm: a present string
/// passes through, a number stringifies, and EVERYTHING else — booleans included — is `""`. This
/// was the byte-identical local `fn s(v, k)` of the four venue `history` modules (binance-family/
/// bybit/okx/deribit) AND of two exec mappers that kept their own copy a round longer
/// (`vike_binance::family::perp_mapper`, `vike_deribit::event_mapper`) — six sites, each of which
/// reads its bool fields (`isMaker`…) via `as_bool` directly and must never see a Python-style
/// `"True"` out of a string accessor. Import as `get_str_boolless as s` to keep call sites
/// verbatim. When a bool SHOULD render `"True"` (a faithful `str(bool)` port), use [`get_str`]
/// instead.
pub fn get_str_boolless(v: &Value, key: &str) -> String {
    match v.get(key) {
        Some(Value::String(x)) => x.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

/// Parse a JSON-ARRAY response body and map every row through `f` — the shared fold of the venue
/// recon-report parsers (`parse_orders_pending`/`parse_open_orders`/`parse_spot_open_orders`/…
/// each carried this byte-identically). Errors keep the per-venue copies' exact strings: serde's
/// own message for unparseable JSON, `"expected a JSON array"` for a parsed non-array. Row mapping
/// stays per-venue in `f` (infallible per row — a malformed row maps to a default-y report, never
/// aborts the batch, exactly as before).
pub fn parse_rows<T>(body: &str, f: impl Fn(&Value) -> T) -> Result<Vec<T>, String> {
    let v: Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    let rows = v.as_array().ok_or_else(|| "expected a JSON array".to_string())?;
    Ok(rows.iter().map(f).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_f64_folds_like_the_former_wrapper_body() {
        let v = serde_json::json!({ "n": 1.5, "s": "2.5", "bad": "x", "b": true });
        assert_eq!(get_f64(&v, "n"), 1.5);
        assert_eq!(get_f64(&v, "s"), 2.5);
        assert_eq!(get_f64(&v, "bad"), 0.0); // unparseable string → the `or 0` default
        assert_eq!(get_f64(&v, "absent"), 0.0);
        assert_eq!(get_f64(&v, "b"), 0.0); // a bool is not a number
    }

    #[test]
    fn get_i64_folds_like_the_former_wrapper_body() {
        let v = serde_json::json!({ "n": 7, "s": "8", "f": 1.5, "bad": "x" });
        assert_eq!(get_i64(&v, "n"), 7);
        assert_eq!(get_i64(&v, "s"), 8);
        assert_eq!(get_i64(&v, "f"), 0); // non-integer JSON number → as_i64 is None → 0
        assert_eq!(get_i64(&v, "bad"), 0);
        assert_eq!(get_i64(&v, "absent"), 0);
    }

    #[test]
    fn get_str_carries_the_bool_arm_a_bool_less_s_does_not() {
        let v = serde_json::json!({ "s": "hi", "n": 3, "b": true });
        assert_eq!(get_str(&v, "s"), "hi");
        assert_eq!(get_str(&v, "n"), "3");
        // Delegating to json_str means a bool stringifies to Python's "True" — the exact reason a
        // Bool-less `s` must NOT be routed through here.
        assert_eq!(get_str(&v, "b"), "True");
        assert_eq!(get_str(&v, "absent"), "");
    }

    #[test]
    fn get_str_boolless_yields_empty_for_a_bool_unlike_get_str() {
        let v = serde_json::json!({ "s": "hi", "n": 3, "b": true, "arr": [1] });
        assert_eq!(get_str_boolless(&v, "s"), "hi");
        assert_eq!(get_str_boolless(&v, "n"), "3");
        // THE divergence from get_str — the reason the history modules' `s` exists:
        assert_eq!(get_str_boolless(&v, "b"), "");
        assert_eq!(get_str(&v, "b"), "True");
        assert_eq!(get_str_boolless(&v, "arr"), "");
        assert_eq!(get_str_boolless(&v, "absent"), "");
    }

    #[test]
    fn parse_rows_maps_each_row_and_keeps_the_error_strings() {
        let out = parse_rows(r#"[{"x":1},{"x":2}]"#, |v| get_i64(v, "x")).unwrap();
        assert_eq!(out, vec![1, 2]);
        assert!(parse_rows("[]", |_| 0).unwrap().is_empty());
        // a parsed NON-array keeps the exact per-venue error string
        assert_eq!(parse_rows(r#"{"a":1}"#, |_| 0).unwrap_err(), "expected a JSON array");
        // unparseable JSON surfaces serde's own message
        assert!(parse_rows("not json", |_| 0).is_err());
    }

    #[test]
    fn get_f64_opt_tells_absent_apart_from_zero() {
        let v = serde_json::json!({ "z": 0.0, "s": "0", "bad": "x" });
        assert_eq!(get_f64_opt(&v, "z"), Some(0.0));
        assert_eq!(get_f64_opt(&v, "s"), Some(0.0));
        assert_eq!(get_f64_opt(&v, "bad"), None);
        assert_eq!(get_f64_opt(&v, "absent"), None);
    }
}
