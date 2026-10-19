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
use crate::symbols::SubmittedSymbols;

/// Map an OANDA `/transactions/sinceid` (or `/transactions/idrange`) response body to the vike
/// events it implies. `resp.transactions` is an array of Transaction objects with the SAME shape
/// as the stream lines, so each maps via `decode_transaction_events` (ORDER_FILL → bare Fill +
/// wrap, ORDER_CANCEL → cancel, others → nothing).
pub fn map_transactions_since(resp: &serde_json::Value, symbols: &SubmittedSymbols) -> Vec<Event> {
    resp.get("transactions")
        .and_then(|t| t.as_array())
        .map(|arr| arr.iter().flat_map(|t| decode_transaction_events(t, symbols)).collect())
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
            kinds(&map_transactions_since(&resp, &SubmittedSymbols::default())),
            vec![
                "Fill:c_fill:101".to_string(),
                "OrderFilled:c_fill".to_string(),
                "OrderCanceled:c_cancel".to_string(),
            ]
        );
        assert_eq!(max_transaction_id(&resp), Some(103));
    }

    /// The backfill goes through the same decode, so a replayed fill of an order this process submitted
    /// carries the submitted symbol too.
    #[test]
    fn replays_a_fill_under_the_symbol_its_order_was_submitted_with() {
        let resp = serde_json::json!({"transactions": [
            {"type": "ORDER_FILL", "id": "101", "time": "2.0", "instrument": "EUR_USD",
             "units": "1000", "price": "1.1", "clientExtensions": {"id": "c1"}}]});
        let symbols = SubmittedSymbols::default();
        symbols.record("c1", "EURUSD");
        match map_transactions_since(&resp, &symbols).first() {
            Some(Event::Fill(f)) => assert_eq!(f.symbol, "EURUSD"),
            other => panic!("expected a bare Fill first, got {other:?}"),
        }
    }

    /// A reconnect that replays the cancel half of an order REPLACE must not terminate the order,
    /// exactly as the live line does not: the replayed pair is the replaced order's cancel and
    /// the replacement's create, neither of which is an event here.
    #[test]
    fn replays_a_replace_pair_as_nothing() {
        let resp = serde_json::json!({
            "transactions": [
                {"type": "ORDER_CANCEL", "id": "201", "time": "4.0", "orderID": "60",
                 "clientOrderID": "c1", "reason": "CLIENT_REQUEST_REPLACED",
                 "replacedByOrderID": "61"},
                {"type": "LIMIT_ORDER", "id": "202", "time": "4.0", "replacesOrderID": "60",
                 "clientExtensions": {"id": "c1"}}
            ],
            "lastTransactionID": "202"
        });
        assert!(map_transactions_since(&resp, &SubmittedSymbols::default()).is_empty());
        assert_eq!(max_transaction_id(&resp), Some(202));
    }

    #[test]
    fn empty_backfill_yields_nothing() {
        let resp = serde_json::json!({ "transactions": [], "lastTransactionID": "7" });
        assert!(map_transactions_since(&resp, &SubmittedSymbols::default()).is_empty());
        assert_eq!(max_transaction_id(&resp), Some(7));
        // no lastTransactionID, no transactions → None
        assert_eq!(max_transaction_id(&serde_json::json!({})), None);
    }
}
