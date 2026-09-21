//! PR-1 T3: the two vike-exec write-sites (fill-carried mark_price, reconcile
//! position_mark_px) feed the PriceBoard alongside the existing `account.set_mark`.

use vike_exec::testing::RecordingClient;
use vike_exec::{
    Account, BalanceMode, EventBus, ExecutionEngine, ReconcileSnapshot, RiskGate, RiskLimits,
};
use vike_model::events::{Event, FillEvent};

fn engine() -> ExecutionEngine<RecordingClient> {
    ExecutionEngine::new(
        Account::new(1.0, "sim", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "sim",
        "BTCUSDT",
    )
}

/// Mirrors the crate's own `matrix_fill`/`filled` fixture idiom (r5_parity.rs,
/// dropped_events_observability.rs): a fully-specified `FillEvent` literal — the strong-typed
/// fields (`Ustr` venue/symbol, `CompactString` trade_id, `LiquiditySide`/`PositionSide` enums)
/// all accept `.into()` from `&str`.
fn fill_with_mark(mark_price: f64, ts: i64) -> FillEvent {
    FillEvent {
        trade_id: "t1".into(),
        client_order_id: "m1".into(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        last_qty: 1.0,
        last_px: 49_999.0,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "taker".into(),
        ts,
        mark_price: Some(mark_price),
        position_side: "BOTH".into(),
    }
}

#[test]
fn fill_carried_mark_price_reaches_the_board() {
    let mut eng = engine();
    let mut bus = EventBus::new();
    // The write site lives in the BARE `Event::Fill` arm of `on_event` (the Account fold lane) —
    // NOT the `Event::OrderFilled` FSM-wrap arm, which only advances the order registry and never
    // touches `account`/`price_board`. Every real venue adapter emits both for one fill
    // (`vec![Event::Fill(fill), wrap]`); this test drives only the bare lane since that's the one
    // that reaches the write site under test.
    bus.publish(Event::Fill(fill_with_mark(50_000.0, 2)), &mut eng);

    let cell = eng.price_board.cell("sim", "BTCUSDT").expect("board fed by fill site");
    assert_eq!(cell.mark, Some((50_000.0, 2)));
    // the account mark (existing behavior) is unchanged and still set
    assert_eq!(eng.account.mark_of("sim", "BTCUSDT"), Some(50_000.0));
}

#[test]
fn reconcile_position_mark_px_reaches_the_board() {
    let mut eng = engine();
    eng.now_ms = 7;
    let snap = ReconcileSnapshot {
        position_mark_px: vec![("BTCUSDT".to_string(), 123.5)],
        ..Default::default()
    };
    eng.apply_snapshot(&snap);

    let cell = eng.price_board.cell("sim", "BTCUSDT").expect("board fed by reconcile site");
    assert_eq!(cell.mark, Some((123.5, 7)));
    // the account mark (existing behavior) is unchanged and still set
    assert_eq!(eng.account.mark_of("sim", "BTCUSDT"), Some(123.5));
}
