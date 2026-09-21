//! Pure CP Gateway JSON → normalized `IbInbound`. cOID → order_ref (order_id left 0; the transport
//! fills the numeric from its coid→order_id map so the shared EventMapper's fill_state matches — see
//! the cpapi cluster corrections doc's "coid→num map trick").
//!
//! `conid` secdef-search key LIVE-VERIFIED (2026-07-15): the gateway returns it as a JSON STRING
//! (`decode_conid` accepts string-or-number).
//!
//! ⚠ **`decode_ws_frame` is DEAD CODE on the live WS path, measured 2026-08-23.** It decodes only
//! `topic == "sor"`, and the gateway never sends a `sor` frame — the subscribe answers nothing while
//! its sibling topics answer on the same socket (`transport/cpapi/mod.rs`'s module doc has the
//! measurement). Accept/cancel are unaffected because they are synthesized from the REST acks, but
//! an EXECUTION row can only reach the mapper through this function, so cpapi fills have no live
//! path. The function is NOT dead on the REST path: [`decode_executions`] reuses it by wrapping each
//! `/iserver/account/trades` row in a synthetic `sor` frame, which is how a fill is recovered on a
//! resync — the only way one is recovered at all today.

use serde_json::Value;

use crate::event_mapper::{IbCommissionReport, IbExecDetails, IbOrderStatus};
use crate::transport::IbInbound;

fn s(v: &Value, k: &str) -> String {
    v.get(k).and_then(|x| x.as_str()).unwrap_or_default().to_string()
}
fn f(v: &Value, k: &str) -> f64 {
    v.get(k).and_then(|x| x.as_f64()).unwrap_or(0.0)
}

/// Decode one CP Gateway WS frame (topic "sor") into zero or more IbInbound.
pub fn decode_ws_frame(frame: &Value) -> Vec<IbInbound> {
    let topic = frame.get("topic").and_then(|t| t.as_str()).unwrap_or_default();
    if topic != "sor" {
        return vec![];
    }
    let Some(args) = frame.get("args").and_then(|a| a.as_array()) else {
        return vec![];
    };
    let mut out = vec![];
    for row in args {
        let coid = s(row, "cOID");
        // Execution row (has execId): emit ExecDetails + Commission (join happens in EventMapper).
        if let Some(exec_id) = row.get("execId").and_then(|e| e.as_str()) {
            out.push(IbInbound::ExecDetails(IbExecDetails {
                order_id: 0,
                order_ref: coid.clone(),
                exec_id: exec_id.to_string(),
                symbol: s(row, "symbol"),
                side_buy: s(row, "side").eq_ignore_ascii_case("BUY"),
                shares: f(row, "cumFill"),
                price: f(row, "price"),
                ts: row.get("ts").and_then(|t| t.as_i64()).unwrap_or(0),
            }));
            out.push(IbInbound::Commission(IbCommissionReport {
                exec_id: exec_id.to_string(),
                commission: f(row, "commission"),
                currency: {
                    let c = s(row, "currency");
                    if c.is_empty() { "USD".into() } else { c }
                },
            }));
            continue;
        }
        // Otherwise a status row (has status).
        if row.get("status").is_some() {
            out.push(IbInbound::OrderStatus(IbOrderStatus {
                order_id: 0,
                order_ref: coid,
                status: s(row, "status"),
                filled: f(row, "filledQuantity"),
                avg_fill_price: f(row, "avgPrice"),
            }));
        }
    }
    out
}

/// GET /iserver/account/trades → executions. Same row shape as the WS execution row.
pub fn decode_executions(v: &Value) -> Vec<IbInbound> {
    let rows = v.as_array().cloned().unwrap_or_default();
    let mut out = vec![];
    for row in &rows {
        out.extend(decode_ws_frame(&serde_json::json!({ "topic": "sor", "args": [row] })));
    }
    out
}

/// GET /iserver/account/orders → open-order snapshot rows (reconnect resync).
pub fn decode_open_orders(v: &Value) -> Vec<IbInbound> {
    let rows = v.get("orders").and_then(|o| o.as_array()).cloned().unwrap_or_default();
    rows.iter().map(|row| IbInbound::OpenOrder { order_id: 0, order_ref: s(row, "cOID") }).collect()
}

/// /iserver/secdef/search → first conid. The live gateway returns `conid` as a JSON **string**
/// (e.g. `"conid":"265598"`); accept a number too for robustness. (Live-verified 2026-07-15: a
/// number-only `.as_i64()` returned None → the order was rejected with "conid or conidex is
/// required".)
pub fn decode_conid(v: &Value) -> Option<i64> {
    let c = v.as_array()?.first()?.get("conid")?;
    c.as_i64().or_else(|| c.as_str()?.trim().parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn ws_status_row_decodes() {
        let frame = json!({
            "topic": "sor",
            "args": [{ "cOID": "c-1", "orderId": 5, "status": "Submitted", "filledQuantity": 0.0, "avgPrice": 0.0 }]
        });
        let out = decode_ws_frame(&frame);
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn non_sor_topic_decodes_nothing() {
        assert!(decode_ws_frame(&json!({ "topic": "other", "args": [] })).is_empty());
    }

    #[test]
    fn conid_decodes_from_string_or_number() {
        // The live gateway returns conid as a STRING — the shape that broke placement.
        assert_eq!(decode_conid(&json!([{ "conid": "265598" }])), Some(265598));
        // A numeric conid still decodes (robustness).
        assert_eq!(decode_conid(&json!([{ "conid": 265598 }])), Some(265598));
        // No results → None.
        assert_eq!(decode_conid(&json!([])), None);
    }
}
