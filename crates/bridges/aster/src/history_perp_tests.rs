use super::*;
use vike_model::events::{Event, LiquiditySide};

fn kinds(evs: &[Event]) -> Vec<String> {
    evs.iter()
        .map(|e| match e {
            Event::Fill(fill) => {
                format!("Fill:{}:{}:{}", fill.client_order_id, fill.trade_id, fill.position_side)
            }
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
