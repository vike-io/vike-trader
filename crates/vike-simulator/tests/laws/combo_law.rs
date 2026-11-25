//! The combo law, backtest side: [`vike_simulator::OrderBrokerSimulator::submit_combo`] priced by nothing (a combo
//! MARKET) or by its NET (`vike_model::combo_net_cross`), the paper book's law. Pinned:
//!
//! 1. a combo fills EVERY leg at once, on the first event after its submit at which every leg has a
//!    fresh mark, a bought leg at its ask and a sold leg at its bid, each at its close without a
//!    quote, a market combo as a taker;
//! 2. a net limit waits until the net crosses it, signed (a credit combo's net and limit are both
//!    negative), and books its legs as makers;
//! 3. on ticks a leg with no mark at the shared ts waits, the whole combo with it, until
//!    `combo_mark_staleness_ms` admits its last mark;
//! 4. the intake refuses whole, in words, with nothing resting; a resting combo commits its legs in
//!    the pre-trade gate's margin fold; `cancel_all` on a leg pulls it; a leg the grid refuses ends
//!    the combo with no leg filled.

use std::sync::Arc;

use vike_marketdata::{Bar, TickQuote, TickTrade};
use vike_model::{
    ComboError, Fill, OrderCombo, OrderComboLeg, OrderRelease, Strategy, SymbolProperties,
    TimeInForce,
};
use vike_simulator::{OrderBrokerSimulator, StrategyEngine, StrategyEngineConfig, TickEvent};

use crate::stop_limit_law::{assert_fill, params};

pub(crate) const A: &str = "LEG_A";
pub(crate) const B: &str = "LEG_B";

/// One strategy action and the step it runs at.
pub(crate) type Act = (usize, Box<dyn Fn(&mut OrderBrokerSimulator)>);

/// `act` at step (or tick index) `step`.
pub(crate) fn at(step: usize, act: impl Fn(&mut OrderBrokerSimulator) + 'static) -> Act {
    (step, Box::new(act))
}

/// Runs each action once, on the first bar, quote or trade at or after its step
/// (`OrderBrokerSimulator::index`), and records every fill `on_fill` hears of.
pub(crate) struct Script {
    acts: Vec<Act>,
    next: usize,
    pub(crate) fills: Vec<Fill>,
}

impl Script {
    pub(crate) fn new(acts: Vec<Act>) -> Self {
        Self { acts, next: 0, fills: Vec::new() }
    }

    fn run(&mut self, ctx: &mut OrderBrokerSimulator) {
        while let Some((step, act)) = self.acts.get(self.next)
            && *step <= ctx.index
        {
            act(ctx);
            self.next += 1;
        }
    }
}

impl Strategy<OrderBrokerSimulator> for Script {
    fn on_bar(&mut self, ctx: &mut OrderBrokerSimulator, _bar: &Bar) {
        self.run(ctx);
    }
    fn on_quote_tick(&mut self, ctx: &mut OrderBrokerSimulator, _q: &TickQuote) {
        self.run(ctx);
    }
    fn on_trade_tick(&mut self, ctx: &mut OrderBrokerSimulator, _t: &TickTrade) {
        self.run(ctx);
    }
    fn on_fill(&mut self, _context: &mut OrderBrokerSimulator, fill: &Fill) {
        self.fills.push(fill.clone());
    }
}

/// The ts of step `i`.
pub(crate) fn ts(i: usize) -> i64 {
    1_000 * (i as i64 + 1)
}

/// A flat bar at `close`, quoted `bid`/`ask` when given.
pub(crate) fn qbar(i: usize, close: f64, quote: Option<(f64, f64)>) -> Bar {
    let mut b = vike_simulator::test_support::bar(ts(i), close, close, close, close, 100.0);
    b.bid = quote.map(|q| q.0);
    b.ask = quote.map(|q| q.1);
    b
}

pub(crate) fn leg(symbol: &str, ratio: i32, limit: Option<f64>) -> OrderComboLeg {
    OrderComboLeg { symbol: symbol.to_string(), ratio, limit }
}

pub(crate) fn combo(
    side: i32,
    qty: f64,
    legs: Vec<OrderComboLeg>,
    net_limit: Option<f64>,
) -> OrderCombo {
    OrderCombo {
        venue: String::new(),
        side,
        qty,
        legs,
        net_limit,
        time_in_force: TimeInForce::Gtc,
        account: None,
        order_client_id: None,
    }
}

