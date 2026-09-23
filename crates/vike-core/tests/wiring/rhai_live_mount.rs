//! Task 9: proves `RhaiStrategy<LiveBroker>` satisfies the live runtime's mount type —
//! `StrategyMount.strategy: Box<dyn Strategy<LiveBroker> + Send>` (`runtime/mod.rs`) — and that
//! driving `on_bar` through a directly-constructed `LiveBroker` buffers a real order. The `Box`
//! coercion below is itself the compile-time `Send` proof: it would fail to compile if
//! `RhaiStrategy<LiveBroker>` were not `Send`.

use std::sync::Arc;
use vike_core::LiveBroker;
use vike_model::{Bar, Strategy};
use vike_script::RhaiStrategy;

fn live_broker(position: f64, close: f64) -> LiveBroker {
    LiveBroker {
        positions: Vec::new(),
        prices: Vec::new(),
        bar_views: Vec::new(),
        position,
        price: close,
        equity: 10_000.0,
        bars: Arc::new(vec![bar(close)]),
        index: 0,
        now: 0,
        multiplier: 1.0,
        lot_size: 0.0,
        submissions: Vec::new(),
        modifications: Vec::new(),
        cancels: Vec::new(),
        brackets: Vec::new(),
        conditionals: Vec::new(),
        mass_cancel: false,
    }
}

fn bar(c: f64) -> Bar {
    Bar {
        ts: 0,
        open: c,
        high: c,
        low: c,
        close: c,
        volume: 0.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: Some("X".into()),
    }
}

#[test]
fn rhai_strategy_boxes_as_send_live_strategy_and_buffers() {
    // 1. it satisfies the live mount type
    let mut strat: Box<dyn Strategy<LiveBroker> + Send> = Box::new(
        RhaiStrategy::<LiveBroker>::compile(
            "fn on_bar() { if close() > 100.0 && position() == 0.0 { buy(2.0); } }",
        )
        .unwrap(),
    );
    // 2. drive on_bar through a real LiveBroker and observe the buffered order
    let mut b = live_broker(0.0, 101.0);
    strat.on_bar(&mut b, &bar(101.0));
    assert_eq!(b.submissions.len(), 1);
}
