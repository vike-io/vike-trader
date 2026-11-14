//! `OrderBroker::instrument_grid` on the simulator: a strategy reads the grid its orders are held to. With
//! a properties source that is the instrument data's point-in-time record at `now`, the one the gate
//! refresh and the fill lane read; without one, an armed gate's own `risk_limits`; with neither,
//! nothing.

use std::sync::Arc;

use vike_model::{Grid, OrderBroker, RiskLimits, Strategy, SymbolProperties};
use vike_simulator::{OrderBrokerSimulator, StrategyEngine, StrategyEngineConfig};

const SYM: &str = "SYM0";

struct Idle;

impl Strategy<OrderBrokerSimulator> for Idle {}

fn engine(params: StrategyEngineConfig) -> StrategyEngine<Idle> {
    let bars = vike_simulator::test_support::flat_minute_bars(2, 100.0, 1.0);
    StrategyEngine::new(vec![(SYM.to_string(), bars)], Idle, params)
}

#[test]
fn a_strategy_reads_the_grid_its_orders_are_held_to() {
    let properties_value = SymbolProperties {
        tick_size: 0.01,
        step_size: 0.001,
        min_qty: 0.002,
        min_notional: 5.0,
        ..Default::default()
    };
    let pit = StrategyEngineConfig {
        default_venue: Some("TEST".into()),
        properties: Some(Arc::new(move |_v: &str, _s: &str, ts: i64| {
            (ts >= 1_000).then_some(properties_value)
        })),
        ..Default::default()
    };
    let mut e = engine(pit);
    e.core.now = 0;
    assert_eq!(OrderBroker::instrument_grid(&e.core, ""), None, "no record at now: unconstrained");
    e.core.now = 2_000;
    let want = Grid {
        tick_size: Some(0.01),
        lot_size: Some(0.001),
        min_notional: Some(5.0),
        min_qty: Some(0.002),
    };
    assert_eq!(OrderBroker::instrument_grid(&e.core, ""), Some(want), "\"\" names the HFT symbol");
    assert_eq!(OrderBroker::instrument_grid(&e.core, SYM), Some(want));
    assert_eq!(OrderBroker::instrument_grid(&e.core, "OTHER"), None, "an unknown symbol");

    // No properties source: an armed gate's own floors are the grid.
    let gated = StrategyEngineConfig {
        risk_limits: Some(RiskLimits {
            tick_size: Some(0.5),
            min_notional: Some(10.0),
            ..RiskLimits::new()
        }),
        ..Default::default()
    };
    let grid = OrderBroker::instrument_grid(&engine(gated).core, "").expect("the gate's grid");
    assert_eq!((grid.tick_size, grid.min_notional, grid.min_qty), (Some(0.5), Some(10.0), None));

    assert_eq!(
        OrderBroker::instrument_grid(&engine(StrategyEngineConfig::default()).core, ""),
        None
    );
}
