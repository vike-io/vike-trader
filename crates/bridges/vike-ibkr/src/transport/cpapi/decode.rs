//! Pure CP Gateway JSON → normalized `IbkrInbound`. cOID → order_ref (order_id left 0; the transport
//! fills the numeric from its order client id→order_id map so the shared EventMapper's fill_state matches — see
//! the cpapi cluster corrections doc's "order client id→num map trick"). A `GET /iserver/account/orders` row
//! names the order client id `order_ref`, not `cOID` (LIVE-PROVEN on a paper account 2026-10-17: the row has no
//! `cOID` key): [`row_order_client_id`] is the one reader of it. The `/trades` and WS `sor` rows are
//! still UNPROVEN (no capture): they read the same spellings by analogy.
//!
//! `conid` secdef-search key LIVE-VERIFIED (2026-07-15): the gateway returns it as a JSON STRING
//! (`decode_conid` accepts string-or-number).
//!
//! ⚠ **`decode_websocket_frame` is DEAD CODE on the live WS path, measured 2026-08-23.** It decodes only
//! `topic == "sor"`, and the gateway never sends a `sor` frame — the subscribe answers nothing while
//! its sibling topics answer on the same socket (`transport/cpapi/mod.rs`'s module doc has the
//! measurement). Accept/cancel are unaffected because they are synthesized from the REST acks, but
//! an EXECUTION row can only reach the mapper through this function, so cpapi fills have no live
//! path. The function is NOT dead on the REST path: [`decode_executions`] reuses it by wrapping each
//! `/iserver/account/trades` row in a synthetic `sor` frame, which is how a fill is recovered on a
//! resync — the only way one is recovered at all today.

use serde_json::Value;
use vike_bridge_core::json::{get_f64_number_only, get_str_borrowed};

use crate::execution::event_mapper::{IbkrCommissionReport, IbkrExecutionDetails, IbkrOrderStatus};
use crate::transport::IbkrInbound;

/// Decode one CP Gateway WS frame (topic "sor") into zero or more IbkrInbound.
pub fn decode_websocket_frame(frame: &Value) -> Vec<IbkrInbound> {
    let topic = frame.get("topic").and_then(|t| t.as_str()).unwrap_or_default();
    if topic != "sor" {
        return vec![];
    }
    let Some(args) = frame.get("args").and_then(|a| a.as_array()) else {
        return vec![];
    };
    let mut out = vec![];
    for row in args {
        let order_client_id = row_order_client_id(row).unwrap_or_default();
        // Execution row (has execId): emit ExecutionDetails + Commission (join happens in EventMapper).
        if let Some(exec_id) = row.get("execId").and_then(|e| e.as_str()) {
            out.push(IbkrInbound::ExecutionDetails(IbkrExecutionDetails {
                order_id: 0,
                order_ref: order_client_id.clone(),
                exec_id: exec_id.to_string(),
                symbol: get_str_borrowed(row, "symbol").to_string(),
                side_buy: get_str_borrowed(row, "side").eq_ignore_ascii_case("BUY"),
                shares: get_f64_number_only(row, "cumFill"),
                price: get_f64_number_only(row, "price"),
                ts: row.get("ts").and_then(|t| t.as_i64()).unwrap_or(0),
                ..Default::default()
            }));
            out.push(IbkrInbound::Commission(IbkrCommissionReport {
                exec_id: exec_id.to_string(),
                commission: vike_bridge_core::json::get_fee_f64(row, "commission", "ibkr"),
                currency: {
                    let c = get_str_borrowed(row, "currency").to_string();
                    if c.is_empty() { "USD".into() } else { c }
                },
            }));
            continue;
        }
        // Otherwise a status row (has status).
        if row.get("status").is_some() {
            out.push(IbkrInbound::OrderStatus(IbkrOrderStatus {
                order_id: 0,
                order_ref: order_client_id,
                status: get_str_borrowed(row, "status").to_string(),
                filled: get_f64_number_only(row, "filledQuantity"),
                average_fill_price: get_f64_number_only(row, "avgPrice"),
                client_id: 0,
            }));
        }
    }
    out
}

/// GET /iserver/account/trades → executions. Same row shape as the WS execution row.
pub fn decode_executions(v: &Value) -> Vec<IbkrInbound> {
    let rows = v.as_array().cloned().unwrap_or_default();
    let mut out = vec![];
    for row in &rows {
        out.extend(decode_websocket_frame(&serde_json::json!({ "topic": "sor", "args": [row] })));
    }
    out
}

/// The order client id (the order client id we set as `cOID` at placement) of one cpapi row (an orders-list
/// row, a `/trades` row or a WS `sor` row): the first NON-EMPTY of `order_ref`, `cOID`,
/// `local_order_id`. ONE reader for [`decode_open_orders`], [`decode_websocket_frame`] (so the executions
/// too) and the reconcile parser (`reconcile::cpapi`).
///
/// ⚠ Only `order_ref` on a `GET /iserver/account/orders` row is a capture (paper, 2026-10-17: the
/// live row carries the `cOID` we sent as `order_ref` and has no `cOID` key). `cOID` is the WS-frame
/// spelling (unproven, kept as the fallback); `local_order_id` is what the live place reply echoed
/// our `cOID` as. `orderRef` is an unconfirmed spelling and is NOT read.
/// `None` = no row field named one (an order placed by hand, or a spelling not listed here).
pub fn row_order_client_id(row: &Value) -> Option<String> {
    ["order_ref", "cOID", "local_order_id"].iter().find_map(|k| {
        row.get(*k).and_then(Value::as_str).filter(|v| !v.is_empty()).map(str::to_string)
    })
}

