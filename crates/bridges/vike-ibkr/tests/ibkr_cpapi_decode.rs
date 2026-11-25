//! Pure decoder fixtures for the cpapi backend — the no-network CI gate. Each asserts a CP Gateway
//! JSON payload decodes to the expected normalized IbkrInbound(s), carrying cOID in order_ref.
//!
//! NOTE: `IbkrInbound` has no `Debug` impl (out of this cluster's scope guardrails — the existing
//! `transport::mod` enum is untouched beyond the feature-gated `mod cpapi`/`pub use` lines), so
//! failures here assert with plain messages/lengths rather than `{:?}` dumps.
#![cfg(feature = "ibkr-cpapi")]

use serde_json::json;
use vike_ibkr::transport::IbkrInbound;
use vike_ibkr::transport::cpapi_decode as d;

#[test]
fn websocket_order_status_carries_order_client_id_in_order_ref() {
    // CP Gateway 'sor' order-update frame (order status).
    let frame = json!({
        "topic": "sor",
        "args": [{ "cOID": "ibkr-1", "orderId": 12345, "status": "Submitted", "filledQuantity": 0.0, "avgPrice": 0.0 }]
    });
    let out = d::decode_websocket_frame(&frame);
    assert_eq!(out.len(), 1, "expected exactly one decoded inbound");
    match &out[0] {
        IbkrInbound::OrderStatus(s) => {
            assert_eq!(s.order_ref, "ibkr-1");
            assert_eq!(s.status, "Submitted");
            assert_eq!(s.order_id, 0); // numeric filled by the transport's order client id map
        }
        _ => panic!("expected an OrderStatus inbound"),
    }
}

#[test]
fn websocket_execution_decodes_execution_details_and_commission() {
    let frame = json!({
        "topic": "sor",
        "args": [{ "cOID": "ibkr-1", "execId": "e1", "symbol": "AAPL", "side": "BUY", "cumFill": 10.0, "price": 190.0, "commission": 1.0, "currency": "USD" }]
    });
    let out = d::decode_websocket_frame(&frame);
    assert_eq!(out.len(), 2, "expected ExecDetails + Commission");
    let has_execution = out.iter().any(
        |i| matches!(i, IbkrInbound::ExecutionDetails(e) if e.order_ref == "ibkr-1" && e.exec_id == "e1"),
    );
    let has_comm = out
        .iter()
        .any(|i| matches!(i, IbkrInbound::Commission(c) if c.exec_id == "e1" && (c.commission - 1.0).abs() < 1e-9));
    assert!(has_execution, "missing ExecDetails carrying the order client id in order_ref");
    assert!(has_comm, "missing Commission for exec_id e1");
}

#[test]
fn secdef_search_extracts_first_conid() {
    let body = json!([{ "conid": 265598, "symbol": "AAPL", "secType": "STK" }]);
    assert_eq!(d::decode_conid(&body), Some(265598));
    assert_eq!(d::decode_conid(&json!([])), None);
}

/// The order client id each `OpenOrder` of `body` carries, in row order.
fn open_order_refs(body: &serde_json::Value) -> Vec<String> {
    d::decode_open_orders(body)
        .into_iter()
        .map(|i| match i {
            IbkrInbound::OpenOrder { order_id, order_ref } => {
                assert_eq!(order_id, 0); // numeric filled by the order client id map
                order_ref
            }
            _ => panic!("expected an OpenOrder inbound"),
        })
        .collect()
}

#[test]
fn open_orders_snapshot_decodes_rows() {
    let body =
        json!({ "orders": [{ "order_ref": "ibkr-7", "orderId": 999, "status": "Submitted" }] });
    assert_eq!(open_order_refs(&body), ["ibkr-7"]);
}

