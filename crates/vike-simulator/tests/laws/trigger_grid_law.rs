//! The owner's G1 where the simulator fires a stop itself: a stop's, a stop-limit's and a take-profit's
//! trigger, and the protective stop an entry carries, join the book on the grid the gate holds the
//! symbol to, by the live gate's own rule (`vike_execution::risk::trigger_on_grid`): a stop away from the
//! market, a stop-limit's and a take-profit's trigger as a limit's price. A backtest stop then fires
//! where a venue would have rested it. With no grid every level stays as stated.

use std::sync::Arc;

use vike_marketdata::Bar;
use vike_model::{Fill, OrderBroker, OrderPlacement, Strategy, SymbolProperties};
use vike_simulator::{OrderBrokerSimulator, StrategyEngine, StrategyEngineConfig};

const SYM: &str = "SYM0";

/// A 0.01 tick from the instrument data, as a profile's `snap_to_properties` wires it.
fn on_grid() -> StrategyEngineConfig {
    let properties_value = SymbolProperties { tick_size: 0.01, ..Default::default() };
    StrategyEngineConfig {
        cash: 1_000_000.0,
        default_venue: Some("TEST".into()),
        properties: Some(Arc::new(move |_v: &str, _s: &str, _ts: i64| Some(properties_value))),
        ..Default::default()
    }
}

fn no_grid() -> StrategyEngineConfig {
    StrategyEngineConfig { cash: 1_000_000.0, ..Default::default() }
}

fn bar(ts: i64, open: f64, high: f64, low: f64, close: f64) -> Bar {
    vike_simulator::test_support::bar(ts, open, high, low, close, 1_000.0)
}

struct Idle;

impl Strategy<OrderBrokerSimulator> for Idle {}

/// What rests after one order of each trigger kind, as `(price, limit, stop)`, and last the seam's
/// stop once amended to a new trigger.
fn rested(params: StrategyEngineConfig) -> Vec<(Option<f64>, Option<f64>, Option<f64>)> {
    let bars = vec![bar(60_000, 100.0, 100.0, 100.0, 100.0)];
    let mut e = StrategyEngine::new(vec![(SYM.to_string(), bars)], Idle, params);
    e.core.now = 60_000;
    let b = &mut e.core;
    b.submit_stop(SYM, 1, 1.0, 100.005, 0.0, true);
    b.submit_stop(SYM, -1, 1.0, 99.995, 0.0, true);
    b.submit_stop_limit(SYM, 1, 1.0, 100.005, 100.02, 0.0, true);
    b.submit_take_profit(SYM, -1, 1.0, 100.005, None, 0.0, true);
    b.submit(SYM, 1, 1.0, 0.0, true, Some(99.995));
    OrderBroker::place(b, SYM, "stop", 1, 1.0, OrderPlacement::Stop { trigger: 100.015 }, false);
    let mut levels: Vec<_> =
        b.symbol[0].pending.iter().map(|o| (o.price, o.limit, o.stop)).collect();
    OrderBroker::amend(b, "stop", None, None, Some(100.023));
    levels.push((b.symbol[0].pending.last().and_then(|o| o.price), None, None));
    levels
}

/// THE CASE. On a 0.01 tick: a buy stop goes up and a sell stop down; a stop-limit's trigger goes as a
/// buy limit's price (down) with its limit as stated; a sell take-profit's goes up; a long entry's
/// protective stop is a sell stop (down); a stop placed through the order seam goes up, and so does
/// the trigger it is amended to. Without a grid nothing moves.
#[test]
fn a_trigger_joins_the_book_on_the_grid_by_the_side_of_its_order() {
    assert_eq!(
        rested(on_grid()),
        vec![
            (Some(100.01), None, None),
            (Some(99.99), None, None),
            (Some(100.0), Some(100.02), None),
            (Some(100.01), None, None),
            (None, None, Some(99.99)),
            (Some(100.02), None, None),
            (Some(100.03), None, None), // amended
        ]
    );
    assert_eq!(
        rested(no_grid()),
        vec![
            (Some(100.005), None, None),
            (Some(99.995), None, None),
            (Some(100.005), Some(100.02), None),
            (Some(100.005), None, None),
            (None, None, Some(99.995)),
            (Some(100.015), None, None),
            (Some(100.023), None, None), // amended
        ]
    );
}

/// Rests one BUY stop at 100.005 on the first bar and records its fills as `(ts, price)`.
struct BuyStop {
    placed: bool,
    fills: Vec<(i64, f64)>,
}

impl Strategy<OrderBrokerSimulator> for BuyStop {
    fn on_bar(&mut self, b: &mut OrderBrokerSimulator, _bar: &Bar) {
        if !std::mem::replace(&mut self.placed, true) {
            b.submit_stop(SYM, 1, 1.0, 100.005, 0.0, true);
        }
    }
    fn on_fill(&mut self, _b: &mut OrderBrokerSimulator, f: &Fill) {
        self.fills.push((f.timestamp, f.price));
    }
}

/// Where it FIRES: a bar that reaches 100.007 fires the stated 100.005 but not the 100.01 the grid puts it
/// at, which fires on the bar that reaches it.
#[test]
fn a_backtest_stop_fires_at_its_level_on_the_grid() {
    let fills = |params| {
        let bars = vec![
            bar(60_000, 100.0, 100.0, 100.0, 100.0),
            bar(120_000, 100.0, 100.007, 100.0, 100.006),
            bar(180_000, 100.006, 100.01, 100.0, 100.0),
        ];
        let s = BuyStop { placed: false, fills: Vec::new() };
        let mut e = StrategyEngine::new(vec![(SYM.to_string(), bars)], s, params);
        let _ = e.run();
        std::mem::take(&mut e.strategy.fills)
    };
    assert_eq!(fills(on_grid()), vec![(180_000, 100.01)], "on the grid: not before 100.01");
    assert_eq!(fills(no_grid()), vec![(120_000, 100.005)], "no grid: at the stated level");
}
