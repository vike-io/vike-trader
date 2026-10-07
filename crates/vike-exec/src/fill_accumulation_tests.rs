use super::*;
use vike_model::events::{OrderAccepted, OrderFilled, OrderPartiallyFilled, OrderSubmitted};

fn order_with_qty(qty: f64) -> ManagedOrder {
    ManagedOrder::new(OrderRequest {
        client_order_id: "c".into(),
        venue: "v".into(),
        symbol: "s".into(),
        side: 1,
        qty,
        order_type: "limit".into(),
        price: Some(100.0),
        ..Default::default()
    })
}

fn submitted() -> Event {
    Event::OrderSubmitted(OrderSubmitted { client_order_id: "c".into(), ts: 0 })
}

fn accepted() -> Event {
    Event::OrderAccepted(OrderAccepted { client_order_id: "c".into(), venue_order_id: None, ts: 0 })
}

fn fill(trade_id: &'static str, qty: f64, px: f64) -> FillEvent {
    FillEvent {
        trade_id: trade_id.into(),
        client_order_id: "c".into(),
        venue: "v".into(),
        symbol: "s".into(),
        side: 1,
        last_qty: qty,
        last_px: px,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "taker".to_string().into(),
        ts: 0,
        mark_price: None,
        position_side: "BOTH".into(),
    }
}

fn partially_filled(trade_id: &'static str, qty: f64, px: f64) -> Event {
    Event::OrderPartiallyFilled(OrderPartiallyFilled {
        client_order_id: "c".into(),
        fill: fill(trade_id, qty, px),
        ts: 0,
    })
}

fn filled(trade_id: &'static str, qty: f64, px: f64) -> Event {
    Event::OrderFilled(OrderFilled {
        client_order_id: "c".into(),
        fill: fill(trade_id, qty, px),
        ts: 0,
    })
}

/// The documented law itself: after N partial fills, `avg_fill_px` is the QUANTITY-WEIGHTED
/// mean of every fill price folded so far, and `filled_qty` is their sum. Three fills with
/// DIFFERENT prices AND different quantities -- a single fill, equal quantities, or equal
/// prices cannot distinguish this from `(sum of prices) / count` or any other wrong fold --
/// exercising exactly the running update `accumulate_fill` performs:
/// `(avg_fill_px * prev + last_px * last_qty) / new`.
#[test]
fn running_vwap_is_the_quantity_weighted_mean_of_all_fills() {
    let mut o = order_with_qty(10.0);
    o.apply(&submitted()).unwrap();
    o.apply(&accepted()).unwrap();

    o.apply(&partially_filled("t1", 2.0, 100.0)).unwrap();
    assert_eq!(o.filled_qty, 2.0);
    assert_eq!(o.avg_fill_px, 100.0, "first fill: VWAP is just its own price");

    o.apply(&partially_filled("t2", 6.0, 110.0)).unwrap();
    assert_eq!(o.filled_qty, 8.0);
    // (100*2 + 110*6) / 8 = 860 / 8 = 107.5
    assert_eq!(o.avg_fill_px, 107.5, "VWAP must weight by quantity, not average prices flat");

    o.apply(&filled("t3", 2.0, 90.0)).unwrap();
    assert_eq!(o.filled_qty, 10.0, "filled_qty is the running SUM of fill quantities");
    // (107.5*8 + 90*2) / 10 = (860 + 180) / 10 = 1040 / 10 = 104.0
    assert_eq!(o.avg_fill_px, 104.0);
    assert_eq!(o.status, OrderStatus::Filled);
}

/// The `if new > 0.0` guard's own boundary: a fill that leaves `filled_qty` at exactly zero
/// -- `prev == 0.0` and `fill.last_qty == 0.0` so `new == 0.0` -- must SKIP the VWAP update
/// rather than divide zero by zero. `>` correctly skips it (`0.0 > 0.0` is false); a mutant
/// `>=` would execute the division, compute `0.0 / 0.0 == NaN`, and poison `avg_fill_px`
/// forever after (NaN contaminates every later `avg_fill_px * prev` term).
#[test]
fn zero_quantity_fill_at_zero_prior_qty_does_not_divide_by_zero() {
    let mut o = order_with_qty(10.0);
    o.apply(&submitted()).unwrap();
    o.apply(&accepted()).unwrap();

    o.apply(&partially_filled("t1", 0.0, 555.0)).unwrap();

    assert_eq!(o.filled_qty, 0.0, "a zero-qty fill folds no quantity");
    assert_eq!(o.avg_fill_px, 0.0, "the guard must skip the update, not divide 0.0/0.0 into NaN");
    assert!(!o.avg_fill_px.is_nan(), "avg_fill_px must never become NaN");
}
