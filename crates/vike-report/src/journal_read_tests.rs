use super::*;
use vike_model::events::TradeId;

fn fill(trade_id: &str, side: i32, qty: f64, px: f64, ts: i64) -> FillEvent {
    FillEvent {
        trade_id: TradeId::new(trade_id).unwrap(),
        client_order_id: "c1".to_string(),
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        side,
        last_qty: qty,
        last_px: px,
        commission: 0.1,
        commission_asset: "USDT".into(),
        liquidity_side: "maker".into(),
        ts,
        mark_price: None,
        position_side: vike_model::events::PositionSide::Both,
    }
}

/// The fills door agrees with itself whatever order the fills arrive in: two round trips handed
/// NEWEST-FIRST (as the desktop holds the rows it read from the datahub) reconstruct the same
/// trades a time-ordered fill stream does.
#[test]
fn a_tearsheet_over_held_fills_does_not_depend_on_their_order() {
    let newest_first = vec![
        fill("t4", -1, 1.0, 102.0, 40),
        fill("t3", 1, 1.0, 105.0, 30),
        fill("t2", -1, 1.0, 110.0, 20),
        fill("t1", 1, 1.0, 100.0, 10),
    ];
    let mut ordered = newest_first.clone();
    ordered.reverse();
    let a = tearsheet_from_fills(newest_first, 1_000.0, 252.0);
    let b = tearsheet_from_fills(ordered, 1_000.0, 252.0);
    assert_eq!(a.rendered_rows(), b.rendered_rows());
    let n = a.rendered_rows().into_iter().find(|(id, _)| *id == "n_trades").map(|(_, v)| v);
    assert_eq!(n.as_deref(), Some("2"), "two closed round trips");
}
