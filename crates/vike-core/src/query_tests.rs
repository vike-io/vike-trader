use super::*;
use vike_exec::{OrderStatus, TradingState};

fn ov(coid: &str, symbol: &str, status: OrderStatus) -> OrderView {
    OrderView {
        client_order_id: coid.into(),
        venue: "sim".into(),
        account: None,
        symbol: symbol.into(),
        side: 1,
        qty: 1.0,
        order_type: "limit".into(),
        price: Some(1.0),
        trigger_price: None,
        status,
        venue_order_id: None,
        filled_qty: 0.0,
        avg_fill_px: 0.0,
    }
}

fn snap() -> CoreSnapshot {
    let mut s = CoreSnapshot::empty("sim", "BTCUSDT");
    s.orders =
        vec![ov("a", "BTCUSDT", OrderStatus::Accepted), ov("b", "ETHUSDT", OrderStatus::Filled)];
    s.positions = vec![PositionView {
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        position_side: "BOTH".into(),
        size: 2.0,
        avg_px: 100.0,
        unrealized: 0.0,
        mark_source: None,
        leverage: 0.0,
        liq_price: 0.0,
        margin_mode: vike_model::MarginMode::Cross,
        isolated_margin: None,
    }];
    s.marks = vec![("sim".into(), "BTCUSDT".into(), 101.0)];
    s.portfolio.equity_total = 1234.0;
    s.trading_state = TradingState::Halted;
    s
}

#[test]
fn accessors_read_the_existing_vecs() {
    let s = snap();
    assert_eq!(s.orders_for("BTCUSDT").count(), 1);
    assert_eq!(s.order("b").unwrap().symbol, "ETHUSDT");
    assert!(s.open_order("a").is_some(), "Accepted is non-terminal");
    assert!(s.open_order("b").is_none(), "Filled is terminal");
    assert_eq!(s.position("sim", "BTCUSDT").unwrap().size, 2.0);
    assert!(s.position("sim", "NOPE").is_none());
    assert_eq!(s.last_mark("sim", "BTCUSDT"), Some(101.0));
    assert_eq!(s.last_mark("sim", "NOPE"), None);
    assert_eq!(s.equity(), 1234.0);
    assert_eq!(s.trading_state(), TradingState::Halted);
}
