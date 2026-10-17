//! Pure decoder fixtures for the cpapi backend — the no-network CI gate. Each asserts a CP Gateway
//! JSON payload decodes to the expected normalized IbInbound(s), carrying cOID in order_ref.
//!
//! NOTE: `IbInbound` has no `Debug` impl (out of this cluster's scope guardrails — the existing
//! `transport::mod` enum is untouched beyond the feature-gated `mod cpapi`/`pub use` lines), so
//! failures here assert with plain messages/lengths rather than `{:?}` dumps.
#![cfg(feature = "ibkr-cpapi")]

use serde_json::json;
use vike_ibkr::transport::IbInbound;
use vike_ibkr::transport::cpapi_decode as d;

#[test]
fn ws_order_status_carries_coid_in_order_ref() {
    // CP Gateway 'sor' order-update frame (order status).
    let frame = json!({
        "topic": "sor",
        "args": [{ "cOID": "ibkr-1", "orderId": 12345, "status": "Submitted", "filledQuantity": 0.0, "avgPrice": 0.0 }]
    });
    let out = d::decode_ws_frame(&frame);
    assert_eq!(out.len(), 1, "expected exactly one decoded inbound");
    match &out[0] {
        IbInbound::OrderStatus(s) => {
            assert_eq!(s.order_ref, "ibkr-1");
            assert_eq!(s.status, "Submitted");
            assert_eq!(s.order_id, 0); // numeric filled by the transport's coid map
        }
        _ => panic!("expected an OrderStatus inbound"),
    }
}

#[test]
fn ws_execution_decodes_exec_details_and_commission() {
    let frame = json!({
        "topic": "sor",
        "args": [{ "cOID": "ibkr-1", "execId": "e1", "symbol": "AAPL", "side": "BUY", "cumFill": 10.0, "price": 190.0, "commission": 1.0, "currency": "USD" }]
    });
    let out = d::decode_ws_frame(&frame);
    assert_eq!(out.len(), 2, "expected ExecDetails + Commission");
    let has_exec = out.iter().any(
        |i| matches!(i, IbInbound::ExecDetails(e) if e.order_ref == "ibkr-1" && e.exec_id == "e1"),
    );
    let has_comm = out
        .iter()
        .any(|i| matches!(i, IbInbound::Commission(c) if c.exec_id == "e1" && (c.commission - 1.0).abs() < 1e-9));
    assert!(has_exec, "missing ExecDetails carrying the coid in order_ref");
    assert!(has_comm, "missing Commission for exec_id e1");
}

#[test]
fn secdef_search_extracts_first_conid() {
    let body = json!([{ "conid": 265598, "symbol": "AAPL", "secType": "STK" }]);
    assert_eq!(d::decode_conid(&body), Some(265598));
    assert_eq!(d::decode_conid(&json!([])), None);
}

/// The coid each `OpenOrder` of `body` carries, in row order.
fn open_order_refs(body: &serde_json::Value) -> Vec<String> {
    d::decode_open_orders(body)
        .into_iter()
        .map(|i| match i {
            IbInbound::OpenOrder { order_id, order_ref } => {
                assert_eq!(order_id, 0); // numeric filled by the coid map
                order_ref
            }
            _ => panic!("expected an OpenOrder inbound"),
        })
        .collect()
}

/// ⚠ FIXTURE PROVENANCE: hand-written, and the field NAME comes from SECONDARY sources, not a capture:
/// IBKR's own pages were unreadable to the research (HTTP 403), and a secondary copy of the Client
/// Portal docs says a `GET /iserver/account/orders` row carries the `cOID` we set at placement in a
/// field named `order_ref`. The row is therefore spelled `order_ref` here, the word the reconcile
/// parser already reads; a live paper check confirming it on this gateway is still owed.
#[test]
fn open_orders_snapshot_decodes_rows() {
    let body =
        json!({ "orders": [{ "order_ref": "ibkr-7", "orderId": 999, "status": "Submitted" }] });
    assert_eq!(open_order_refs(&body), ["ibkr-7"]);
}

/// The fallbacks stay pinned, one test per spelling: `cOID` (the WS-frame spelling) and
/// `local_order_id` (what the live place reply echoed our `cOID` as).
#[test]
fn open_orders_fall_back_to_coid_spelling() {
    let body = json!({ "orders": [{ "cOID": "ibkr-8", "orderId": 1, "status": "Submitted" }] });
    assert_eq!(open_order_refs(&body), ["ibkr-8"]);
}

#[test]
fn open_orders_fall_back_to_local_order_id_spelling() {
    let body = json!({ "orders": [{ "local_order_id": "ibkr-9", "orderId": 1 }] });
    assert_eq!(open_order_refs(&body), ["ibkr-9"]);
}

/// Order of preference is `order_ref`, `cOID`, `local_order_id`, and an EMPTY value is skipped, not
/// taken. Another spelling (`orderRef`) is unconfirmed and is NOT read.
#[test]
fn open_orders_prefer_order_ref_and_skip_empty_values() {
    let body = json!({ "orders": [
        { "order_ref": "a", "cOID": "b", "local_order_id": "c" },
        { "order_ref": "", "cOID": "b", "local_order_id": "c" },
        { "cOID": "", "local_order_id": "c" },
        { "orderRef": "nope" },
    ] });
    assert_eq!(open_order_refs(&body), ["a", "b", "c", ""]);
}

#[test]
fn executions_snapshot_reuses_ws_row_shape() {
    let body = json!([
        { "cOID": "ibkr-2", "execId": "e2", "symbol": "MSFT", "side": "SELL", "cumFill": 3.0, "price": 400.0, "commission": 0.5, "currency": "USD" }
    ]);
    let out = d::decode_executions(&body);
    assert_eq!(out.len(), 2, "expected ExecDetails + Commission");
    assert!(
        out.iter()
            .any(|i| matches!(i, IbInbound::ExecDetails(e) if e.exec_id == "e2" && !e.side_buy))
    );
}

/// A `/trades` row names the coid `order_ref` too (the reconcile parser has always read it so;
/// secondary sources, not a capture). Only the COID reader is aligned: the row's other fields stay
/// the hand-written `execId`/`cumFill` shape (see `decode_ws_frame`).
#[test]
fn executions_snapshot_reads_the_coid_from_order_ref() {
    let body = json!([
        { "order_ref": "ibkr-3", "execId": "e3", "symbol": "MSFT", "side": "BUY", "cumFill": 1.0, "price": 400.0 }
    ]);
    let out = d::decode_executions(&body);
    assert!(
        out.iter().any(|i| matches!(i, IbInbound::ExecDetails(e) if e.order_ref == "ibkr-3")),
        "the execution must carry the coid from `order_ref`"
    );
}
