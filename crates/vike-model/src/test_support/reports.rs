//! Venue fill REPORTS and fills as a reconcile, an ingest or an accounting test receives them: the
//! copies the core's, the trade hub's, the analytics and the journal suites each spelled.

use crate::FillReport;
use crate::events::{FillEvent, LiquiditySide, PositionSide, TradeId};

/// One fill of binance `BTCUSDT`: `qty` at `px` on `side` at `ts`, paying `commission`.
///
/// DEFAULTS: trade id `t<ts>-<side>`, no client order id, an empty commission asset, `"taker"`, marked
/// at its own price, one-way (`PositionSide::Both`).
pub fn fill_event(side: i32, qty: f64, px: f64, ts: i64, commission: f64) -> FillEvent {
    FillEvent {
        // minted here, not read off a wire — same `t<ts>-<side>` bytes as before
        trade_id: TradeId::prefixed("t", format_args!("{ts}-{side}")),
        client_order_id: String::new(),
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        side,
        last_qty: qty,
        last_px: px,
        commission,
        commission_asset: String::new().into(),
        liquidity_side: "taker".into(),
        ts,
        mark_price: Some(px),
        position_side: PositionSide::Both,
    }
}

/// A fill of an EXTERNAL order (a venue id, no local client order id): a `1.0` taker buy of binance
/// `BTCUSDT` at `100.0`, trade `t1` of venue order `v9`, no commission, at `ts` 5.
pub fn external_fill_report() -> FillReport {
    FillReport {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        trade_id: "t1".into(),
        venue_order_id: "v9".into(),
        client_order_id: None, // external order — no local coid
        side: 1,
        last_qty: 1.0,
        last_px: 100.0,
        commission: 0.0,
        commission_asset: "USDT".into(),
        liquidity_side: LiquiditySide::Taker,
        ts: 5,
    }
}
