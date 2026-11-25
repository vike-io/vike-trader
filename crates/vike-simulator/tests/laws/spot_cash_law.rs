//! The spot (cash) law, backtest side: on a symbol listed in [`vike_simulator::StrategyEngineConfig::cash_symbols`]
//! a SELL never sells more than the long holds less the sells already out, refused at the submit in
//! the live engine's own words: the ONE rule `vike_execution::risk::cash_oversell_refusal` that
//! `Engine::cash_oversell_denial` asks. Pinned:
//!
//! 1. a sell past the long is refused, a sell within it rests, and a close whose base a resting sell
//!    already holds is refused too; nothing opens a short;
//! 2. a spot ladder's exits are RELEASED: the stop and the targets each cover the long, and the group
//!    claims its largest set once (`vike_execution::ContingencyBook::linked_exits`), while a plain sell
//!    beside them finds nothing free;
//! 3. a margin run (no symbol listed) sells past the long as before and goes short;
//! 4. under the opt-in latency model a tagged quote counts as the account sees it: a submit or a
//!    modify still in flight holds its new size, so a raise past the long and a second quote beside
//!    it are refused before either reaches the book.

use vike_marketdata::{Bar, TickQuote};
use vike_model::{
    Fill, LadderSpec, LadderTarget, OrderBroker, OrderKind, OrderPlacement, Strategy,
};
use vike_simulator::{
    LatencyModelKind, OrderBrokerSimulator, StrategyEngine, StrategyEngineConfig,
};

use crate::stop_limit_law::{FLAT, SYM, bar, params};

type Act = Box<dyn Fn(&mut OrderBrokerSimulator)>;
type Step = (usize, Act);

/// One action at one bar index.
fn step(at: usize, act: impl Fn(&mut OrderBrokerSimulator) + 'static) -> Step {
    (at, Box::new(act))
}

/// Runs each step from its bar's `on_bar`, and records every fill.
struct Steps {
    steps: Vec<Step>,
    fills: Vec<Fill>,
}

impl Strategy<OrderBrokerSimulator> for Steps {
    fn on_bar(&mut self, ctx: &mut OrderBrokerSimulator, _bar: &Bar) {
        for (at, act) in &self.steps {
            if *at == ctx.index {
                act(ctx);
            }
        }
    }
    fn on_fill(&mut self, _context: &mut OrderBrokerSimulator, fill: &Fill) {
        self.fills.push(fill.clone());
    }
}

fn run(cash: bool, steps: Vec<Step>) -> StrategyEngine<Steps> {
    run_funded(cash, params().cash, steps)
}

fn run_funded(cash: bool, funds: f64, steps: Vec<Step>) -> StrategyEngine<Steps> {
    let cash_symbols = if cash { vec![SYM.to_string()] } else { Vec::new() };
    let p = StrategyEngineConfig { cash_symbols, cash: funds, ..params() };
    let bars = (0..4).map(|i| bar(1_000 * (i + 1), FLAT)).collect();
    let mut engine =
        StrategyEngine::new(vec![(SYM.into(), bars)], Steps { steps, fills: Vec::new() }, p);
    engine.run();
    engine
}

fn refused(free: &str, sells: &str) -> String {
    format!(
        "a spot account cannot sell more than it holds: it has {free}, this sells {sells}; nothing was sent"
    )
}

fn reasons(engine: &StrategyEngine<Steps>) -> Vec<&str> {
    engine.core.dropped.iter().map(|d| d.reason.as_str()).collect()
}

/// Long 1 at 100, then a market sell of 2 and a limit sell of 1 at 110, then a close.
fn oversell_script() -> Vec<Step> {
    vec![
        step(0, |b| b.submit(SYM, 1, 1.0, 0.0, true, None)),
        step(1, |b| {
            b.submit(SYM, -1, 2.0, 0.0, true, None);
            b.submit_limit(SYM, -1, 1.0, 110.0, 0.0, true, None);
        }),
        step(2, |b| b.submit_close(SYM)),
    ]
}

#[test]
fn a_spot_sell_past_the_long_is_refused_in_the_live_engines_words() {
    let engine = run(true, oversell_script());
    assert_eq!(
        reasons(&engine),
        vec![refused("1", "2"), refused("0", "1")],
        "the sell of 2, the close"
    );
    let fills: Vec<(i32, f64)> = engine.strategy.fills.iter().map(|f| (f.side, f.size)).collect();
    assert_eq!(fills, vec![(1, 1.0)], "only the buy filled");
    assert_eq!(engine.core.position_of(SYM).size, 1.0, "never short");
    let resting: Vec<(OrderKind, i32, f64)> =
        engine.core.pending_of(SYM).iter().map(|o| (o.kind, o.side, o.size)).collect();
    assert_eq!(resting, vec![(OrderKind::Limit, -1, 1.0)], "the sell within the long rests");
}