/// GET /iserver/account/orders → open-order snapshot rows (reconnect resync).
pub fn decode_open_orders(v: &Value) -> Vec<IbkrInbound> {
    let rows = v.get("orders").and_then(|o| o.as_array()).cloned().unwrap_or_default();
    rows.iter()
        .map(|row| IbkrInbound::OpenOrder {
            order_id: 0,
            order_ref: row_order_client_id(row).unwrap_or_default(),
        })
        .collect()
}

/// GET /iserver/account/orders → `(order client id, the Client Portal's own order id)` of every row that names both: the
/// id a cancel and an amend address (`DELETE /iserver/account/{acct}/order/{orderId}`). Our allocated numeric is
/// NOT that id (live-verified 2026-07-15: "OrderID 101 doesn't exist"). The row's `orderId` is in the live
/// capture of 2026-10-17 (a JSON number there, `tests/ibkr_cpapi_decode.rs`'s
/// `open_orders_live_row_binds_the_order_client_id_from_order_ref`), read as the reconcile reads it (a number or a
/// string); a row without an order client id or an id is left out.
pub fn gateway_order_ids(v: &Value) -> Vec<(String, String)> {
    let rows = v.get("orders").and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default();
    rows.iter()
        .filter_map(|row| {
            let id = vike_bridge_core::json::get_str_boolless(row, "orderId");
            Some((row_order_client_id(row)?, id)).filter(|(_, id)| !id.is_empty())
        })
        .collect()
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
    fn websocket_status_row_decodes() {
        let frame = json!({
            "topic": "sor",
            "args": [{ "cOID": "c-1", "orderId": 5, "status": "Submitted", "filledQuantity": 0.0, "avgPrice": 0.0 }]
        });
        let out = decode_websocket_frame(&frame);
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn non_sor_topic_decodes_nothing() {
        assert!(decode_websocket_frame(&json!({ "topic": "other", "args": [] })).is_empty());
    }

    /// The gateway's own id of each listed order of ours, a number (the live row) or a string; a row with no order client id
    /// or no id names nothing.
    #[test]
    fn gateway_order_ids_pair_the_order_client_id_with_the_rows_order_id() {
        let body = json!({ "orders": [
            { "order_ref": "c-1", "orderId": 1911375756 },
            { "order_ref": "c-2", "orderId": "42" },
            { "order_ref": "c-3" },
            { "orderId": 7 },
        ] });
        assert_eq!(
            gateway_order_ids(&body),
            [("c-1".to_string(), "1911375756".to_string()), ("c-2".to_string(), "42".to_string())]
        );
        assert!(gateway_order_ids(&json!({})).is_empty());
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

    /// A frame's text field is a JSON string only (`""` otherwise) and its number field a JSON NUMBER
    /// only (a quoted number reads `0.0`); a currency that reads `""` is USD. Pinned on the edge inputs
    /// before the two reads moved onto `vike_bridge_core::json`.
    #[test]
    fn a_text_field_reads_a_string_only_and_a_number_field_a_json_number_only() {
        let execution =
            |row: Value| match decode_websocket_frame(&json!({ "topic": "sor", "args": [row] }))
                .as_slice()
            {
                [IbkrInbound::ExecutionDetails(d), IbkrInbound::Commission(c)] => {
                    (d.symbol.clone(), d.side_buy, d.shares, d.price, c.currency.clone())
                }
                _ => panic!("an execution row is its details and its commission"),
            };
        assert_eq!(
            execution(
                json!({ "execId": "E1", "symbol": "AAPL", "side": "sell", "cumFill": 5, "price": 101.5, "currency": "EUR" })
            ),
            ("AAPL".to_string(), false, 5.0, 101.5, "EUR".to_string())
        );
        assert_eq!(
            execution(
                json!({ "execId": "E2", "symbol": 7, "side": "buy", "cumFill": "5", "price": null, "currency": true })
            ),
            (String::new(), true, 0.0, 0.0, "USD".to_string())
        );
        assert_eq!(
            execution(json!({ "execId": "E3", "side": true, "currency": " " })),
            (String::new(), false, 0.0, 0.0, " ".to_string())
        );
        let status =
            |row: Value| match decode_websocket_frame(&json!({ "topic": "sor", "args": [row] }))
                .as_slice()
            {
                [IbkrInbound::OrderStatus(s)] => (s.status.clone(), s.filled, s.average_fill_price),
                _ => panic!("a status row is one status"),
            };
        assert_eq!(
            status(json!({ "status": "Filled", "filledQuantity": 2, "avgPrice": 99.5 })),
            ("Filled".to_string(), 2.0, 99.5)
        );
        assert_eq!(
            status(json!({ "status": 3, "filledQuantity": "2", "avgPrice": [1] })),
            (String::new(), 0.0, 0.0)
        );
    }
}
