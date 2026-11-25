//! `StrategyEngineConfig::slippage_amount`: a fixed PRICE amount against the trader on every fill that is not
//! a limit fill, read through `vike_fills::broker_sim::adverse_fill_amount` by `OrderBrokerSimulator::fill_terms`, the answer
//! `OrderBrokerSimulator::apply_fill` and the cash pre-check of `StrategyEngine::fill_step_gated` both read.
//!
//! What each test pins, on hand-computed fills:
//!
//! - the default is zero and the frozen price survives it bit for bit, the flat fraction included;
//! - market and stop fills pay it, through a gap too, and a limit fill pays nothing: on the coarse,
//!   sub-bar and tick lanes;
//! - every other fill that is not a limit pays it (market-close, trailing, protective stop, a forced
//!   close), a tagged maker quote does not;
//! - the cash pre-check judges the price the fill is charged;
//! - the fraction prices first and the amount is added to its result;
//! - an amount cannot sell a positive quote at or below zero: it saturates, and the run says so.

use vike_fills::broker_sim::{MIN_ADVERSE_FACTOR, adverse_fill_price};
use vike_marketdata::{Bar, TickQuote};
use vike_model::{Fill, OrderBroker, OrderPlacement, Strategy};
use vike_simulator::{FillModelKind, OrderBrokerSimulator, StrategyEngine, StrategyEngineConfig};

const SYM: &str = "TEST";

fn bar(ts: i64, o: f64, h: f64, l: f64, c: f64) -> Bar {
    vike_simulator::test_support::bar(ts, o, h, l, c, 100.0)
}

/// The tagged quote (`Act::Tagged`) fills by the maker law (`StrategyEngineConfig::optimistic_quotes`); no
/// other order here is placed by tag.
fn params(amount: f64) -> StrategyEngineConfig {
    StrategyEngineConfig {
        cash: 100_000.0,
        slippage_amount: amount,
        optimistic_quotes: true,
        ..StrategyEngineConfig::default()
    }
}

#[derive(Clone, Copy)]
enum Act {
    Market(i32, f64),
    MarketClose(i32, f64),
    Limit(i32, f64, f64),
    Stop(i32, f64, f64),
    Trailing(i32, f64, f64),
    Protected(i32, f64, f64),
    Tagged(i32, f64, f64),
}

/// Plays `script` (bar index, action) in order and collects fills.
struct Scripted {
    script: Vec<(usize, Act)>,
    fills: Vec<Fill>,
}

impl Strategy<OrderBrokerSimulator> for Scripted {
    fn on_bar(&mut self, ctx: &mut OrderBrokerSimulator, _bar: &Bar) {
        for (at, act) in self.script.clone() {
            if at != ctx.index {
                continue;
            }
            match act {
                Act::Market(side, size) => ctx.submit(SYM, side, size, 0.0, true, None),
                Act::MarketClose(side, size) => ctx.submit_market_close(SYM, side, size, 0.0, true),
                Act::Limit(side, size, px) => {
                    ctx.submit_limit(SYM, side, size, px, 0.0, true, None)
                }
                Act::Stop(side, size, px) => ctx.submit_stop(SYM, side, size, px, 0.0, true),
                Act::Trailing(side, size, trail) => {
                    ctx.submit_trailing(SYM, side, size, trail, 0.0, true)
                }
                Act::Protected(side, size, stop) => {
                    ctx.submit(SYM, side, size, 0.0, true, Some(stop))
                }
                Act::Tagged(side, size, px) => {
                    ctx.place("", "quote", side, size, OrderPlacement::Limit { price: px }, false)
                }
            }
        }
    }
    fn on_fill(&mut self, _context: &mut OrderBrokerSimulator, fill: &Fill) {
        self.fills.push(fill.clone());
    }
}

fn run(
    script: Vec<(usize, Act)>,
    bars: Vec<Bar>,
    p: StrategyEngineConfig,
) -> StrategyEngine<Scripted> {
    let strat = Scripted { script, fills: Vec::new() };
    let mut engine = StrategyEngine::new(vec![(SYM.into(), bars)], strat, p);
    engine.run();
    engine
}

fn sides_and_prices(engine: &StrategyEngine<Scripted>) -> Vec<(i32, f64)> {
    engine.strategy.fills.iter().map(|f| (f.side, f.price)).collect()
}

