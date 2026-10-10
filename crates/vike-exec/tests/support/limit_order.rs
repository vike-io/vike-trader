//! The plain resting limit `OrderRequest` (binance, BTCUSDT, buy 1 @ 100) the cancel-path engine suites submit.

use vike_model::OrderRequest;

pub fn req(coid: &str) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.into(),
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 1.0,
        order_type: "limit".into(),
        price: Some(100.0),
        ts: 1,
        ..Default::default()
    }
}
