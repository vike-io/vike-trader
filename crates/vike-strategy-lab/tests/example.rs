//! `strategies/other/example.rs`, compiled as a module and driven: it builds from empty params at
//! its header's default size and places exactly one order.

use vike_marketdata::test_support::flat_bar_unit_volume_on;
use vike_model::OrderPlacement;
use vike_model::params::{ParamValue, Params};
use vike_model::strategy::OrderBrokerMock;

#[expect(
    clippy::module_inception,
    reason = "the shipped file keeps its name inside this file's module"
)]
#[path = "../../../strategies/other/example.rs"]
mod example;

#[test]
fn the_example_buys_once_and_holds() {
    let mut broker = OrderBrokerMock::default();
    let mut strategy = example::build::<OrderBrokerMock>(&Params::new());
    for timestamp in [0, 60_000, 120_000] {
        strategy.on_bar(&mut broker, &flat_bar_unit_volume_on("BTCUSDT", timestamp, 100.0));
    }
    assert_eq!(broker.placed.len(), 1, "{:?}", broker.placed);
    let (symbol, _, side, quantity, order, reduce_only) = &broker.placed[0];
    assert_eq!((symbol.as_str(), *side, *quantity, *reduce_only), ("BTCUSDT", 1, 1.0, false));
    assert!(matches!(order, OrderPlacement::Market));

    let mut sized = Params::new();
    sized.insert("qty", ParamValue::Float(2.5));
    let mut broker = OrderBrokerMock::default();
    example::build::<OrderBrokerMock>(&sized)
        .on_bar(&mut broker, &flat_bar_unit_volume_on("BTCUSDT", 0, 100.0));
    assert_eq!(broker.placed[0].3, 2.5);
}