/// Flat; a bar that opens at 100 and reaches 99; a bar that gaps down to 90, through a 95 stop.
fn gap_bars() -> Vec<Bar> {
    vec![
        bar(1_000, 100.0, 100.0, 100.0, 100.0),
        bar(2_000, 100.0, 101.0, 99.0, 100.0),
        bar(3_000, 90.0, 91.0, 80.0, 85.0),
    ]
}

/// Buys at market and with a 99 limit on bar 0, then rests a stop for both at 95 on bar 1.
fn entries_then_stop() -> Vec<(usize, Act)> {
    vec![(0, Act::Market(1, 1.0)), (0, Act::Limit(1, 1.0, 99.0)), (1, Act::Stop(-1, 2.0, 95.0))]
}

#[test]
fn the_default_is_zero_and_the_frozen_price_survives_it_bit_for_bit() {
    assert_eq!(StrategyEngineConfig::default().slippage_amount.to_bits(), 0.0_f64.to_bits());
    let p = StrategyEngineConfig { slippage: 0.001, ..params(0.0) };
    let engine = run(entries_then_stop(), gap_bars(), p);
    let want = [
        adverse_fill_price(100.0, 1, 0.001),
        adverse_fill_price(99.0, 1, 0.001),
        adverse_fill_price(90.0, -1, 0.001),
    ];
    let got: Vec<u64> = engine.strategy.fills.iter().map(|f| f.price.to_bits()).collect();
    assert_eq!(got, want.map(f64::to_bits), "{:?}", engine.strategy.fills);
}

#[test]
fn market_and_stop_fills_pay_the_amount_and_a_limit_fill_does_not() {
    let engine = run(entries_then_stop(), gap_bars(), params(0.5));
    assert_eq!(
        sides_and_prices(&engine),
        vec![(1, 100.5), (1, 99.0), (-1, 89.5)],
        "market 100 + 0.5; limit 99 flat; the stop through the gap fills at the open 90, - 0.5"
    );
    assert_eq!(engine.core.position_of(SYM).size, 0.0);
    assert_eq!(
        engine.core.cash,
        100_000.0 - 100.5 - 99.0 + 2.0 * 89.5,
        "the cash is charged it too"
    );
}

#[test]
fn every_other_fill_that_is_not_a_limit_pays_it_and_a_tagged_quote_does_not() {
    let last = |script, p| *sides_and_prices(&run(script, gap_bars(), p)).last().unwrap();
    assert_eq!(
        last(vec![(0, Act::MarketClose(1, 1.0))], params(0.5)),
        (1, 100.5),
        "market-close: the next bar's close 100"
    );
    assert_eq!(
        last(vec![(0, Act::Trailing(-1, 1.0, 5.0))], params(0.5)),
        (-1, 89.5),
        "trailing: its stop is at 96 by the gap bar, which opens at 90"
    );
    assert_eq!(
        last(vec![(0, Act::Protected(1, 1.0, 95.0))], params(0.5)),
        (-1, 89.5),
        "protective stop: through the gap to the open 90"
    );
    let inactive_on_the_gap_bar = StrategyEngineConfig {
        active_mask: Some(vec![(SYM.into(), vec![true, true, false])]),
        ..params(0.5)
    };
    assert_eq!(
        last(vec![(0, Act::Market(1, 1.0))], inactive_on_the_gap_bar),
        (-1, 89.5),
        "a forced close: the membership exit at the open 90"
    );
    assert_eq!(
        last(vec![(0, Act::Tagged(1, 1.0, 99.0))], params(0.5)),
        (1, 99.0),
        "a tagged quote is a limit"
    );
}

#[test]
fn the_cash_pre_check_judges_the_price_the_fill_is_charged() {
    let gated = |cash, amount| StrategyEngineConfig { cash, cash_gate: true, ..params(amount) };
    let flat =
        || vec![bar(1_000, 100.0, 100.0, 100.0, 100.0), bar(2_000, 100.0, 100.0, 100.0, 100.0)];
    let buy = || vec![(0, Act::Market(1, 1.0))];

    let unslipped = run(buy(), flat(), gated(100.25, 0.0));
    assert_eq!(sides_and_prices(&unslipped), vec![(1, 100.0)], "100 is affordable from 100.25");

    let short_by_a_quarter = run(buy(), flat(), gated(100.25, 0.5));
    assert!(short_by_a_quarter.strategy.fills.is_empty(), "100.5 is not");
    assert_eq!(short_by_a_quarter.core.dropped.len(), 1, "and it is dropped in the gate's words");

    let exact = run(buy(), flat(), gated(100.25, 0.25));
    assert_eq!(sides_and_prices(&exact), vec![(1, 100.25)], "100.25 fits exactly");
    assert_eq!(exact.core.cash, 0.0, "and is what the fill charged");
}

