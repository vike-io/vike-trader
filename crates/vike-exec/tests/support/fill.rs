//! The plain taker `FillEvent` (no commission, `ts` 1, one-way position side) the engine and risk
//! suites fold through `on_event`; each suite passes its own venue.

use vike_model::events::{FillEvent, PositionSide};

pub fn fill(
    venue: &str,
    coid: &str,
    trade_id: &'static str,
    symbol: &str,
    side: i32,
    qty: f64,
    px: f64,
) -> FillEvent {
    FillEvent {
        trade_id: trade_id.into(),
        client_order_id: coid.to_string(),
        venue: venue.into(),
        symbol: symbol.into(),
        side,
        last_qty: qty,
        last_px: px,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "taker".into(),
        ts: 1,
        mark_price: None,
        position_side: PositionSide::Both,
    }
}
