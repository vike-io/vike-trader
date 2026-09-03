//! Polymarket REST history → replay events, for the audit-A3 post-reconnect resync. Mirrors the
//! same pattern as vike-binance's `history.rs` (now its own bridge crate, crate-reorg Phase 3 PR
//! H): `/data/trades` rows reuse the live trade-decode path and `/data/orders` rows the order
//! path, re-keyed CLOB→coid via the shared registry so a replayed fill/cancel folds
//! byte-identically to the WS stream (the core's `trade_id` dedup absorbs the overlap). This is also
//! the reconciliation backstop for the rare MATCHED-then-FAILED divergence the live decoder skips.

use serde_json::Value;
use vike_model::events::Event;

use crate::registry::PolymarketRegistry;
use crate::user_ws::decode_typed;

/// Replay recent Polymarket history (audit A3). `trades` = `GET /data/trades` array; `orders` =
/// `GET /data/orders` array. Emits the same bare-`Fill` + wrap / cancel / terminal shapes as the
/// live [`decode_user`](super::user_ws::decode_user), so the core dedups the overlap.
pub fn map_polymarket_history(
    trades: &Value,
    orders: &Value,
    reg: &PolymarketRegistry,
) -> Vec<Event> {
    let mut out = Vec::new();
    for t in trades.as_array().into_iter().flatten() {
        out.extend(decode_typed("trade", t, reg));
    }
    for o in orders.as_array().into_iter().flatten() {
        out.extend(decode_typed("order", o, reg));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::PolymarketRegistry;
    use vike_model::events::Event;

    #[test]
    fn replays_taker_and_cancel_rekeyed() {
        let reg = PolymarketRegistry::new();
        let _ = reg.on_accept("c-fill", "0xT", 1);
        let _ = reg.on_accept("c-cxl", "0xC", -1);
        let trades = serde_json::json!([{
            "id": "h1", "status": "CONFIRMED", "asset_id": "111",
            "side": "BUY", "size": "10", "price": "0.5",
            "taker_order_id": "0xT", "maker_orders": []
        }]);
        let orders = serde_json::json!([
            { "id": "0xC", "type": "CANCELLATION" }
        ]);
        let evs = map_polymarket_history(&trades, &orders, &reg);
        assert!(evs
            .iter()
            .any(|e| matches!(e, Event::Fill(f) if f.trade_id == "h1:0xT" && f.client_order_id == "c-fill")));
        assert!(evs
            .iter()
            .any(|e| matches!(e, Event::OrderCanceled(c) if c.client_order_id == "c-cxl")));
    }
}
