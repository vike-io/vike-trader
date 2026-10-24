//! Binance spot + perp REST order-history → events, for the audit-A3 post-reconnect resync: the
//! venue face of the shared [`crate::family::history`].
//!
//! Aster's order/trade JSON is a Binance fork (identical field names/shapes), so the replay logic
//! itself lives once in `family` and both venues re-export it under their own names (F0, dedup
//! rung 1). The contract and the public paths here are unchanged — see the family module for the
//! reconnect-gap/dedup/dual-publish notes. The tests below stay HERE, exercising the shared code
//! through Binance's own wrapper: Aster's twin does the same through its own, so the one shared
//! implementation is proven twice.

pub use crate::family::history::{
    map_history as map_binance_history, map_perp_history as map_binance_perp_history,
};

#[cfg(test)]
mod tests {
    use super::*;
    use vike_model::events::Event;

    fn kinds(evs: &[Event]) -> Vec<String> {
        evs.iter()
            .map(|e| match e {
                Event::Fill(fill) => format!("Fill:{}:{}", fill.client_order_id, fill.trade_id),
                Event::OrderFilled(w) => format!("OrderFilled:{}", w.client_order_id),
                Event::OrderPartiallyFilled(w) => {
                    format!("OrderPartiallyFilled:{}", w.client_order_id)
                }
                Event::OrderCanceled(w) => format!("OrderCanceled:{}", w.client_order_id),
                Event::OrderExpired(w) => format!("OrderExpired:{}", w.client_order_id),
                Event::OrderRejected(w) => format!("OrderRejected:{}", w.client_order_id),
                other => format!("other:{other:?}"),
            })
            .collect()
    }

    #[test]
    fn replays_gap_fill_cancel_expire_and_skips_open() {
        let all_orders = serde_json::json!([
            {"orderId": 9,  "clientOrderId": "c_fill",   "status": "FILLED",   "origQty": "1.0", "updateTime": 7},
            {"orderId": 10, "clientOrderId": "c_cancel", "status": "CANCELED", "origQty": "2.0", "updateTime": 8},
            {"orderId": 11, "clientOrderId": "c_open",   "status": "NEW",      "origQty": "1.0", "updateTime": 6},
            {"orderId": 12, "clientOrderId": "c_expire", "status": "EXPIRED",  "origQty": "1.0", "updateTime": 9}
        ]);
        let my_trades = serde_json::json!([
            {"id": 100, "orderId": 9, "price": "50000", "qty": "1.0", "commission": "0.1", "time": 5, "isBuyer": true, "isMaker": false}
        ]);
        let evs = map_binance_history(&all_orders, &my_trades, "binance", "BTCUSDT");
        // order 9 (FILLED, one trade) -> Fill + OrderFilled; order 10 -> OrderCanceled;
        // order 11 (NEW) -> nothing; order 12 -> OrderExpired.
        assert_eq!(
            kinds(&evs),
            vec![
                "Fill:c_fill:100".to_string(),
                "OrderFilled:c_fill".to_string(),
                "OrderCanceled:c_cancel".to_string(),
                "OrderExpired:c_expire".to_string(),
            ]
        );
        // the fill carries the correct economics
        if let Event::Fill(fill) = &evs[0] {
            assert_eq!(fill.last_qty, 1.0);
            assert_eq!(fill.last_px, 50000.0);
            assert_eq!(fill.side, 1);
            assert_eq!(fill.symbol, "BTCUSDT");
        } else {
            panic!("first event must be the bare Fill");
        }
    }