/// A bar run over `A` and `B`.
pub(crate) fn run_two(
    p: StrategyEngineConfig,
    a: Vec<Bar>,
    b: Vec<Bar>,
    acts: Vec<Act>,
) -> StrategyEngine<Script> {
    let mut engine = StrategyEngine::new(vec![(A.into(), a), (B.into(), b)], Script::new(acts), p);
    engine.run();
    engine
}

/// A tick replay over `A` and `B`, every leg's mark at most `staleness_ms` old when a combo fills.
pub(crate) fn run_two_ticks(
    staleness_ms: i64,
    a: Vec<TickEvent>,
    b: Vec<TickEvent>,
    acts: Vec<Act>,
) -> StrategyEngine<Script> {
    let symbols = vec![(A.into(), Vec::new()), (B.into(), Vec::new())];
    let mut engine = StrategyEngine::new(symbols, Script::new(acts), params());
    engine.core.combo_mark_staleness_ms = staleness_ms;
    engine.run_ticks(&[(A.to_string(), a), (B.to_string(), b)]);
    engine
}

pub(crate) fn q(symbol: &str, ts: i64, bid: f64, ask: f64) -> TickEvent {
    vike_simulator::test_support::quote(symbol, ts, bid, ask, 1_000.0, 1_000.0)
}

/// The fills as `(symbol, ts, side, size, price)`.
pub(crate) fn fills_of(engine: &StrategyEngine<Script>) -> Vec<(String, i64, i32, f64, f64)> {
    let f = &engine.strategy.fills;
    f.iter().map(|f| (f.symbol.clone(), f.timestamp, f.side, f.size, f.price)).collect()
}

/// Whether the run's refusals hold one with these words.
pub(crate) fn refused(engine_core: &OrderBrokerSimulator, words: &str) -> bool {
    engine_core.dropped.iter().any(|d| d.reason.contains(words))
}

fn a_bars() -> Vec<Bar> {
    vec![
        qbar(0, 10.0, Some((9.9, 10.1))),
        qbar(1, 11.0, Some((10.9, 11.1))),
        qbar(2, 10.0, Some((9.9, 10.1))),
    ]
}

fn b_bars() -> Vec<Bar> {
    vec![
        qbar(0, 5.0, Some((4.9, 5.1))),
        qbar(1, 6.0, Some((5.9, 6.1))),
        qbar(2, 7.0, Some((6.9, 7.1))),
    ]
}

#[test]
fn a_combo_market_fills_every_leg_at_once_a_bought_leg_at_its_ask_a_sold_one_at_its_bid() {
    // buy 2 units of (+1 A, -2 B): A is bought 2, B sold 4
    let spec = combo(1, 2.0, vec![leg(A, 1, None), leg(B, -2, None)], None);
    let engine =
        run_two(params(), a_bars(), b_bars(), vec![at(0, move |b| assert!(b.submit_combo(&spec)))]);
    let fills = fills_of(&engine);
    assert_eq!(
        fills,
        vec![(A.to_string(), ts(1), 1, 2.0, 11.1), (B.to_string(), ts(1), -1, 4.0, 5.9)],
        "both legs on the step after the submit, at the ask and the bid"
    );
    for f in &engine.strategy.fills {
        assert_fill("a combo market is a taker", f, ts(1), f.price, false);
    }
    assert_eq!((engine.core.position_of(A).size, engine.core.position_of(B).size), (2.0, -4.0));
}

#[test]
fn a_leg_with_no_quote_trades_at_its_close() {
    let a = vec![qbar(0, 10.0, None), qbar(1, 11.0, None)];
    let b = vec![qbar(0, 5.0, None), qbar(1, 6.0, None)];
    let spec = combo(-1, 1.0, vec![leg(A, 1, None), leg(B, -1, None)], None);
    let engine = run_two(params(), a, b, vec![at(0, move |b| assert!(b.submit_combo(&spec)))]);
    assert_eq!(
        fills_of(&engine),
        vec![(A.to_string(), ts(1), -1, 1.0, 11.0), (B.to_string(), ts(1), 1, 1.0, 6.0)],
        "selling the combo flips every leg"
    );
}

