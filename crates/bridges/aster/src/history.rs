//! Aster spot + perp REST order-history → events, for the audit-A3 post-reconnect resync: the
//! venue face of the shared [`vike_binance::family::history`].
//!
//! Aster's order/trade JSON is a Binance fork (identical field names/shapes), so this module's
//! replay logic — previously a byte-for-byte copy of Binance's — now lives once in the
//! Binance-wire-grammar core and is re-exported here under Aster's names (F0, dedup rung 1).
//! `venue` stays a caller-supplied parameter. The contract and the public paths here are
//! unchanged — see the family module for the reconnect-gap/dedup/dual-publish notes. The tests
//! below stay HERE, exercising the shared code through Aster's own wrapper: Binance's twin does
//! the same through its own, so the one shared implementation is proven twice.

pub use vike_binance::family::history::{
    map_history as map_aster_history, map_perp_history as map_aster_perp_history,
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
        let evs = map_aster_history(&all_orders, &my_trades, "aster", "BTCUSDT");
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
            assert_eq!(fill.venue, "aster");
        } else {
            panic!("first event must be the bare Fill");
        }
    }

    #[test]
    fn history_fill_surfaces_the_commission_asset() {
        // The account-trade REST row carries `commissionAsset` alongside `commission`.
        let all_orders = serde_json::json!([
            {"orderId": 9, "clientOrderId": "c_fill", "status": "FILLED", "origQty": "1.0", "updateTime": 7}
        ]);
        let my_trades = serde_json::json!([
            {"id": 100, "orderId": 9, "price": "50000", "qty": "1.0", "commission": "0.1", "commissionAsset": "USDT", "time": 5, "isBuyer": true, "isMaker": false}
        ]);
        let evs = map_aster_history(&all_orders, &my_trades, "aster", "BTCUSDT");
        if let Event::Fill(fill) = &evs[0] {
            assert_eq!(fill.commission, 0.1);
            assert_eq!(fill.commission_asset, "USDT");
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
        let evs = map_aster_history(&all_orders, &my_trades, "aster", "BTCUSDT");
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

    /// ⚠ The A3 SPOT resync replays **Aster's own** `GET /api/v3/userTrades` rows, which are
    /// FUTURES-shaped (`side`/`maker`) — not Binance spot's `isBuyer`/`isMaker`, the spelling every
    /// fixture above uses because this module was ported from Binance's twin.
    ///
    /// This is the regression that a path-only fix would have introduced. Before 2026-08-05 the
    /// path const pointed at Binance's `/api/v3/myTrades`, which 404s on aster; `exec.rs` swallows
    /// that into `json!([])`, so the resync replayed nothing and the grammar mismatch stayed
    /// invisible. Correcting the path alone would have started feeding REAL rows through an
    /// `isBuyer`-only reader — every replayed buy booked as a SELL, at the moment the core is
    /// rebuilding state after a disconnect. `side:"BUY"` MUST map to `+1` here.
    #[test]
    fn spot_resync_replays_asters_futures_shaped_user_trades() {
        let all_orders = serde_json::json!([
            {"orderId": 266358, "clientOrderId": "c_buy", "status": "FILLED", "origQty": "2.0", "updateTime": 9}
        ]);
        // The published /api/v3/userTrades row shape (asterdex/api-docs, read 2026-08-05):
        // `side` + `maker` + `buyer`, and NO `isBuyer`/`isMaker` anywhere.
        let user_trades = serde_json::json!([
            {"symbol": "BNBUSDT", "id": 1002, "orderId": 266358, "side": "BUY", "price": "1",
             "qty": "2", "quoteQty": "2", "commission": "0.00105000", "commissionAsset": "BNB",
             "time": 1755656788798i64, "counterpartyId": 19, "createUpdateId": null,
             "maker": false, "buyer": true}
        ]);
        let evs = map_aster_history(&all_orders, &user_trades, "aster", "BNBUSDT");
        assert_eq!(
            kinds(&evs),
            vec!["Fill:c_buy:1002".to_string(), "OrderFilled:c_buy".to_string()]
        );
        let Event::Fill(fill) = &evs[0] else { panic!("first event must be the bare Fill") };
        assert_eq!(fill.side, 1, "side:\"BUY\" -> +1 (an isBuyer-only reader gives -1)");
        assert_eq!(fill.last_qty, 2.0);
        assert_eq!(fill.last_px, 1.0);
        assert_eq!(fill.commission, 0.00105);
        assert_eq!(fill.commission_asset, "BNB");
        assert_eq!(
            fill.liquidity_side,
            vike_model::events::LiquiditySide::Taker,
            "maker:false -> Taker"
        );
        assert_eq!(fill.ts, 1755656788798);
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
        let evs = map_aster_perp_history(&all_orders, &user_trades, "aster", "BTCUSDT");
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
