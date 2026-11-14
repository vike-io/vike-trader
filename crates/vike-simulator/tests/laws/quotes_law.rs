//! `StrategyEngineConfig::optimistic_quotes` (decision 0277, rule 3): the simulator keeps ONE resting-limit
//! lane, and which law a TAGGED plain limit fills by is the run's option, never the verb that placed it.
//!
//! What each test pins:
//!
//! - off (the default), a quote (a tagged plain limit, `OrderBroker::place`) fills exactly as its
//!   untagged twin: under the volume cap, its end heard;
//! - on, the same quote fills by the maker law: whole at the touch, outside the volume cap, and its end
//!   heard as every seam order's is (`Filled`, the owner's ruling of 2026-11-12: a maker frees its side
//!   on it live), and an untagged limit beside it keeps the ordinary law.

use vike_marketdata::Bar;
use vike_model::{Fill, OrderBroker, OrderEvent, OrderEventKind, OrderPlacement, Strategy};
use vike_simulator::{OrderBrokerSimulator, StrategyEngine, StrategyEngineConfig};

const SYM: &str = "QUOTE";

/// Bar 1 trades down through 95 on a volume of 4: a cap of a quarter lets one unit fill there.
fn bars() -> Vec<Bar> {
    let bar = |ts, o, h, l, c| vike_simulator::test_support::bar(ts, o, h, l, c, 4.0);
    vec![
        bar(1_000, 100.0, 101.0, 99.0, 100.0),
        bar(2_000, 100.0, 100.0, 94.0, 95.0),
        bar(3_000, 95.0, 96.0, 95.0, 95.0),
    ]
}

/// Places a buy of 2 at 95 on bar 0 by `verb`; records every fill and every order event.
struct Once {
    verb: fn(&mut OrderBrokerSimulator),
    fills: Vec<Fill>,
    ends: Vec<OrderEvent>,
}

impl Strategy<OrderBrokerSimulator> for Once {
    fn on_bar(&mut self, b: &mut OrderBrokerSimulator, _bar: &Bar) {
        if b.index == 0 {
            (self.verb)(b);
        }
    }
    fn on_fill(&mut self, _b: &mut OrderBrokerSimulator, fill: &Fill) {
        self.fills.push(fill.clone());
    }
    fn on_order_event(&mut self, _b: &mut OrderBrokerSimulator, event: &OrderEvent) {
        self.ends.push(event.clone());
    }
}

fn run(verb: fn(&mut OrderBrokerSimulator), optimistic_quotes: bool) -> StrategyEngine<Once> {
    let params = StrategyEngineConfig {
        cash: 100_000.0,
        volume_limit: Some(0.25),
        optimistic_quotes,
        ..StrategyEngineConfig::default()
    };
    let strategy = Once { verb, fills: Vec::new(), ends: Vec::new() };
    let mut engine = StrategyEngine::new(vec![(SYM.into(), bars())], strategy, params);
    engine.run();
    engine
}

fn quote(b: &mut OrderBrokerSimulator) {
    OrderBroker::place(b, "", "q", 1, 2.0, OrderPlacement::Limit { price: 95.0 }, false);
}

fn untagged(b: &mut OrderBrokerSimulator) {
    OrderBrokerSimulator::submit_limit(b, SYM, 1, 2.0, 95.0, 0.0, true, None);
}

/// `(ts, size, price)` of each fill, the tag left out.
fn filled(engine: &StrategyEngine<Once>) -> Vec<(i64, f64, f64)> {
    engine.strategy.fills.iter().map(|f| (f.timestamp, f.size, f.price)).collect()
}

#[test]
fn off_the_option_a_quote_fills_as_its_untagged_twin_and_its_end_is_heard() {
    let twin = run(untagged, false);
    assert_eq!(filled(&twin), [(2_000, 1.0, 95.0)], "the volume cap fills one of 2");
    let engine = run(quote, false);
    assert_eq!(filled(&engine), filled(&twin), "the ordinary limit law");
    assert_eq!(engine.strategy.fills[0].tag.as_deref(), Some("q"));
    assert_eq!(engine.core.dropped, twin.core.dropped, "the capped rest in the same words");
    assert_eq!(
        engine.strategy.ends.len(),
        1,
        "its end reaches the strategy: {:?}",
        engine.strategy.ends
    );
}

#[test]
fn on_the_option_a_quote_fills_by_the_maker_law_and_an_untagged_limit_keeps_its_own() {
    let engine = run(quote, true);
    assert_eq!(filled(&engine), [(2_000, 2.0, 95.0)], "whole at the touch, outside the cap");
    assert!(engine.strategy.fills[0].is_maker);
    assert_eq!(engine.strategy.fills[0].tag.as_deref(), Some("q"));
    assert!(engine.core.dropped.is_empty(), "{:?}", engine.core.dropped);
    let end = OrderEvent {
        order_client_id: String::new(),
        tag: Some("q".into()),
        kind: OrderEventKind::Filled,
    };
    assert_eq!(engine.strategy.ends, [end], "its end reaches the strategy, as live sends it");
    let twin = run(untagged, true);
    assert_eq!(filled(&twin), filled(&run(untagged, false)), "an untagged limit is no quote");
}
