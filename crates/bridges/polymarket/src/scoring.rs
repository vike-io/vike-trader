//! `scoring` — a thin, READ-ONLY client for Polymarket's CLOB **order-scoring** endpoints, over the
//! crate's existing L2-authed GET ([`crate::exec::get_signed`]) — no new HTTP/WS/TLS stack. Answers
//! the one question rewards-aware quoting needs at runtime: *is this resting order currently earning
//! liquidity rewards?* (i.e. is it inside `max_spread`, at least `min_size`, and older than `moas`?).
//! The reward PARAMETERS are parsed by [`crate::rewards`]; this is the live per-order status check.
//!
//! Two endpoints, both **L2-authed** (401 unauthenticated), both plain GETs:
//! - `GET /order-scoring?order_id=<hash>` → `{"scoring": true}` — one order.
//! - `GET /orders-scoring?order_ids=<h>&order_ids=<h>` → a MAP `{ "<hash>": bool }` — a batch. Note
//!   the batch takes **repeated query params**, NOT a JSON body.
//!
//! Order ids are `0x`-prefixed hex hashes, so the query strings are built verbatim (the same raw
//! `key=value` shape [`crate::exec::get_trades`]/`get_orders` use); there is nothing to
//! percent-encode. `get_signed` signs the request PATH with an empty body (the CLOB L2 scheme does
//! not sign the query string), exactly like the existing history reads.
//!
//! OFF/additive: nothing in this crate calls these by default — they are a new surface the vike-mm
//! rewards-term lane / mount opts into. A build that never calls them is byte-identical.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::config::PolymarketCreds;

/// CLOB path for the single-order scoring query.
pub const ORDER_SCORING_PATH: &str = "/order-scoring";
/// CLOB path for the batch scoring query (repeated `order_ids=` params).
pub const ORDERS_SCORING_PATH: &str = "/orders-scoring";

/// Build the `/order-scoring` query string: `order_id=<hash>`. Order ids are hex hashes — built
/// verbatim, nothing to encode.
pub fn order_scoring_query(order_id: &str) -> String {
    format!("order_id={order_id}")
}

/// Build the `/orders-scoring` query string with **repeated** `order_ids=` params (NOT a JSON body):
/// `order_ids=<h1>&order_ids=<h2>&…`. An empty slice yields an empty query string (the caller should
/// skip the request rather than fetch every order).
pub fn orders_scoring_query(order_ids: &[&str]) -> String {
    order_ids.iter().map(|id| format!("order_ids={id}")).collect::<Vec<_>>().join("&")
}

/// Parse the `/order-scoring` response `{"scoring": <bool>}` → the bool. `None` when the field is
/// absent or not a bool (the caller must not read that as `false` — the request may have failed
/// shape).
pub fn parse_order_scoring(v: &Value) -> Option<bool> {
    v.get("scoring").and_then(|s| s.as_bool())
}

/// Parse the `/orders-scoring` response MAP `{ "<order_id>": <bool> }` → a [`BTreeMap`] (sorted,
/// deterministic). Entries whose value is not a bool are skipped; a non-object body yields an empty
/// map. An order id absent from the returned map was not scored / not found — the caller decides how
/// to treat a missing key (this parser does not synthesize `false`).
pub fn parse_orders_scoring(v: &Value) -> BTreeMap<String, bool> {
    let mut out = BTreeMap::new();
    if let Some(obj) = v.as_object() {
        for (k, val) in obj {
            if let Some(b) = val.as_bool() {
                out.insert(k.clone(), b);
            }
        }
    }
    out
}

/// `GET /order-scoring?order_id=…` (L2-authed) → whether the order is currently scoring rewards.
/// `Err` on a network / auth / decode failure OR a response missing a bool `scoring` field.
pub fn fetch_order_scoring(
    base: &str,
    creds: &PolymarketCreds,
    order_id: &str,
) -> Result<bool, String> {
    let v =
        crate::exec::get_signed(base, ORDER_SCORING_PATH, &order_scoring_query(order_id), creds)?;
    parse_order_scoring(&v)
        .ok_or_else(|| format!("order-scoring: response missing a bool `scoring` field: {v}"))
}

/// `GET /orders-scoring?order_ids=…&order_ids=…` (L2-authed) → the `{ order_id: scoring }` map.
/// An empty `order_ids` returns an empty map WITHOUT a request. `Err` on a network / auth / decode
/// failure; a well-formed-but-empty response is `Ok` with an empty map.
pub fn fetch_orders_scoring(
    base: &str,
    creds: &PolymarketCreds,
    order_ids: &[&str],
) -> Result<BTreeMap<String, bool>, String> {
    if order_ids.is_empty() {
        return Ok(BTreeMap::new());
    }
    let v = crate::exec::get_signed(
        base,
        ORDERS_SCORING_PATH,
        &orders_scoring_query(order_ids),
        creds,
    )?;
    Ok(parse_orders_scoring(&v))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- query builders ----------------------------------------------------------------------

    #[test]
    fn order_scoring_query_is_a_single_param() {
        assert_eq!(order_scoring_query("0xabc123"), "order_id=0xabc123");
    }

    #[test]
    fn orders_scoring_query_repeats_the_param_not_a_json_body() {
        // the load-bearing detail: repeated `order_ids=`, NOT a comma list or a JSON array
        assert_eq!(
            orders_scoring_query(&["0xa", "0xb", "0xc"]),
            "order_ids=0xa&order_ids=0xb&order_ids=0xc"
        );
        assert_eq!(orders_scoring_query(&["0xonly"]), "order_ids=0xonly");
        // empty slice → empty string (caller skips the request)
        assert_eq!(orders_scoring_query(&[]), "");
    }

    // ---- response parsing --------------------------------------------------------------------

    #[test]
    fn parse_order_scoring_reads_the_bool() {
        assert_eq!(parse_order_scoring(&serde_json::json!({ "scoring": true })), Some(true));
        assert_eq!(parse_order_scoring(&serde_json::json!({ "scoring": false })), Some(false));
        // absent / non-bool → None (never a defaulted false)
        assert_eq!(parse_order_scoring(&serde_json::json!({})), None);
        assert_eq!(parse_order_scoring(&serde_json::json!({ "scoring": "yes" })), None);
        assert_eq!(parse_order_scoring(&serde_json::json!({ "scoring": 1 })), None);
    }

    #[test]
    fn parse_orders_scoring_reads_the_map() {
        let v = serde_json::json!({ "0xabc": true, "0xdef": false });
        let m = parse_orders_scoring(&v);
        assert_eq!(m.len(), 2);
        assert_eq!(m.get("0xabc"), Some(&true));
        assert_eq!(m.get("0xdef"), Some(&false));
    }

    #[test]
    fn parse_orders_scoring_skips_non_bool_values_and_non_objects() {
        // a stray non-bool entry is dropped, not coerced
        let v = serde_json::json!({ "0xabc": true, "weird": 5, "0xstr": "no" });
        let m = parse_orders_scoring(&v);
        assert_eq!(m.len(), 1);
        assert_eq!(m.get("0xabc"), Some(&true));
        // a non-object body (array / string / null) yields an empty map, never a panic
        assert!(parse_orders_scoring(&serde_json::json!([])).is_empty());
        assert!(parse_orders_scoring(&serde_json::json!("x")).is_empty());
        assert!(parse_orders_scoring(&serde_json::json!(null)).is_empty());
    }
}
