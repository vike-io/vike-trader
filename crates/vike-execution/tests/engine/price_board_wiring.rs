//! The two vike-execution write-sites (fill-carried `mark_price`, reconcile `position_mark_price`) feed
//! the `PriceBoard` alongside `account.set_mark`.

use vike_execution::testing::RecordingClient;
use vike_execution::{Engine, EventBus, ReconcileSnapshot};
use vike_model::events::{Event, FillEvent};

use vike_execution::testing::EngineBuilder;

fn engine() -> Engine<RecordingClient> {
    EngineBuilder::default().build()
}

/// A fully-specified `FillEvent` literal (`r5_parity.rs`'s `matrix_fill` idiom); the strong-typed
/// fields all accept `.into()` from `&str`.
fn fill_with_mark(mark_price: f64, ts: i64) -> FillEvent {
    FillEvent {
        trade_id: "t1".into(),
        order_client_id: "m1".into(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        last_qty: 1.0,
        last_price: 49_999.0,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "taker".into(),
        timestamp: ts,
        mark_price: Some(mark_price),
        position_side: vike_model::events::PositionSide::Both,
    }
}

#[test]
fn fill_carried_mark_price_reaches_the_board() {
    let mut engine_under_test = engine();
    let mut bus = EventBus::new();
    // The write site is the BARE `Event::Fill` arm of `on_event` (the Account lane), NOT the
    // `Event::OrderFilled` wrap arm, which never touches `account`/`price_board`. Adapters emit
    // both for one fill; only the bare one reaches the write site.
    bus.publish(Event::Fill(fill_with_mark(50_000.0, 2)), &mut engine_under_test);

    let cell =
        engine_under_test.price_board.cell("sim", "BTCUSDT").expect("board fed by fill site");
    assert_eq!(cell.mark, Some((50_000.0, 2)));
    // the account mark (existing behavior) is unchanged and still set
    assert_eq!(engine_under_test.account.mark_of("sim", "BTCUSDT"), Some(50_000.0));
}

#[test]
fn reconcile_position_mark_price_reaches_the_board() {
    let mut engine_under_test = engine();
    engine_under_test.now_ms = 7;
    let snap = ReconcileSnapshot {
        position_mark_price: vec![("BTCUSDT".to_string(), 123.5)],
        ..Default::default()
    };
    engine_under_test.apply_snapshot(&snap);

    let cell =
        engine_under_test.price_board.cell("sim", "BTCUSDT").expect("board fed by reconcile site");
    assert_eq!(cell.mark, Some((123.5, 7)));
    // the account mark (existing behavior) is unchanged and still set
    assert_eq!(engine_under_test.account.mark_of("sim", "BTCUSDT"), Some(123.5));
}
