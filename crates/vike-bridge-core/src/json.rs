//! Shared venue-JSON coercion over string-or-number fields: venue payloads carry numbers as decimal
//! STRINGS (`"0.00100000"`), and this is the ONE place that convention is decoded.
//!
//! Two layers: the value-level coercions ([`json_num`]/[`json_str`]/[`json_int`]) and the keyed
//! accessors over them ([`get_f64`]/[`get_str`]/[`get_i64`] + [`get_f64_opt`], plus the Bool-LESS
//! [`get_str_boolless`]). ⚠ Read [`get_str`]'s CAUTION before routing a keyed string read here: a
//! site whose bools must read `""` uses [`get_str_boolless`]. Also [`parse_rows`], the shared
//! parse-a-JSON-array-body fold of the venue recon-report parsers.

use serde_json::Value;

/// A JSON number, or a string that parses as one; anything else `None`.
pub fn json_num(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.parse::<f64>().ok(),
        _ => None,
    }
}

/// String coercion of a present JSON scalar: strings pass through, numbers stringify, booleans
/// become capitalised `"True"`/`"False"`; null/array/object -> `""`. An absent key is the caller's
/// concern ([`get_str`] maps it to `""`).
///
/// ⚠ The `Bool` arm is deliberate. A site where a bool must read `""` (the venue `history` modules,
/// the binance-family perp mapper, deribit's exec `event_mapper`) uses [`get_str_boolless`], never
/// this or [`get_str`].
pub fn json_str(v: &Value) -> String {
    match v {
        Value::String(x) => x.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => if *b { "True" } else { "False" }.to_string(),
        _ => String::new(),
    }
}

/// Integer coercion: numbers via `as_i64` (a non-integer number is `None`), strings parsed as
/// `i64`; any other value `None`. [`get_i64`] adds the `0` default.
pub fn json_int(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => s.parse::<i64>().ok(),
        _ => None,
    }
}

// ---- keyed accessors: `.get(key)` + a value-level coercion above + a default ------------------

/// Keyed [`json_str`], `""` when the key is absent.
///
/// CAUTION: this inherits [`json_str`]'s `Bool` arm (`"True"`/`"False"`), so it is NOT a drop-in
/// where a bool must read `""`: that is [`get_str_boolless`].
pub fn get_str(v: &Value, key: &str) -> String {
    v.get(key).map(json_str).unwrap_or_default()
}

/// Keyed [`json_num`], `0.0` when absent or unparseable.
pub fn get_f64(v: &Value, key: &str) -> f64 {
    v.get(key).and_then(json_num).unwrap_or(0.0)
}

/// Keyed [`json_int`], `0` when absent or unparseable.
pub fn get_i64(v: &Value, key: &str) -> i64 {
    v.get(key).and_then(json_int).unwrap_or(0)
}

/// [`get_f64`] without the default, to tell an absent or unparseable field from a real `0.0`.
pub fn get_f64_opt(v: &Value, key: &str) -> Option<f64> {
    v.get(key).and_then(json_num)
}

/// [`get_str`]'s twin WITHOUT the `Bool` arm: a present string passes through, a number
/// stringifies, EVERYTHING else (booleans included) is `""`. For the sites that read their bool
/// fields (`isMaker`…) via `as_bool` and must never see `"True"` out of a string accessor (the
/// venue `history` modules, the binance-family perp mapper, deribit's exec `event_mapper`).
pub fn get_str_boolless(v: &Value, key: &str) -> String {
    match v.get(key) {
        Some(Value::String(x)) => x.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

/// Parse a JSON-ARRAY response body and map every row through `f`: the shared fold of the venue
/// recon-report parsers. Errors: serde's own message for unparseable JSON, `"expected a JSON
/// array"` for a parsed non-array. `f` is infallible per row: a malformed row maps to a default-y
/// report and never aborts the batch.
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
        // Delegating to json_str means a bool stringifies to "True": why a Bool-less site must not
        // route here.
        assert_eq!(get_str(&v, "b"), "True");
        assert_eq!(get_str(&v, "absent"), "");
    }

    #[test]
    fn get_str_boolless_yields_empty_for_a_bool_unlike_get_str() {
        let v = serde_json::json!({ "s": "hi", "n": 3, "b": true, "arr": [1] });
        assert_eq!(get_str_boolless(&v, "s"), "hi");
        assert_eq!(get_str_boolless(&v, "n"), "3");
        // THE divergence from get_str:
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
        // a parsed NON-array keeps the exact error string
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