#[test]
fn the_sub_bar_lane_charges_it_the_same_way() {
    // Step 1 is three sub-bars: the market buy fills on the first, the stop and the limit on the second.
    let p = StrategyEngineConfig {
        granular_by_symbol: vec![(
            SYM.into(),
            vec![
                bar(2_000, 100.0, 100.0, 100.0, 100.0),
                bar(2_500, 96.0, 96.0, 94.0, 94.5),
                bar(2_750, 94.5, 94.5, 94.5, 94.5),
            ],
        )],
        ..params(0.5)
    };
    let script = vec![
        (0, Act::Market(1, 2.0)),
        (0, Act::Stop(-1, 1.0, 95.0)),
        (0, Act::Limit(1, 1.0, 94.75)),
    ];
    let bars = vec![bar(1_000, 100.0, 100.0, 100.0, 100.0), bar(2_000, 100.0, 100.0, 94.0, 94.5)];
    let engine = run(script, bars, p);
    assert_eq!(sides_and_prices(&engine), vec![(1, 100.5), (-1, 94.5), (1, 94.75)]);
}

/// On the first quote: a market buy and a buy limit at 99.
struct TickFlow {
    armed: bool,
    fills: Vec<Fill>,
}

impl Strategy<OrderBrokerSimulator> for TickFlow {
    fn on_quote_tick(&mut self, ctx: &mut OrderBrokerSimulator, _q: &TickQuote) {
        if !self.armed {
            self.armed = true;
            ctx.submit(SYM, 1, 1.0, 0.0, true, None);
            ctx.submit_limit(SYM, 1, 1.0, 99.0, 0.0, true, None);
        }
    }
    fn on_fill(&mut self, _context: &mut OrderBrokerSimulator, fill: &Fill) {
        self.fills.push(fill.clone());
    }
}

#[test]
fn the_tick_lane_charges_a_market_order_on_top_of_the_ask_and_a_limit_nothing() {
    let quote = |ts, bid, ask| vike_simulator::test_support::quote(SYM, ts, bid, ask, 1.0, 1.0);
    let ticks = vec![(
        SYM.to_string(),
        vec![quote(1_000, 100.0, 101.0), quote(2_000, 100.0, 101.0), quote(3_000, 97.0, 98.0)],
    )];
    let p = StrategyEngineConfig { fill_model: FillModelKind::Tick, ..params(0.5) };
    let strat = TickFlow { armed: false, fills: Vec::new() };
    let mut engine = StrategyEngine::new(vec![(SYM.into(), Vec::new())], strat, p);
    engine.run_ticks(&ticks);
    let got: Vec<f64> = engine.strategy.fills.iter().map(|f| f.price).collect();
    assert_eq!(got, vec![101.5, 98.0], "the ask 101 + 0.5; the limit at the ask 98, flat");
}

#[test]
fn the_fraction_prices_first_and_the_amount_is_added_to_its_result() {
    let p = StrategyEngineConfig { slippage: 0.01, ..params(0.5) };
    let engine = run(vec![(0, Act::Market(1, 1.0))], gap_bars(), p);
    assert_eq!(
        engine.strategy.fills[0].price.to_bits(),
        (adverse_fill_price(100.0, 1, 0.01) + 0.5).to_bits()
    );
}

#[test]
fn an_amount_cannot_sell_a_positive_quote_through_zero() {
    let bars = vec![
        bar(1_000, 1.0, 1.0, 1.0, 1.0),
        bar(2_000, 1.0, 1.0, 1.0, 1.0),
        bar(3_000, 1.0, 1.0, 1.0, 1.0),
    ];
    let script = vec![(0, Act::Market(1, 1.0)), (1, Act::Market(-1, 1.0))];
    let engine = run(script, bars, params(5.0));
    assert_eq!(
        sides_and_prices(&engine),
        vec![(1, 6.0), (-1, MIN_ADVERSE_FACTOR)],
        "1 + 5 buys; 1 - 5 would sell at -4 and saturates at the floor of the quote 1 instead"
    );
    assert_eq!(engine.core.slippage_saturations, 1, "and the run says so");
}