/// **Each refusal carries the time of the bar it was refused in** (decision 0078's amendment of 2026-11-19): the sell
/// of 2 at the second bar's open, the close at the third's, aligned with `dropped` by index, in the engine and in the
/// run's result alike.
#[test]
fn each_refusal_carries_the_time_of_its_bar() {
    let cash_symbols = vec![SYM.to_string()];
    let p = StrategyEngineConfig { cash_symbols, ..params() };
    let bars = (0..4).map(|i| bar(1_000 * (i + 1), FLAT)).collect();
    let steps = Steps { steps: oversell_script(), fills: Vec::new() };
    let mut engine = StrategyEngine::new(vec![(SYM.into(), bars)], steps, p);
    let result = engine.run();
    assert_eq!(engine.core.dropped_at, vec![2_000, 3_000]);
    let simulator = result.simulator().expect("a backtest's record");
    assert_eq!((simulator.dropped.len(), &simulator.dropped_at), (2, &vec![2_000, 3_000]));
}

/// On 100 of cash at a price of 100 and the taker's 0.2%: a buy of 1 costs 100.2 with its fee, a buy of
/// 0.99 costs 99.198.
fn overbuy_script() -> Vec<Step> {
    vec![
        step(0, |b| b.submit(SYM, 1, 1.0, 0.0, true, None)),
        step(1, |b| b.submit(SYM, 1, 0.99, 0.0, true, None)),
    ]
}

#[test]
fn a_spot_buy_past_the_free_cash_is_refused_with_its_fee_in_it() {
    let engine = run_funded(true, 100.0, overbuy_script());
    assert_eq!(
        reasons(&engine),
        vec![vike_model::OrderDenial::CashOverbuy.template()],
        "the buy of 1"
    );
    let fills: Vec<(i32, f64)> = engine.strategy.fills.iter().map(|f| (f.side, f.size)).collect();
    assert_eq!(fills, vec![(1, 0.99)], "the buy the cash pays for filled");
    assert!(engine.core.cash >= 0.0, "spot cash never goes below zero: {}", engine.core.cash);
}

/// Runs each step once per bar INDEX, however many symbols the step fans `on_bar` out to, and records every fill.
struct OncePerIndex {
    steps: Vec<Step>,
    done: Vec<usize>,
    fills: Vec<Fill>,
}

impl Strategy<OrderBrokerSimulator> for OncePerIndex {
    fn on_bar(&mut self, ctx: &mut OrderBrokerSimulator, _bar: &Bar) {
        if self.done.contains(&ctx.index) {
            return;
        }
        self.done.push(ctx.index);
        for (at, act) in &self.steps {
            if *at == ctx.index {
                act(ctx);
            }
        }
    }
    fn on_fill(&mut self, _context: &mut OrderBrokerSimulator, fill: &Fill) {
        self.fills.push(fill.clone());
    }
}

/// **A run that also trades a derivative judges a spot buy against its FREE cash**, not its whole cash: on 1000, a
/// perpetual short of 5 at 100 (fee 1.0) leaves 1499 of cash, of which the short's proceeds (500) and its margin at the
/// full rate (500) are not free, so 499 is. A spot buy of 5 (501.00 with its fee) is refused, one of 4 (400.80) fills.
#[test]
fn a_spot_buy_beside_a_derivative_is_judged_against_the_cash_its_margin_leaves() {
    const PERP: &str = "PERP";
    let p = StrategyEngineConfig { cash_symbols: vec![SYM.to_string()], cash: 1_000.0, ..params() };
    let bars: Vec<Bar> = (0..4).map(|i| bar(1_000 * (i + 1), FLAT)).collect();
    let steps = vec![
        step(0, |b| b.submit(PERP, -1, 5.0, 0.0, true, None)),
        step(1, |b| b.submit(SYM, 1, 5.0, 0.0, true, None)),
        step(2, |b| b.submit(SYM, 1, 4.0, 0.0, true, None)),
    ];
    let strategy = OncePerIndex { steps, done: Vec::new(), fills: Vec::new() };
    let mut engine =
        StrategyEngine::new(vec![(SYM.into(), bars.clone()), (PERP.into(), bars)], strategy, p);
    engine.run();
    let reasons: Vec<&str> = engine.core.dropped.iter().map(|d| d.reason.as_str()).collect();
    assert_eq!(reasons, vec![vike_model::OrderDenial::CashOverbuy.template()], "the spot buy of 5");
    let fills: Vec<(&str, i32, f64)> =
        engine.strategy.fills.iter().map(|f| (f.symbol.as_str(), f.side, f.size)).collect();
    assert_eq!(fills, vec![(PERP, -1, 5.0), (SYM, 1, 4.0)]);
}

