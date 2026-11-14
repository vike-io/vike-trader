//! Binance spot + perp REST order-history → events, for the audit-A3 post-reconnect resync: the
//! venue face of the shared [`crate::family::execution::resync`].
//!
//! Aster's order/trade JSON is a Binance fork (identical field names/shapes), so the replay logic
//! itself lives once in `family` and both venues re-export it under their own names (F0, dedup
//! rung 1). The contract and the public paths here are unchanged — see the family module for the
//! reconnect-gap/dedup/dual-publish notes. The tests below stay HERE, exercising the shared code
//! through Binance's own wrapper: Aster's twin does the same through its own, so the one shared
//! implementation is proven twice.

pub use crate::family::execution::resync::{
    map_history as map_binance_history, map_perp_history as map_binance_perp_history,
};

#[cfg(test)]
mod tests {
    use super::*;
    use vike_bridge_core::execution_drive::event_tags;
    use vike_model::events::Event;

    #[test]
    fn replays_gap_fill_cancel_expire_and_skips_open() {
        let all_orders = serde_json::json!([
            {"orderId": 9,  "clientOrderId": "cfill",   "status": "FILLED",   "origQty": "1.0", "updateTime": 7},
            {"orderId": 10, "clientOrderId": "ccancel", "status": "CANCELED", "origQty": "2.0", "updateTime": 8},
            {"orderId": 11, "clientOrderId": "copen",   "status": "NEW",      "origQty": "1.0", "updateTime": 6},
            {"orderId": 12, "clientOrderId": "cexpire", "status": "EXPIRED",  "origQty": "1.0", "updateTime": 9}
        ]);
        let my_trades = serde_json::json!([
            {"id": 100, "orderId": 9, "price": "50000", "qty": "1.0", "commission": "0.1", "time": 5, "isBuyer": true, "isMaker": false}
        ]);
        let evs = map_binance_history(&all_orders, &my_trades, "binance", "BTCUSDT", None);
        // order 9 (FILLED, one trade) -> Fill + OrderFilled; order 10 -> OrderCanceled;
        // order 11 (NEW) -> nothing; order 12 -> OrderExpired.
        assert_eq!(
            event_tags(&evs),
            vec![
                "Fill:cfill:100".to_string(),
                "OrderFilled:cfill".to_string(),
                "OrderCanceled:ccancel".to_string(),
                "OrderExpired:cexpire".to_string(),
            ]
        );
        // the fill carries the correct economics
        if let Event::Fill(fill) = &evs[0] {
            assert_eq!(fill.last_qty, 1.0);
            assert_eq!(fill.last_price, 50000.0);
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
            {"orderId": 9, "clientOrderId": "cfill", "status": "FILLED", "origQty": "1.0", "updateTime": 7}
        ]);
        let my_trades = serde_json::json!([
            {"id": 100, "orderId": 9, "price": "50000", "qty": "1.0", "commission": "0.1", "commissionAsset": "BNB", "time": 5, "isBuyer": true, "isMaker": false}
        ]);
        let evs = map_binance_history(&all_orders, &my_trades, "binance", "BTCUSDT", None);
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
            {"orderId": 20, "clientOrderId": "cpc", "status": "CANCELED", "origQty": "3.0", "updateTime": 9}
        ]);
        let my_trades = serde_json::json!([
            {"id": 201, "orderId": 20, "price": "100", "qty": "1.0", "commission": "0", "time": 5, "isBuyer": true, "isMaker": true},
            {"id": 202, "orderId": 20, "price": "101", "qty": "1.0", "commission": "0", "time": 6, "isBuyer": true, "isMaker": true}
        ]);
        let evs = map_binance_history(&all_orders, &my_trades, "binance", "BTCUSDT", None);
        assert_eq!(
            event_tags(&evs),
            vec![
                "Fill:cpc:201".to_string(),
                "OrderPartiallyFilled:cpc".to_string(),
                "Fill:cpc:202".to_string(),
                "OrderPartiallyFilled:cpc".to_string(),
                "OrderCanceled:cpc".to_string(),
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
                    fill.order_client_id, fill.trade_id, fill.position_side
                ),
                Event::OrderFilled(w) => format!("OrderFilled:{}", w.order_client_id),
                Event::OrderPartiallyFilled(w) => {
                    format!("OrderPartiallyFilled:{}", w.order_client_id)
                }
                Event::OrderCanceled(w) => format!("OrderCanceled:{}", w.order_client_id),
                Event::OrderExpired(w) => format!("OrderExpired:{}", w.order_client_id),
                other => format!("other:{other:?}"),
            })
            .collect()
    }

    #[test]
    fn perp_replays_fill_with_position_side_and_cancel() {
        let all_orders = serde_json::json!([
            {"orderId": 9, "clientOrderId": "cfill", "status": "FILLED", "origQty": "1.0", "updateTime": 7},
            {"orderId": 10, "clientOrderId": "ccancel", "status": "CANCELED", "origQty": "2.0", "updateTime": 8}
        ]);
        // fapi userTrades: side=SELL, maker=true, positionSide=SHORT (hedge mode)
        let user_trades = serde_json::json!([
            {"id": 100, "orderId": 9, "price": "60000", "qty": "1.0", "commission": "0.2", "maker": true, "time": 5, "side": "SELL", "positionSide": "SHORT"}
        ]);
        let evs = map_binance_perp_history(&all_orders, &user_trades, "binance", "BTCUSDT", None);
        assert_eq!(
            kinds(&evs),
            vec![
                "Fill:cfill:100:SHORT".to_string(),
                "OrderFilled:cfill".to_string(),
                "OrderCanceled:ccancel".to_string(),
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