#[test]
fn a_net_limit_waits_until_the_net_crosses_and_books_its_legs_as_makers() {
    // the net of (+1 A, -1 B) bought: A's ask less B's bid. Step 1: 11.1 - 5.9 = 5.2 > 4; step 2:
    // 10.1 - 6.9 = 3.2 <= 4.
    let spec = combo(1, 1.0, vec![leg(A, 1, None), leg(B, -1, None)], Some(4.0));
    let engine =
        run_two(params(), a_bars(), b_bars(), vec![at(0, move |b| assert!(b.submit_combo(&spec)))]);
    let fills = fills_of(&engine);
    assert_eq!(
        fills,
        vec![(A.to_string(), ts(2), 1, 1.0, 10.1), (B.to_string(), ts(2), -1, 1.0, 6.9)]
    );
    for f in &engine.strategy.fills {
        assert_fill("a net-limit combo is a maker", f, ts(2), f.price, true);
    }
}

#[test]
fn a_credit_combo_crosses_on_its_signed_net() {
    // selling (+1 A, -1 B): A sold at its bid, B bought at its ask, the net their signed sum. It
    // fills when the net is AT OR ABOVE the (negative) limit.
    let a = vec![
        qbar(0, 4.0, Some((3.9, 4.1))),
        qbar(1, 4.0, Some((3.9, 4.1))),
        qbar(2, 5.0, Some((4.9, 5.1))),
    ];
    let b = (0..3).map(|i| qbar(i, 6.0, Some((5.9, 6.1)))).collect();
    // step 1: 3.9 - 6.1 = -2.2 < -2.0, waits; step 2: 4.9 - 6.1 = -1.2 >= -2.0, fills
    let spec = combo(-1, 1.0, vec![leg(A, 1, None), leg(B, -1, None)], Some(-2.0));
    let engine = run_two(params(), a, b, vec![at(0, move |b| assert!(b.submit_combo(&spec)))]);
    assert_eq!(
        fills_of(&engine),
        vec![(A.to_string(), ts(2), -1, 1.0, 4.9), (B.to_string(), ts(2), 1, 1.0, 6.1)]
    );
}

#[test]
fn on_ticks_a_leg_with_no_fresh_mark_waits_and_the_bound_admits_an_older_one() {
    let a = || vec![q(A, 500, 9.9, 10.1), q(A, 1_000, 9.9, 10.1), q(A, 2_000, 10.9, 11.1)];
    let b = || vec![q(B, 1_500, 4.9, 5.1), q(B, 2_000, 5.9, 6.1)];
    let spec = combo(1, 1.0, vec![leg(A, 1, None), leg(B, -1, None)], None);
    let act = |spec: OrderCombo| vec![at(0, move |b| assert!(b.submit_combo(&spec)))];
    // bound 0: B's tick at 1500 finds A's mark from 1000 stale; both legs mark at 2000 and fill there
    let strict = run_two_ticks(0, a(), b(), act(spec.clone()));
    assert_eq!(
        fills_of(&strict),
        vec![(A.to_string(), 2_000, 1, 1.0, 11.1), (B.to_string(), 2_000, -1, 1.0, 5.9)]
    );
    // bound 500: at 1500 A's mark from 1000 is 500 old, not older than the bound
    let wide = run_two_ticks(500, a(), b(), act(spec));
    assert_eq!(
        fills_of(&wide),
        vec![(A.to_string(), 1_500, 1, 1.0, 10.1), (B.to_string(), 1_500, -1, 1.0, 4.9)]
    );
}

