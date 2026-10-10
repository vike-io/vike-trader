//! OANDA transactions-since → events, for the audit-A3 post-reconnect resync.
//!
//! On a transactions-stream reconnect a terminal (fill/cancel) that landed during the
//! reconnect window is lost — the stream carries only transactions from connect time, and
//! does not replay. OANDA offers an EXACT recovery the crypto venues lack:
//! `GET /v3/accounts/{id}/transactions/sinceid?id={lastSeen}` returns precisely the
//! transactions after `lastSeen`. This maps that response's transactions through the SAME
//! [`decode_transaction_events`](crate::stream::decode_transaction_events) used live, so a
//! replayed fill folds byte-identically to a live one; the core's `trade_id`/FSM dedup
//! absorbs any overlap with the freshly-reopened stream.

use vike_model::events::Event;

use crate::stream::decode_transaction_events;

/// Map an OANDA `/transactions/sinceid` (or `/transactions/idrange`) response body to the vike
/// events it implies. `resp.transactions` is an array of Transaction objects with the SAME shape
/// as the stream lines, so each maps via `decode_transaction_events` (ORDER_FILL → bare Fill +
/// wrap, ORDER_CANCEL → cancel, others → nothing).
pub fn map_transactions_since(resp: &serde_json::Value) -> Vec<Event> {
    resp.get("transactions")
        .and_then(|t| t.as_array())
        .map(|arr| arr.iter().flat_map(decode_transaction_events).collect())
        .unwrap_or_default()
}

/// The highest transaction id in a sinceid response — the new `lastSeen` watermark after a
/// backfill. Prefers the response's `lastTransactionID`; falls back to the max `id` across the
/// returned transactions. `None` when neither is present/parseable (empty backfill).
pub fn max_transaction_id(resp: &serde_json::Value) -> Option<u64> {
    if let Some(last) =
        resp.get("lastTransactionID").and_then(|v| v.as_str()).and_then(|s| s.parse().ok())
    {
        return Some(last);
    }
    resp.get("transactions").and_then(|t| t.as_array()).and_then(|arr| {
        arr.iter()
            .filter_map(|t| {
                t.get("id").and_then(|v| v.as_str()).and_then(|s| s.parse::<u64>().ok())
            })
            .max()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(evs: &[Event]) -> Vec<String> {
        evs.iter()
            .map(|e| match e {
                Event::Fill(f) => format!("Fill:{}:{}", f.client_order_id, f.trade_id),
                Event::OrderFilled(w) => format!("OrderFilled:{}", w.client_order_id),
                Event::OrderCanceled(w) => format!("OrderCanceled:{}", w.client_order_id),
                other => format!("other:{other:?}"),
            })
            .collect()
    }

    #[test]
    fn replays_gap_fill_and_cancel_as_dual_publish_and_skips_irrelevant() {
        let resp = serde_json::json!({
            "transactions": [
                {"type": "ORDER_FILL", "id": "101", "time": "2.0", "orderID": "50",
                 "instrument": "EUR_USD", "units": "1000", "price": "1.1", "commission": "0.02",
                 "clientExtensions": {"id": "c_fill"}},
                {"type": "HEARTBEAT", "id": "102", "time": "3.0"},
                {"type": "ORDER_CANCEL", "id": "103", "time": "4.0", "orderID": "60",
                 "reason": "CLIENT_REQUEST", "clientExtensions": {"id": "c_cancel"}}
            ],
            "lastTransactionID": "103"
        });
        // The fill dual-publishes (bare Fill + wrap); the cancel is one event; HEARTBEAT drops.
        assert_eq!(
            kinds(&map_transactions_since(&resp)),
            vec![
                "Fill:c_fill:101".to_string(),
                "OrderFilled:c_fill".to_string(),
                "OrderCanceled:c_cancel".to_string(),
            ]
        );
        assert_eq!(max_transaction_id(&resp), Some(103));
    }

    #[test]
    fn empty_backfill_yields_nothing() {
        let resp = serde_json::json!({ "transactions": [], "lastTransactionID": "7" });
        assert!(map_transactions_since(&resp).is_empty());
        assert_eq!(max_transaction_id(&resp), Some(7));
        // no lastTransactionID, no transactions → None
        assert_eq!(max_transaction_id(&serde_json::json!({})), None);
    }
}
