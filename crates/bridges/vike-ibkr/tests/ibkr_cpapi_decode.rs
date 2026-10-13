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

#[test]
fn open_orders_snapshot_decodes_rows() {
    let body = json!({ "orders": [{ "cOID": "ibkr-7", "orderId": 999, "status": "Submitted" }] });
    let out = d::decode_open_orders(&body);
    assert_eq!(out.len(), 1);
    match &out[0] {
        IbInbound::OpenOrder { order_id, order_ref } => {
            assert_eq!(order_ref, "ibkr-7");
            assert_eq!(*order_id, 0); // numeric filled by the coid map
        }
        _ => panic!("expected an OpenOrder inbound"),
    }
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