#[test]
fn a_margin_run_buys_past_its_cash_as_before() {
    let engine = run_funded(false, 100.0, overbuy_script());
    assert!(engine.core.dropped.is_empty(), "nothing refused: {:?}", engine.core.dropped);
    let fills: Vec<(i32, f64)> = engine.strategy.fills.iter().map(|f| (f.side, f.size)).collect();
    assert_eq!(fills, vec![(1, 1.0), (1, 0.99)]);
}

#[test]
fn a_margin_run_sells_past_the_long_as_before() {
    let engine = run(false, oversell_script());
    assert!(engine.core.dropped.is_empty(), "nothing refused: {:?}", engine.core.dropped);
    let fills: Vec<(i32, f64)> = engine.strategy.fills.iter().map(|f| (f.side, f.size)).collect();
    // the buy, the sell of 2 (short 1), then the close of that short
    assert_eq!(fills, vec![(1, 1.0), (-1, 2.0), (1, 1.0)]);
}

#[test]
fn a_spot_ladders_exits_are_released_as_one_claim() {
    let ladder = LadderSpec {
        venue: String::new(),
        symbol: SYM.into(),
        side: 1,
        qty: 3.0,
        entry_price: None,
        stop_loss: 95.0,
        targets: vec![
            LadderTarget { price: 110.0, qty: 1.0 },
            LadderTarget { price: 120.0, qty: 2.0 },
        ],
        account: None,
    };
    let steps = vec![
        step(0, move |b| assert_eq!(b.submit_ladder(&ladder).len(), 4)),
        // the exits rest from the entry's fill: a plain sell beside them finds nothing free
        step(1, |b| b.submit_limit(SYM, -1, 1.0, 130.0, 0.0, true, None)),
    ];
    let engine = run(true, steps);
    let resting: Vec<(OrderKind, Option<f64>, f64)> =
        engine.core.pending_of(SYM).iter().map(|o| (o.kind, o.price, o.size)).collect();
    assert_eq!(
        resting,
        vec![
            (OrderKind::Stop, Some(95.0), 3.0),
            (OrderKind::Limit, Some(110.0), 1.0),
            (OrderKind::Limit, Some(120.0), 2.0),
        ],
        "the stop and both targets, each covering the long of 3"
    );
    assert_eq!(reasons(&engine), vec![refused("0", "1")], "only the plain sell beside them");
}

/// Runs each `(tick ts, action)` from that quote's `on_quote_tick`.
struct TickSteps(Vec<(i64, Act)>);

impl Strategy<OrderBrokerSimulator> for TickSteps {
    fn on_quote_tick(&mut self, ctx: &mut OrderBrokerSimulator, q: &TickQuote) {
        for (at, act) in &self.0 {
            if *at == q.timestamp {
                act(ctx);
            }
        }
    }
}

#[test]
fn a_tagged_quote_in_flight_counts_at_its_new_size() {
    // a long of 2 bought at 1000 lands at 1015 and fills at 1020; at 1030 a quote "a" sells 1 at
    // 110, raised to 2 at 1035, both still in flight (15 ms) when the 1040 actions are judged
    let act = |at: i64, f: fn(&mut OrderBrokerSimulator)| (at, Box::new(f) as Act);
    let steps = TickSteps(vec![
        act(1_000, |b| b.submit(SYM, 1, 2.0, 0.0, true, None)),
        act(1_030, |b| b.place("", "a", -1, 1.0, OrderPlacement::Limit { price: 110.0 }, false)),
        act(1_035, |b| b.amend("a", Some(2.0), None, None)),
        act(1_040, |b| {
            b.amend("a", Some(3.0), None, None); // past the long of 2
            // beside "a" at 2: nothing is free
            b.place("", "b", -1, 1.0, OrderPlacement::Limit { price: 111.0 }, false);
        }),
    ]);
    let p = StrategyEngineConfig {
        cash_symbols: vec![SYM.to_string()],
        latency_model: Some(LatencyModelKind::constant(15_000_000, 0)),
        optimistic_quotes: true,
        ..params()
    };
    let tape: Vec<_> = [1_000, 1_020, 1_030, 1_035, 1_040, 1_100]
        .into_iter()
        .map(|ts| vike_simulator::test_support::quote(SYM, ts, 99.0, 100.0, 1_000.0, 1_000.0))
        .collect();
    let mut engine = StrategyEngine::new(vec![(SYM.into(), Vec::new())], steps, p);
    engine.run_ticks(&[(SYM.to_string(), tape)]);
    assert_eq!(engine.core.position_of(SYM).size, 2.0, "the long of 2 filled");
    let why: Vec<&str> = engine.core.dropped.iter().map(|d| d.reason.as_str()).collect();
    assert_eq!(
        why,
        vec![refused("2", "3"), refused("0", "1")],
        "the raise past 2, the second quote"
    );
}