/// LIVE-PROVEN shape (paper account, Client Portal gateway, 2026-10-17): the row of an order placed
/// with `"cOID":"n066ref171246a"` names it `order_ref` and has NO `cOID` key. The key set is the
/// captured one; the values besides the order client id, the order id and the description are illustrative.
#[test]
fn open_orders_live_row_binds_the_order_client_id_from_order_ref() {
    let body = json!({ "orders": [{
        "acct": "DUQ186573", "account": "DUQ186573", "conid": 265598, "conidex": "265598",
        "orderId": 1911375756, "order_ref": "n066ref171246a", "ticker": "AAPL", "secType": "STK",
        "listingExchange": "NASDAQ.NMS", "description1": "AAPL", "companyName": "APPLE INC",
        "orderDesc": "Buy 1 AAPL Limit 274.19, GTC", "sizeAndFills": "0/1",
        "remainingQuantity": 1.0, "filledQuantity": 0.0, "side": "BUY", "orderType": "Limit",
        "origOrderType": "LIMIT", "price": 274.19, "timeInForce": "GTC", "outsideRTH": false,
        "cashCcy": "USD", "isEventTrading": "0", "bgColor": "#FFFFFF", "fgColor": "#000000",
        "lastExecutionTime": "261017171246", "lastExecutionTime_r": 1792257166000_i64,
        "order_ccp_status": "PreSubmitted"
    }] });
    assert_eq!(open_order_refs(&body), ["n066ref171246a"]);
}

/// The fallbacks stay pinned, one test per spelling: `cOID` (the WS-frame spelling) and
/// `local_order_id` (what the live place reply echoed our `cOID` as).
#[test]
fn open_orders_fall_back_to_order_client_id_spelling() {
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
fn executions_snapshot_reuses_websocket_row_shape() {
    let body = json!([
        { "cOID": "ibkr-2", "execId": "e2", "symbol": "MSFT", "side": "SELL", "cumFill": 3.0, "price": 400.0, "commission": 0.5, "currency": "USD" }
    ]);
    let out = d::decode_executions(&body);
    assert_eq!(out.len(), 2, "expected ExecDetails + Commission");
    assert!(out.iter().any(
        |i| matches!(i, IbkrInbound::ExecutionDetails(e) if e.exec_id == "e2" && !e.side_buy)
    ));
}

/// A `/trades` row names the order client id `order_ref` too (the reconcile parser has always read it so;
/// secondary sources, not a capture). Only the ORDER CLIENT ID reader is aligned: the row's other fields stay
/// the hand-written `execId`/`cumFill` shape (see `decode_websocket_frame`).
#[test]
fn executions_snapshot_reads_the_order_client_id_from_order_ref() {
    let body = json!([
        { "order_ref": "ibkr-3", "execId": "e3", "symbol": "MSFT", "side": "BUY", "cumFill": 1.0, "price": 400.0 }
    ]);
    let out = d::decode_executions(&body);
    assert!(
        out.iter()
            .any(|i| matches!(i, IbkrInbound::ExecutionDetails(e) if e.order_ref == "ibkr-3")),
        "the execution must carry the order client id from `order_ref`"
    );
}

/// F-7: a `commission` the gateway sent that is not a number is COUNTED (`get_fee_f64`) on the
/// execution decode and on the reconcile `/trades` read, not booked as a silent `0.0`; a quoted
/// number is read on both (the decode read numbers only and booked `"1.05"` as `0.0`). Red before
/// the fix: neither counted, and the decode's quoted figure read zero.
#[test]
fn a_commission_that_is_not_a_number_is_counted_on_the_decode_and_the_reconcile_read() {
    use serde_json::Value;
    use vike_bridge_core::json::malformed_fee_count;
    let decode = |fee: Value| {
        let row = json!([{ "execId": "e4", "symbol": "AAPL", "side": "BUY", "cumFill": 1.0,
                           "price": 190.0, "commission": fee, "currency": "USD" }]);
        let out = d::decode_executions(&row);
        let commission = out.iter().find_map(|i| match i {
            IbkrInbound::Commission(c) => Some(c.commission),
            _ => None,
        });
        commission.expect("a Commission inbound")
    };
    let reconcile = |fee: Value| {
        let row = json!([{ "execution_id": "e4", "conid": 265598, "side": "B", "size": 1,
                           "price": 190.0, "commission": fee, "trade_time_r": 1 }]);
        vike_ibkr::reconcile::cpapi::parse_fill_reports(
            &row.to_string(),
            "DU1234567",
            265598,
            "AAPL",
        )
        .unwrap()[0]
            .commission
    };
    type Read<'a> = &'a dyn Fn(Value) -> f64;
    let legs: [(&str, Read<'_>); 2] = [("decode", &decode), ("reconcile", &reconcile)];
    for (leg, read) in legs {
        let before = malformed_fee_count();
        assert_eq!(read(json!("oops")), 0.0, "{leg}: the fill still books, fee-free");
        assert!(
            malformed_fee_count() > before,
            "{leg}: the unreadable `commission` must be counted"
        );
        assert_eq!(read(json!("1.05")), 1.05, "{leg}: a quoted commission is read");
    }
}