    #[test]
    fn history_fill_surfaces_the_commission_asset() {
        // REST myTrades carries `commissionAsset` alongside `commission`.
        let all_orders = serde_json::json!([
            {"orderId": 9, "clientOrderId": "c_fill", "status": "FILLED", "origQty": "1.0", "updateTime": 7}
        ]);
        let my_trades = serde_json::json!([
            {"id": 100, "orderId": 9, "price": "50000", "qty": "1.0", "commission": "0.1", "commissionAsset": "BNB", "time": 5, "isBuyer": true, "isMaker": false}
        ]);
        let evs = map_binance_history(&all_orders, &my_trades, "binance", "BTCUSDT");
        if let Event::Fill(fill) = &evs[0] {
            assert_eq!(fill.commission, 0.1);
            assert_eq!(fill.commission_asset, "BNB");
        } else {
            panic!("first event must be the bare Fill");
        }
    }

    #[test]
    fn partial_fills_then_cancel_wraps_as_partially_filled() {
        // an order that took two partial fills then canceled the remainder
        let all_orders = serde_json::json!([
            {"orderId": 20, "clientOrderId": "c_pc", "status": "CANCELED", "origQty": "3.0", "updateTime": 9}
        ]);
        let my_trades = serde_json::json!([
            {"id": 201, "orderId": 20, "price": "100", "qty": "1.0", "commission": "0", "time": 5, "isBuyer": true, "isMaker": true},
            {"id": 202, "orderId": 20, "price": "101", "qty": "1.0", "commission": "0", "time": 6, "isBuyer": true, "isMaker": true}
        ]);
        let evs = map_binance_history(&all_orders, &my_trades, "binance", "BTCUSDT");
        assert_eq!(
            kinds(&evs),
            vec![
                "Fill:c_pc:201".to_string(),
                "OrderPartiallyFilled:c_pc".to_string(),
                "Fill:c_pc:202".to_string(),
                "OrderPartiallyFilled:c_pc".to_string(),
                "OrderCanceled:c_pc".to_string(),
            ]
        );
    }
}

#[cfg(test)]
mod perp_tests {
    use super::*;
    use vike_model::events::{Event, LiquiditySide};

    fn kinds(evs: &[Event]) -> Vec<String> {
        evs.iter()
            .map(|e| match e {
                Event::Fill(fill) => format!(
                    "Fill:{}:{}:{}",
                    fill.client_order_id, fill.trade_id, fill.position_side
                ),
                Event::OrderFilled(w) => format!("OrderFilled:{}", w.client_order_id),
                Event::OrderPartiallyFilled(w) => {
                    format!("OrderPartiallyFilled:{}", w.client_order_id)
                }
                Event::OrderCanceled(w) => format!("OrderCanceled:{}", w.client_order_id),
                Event::OrderExpired(w) => format!("OrderExpired:{}", w.client_order_id),
                other => format!("other:{other:?}"),
            })
            .collect()
    }

    #[test]
    fn perp_replays_fill_with_position_side_and_cancel() {
        let all_orders = serde_json::json!([
            {"orderId": 9, "clientOrderId": "c_fill", "status": "FILLED", "origQty": "1.0", "updateTime": 7},
            {"orderId": 10, "clientOrderId": "c_cancel", "status": "CANCELED", "origQty": "2.0", "updateTime": 8}
        ]);
        // fapi userTrades: side=SELL, maker=true, positionSide=SHORT (hedge mode)
        let user_trades = serde_json::json!([
            {"id": 100, "orderId": 9, "price": "60000", "qty": "1.0", "commission": "0.2", "maker": true, "time": 5, "side": "SELL", "positionSide": "SHORT"}
        ]);
        let evs = map_binance_perp_history(&all_orders, &user_trades, "binance", "BTCUSDT");
        assert_eq!(
            kinds(&evs),
            vec![
                "Fill:c_fill:100:SHORT".to_string(),
                "OrderFilled:c_fill".to_string(),
                "OrderCanceled:c_cancel".to_string(),
            ]
        );
        if let Event::Fill(fill) = &evs[0] {
            assert_eq!(fill.side, -1, "SELL -> -1");
            assert_eq!(fill.last_qty, 1.0);
            assert_eq!(fill.liquidity_side, LiquiditySide::Maker);
        } else {
            panic!("first event must be the bare Fill");
        }
    }
}