#[test]
fn the_intake_refuses_whole_in_words_and_nothing_rests() {
    let gtc = |legs: Vec<OrderComboLeg>| combo(1, 1.0, legs, None);
    let mut ioc = gtc(vec![leg(A, 1, None), leg(B, -1, None)]);
    ioc.time_in_force = TimeInForce::Ioc;
    // the run's own word, then the model's (`OrderCombo::validate`, and the books' lifetime rule
    // `OrderCombo::book_lifetime_refusal` for the IOC one), the paper book's too
    let cases: Vec<(OrderCombo, String)> = vec![
        (
            gtc(vec![leg(A, 1, None), leg("ZZZ", -1, None)]),
            "combo leg ZZZ is not a symbol of this run".to_string(),
        ),
        (
            gtc(vec![leg(A, 1, None), leg(A, -1, None)]),
            ComboError::RepeatedLegSymbol(A.to_string()).to_string(),
        ),
        (ioc, ComboError::NotGtc(TimeInForce::Ioc).to_string()),
        (gtc(vec![leg(A, 1, None)]), ComboError::TooFewLegs(1).to_string()),
    ];
    for (spec, words) in cases {
        let words = words.as_str();
        let engine = run_two(
            params(),
            a_bars(),
            b_bars(),
            vec![at(0, move |b| assert!(!b.submit_combo(&spec)))],
        );
        assert!(refused(&engine.core, words), "{words}: {:?}", engine.core.dropped);
        assert!(engine.strategy.fills.is_empty(), "{words}: nothing filled");
    }
    // inside a release: a release holds one order
    let spec = combo(1, 1.0, vec![leg(A, 1, None), leg(B, -1, None)], None);
    let release = OrderRelease { at: ts(2), price: None };
    let held = at(0, move |b| {
        let spec = spec.clone();
        b.submit_released(release, move |b| assert!(!b.submit_combo(&spec)));
    });
    let engine = run_two(params(), a_bars(), b_bars(), vec![held]);
    assert!(refused(&engine.core, "a combo takes no release"));
    assert!(engine.strategy.fills.is_empty());
}

#[test]
fn a_resting_combo_commits_its_legs_in_the_margin_the_gate_judges() {
    let flat = || (0..3).map(|i| qbar(i, 100.0, None)).collect::<Vec<Bar>>();
    // leverage 1: initial margin is the whole notional, 1 000 of cash
    let p = || StrategyEngineConfig { cash: 1_000.0, leverage: Some(1.0), ..params() };
    // a buy whose net limit never crosses (net 0 > -1 000): it rests, committing 4 × 100 per leg
    let resting = combo(1, 4.0, vec![leg(A, 1, None), leg(B, -1, None)], Some(-1_000.0));
    let then_buy = move |with_combo: bool| {
        let spec = resting.clone();
        at(0, move |b| {
            if with_combo {
                assert!(b.submit_combo(&spec), "each leg alone fits the margin");
            }
            b.submit(A, 1, 3.0, 0.0, true, None);
        })
    };
    let engine = run_two(p(), flat(), flat(), vec![then_buy(true)]);
    assert!(refused(&engine.core, "insufficient-margin"), "{:?}", engine.core.dropped);
    assert_eq!(engine.core.position_of(A).size, 0.0, "the buy was denied, the combo still rests");
    let engine = run_two(p(), flat(), flat(), vec![then_buy(false)]);
    assert_eq!(engine.core.position_of(A).size, 3.0, "without the combo the same buy fits");
}

#[test]
fn cancel_all_on_a_leg_pulls_the_combo() {
    let spec = combo(1, 1.0, vec![leg(A, 1, None), leg(B, -1, None)], None);
    let act = at(0, move |b| {
        assert!(b.submit_combo(&spec));
        b.cancel_all(B);
    });
    let engine = run_two(params(), a_bars(), b_bars(), vec![act]);
    assert!(engine.strategy.fills.is_empty(), "the combo went with B's orders");
}

#[test]
fn a_leg_the_grid_refuses_ends_the_combo_with_no_leg_filled() {
    // B's grid takes nothing below 10; A has none
    let grid = Arc::new(|_v: &str, s: &str, _t: i64| {
        (s == B).then_some(SymbolProperties { min_qty: 10.0, ..Default::default() })
    });
    let p = StrategyEngineConfig {
        default_venue: Some("SIM".into()),
        properties: Some(grid),
        ..params()
    };
    let spec = combo(1, 1.0, vec![leg(A, 1, None), leg(B, -4, None)], None);
    let engine =
        run_two(p, a_bars(), b_bars(), vec![at(0, move |b| assert!(b.submit_combo(&spec)))]);
    assert!(engine.strategy.fills.is_empty(), "all its legs or none: A did not fill either");
    assert_eq!(engine.core.position_of(A).size, 0.0);
    assert!(
        refused(&engine.core, "the grid does not take 4 LEG_B (min_qty)"),
        "{:?}",
        engine.core.dropped
    );
}
