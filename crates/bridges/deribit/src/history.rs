//! Deribit WS-RPC order/trade-history → events, for the audit-A3 post-reconnect resync.
//! Mirrors `binance/history.rs`, but Deribit's `get_user_trades` row IS the WS `user.trades` row
//! (identical schema), so fills REUSE `map_deribit_trade` verbatim — a replayed fill is byte-
//! identical to a live one (same SIGNED fee, side, coin-unit qty, per-trade `state`-driven wrap).
//! `label` (our coid) is carried directly on both trade and order rows, so no id-join is needed.
//! Non-fill terminals (orders that were cancelled/rejected during the gap without trading) come
//! from the order-history `order_state`; `filled` order rows are SKIPPED (the fill wrap already
//! terminalizes them).

use serde_json::Value;
// `get_str_boolless as s` — the shared Bool-LESS `str(x)` coercion (a bool yields `""`, never
// Python's `"True"`). Deliberately NOT `get_str`, which carries `json_str`'s `Bool` arm — see
// `get_str_boolless`'s doc.
use vike_bridge_core::json::{get_i64 as i, get_str_boolless as s};
use vike_model::events::{Event, OrderCanceled, OrderRejected};

use crate::event_mapper::map_deribit_trade;

/// Replay recent Deribit history (audit A3). `order_history` = `private/get_order_history_by_instrument`
/// result (a bare array); `user_trades` = the already-unwrapped `.trades` array from
/// `private/get_user_trades_by_instrument`. Emits every trade's fill (via `map_deribit_trade`), then
/// a non-fill terminal for each cancelled/rejected order.
pub fn map_deribit_history(
    order_history: &Value,
    user_trades: &Value,
    venue: &str,
    symbol: &str,
) -> Vec<Event> {
    let mut events = Vec::new();

    // Fills — byte-identical to the live WS fold (map_deribit_trade reads `label`, SIGNED `fee`,
    // `direction`, coin-unit `amount`, and per-trade `state` for the OrderFilled/PartiallyFilled wrap).
    if let Some(trades) = user_trades.as_array() {
        for t in trades {
            events.extend(map_deribit_trade(t, venue, symbol));
        }
    }

    // Non-fill terminals from order history. `filled` rows are already terminalized by their fill
    // wrap; open/untriggered/triggered are still live.
    if let Some(orders) = order_history.as_array() {
        for o in orders {
            let coid = s(o, "label");
            let ts = i(o, "last_update_timestamp");
            match o.get("order_state").and_then(|x| x.as_str()).unwrap_or("") {
                "cancelled" => events.push(Event::OrderCanceled(OrderCanceled {
                    client_order_id: coid,
                    reason: "reconcile: order closed during reconnect gap".to_string().into(),
                    ts,
                })),
                "rejected" => events.push(Event::OrderRejected(OrderRejected {
                    client_order_id: coid,
                    reason: "reconcile: order rejected during reconnect gap".to_string().into(),
                    ts,
                })),
                _ => {}
            }
        }
    }
    events
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(evs: &[Event]) -> Vec<String> {
        evs.iter()
            .map(|e| match e {
                Event::Fill(fill) => format!("Fill:{}:{}", fill.client_order_id, fill.trade_id),
                Event::OrderFilled(w) => format!("OrderFilled:{}", w.client_order_id),
                Event::OrderPartiallyFilled(w) => {
                    format!("OrderPartiallyFilled:{}", w.client_order_id)
                }
                Event::OrderCanceled(w) => format!("OrderCanceled:{}", w.client_order_id),
                Event::OrderRejected(w) => format!("OrderRejected:{}", w.client_order_id),
                other => format!("other:{other:?}"),
            })
            .collect()
    }

    #[test]
    fn replays_fills_then_cancel_and_reject_terminals() {
        // one filled trade (state=filled → OrderFilled wrap) + a cancelled and a rejected order.
        let user_trades = serde_json::json!([
            {"trade_id": "t1", "label": "c_fill", "order_id": "o9", "price": 50000.0, "amount": 10.0,
             "direction": "buy", "fee": -0.001, "liquidity": "M", "timestamp": 5, "state": "filled"}
        ]);
        let order_history = serde_json::json!([
            {"label": "c_fill", "order_id": "o9", "order_state": "filled", "last_update_timestamp": 7},
            {"label": "c_cancel", "order_id": "o10", "order_state": "cancelled", "last_update_timestamp": 8},
            {"label": "c_rej", "order_id": "o11", "order_state": "rejected", "last_update_timestamp": 9},
            {"label": "c_open", "order_id": "o12", "order_state": "open", "last_update_timestamp": 6}
        ]);
        let evs = map_deribit_history(&order_history, &user_trades, "deribit", "BTC-PERPETUAL");
        assert_eq!(
            kinds(&evs),
            vec![
                "Fill:c_fill:t1".to_string(),
                "OrderFilled:c_fill".to_string(),
                // c_fill order row (filled) is skipped — terminalized by the wrap above
                "OrderCanceled:c_cancel".to_string(),
                "OrderRejected:c_rej".to_string(),
                // c_open (open) → no terminal
            ]
        );
        // fee stays SIGNED (a maker rebate is negative — never abs'd)
        if let Event::Fill(fill) = &evs[0] {
            assert!(
                fill.commission < 0.0,
                "maker rebate stays a negative fee: {}",
                fill.commission
            );
        } else {
            panic!("first event must be the bare Fill");
        }
    }
}
